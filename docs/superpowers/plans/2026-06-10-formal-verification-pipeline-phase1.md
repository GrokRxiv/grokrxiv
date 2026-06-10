# GrokRxiv Formal Verification Workflow (AgentHero DAGOps App Workflow) — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `paper-verify` DAG to GrokRxiv that extracts a structured claim inventory from an already-extracted paper, validates the claim dependency graph, generates Lean 4 statement formalizations, type-checks them with the Lean toolchain, and emits a per-paper formal verification report (PASS / PARTIAL / FAIL per claim, coverage metrics) — runnable as `agh app run grokrxiv formal-verify <PAPER_ID_OR_PATH>`.

**Architecture:** Follows the existing citation-validation pattern exactly: a new DAG manifest with agent nodes (claim extraction, formalization) and deterministic Rust tool nodes (graph validation, Lean type-check, report render). Domain logic lives in a new app-owned crate `grokrxiv-formal`; the app orchestrator only adds thin handler glue and registry entries. The Lean toolchain is an operator-provisioned external dependency (like the `claude`/`gemini` CLIs), overridable via env for tests.

**Tech Stack:** Rust (tokio, serde, anyhow — workspace conventions), Lean 4 + lake (external), JSON Schema contracts, YAML DAG/agent manifests, existing `runner: cli` agent path (operator subscriptions, $0/paper — see memory `cli-path-is-cost-control`).

---

## North Star (the full program)

A new **GrokRxiv agentic team workflow on the AgentHero platform**: AgentHero's DAGOps runtime orchestrates a team of agents (claim extractor, formalizer) and deterministic tools (claim-graph validation, Lean type-check, report render) that together give every paper a continuously updated **verification identity** — claim inventory, formal statements, machine-checked status, and integrity scores — alongside its arXiv identity.

The product boundary does not move: **AgentHero owns** scheduling, DAG execution, workers, runner dispatch, and runtime state; **GrokRxiv owns** this workflow's contracts (DAG manifest, agent YAMLs, prompts, schemas, tool handlers) under `agenthero/apps/grokrxiv/`, exactly like the existing paper-review, citation-validation, and paper-publish workflows it composes with. The "CI/CD pipeline for mathematical truth claims" is what the workflow *delivers*, not a new identity for GrokRxiv — and any future DAGOps app gets the same continuous-verification machinery from the platform for free.

```
arXiv ID     → publication identity
DOI          → citation identity
GrokRxiv ID  → verification identity   (Formal Soundness, Proof Coverage,
                                         Citation Integrity, Semantic Consistency)
```

### Phase roadmap (each later phase gets its own plan once the previous lands)

| Phase | Deliverable | Builds on |
|---|---|---|
| **0** (in this plan) | Green `main`: relocate root `research/` (fails `dag_app_registry` boundary test), fresh branch | — |
| **1** (this plan) | `paper-verify` DAG: claims → graph check → Lean statements → type-check → report artifact | existing `derive_theorems`/`theorem_graph.json`, citation-validation DAG pattern |
| **2** | Persistence & scoring: `grokrxiv_formal_verifications` projection, proof-coverage in review verdict + web UI, cross-paper claim/knowledge-graph tables | Phase 1 report schema |
| **3** | Categorical semantic layer: semantic-spec schema (objects/morphisms/functors/adjunctions/natural transformations), semantic-mapper agent between claim inventory and formalizer, semantic-consistency verifier | Phase 1 claim inventory |
| **4** | Proof generation loop: agentic runner attempts real proofs (replace `sorry`), iterates on compiler feedback. **Depends on resolving the supervisor-runner fork** (orphaned `supervisor_runner/` vs production `agents/runners/cli.rs`) and the Track-D sandbox stub | Phase 1 Lean harness |
| **5** | Verification identity & re-verification: GX-IDs, scheduler re-runs verification when mathlib/model pins change, PR-fixer suggestions to authors | Phases 2–4 |

**Why type-check before proof search:** the hardest problem (per the spec) is Claim Extraction → Semantic Specification, not proof generation. A statement that *elaborates* in Lean proves the pipeline reconstructed the paper's mathematical intent precisely. Phase 1 makes that measurable (formalization rate, elaboration rate) before any proof automation.

### Out of scope for Phase 1 (deliberately)

- No proof search / proof generation (`sorry` bodies are expected; `partial` is the target outcome).
- No categorical semantic layer (Phase 3) — but the claim-inventory schema carries a free-form `semantics` slot so Phase 3 won't need a schema migration.
- No DB migrations/projections, no web UI, no GX score integration (Phase 2).
- No changes to the review/citation/revise DAGs.
- No mathlib proof-cache management beyond an operator-provisioned workspace.

### Ground rules for the executing engineer

- **Docs in this repo are known to drift** (e.g. `docs/DEPLOYMENT.md` references env names that never shipped; `research/` plans reference an RPT3 doc that isn't in the repo). **Code and tests are the only authority.** Every wiring task below starts by reading the neighboring implementation, not docs.
- Conventions (from `agenthero/apps/grokrxiv/CLAUDE.md`, verified current): app-owned code only under `agenthero/apps/grokrxiv/`; root `crates/` stays platform-only; smoke via `agh app run grokrxiv <action>`.
- Pre-existing red test: `dag_app_registry::app_contracts_are_owned_by_app_roots` fails on `main` because root `research/` exists (forbidden list at `crates/orchestrator/tests/dag_app_registry.rs:483-497`). Task 0 fixes it by moving the directory; the other 13 failures in that target are PoisonError cascade from this one panic.

---

## File Structure

```
agenthero/apps/grokrxiv/
  schemas/
    claim_inventory.schema.json                 # NEW — claims with kinds + dependency edges
    formalization.schema.json                   # NEW — Lean statement per claim
    formal_verification_report.schema.json      # NEW — per-claim status + coverage
  crates/formal/                                # NEW crate: grokrxiv-formal
    Cargo.toml
    src/lib.rs                                  # re-exports, shared types
    src/claim_graph.rs                          # duplicate/dangling/cycle validation
    src/lean/mod.rs
    src/lean/workspace.rs                       # writes Claims/<id>.lean into template workspace
    src/lean/runner.rs                          # spawns `lake env lean --json`, env-overridable bins
    src/lean/diagnostics.rs                     # parses lean --json lines → per-claim classification
    src/report.rs                               # report JSON + markdown rendering
    tests/fixtures/fake-lean.sh                 # fake toolchain for unit tests
    tests/lean_check.rs                         # integration tests w/ fake + #[ignore]d live test
  crates/orchestrator/
    src/formal_verify.rs                        # NEW — thin tool-handler glue over grokrxiv-formal
    src/dag_tools.rs                            # MODIFY — register formal_verify::* handlers
    src/lib.rs                                  # MODIFY — `pub mod formal_verify;`
    Cargo.toml                                  # MODIFY — add grokrxiv-formal dep
  dags/paper-verify.yaml                        # NEW DAG manifest
  agents/paper-verify/claim_extractor.yaml      # NEW agent contract
  agents/paper-verify/formalizer.yaml           # NEW agent contract
  prompts/paper-verify/claim_extractor.md       # NEW prompt
  prompts/paper-verify/formalizer.md            # NEW prompt
  app.yaml                                      # MODIFY — add formal-verify action
docs/research/                                  # MOVED from root research/ (Task 0)
```

---

### Task 0: Checkpoint, green main, branch

Per `agenthero/apps/grokrxiv/CLAUDE.md` Plan Run Workflow: checkpoint, revalidate `main`, fresh branch.

**Files:**
- Move: `research/` → `docs/research/`

- [ ] **Step 1: Confirm clean tree** (CLI styling work was committed as `ce2657d`)

Run: `git status --short`
Expected: empty (if not, stop and commit what's there first)

- [ ] **Step 2: Move root research/ to docs/research/**

The boundary test forbids root `research/` but allows anything under `docs/`. The move is wholesale, so intra-directory relative links keep working.

```bash
git mv research docs/research
```

> **Decision point (already defaulted):** if you'd rather keep `research/` at the root, instead remove `"research"` from the forbidden list at `crates/orchestrator/tests/dag_app_registry.rs:490` — but that weakens the app-boundary contract for every future app. The move is the recommended option.

- [ ] **Step 3: Check nothing referenced the old path**

Run: `grep -rn '"research/\|(research/\| research/' --include='*.rs' --include='*.toml' --include='*.json' --include='*.ts' --include='*.tsx' crates agenthero docs/research/site 2>/dev/null | grep -v docs/research`
Expected: no hits pointing at root `research/` (web/site code inside the moved dir is self-relative). Fix any hit by updating the path.

- [ ] **Step 4: Revalidate**

Run: `cargo test -p agenthero-orchestrator --test dag_app_registry`
Expected: PASS (all 16 — the 13 PoisonError cascades disappear with the root cause)

- [ ] **Step 5: Commit and branch**

```bash
git add -A
git commit -m "chore: move research artifacts under docs/ to restore app-boundary contract"
git checkout -b feature/formal-verify-phase1
```

---

### Task 1: Claim inventory schema

**Files:**
- Create: `agenthero/apps/grokrxiv/schemas/claim_inventory.schema.json`

- [ ] **Step 1: Check house schema conventions**

Run: `head -30 agenthero/apps/grokrxiv/schemas/citation_review.schema.json`
Match its `$schema` draft version and id/title style in the next step (adjust the header lines only — the body below stands).

- [ ] **Step 2: Write the schema**

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "ClaimInventory",
  "type": "object",
  "additionalProperties": false,
  "required": ["paper_ref", "claims"],
  "properties": {
    "paper_ref": { "type": "string", "minLength": 1 },
    "claims": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "kind", "statement_text"],
        "properties": {
          "id": { "type": "string", "pattern": "^C[0-9]{3,}$" },
          "kind": {
            "type": "string",
            "enum": ["definition", "lemma", "proposition", "theorem", "corollary", "conjecture"]
          },
          "name": { "type": "string" },
          "statement_text": { "type": "string", "minLength": 1 },
          "statement_tex": { "type": "string" },
          "dependencies": {
            "type": "array",
            "items": { "type": "string", "pattern": "^C[0-9]{3,}$" },
            "default": []
          },
          "references": {
            "type": "array",
            "items": { "type": "string" },
            "default": [],
            "description": "Bibliography keys this claim's proof or statement leans on."
          },
          "source_span": { "type": "string", "description": "Section/heading locator in body.md." },
          "semantics": {
            "type": "object",
            "description": "Reserved for the Phase 3 categorical semantic layer.",
            "additionalProperties": true
          }
        }
      }
    }
  }
}
```

- [ ] **Step 3: Commit**

```bash
git add agenthero/apps/grokrxiv/schemas/claim_inventory.schema.json
git commit -m "feat(formal): add claim inventory schema"
```

---

### Task 2: Formalization and report schemas

**Files:**
- Create: `agenthero/apps/grokrxiv/schemas/formalization.schema.json`
- Create: `agenthero/apps/grokrxiv/schemas/formal_verification_report.schema.json`

- [ ] **Step 1: Write formalization.schema.json**

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "Formalization",
  "type": "object",
  "additionalProperties": false,
  "required": ["paper_ref", "lean_edition", "items"],
  "properties": {
    "paper_ref": { "type": "string", "minLength": 1 },
    "lean_edition": { "type": "string", "enum": ["lean4"] },
    "items": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["claim_id", "formalizable"],
        "properties": {
          "claim_id": { "type": "string", "pattern": "^C[0-9]{3,}$" },
          "formalizable": { "type": "boolean" },
          "skip_reason": {
            "type": "string",
            "description": "Required when formalizable=false: why no Lean statement was attempted."
          },
          "imports": { "type": "array", "items": { "type": "string" }, "default": [] },
          "lean_statement": {
            "type": "string",
            "description": "Complete Lean 4 declaration ending in `:= sorry` (statement-only contract)."
          },
          "notes": { "type": "string" }
        }
      }
    }
  }
}
```

- [ ] **Step 2: Write formal_verification_report.schema.json**

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "FormalVerificationReport",
  "type": "object",
  "additionalProperties": false,
  "required": ["paper_ref", "toolchain", "claims", "coverage"],
  "properties": {
    "paper_ref": { "type": "string" },
    "toolchain": {
      "type": "object",
      "additionalProperties": false,
      "required": ["lean_version"],
      "properties": {
        "lean_version": { "type": "string" },
        "workspace": { "type": "string" }
      }
    },
    "claims": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["claim_id", "status"],
        "properties": {
          "claim_id": { "type": "string" },
          "status": { "type": "string", "enum": ["pass", "partial", "fail", "not_formalized"] },
          "diagnostics": { "type": "array", "items": { "type": "string" }, "default": [] }
        }
      }
    },
    "coverage": {
      "type": "object",
      "additionalProperties": false,
      "required": ["claim_count", "formalized_count", "elaborated_count", "proven_count"],
      "properties": {
        "claim_count": { "type": "integer", "minimum": 0 },
        "formalized_count": { "type": "integer", "minimum": 0 },
        "elaborated_count": { "type": "integer", "minimum": 0, "description": "pass + partial" },
        "proven_count": { "type": "integer", "minimum": 0, "description": "pass only" }
      }
    }
  }
}
```

- [ ] **Step 3: Commit**

```bash
git add agenthero/apps/grokrxiv/schemas/formalization.schema.json agenthero/apps/grokrxiv/schemas/formal_verification_report.schema.json
git commit -m "feat(formal): add formalization and verification report schemas"
```

---

### Task 3: `grokrxiv-formal` crate skeleton + shared types

**Files:**
- Create: `agenthero/apps/grokrxiv/crates/formal/Cargo.toml`
- Create: `agenthero/apps/grokrxiv/crates/formal/src/lib.rs`
- Modify: workspace members list (run `grep -n "crates/verifier" Cargo.toml` at repo root to find where app crates are registered; add `formal` beside `verifier`)

- [ ] **Step 1: Check how a sibling app crate declares itself**

Run: `cat agenthero/apps/grokrxiv/crates/verifier/Cargo.toml`
Mirror its `[package]` metadata style (edition/workspace inheritance) exactly.

- [ ] **Step 2: Write Cargo.toml**

```toml
[package]
name = "grokrxiv-formal"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
description = "GrokRxiv formal verification: claim graphs, Lean workspaces, type-check reports."

[dependencies]
anyhow = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }

[dev-dependencies]
tempfile = "3"
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

(If `tempfile` is already a workspace dep — check with `grep -n tempfile Cargo.toml` at root — use `{ workspace = true }`.)

- [ ] **Step 3: Write src/lib.rs with the shared types (serde mirrors of the Task 1–2 schemas)**

```rust
//! GrokRxiv formal verification domain crate.
//!
//! Pure logic: claim-graph validation, Lean workspace generation, diagnostic
//! parsing, and report building. No DAG or DB awareness — the app
//! orchestrator's `formal_verify` module is the only glue layer.

pub mod claim_graph;
pub mod lean;
pub mod report;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimInventory {
    pub paper_ref: String,
    pub claims: Vec<Claim>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    pub statement_text: String,
    #[serde(default)]
    pub statement_tex: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub source_span: Option<String>,
    #[serde(default)]
    pub semantics: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Formalization {
    pub paper_ref: String,
    pub lean_edition: String,
    pub items: Vec<FormalizationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormalizationItem {
    pub claim_id: String,
    pub formalizable: bool,
    #[serde(default)]
    pub skip_reason: Option<String>,
    #[serde(default)]
    pub imports: Vec<String>,
    #[serde(default)]
    pub lean_statement: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}
```

- [ ] **Step 4: Register the crate and verify it builds**

Add the crate to the workspace members where the other `agenthero/apps/grokrxiv/crates/*` entries live (find with `grep -n "apps/grokrxiv/crates" Cargo.toml`).

Run: `cargo check -p grokrxiv-formal`
Expected: clean

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock agenthero/apps/grokrxiv/crates/formal
git commit -m "feat(formal): scaffold grokrxiv-formal crate with claim/formalization types"
```

---

### Task 4: Claim graph validation (TDD)

**Files:**
- Create: `agenthero/apps/grokrxiv/crates/formal/src/claim_graph.rs`

- [ ] **Step 1: Write the failing tests (in-module)**

```rust
//! Claim dependency graph validation: ids unique, dependencies resolve,
//! and the dependency relation is acyclic (claims may only depend on
//! other claims, ultimately grounding out in definitions).

use std::collections::{BTreeMap, BTreeSet};

use crate::ClaimInventory;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ClaimGraphIssues {
    pub duplicate_ids: Vec<String>,
    pub dangling_dependencies: Vec<String>,
    pub cycles: Vec<Vec<String>>,
}

impl ClaimGraphIssues {
    pub fn is_clean(&self) -> bool {
        self.duplicate_ids.is_empty()
            && self.dangling_dependencies.is_empty()
            && self.cycles.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Claim;

    fn claim(id: &str, deps: &[&str]) -> Claim {
        Claim {
            id: id.into(),
            kind: "theorem".into(),
            name: None,
            statement_text: format!("statement {id}"),
            statement_tex: None,
            dependencies: deps.iter().map(|d| d.to_string()).collect(),
            references: vec![],
            source_span: None,
            semantics: None,
        }
    }

    fn inventory(claims: Vec<Claim>) -> ClaimInventory {
        ClaimInventory { paper_ref: "p1".into(), claims }
    }

    #[test]
    fn clean_dag_has_no_issues() {
        let inv = inventory(vec![claim("C001", &[]), claim("C002", &["C001"])]);
        assert!(validate(&inv).is_clean());
    }

    #[test]
    fn duplicate_ids_are_reported() {
        let inv = inventory(vec![claim("C001", &[]), claim("C001", &[])]);
        assert_eq!(validate(&inv).duplicate_ids, vec!["C001".to_string()]);
    }

    #[test]
    fn dangling_dependency_is_reported() {
        let inv = inventory(vec![claim("C002", &["C999"])]);
        assert_eq!(
            validate(&inv).dangling_dependencies,
            vec!["C002 -> C999".to_string()]
        );
    }

    #[test]
    fn cycle_is_reported() {
        let inv = inventory(vec![claim("C001", &["C002"]), claim("C002", &["C001"])]);
        let issues = validate(&inv);
        assert_eq!(issues.cycles.len(), 1);
        assert!(issues.cycles[0].contains(&"C001".to_string()));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod claim_graph;` is already in lib.rs (Task 3). Run:
`cargo test -p grokrxiv-formal claim_graph`
Expected: FAIL — `validate` not found

- [ ] **Step 3: Implement `validate`**

```rust
pub fn validate(inventory: &ClaimInventory) -> ClaimGraphIssues {
    let mut seen = BTreeSet::new();
    let mut duplicate_ids = Vec::new();
    for claim in &inventory.claims {
        if !seen.insert(claim.id.clone()) && !duplicate_ids.contains(&claim.id) {
            duplicate_ids.push(claim.id.clone());
        }
    }

    let ids: BTreeSet<&str> = inventory.claims.iter().map(|c| c.id.as_str()).collect();
    let mut dangling_dependencies = Vec::new();
    for claim in &inventory.claims {
        for dep in &claim.dependencies {
            if !ids.contains(dep.as_str()) {
                dangling_dependencies.push(format!("{} -> {dep}", claim.id));
            }
        }
    }

    // Iterative three-color DFS over the dependency relation.
    let adjacency: BTreeMap<&str, Vec<&str>> = inventory
        .claims
        .iter()
        .map(|c| {
            (
                c.id.as_str(),
                c.dependencies
                    .iter()
                    .map(String::as_str)
                    .filter(|d| ids.contains(*d))
                    .collect(),
            )
        })
        .collect();
    let mut color: BTreeMap<&str, u8> = BTreeMap::new(); // 0 white, 1 grey, 2 black
    let mut cycles: Vec<Vec<String>> = Vec::new();

    fn dfs<'a>(
        node: &'a str,
        adjacency: &BTreeMap<&'a str, Vec<&'a str>>,
        color: &mut BTreeMap<&'a str, u8>,
        stack: &mut Vec<&'a str>,
        cycles: &mut Vec<Vec<String>>,
    ) {
        color.insert(node, 1);
        stack.push(node);
        for next in adjacency.get(node).into_iter().flatten() {
            match color.get(next).copied().unwrap_or(0) {
                0 => dfs(next, adjacency, color, stack, cycles),
                1 => {
                    let start = stack.iter().position(|n| n == next).unwrap_or(0);
                    cycles.push(stack[start..].iter().map(|s| s.to_string()).collect());
                }
                _ => {}
            }
        }
        stack.pop();
        color.insert(node, 2);
    }

    for claim in &inventory.claims {
        if color.get(claim.id.as_str()).copied().unwrap_or(0) == 0 {
            let mut stack = Vec::new();
            dfs(claim.id.as_str(), &adjacency, &mut color, &mut stack, &mut cycles);
        }
    }

    ClaimGraphIssues { duplicate_ids, dangling_dependencies, cycles }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p grokrxiv-formal claim_graph`
Expected: 4 passed

- [ ] **Step 5: Commit**

```bash
git add agenthero/apps/grokrxiv/crates/formal/src/claim_graph.rs
git commit -m "feat(formal): validate claim dependency graphs"
```

---

### Task 5: Lean diagnostics parser (TDD)

**Files:**
- Create: `agenthero/apps/grokrxiv/crates/formal/src/lean/mod.rs`
- Create: `agenthero/apps/grokrxiv/crates/formal/src/lean/diagnostics.rs`

- [ ] **Step 1: Create lean/mod.rs**

```rust
//! Lean toolchain integration: workspace generation, process execution,
//! and diagnostic classification.

pub mod diagnostics;
pub mod runner;
pub mod workspace;
```

(`runner`/`workspace` files arrive in Tasks 6–7; create empty files `src/lean/runner.rs` and `src/lean/workspace.rs` now so the module compiles: each just `//! placeholder filled by Tasks 6-7` — they gain content within this plan, which is not a TBD escape hatch.)

- [ ] **Step 2: Write failing tests in diagnostics.rs**

`lean --json` emits one JSON object per line, e.g.
`{"severity":"error","pos":{"line":3,"column":10},"endPos":null,"data":"unknown identifier 'Grp'","fileName":"Claims/C001.lean"}`. A `sorry` body elaborates with a warning containing `declaration uses 'sorry'`.

```rust
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Pass,
    Partial,
    Fail,
    NotFormalized,
}

#[derive(Debug, Deserialize)]
struct LeanDiagnostic {
    severity: String,
    #[serde(default)]
    data: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_means_fail() {
        let out = r#"{"severity":"error","data":"unknown identifier 'Grp'"}"#;
        let (status, diags) = classify(out);
        assert_eq!(status, ClaimStatus::Fail);
        assert_eq!(diags, vec!["error: unknown identifier 'Grp'".to_string()]);
    }

    #[test]
    fn sorry_warning_means_partial() {
        let out = r#"{"severity":"warning","data":"declaration uses 'sorry'"}"#;
        assert_eq!(classify(out).0, ClaimStatus::Partial);
    }

    #[test]
    fn clean_output_means_pass() {
        assert_eq!(classify("").0, ClaimStatus::Pass);
    }

    #[test]
    fn non_json_noise_is_kept_as_diagnostic_without_failing() {
        let out = "warning: building configuration\n{\"severity\":\"warning\",\"data\":\"declaration uses 'sorry'\"}";
        let (status, diags) = classify(out);
        assert_eq!(status, ClaimStatus::Partial);
        assert!(diags.iter().any(|d| d.contains("building configuration")));
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p grokrxiv-formal diagnostics`
Expected: FAIL — `classify` not found

- [ ] **Step 4: Implement `classify`**

```rust
/// Classify one claim file's `lean --json` stdout.
///
/// fail    — any error-severity diagnostic
/// partial — elaborates, but the proof body is `sorry`
/// pass    — elaborates with a complete proof
pub fn classify(stdout: &str) -> (ClaimStatus, Vec<String>) {
    let mut diagnostics = Vec::new();
    let mut has_error = false;
    let mut has_sorry = false;
    for line in stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match serde_json::from_str::<LeanDiagnostic>(line) {
            Ok(diag) => {
                if diag.severity == "error" {
                    has_error = true;
                }
                if diag.data.contains("declaration uses 'sorry'") {
                    has_sorry = true;
                }
                diagnostics.push(format!("{}: {}", diag.severity, diag.data));
            }
            // Lake/lean prelude noise is not a verdict signal; keep it for audit.
            Err(_) => diagnostics.push(line.to_string()),
        }
    }
    let status = if has_error {
        ClaimStatus::Fail
    } else if has_sorry {
        ClaimStatus::Partial
    } else {
        ClaimStatus::Pass
    };
    (status, diagnostics)
}
```

- [ ] **Step 5: Run tests, then commit**

Run: `cargo test -p grokrxiv-formal diagnostics` — Expected: 4 passed

```bash
git add agenthero/apps/grokrxiv/crates/formal/src/lean
git commit -m "feat(formal): classify lean --json diagnostics into claim statuses"
```

---

### Task 6: Lean workspace writer (TDD)

**Files:**
- Modify: `agenthero/apps/grokrxiv/crates/formal/src/lean/workspace.rs`

The runner does **not** create Lean projects from scratch (mathlib builds take ~hours). The operator provisions a template workspace once (`lake new`, optional mathlib + `lake exe cache get`) and points `GROKRXIV_LEAN_WORKSPACE` at it. We only write claim files into `Claims/` inside a copy-on-run scratch dir.

- [ ] **Step 1: Write failing tests**

```rust
//! Writes one `Claims/<claim_id>.lean` file per formalizable item into a
//! run-scoped directory inside the operator-provisioned Lean workspace.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::Formalization;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FormalizationItem;

    fn item(claim_id: &str, statement: Option<&str>) -> FormalizationItem {
        FormalizationItem {
            claim_id: claim_id.into(),
            formalizable: statement.is_some(),
            skip_reason: statement.is_none().then(|| "prose-only".into()),
            imports: vec![],
            lean_statement: statement.map(|s| s.to_string()),
            notes: None,
        }
    }

    #[test]
    fn writes_one_file_per_formalizable_claim() {
        let tmp = tempfile::tempdir().unwrap();
        let formalization = Formalization {
            paper_ref: "p1".into(),
            lean_edition: "lean4".into(),
            items: vec![
                item("C001", Some("theorem c001 : 1 + 1 = 2 := sorry")),
                item("C002", None),
            ],
        };
        let written = write_claim_files(tmp.path(), &formalization).unwrap();
        assert_eq!(written, vec![(String::from("C001"), tmp.path().join("Claims/C001.lean"))]);
        let body = std::fs::read_to_string(tmp.path().join("Claims/C001.lean")).unwrap();
        assert!(body.contains("theorem c001"));
    }

    #[test]
    fn imports_are_prepended() {
        let tmp = tempfile::tempdir().unwrap();
        let mut it = item("C001", Some("theorem t : True := sorry"));
        it.imports = vec!["Mathlib.Order.Basic".into()];
        let formalization = Formalization {
            paper_ref: "p1".into(),
            lean_edition: "lean4".into(),
            items: vec![it],
        };
        write_claim_files(tmp.path(), &formalization).unwrap();
        let body = std::fs::read_to_string(tmp.path().join("Claims/C001.lean")).unwrap();
        assert!(body.starts_with("import Mathlib.Order.Basic\n"));
    }

    #[test]
    fn rejects_path_traversal_in_claim_id() {
        let tmp = tempfile::tempdir().unwrap();
        let formalization = Formalization {
            paper_ref: "p1".into(),
            lean_edition: "lean4".into(),
            items: vec![item("../evil", Some("theorem t : True := sorry"))],
        };
        assert!(write_claim_files(tmp.path(), &formalization).is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p grokrxiv-formal workspace` — Expected: FAIL, `write_claim_files` not found

- [ ] **Step 3: Implement**

```rust
/// Write `Claims/<claim_id>.lean` for every formalizable item.
/// Returns `(claim_id, file_path)` pairs in input order.
pub fn write_claim_files(
    workspace: &Path,
    formalization: &Formalization,
) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let claims_dir = workspace.join("Claims");
    std::fs::create_dir_all(&claims_dir).context("create Claims dir")?;
    let mut written = Vec::new();
    for item in formalization.items.iter().filter(|i| i.formalizable) {
        if !item
            .claim_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            anyhow::bail!("claim id `{}` is not a safe file name", item.claim_id);
        }
        let statement = item.lean_statement.as_deref().ok_or_else(|| {
            anyhow::anyhow!("claim `{}` is formalizable but has no lean_statement", item.claim_id)
        })?;
        let mut body = String::new();
        for import in &item.imports {
            body.push_str(&format!("import {import}\n"));
        }
        if !item.imports.is_empty() {
            body.push('\n');
        }
        body.push_str(statement);
        body.push('\n');
        let path = claims_dir.join(format!("{}.lean", item.claim_id));
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        written.push((item.claim_id.clone(), path));
    }
    Ok(written)
}
```

- [ ] **Step 4: Run tests, then commit**

Run: `cargo test -p grokrxiv-formal workspace` — Expected: 3 passed

```bash
git add agenthero/apps/grokrxiv/crates/formal/src/lean/workspace.rs
git commit -m "feat(formal): write per-claim lean files into the operator workspace"
```

---

### Task 7: Lean runner with env-overridable binaries (TDD)

**Files:**
- Modify: `agenthero/apps/grokrxiv/crates/formal/src/lean/runner.rs`
- Create: `agenthero/apps/grokrxiv/crates/formal/tests/fixtures/fake-lean.sh`
- Create: `agenthero/apps/grokrxiv/crates/formal/tests/lean_check.rs`

Pattern precedent: `ClaudeRunner::with_binary` in `agenthero/apps/grokrxiv/crates/orchestrator/src/supervisor_runner/claude.rs` (env-overridable binary, stdin/stdout contract, timeout). Read it before starting.

- [ ] **Step 1: Write the fake toolchain fixture**

`tests/fixtures/fake-lean.sh` (mark executable):

```bash
#!/bin/sh
# Fake `lake` for tests. Invoked as: lake env lean --json <file>
# Emits diagnostics keyed on the claim file name so tests can steer outcomes.
case "$4" in
  *FAIL*)    echo '{"severity":"error","data":"unknown identifier"}' ;;
  *PARTIAL*) echo '{"severity":"warning","data":"declaration uses '"'"'sorry'"'"'"}' ;;
  *)         : ;; # pass: no diagnostics
esac
exit 0
```

Run: `chmod +x agenthero/apps/grokrxiv/crates/formal/tests/fixtures/fake-lean.sh`

- [ ] **Step 2: Write failing integration test `tests/lean_check.rs`**

```rust
use grokrxiv_formal::lean::diagnostics::ClaimStatus;
use grokrxiv_formal::lean::runner::LeanRunner;
use grokrxiv_formal::{Formalization, FormalizationItem};

fn item(claim_id: &str) -> FormalizationItem {
    FormalizationItem {
        claim_id: claim_id.into(),
        formalizable: true,
        skip_reason: None,
        imports: vec![],
        lean_statement: Some(format!("theorem {} : True := sorry", claim_id.to_lowercase())),
        notes: None,
    }
}

fn fixture_runner(workspace: &std::path::Path) -> LeanRunner {
    let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-lean.sh");
    LeanRunner::with_lake_binary(fake.to_string_lossy().into_owned(), workspace.to_path_buf())
}

#[tokio::test]
async fn classifies_each_claim_file_independently() {
    let tmp = tempfile::tempdir().unwrap();
    let formalization = Formalization {
        paper_ref: "p1".into(),
        lean_edition: "lean4".into(),
        items: vec![item("CPASS001"), item("CPARTIAL002"), item("CFAIL003")],
    };
    let results = fixture_runner(tmp.path()).check(&formalization).await.unwrap();
    let by_id: std::collections::BTreeMap<_, _> =
        results.iter().map(|r| (r.claim_id.as_str(), r.status)).collect();
    assert_eq!(by_id["CPASS001"], ClaimStatus::Pass);
    assert_eq!(by_id["CPARTIAL002"], ClaimStatus::Partial);
    assert_eq!(by_id["CFAIL003"], ClaimStatus::Fail);
}

#[tokio::test]
async fn non_formalizable_claims_are_reported_not_formalized() {
    let tmp = tempfile::tempdir().unwrap();
    let formalization = Formalization {
        paper_ref: "p1".into(),
        lean_edition: "lean4".into(),
        items: vec![FormalizationItem {
            claim_id: "C001".into(),
            formalizable: false,
            skip_reason: Some("prose-only".into()),
            imports: vec![],
            lean_statement: None,
            notes: None,
        }],
    };
    let results = fixture_runner(tmp.path()).check(&formalization).await.unwrap();
    assert_eq!(results[0].status, ClaimStatus::NotFormalized);
}

/// Live smoke against a real toolchain. Operators opt in:
/// `GROKRXIV_LEAN_WORKSPACE=~/lean/grokrxiv-verify cargo test -p grokrxiv-formal -- --ignored`
#[tokio::test]
#[ignore = "requires elan/lake on PATH and GROKRXIV_LEAN_WORKSPACE"]
async fn live_toolchain_smoke() {
    let workspace = std::env::var("GROKRXIV_LEAN_WORKSPACE").expect("set GROKRXIV_LEAN_WORKSPACE");
    let formalization = Formalization {
        paper_ref: "smoke".into(),
        lean_edition: "lean4".into(),
        items: vec![item("CSMOKE001")],
    };
    let results = LeanRunner::from_env(workspace.into())
        .check(&formalization)
        .await
        .unwrap();
    assert_eq!(results[0].status, ClaimStatus::Partial); // sorry body type-checks
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p grokrxiv-formal --test lean_check`
Expected: FAIL — `LeanRunner` not found

- [ ] **Step 4: Implement runner.rs**

```rust
//! Spawns `lake env lean --json <claim file>` per claim inside the
//! operator-provisioned workspace. Binary override order mirrors the
//! CLI-runner convention (`AGENTHERO_CLAUDE_BIN` et al.).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use tokio::process::Command;
use tokio::time::timeout;

use super::diagnostics::{classify, ClaimStatus};
use super::workspace::write_claim_files;
use crate::Formalization;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClaimCheckResult {
    pub claim_id: String,
    pub status: ClaimStatus,
    pub diagnostics: Vec<String>,
}

pub struct LeanRunner {
    lake_binary: String,
    workspace: PathBuf,
    pub timeout: Duration,
}

impl LeanRunner {
    /// Resolve `lake` from `GROKRXIV_LAKE_BIN` or PATH.
    pub fn from_env(workspace: PathBuf) -> Self {
        Self {
            lake_binary: std::env::var("GROKRXIV_LAKE_BIN").unwrap_or_else(|_| "lake".into()),
            workspace,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn with_lake_binary(lake_binary: String, workspace: PathBuf) -> Self {
        Self { lake_binary, workspace, timeout: DEFAULT_TIMEOUT }
    }

    pub async fn check(&self, formalization: &Formalization) -> anyhow::Result<Vec<ClaimCheckResult>> {
        let written = write_claim_files(&self.workspace, formalization)?;
        let mut results = Vec::new();
        for item in &formalization.items {
            if !item.formalizable {
                results.push(ClaimCheckResult {
                    claim_id: item.claim_id.clone(),
                    status: ClaimStatus::NotFormalized,
                    diagnostics: item.skip_reason.clone().into_iter().collect(),
                });
                continue;
            }
            let path = written
                .iter()
                .find(|(id, _)| *id == item.claim_id)
                .map(|(_, p)| p.clone())
                .context("claim file missing after write")?;
            let output = timeout(
                self.timeout,
                Command::new(&self.lake_binary)
                    .arg("env")
                    .arg("lean")
                    .arg("--json")
                    .arg(&path)
                    .current_dir(&self.workspace)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output(),
            )
            .await
            .with_context(|| format!("lean check timed out for {}", item.claim_id))?
            .with_context(|| format!("spawn `{}`", self.lake_binary))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let (status, diagnostics) = classify(&stdout);
            results.push(ClaimCheckResult { claim_id: item.claim_id.clone(), status, diagnostics });
        }
        Ok(results)
    }
}
```

- [ ] **Step 5: Run tests, then commit**

Run: `cargo test -p grokrxiv-formal --test lean_check`
Expected: 2 passed, 1 ignored

```bash
git add agenthero/apps/grokrxiv/crates/formal
git commit -m "feat(formal): lean runner with env-overridable lake binary"
```

---

### Task 8: Report builder (TDD)

**Files:**
- Create: `agenthero/apps/grokrxiv/crates/formal/src/report.rs`

- [ ] **Step 1: Write failing test**

```rust
//! Builds the formal verification report (JSON contract: schemas/
//! formal_verification_report.schema.json) and its markdown rendering.

use serde::Serialize;

use crate::lean::diagnostics::ClaimStatus;
use crate::lean::runner::ClaimCheckResult;
use crate::ClaimInventory;

#[derive(Debug, Serialize)]
pub struct Report {
    pub paper_ref: String,
    pub toolchain: Toolchain,
    pub claims: Vec<ClaimCheckResult>,
    pub coverage: Coverage,
}

#[derive(Debug, Serialize)]
pub struct Toolchain {
    pub lean_version: String,
    pub workspace: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Coverage {
    pub claim_count: usize,
    pub formalized_count: usize,
    pub elaborated_count: usize,
    pub proven_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(id: &str, status: ClaimStatus) -> ClaimCheckResult {
        ClaimCheckResult { claim_id: id.into(), status, diagnostics: vec![] }
    }

    #[test]
    fn coverage_counts_statuses() {
        let results = vec![
            result("C001", ClaimStatus::Pass),
            result("C002", ClaimStatus::Partial),
            result("C003", ClaimStatus::Fail),
            result("C004", ClaimStatus::NotFormalized),
        ];
        assert_eq!(
            coverage(&results),
            Coverage { claim_count: 4, formalized_count: 3, elaborated_count: 2, proven_count: 1 }
        );
    }

    #[test]
    fn markdown_lists_every_claim_with_status() {
        let inv = ClaimInventory { paper_ref: "p1".into(), claims: vec![] };
        let report = Report {
            paper_ref: "p1".into(),
            toolchain: Toolchain { lean_version: "4.x".into(), workspace: "/w".into() },
            claims: vec![result("C001", ClaimStatus::Partial)],
            coverage: coverage(&[result("C001", ClaimStatus::Partial)]),
        };
        let md = render_markdown(&report, &inv);
        assert!(md.contains("# Formal Verification Report"));
        assert!(md.contains("C001"));
        assert!(md.contains("partial"));
        assert!(md.contains("1/1 formalized"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p grokrxiv-formal report` — Expected: FAIL

- [ ] **Step 3: Implement**

```rust
pub fn coverage(results: &[ClaimCheckResult]) -> Coverage {
    Coverage {
        claim_count: results.len(),
        formalized_count: results
            .iter()
            .filter(|r| r.status != ClaimStatus::NotFormalized)
            .count(),
        elaborated_count: results
            .iter()
            .filter(|r| matches!(r.status, ClaimStatus::Pass | ClaimStatus::Partial))
            .count(),
        proven_count: results.iter().filter(|r| r.status == ClaimStatus::Pass).count(),
    }
}

pub fn render_markdown(report: &Report, inventory: &ClaimInventory) -> String {
    let mut md = String::from("# Formal Verification Report\n\n");
    md.push_str(&format!("paper: `{}`\n\n", report.paper_ref));
    md.push_str(&format!(
        "**Coverage:** {}/{} formalized, {} elaborated, {} proven\n\n",
        report.coverage.formalized_count,
        report.coverage.claim_count,
        report.coverage.elaborated_count,
        report.coverage.proven_count
    ));
    md.push_str("| Claim | Kind | Status |\n|---|---|---|\n");
    for claim_result in &report.claims {
        let kind = inventory
            .claims
            .iter()
            .find(|c| c.id == claim_result.claim_id)
            .map(|c| c.kind.as_str())
            .unwrap_or("?");
        let status = serde_json::to_value(claim_result.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        md.push_str(&format!("| {} | {} | {} |\n", claim_result.claim_id, kind, status));
    }
    md
}
```

- [ ] **Step 4: Run tests, then commit**

Run: `cargo test -p grokrxiv-formal report` — Expected: 2 passed

```bash
git add agenthero/apps/grokrxiv/crates/formal/src/report.rs
git commit -m "feat(formal): build verification report with coverage metrics"
```

---

### Task 9: Orchestrator handler glue + tool registry

**Files:**
- Create: `agenthero/apps/grokrxiv/crates/orchestrator/src/formal_verify.rs`
- Modify: `agenthero/apps/grokrxiv/crates/orchestrator/src/dag_tools.rs` (add three `RustToolDescriptor` entries)
- Modify: `agenthero/apps/grokrxiv/crates/orchestrator/src/lib.rs` (add `pub mod formal_verify;`)
- Modify: `agenthero/apps/grokrxiv/crates/orchestrator/Cargo.toml` (add `grokrxiv-formal = { path = "../formal" }`)

- [ ] **Step 1: Discover the executable handler contract (do not trust this plan or docs — read code)**

The registry (`dag_tools.rs`) maps handler *names* to modules; the execution side lives elsewhere. Find it:

Run: `grep -rn "citation_validation::bibtex_reference_parser\|bibtex_reference_parser" agenthero/apps/grokrxiv/crates/orchestrator/src/ agenthero/apps/grokrxiv/rust/src/ | grep -v dag_tools`

Read the dispatch site and one handler implementation end-to-end. Note: (a) the exact function signature (sync/async, artifact-dir args vs typed inputs), (b) how a handler reads its declared `inputs` and writes its declared `outputs`.

- [ ] **Step 2: Register the three handlers in dag_tools.rs**

Append to `RUST_TOOL_HANDLERS` (after the `citation_validation::*` block):

```rust
    RustToolDescriptor {
        handler: "formal_verify::claim_graph_check",
        module: "formal_verify",
        description: "Validate the extracted claim inventory: unique ids, resolvable dependencies, acyclic graph.",
    },
    RustToolDescriptor {
        handler: "formal_verify::lean_check",
        module: "formal_verify",
        description: "Type-check Lean statement formalizations and classify each claim pass/partial/fail.",
    },
    RustToolDescriptor {
        handler: "formal_verify::report_render",
        module: "formal_verify",
        description: "Render the formal verification report markdown from the report JSON.",
    },
```

- [ ] **Step 3: Implement formal_verify.rs**

The core bodies below are fixed; adapt only the outer fn signatures to what Step 1 found (the citation_validation handlers are the template):

```rust
//! DAG tool handlers for the paper-verify DAG. Thin glue over
//! `grokrxiv-formal`; all real logic and its tests live in that crate.

use std::path::Path;

use anyhow::Context;
use grokrxiv_formal::lean::runner::LeanRunner;
use grokrxiv_formal::report::{coverage, render_markdown, Report, Toolchain};
use grokrxiv_formal::{claim_graph, ClaimInventory, Formalization};

/// inputs: formal_verify/claims.json
/// outputs: formal_verify/claim_graph.json (issues; node fails when not clean)
pub fn claim_graph_check(inputs_dir: &Path, outputs_dir: &Path) -> anyhow::Result<()> {
    let inventory: ClaimInventory = read_json(&inputs_dir.join("formal_verify/claims.json"))?;
    let issues = claim_graph::validate(&inventory);
    write_json(&outputs_dir.join("formal_verify/claim_graph.json"), &issues)?;
    anyhow::ensure!(
        issues.is_clean(),
        "claim graph invalid: {} duplicate ids, {} dangling deps, {} cycles",
        issues.duplicate_ids.len(),
        issues.dangling_dependencies.len(),
        issues.cycles.len()
    );
    Ok(())
}

/// inputs: formal_verify/claims.json, formal_verify/formalization.json
/// outputs: formal_verify/formal_verification_report.json
pub async fn lean_check(inputs_dir: &Path, outputs_dir: &Path) -> anyhow::Result<()> {
    let inventory: ClaimInventory = read_json(&inputs_dir.join("formal_verify/claims.json"))?;
    let formalization: Formalization =
        read_json(&inputs_dir.join("formal_verify/formalization.json"))?;
    let workspace = std::env::var("GROKRXIV_LEAN_WORKSPACE").context(
        "GROKRXIV_LEAN_WORKSPACE is not set. Provision a Lean 4 lake workspace \
         (lake new grokrxiv-verify; optionally add mathlib + `lake exe cache get`) \
         and point GROKRXIV_LEAN_WORKSPACE at it.",
    )?;
    let runner = LeanRunner::from_env(workspace.clone().into());
    let results = runner.check(&formalization).await?;
    let report = Report {
        paper_ref: inventory.paper_ref.clone(),
        toolchain: Toolchain {
            lean_version: lean_version().unwrap_or_else(|| "unknown".into()),
            workspace,
        },
        coverage: coverage(&results),
        claims: results,
    };
    write_json(
        &outputs_dir.join("formal_verify/formal_verification_report.json"),
        &report,
    )
}

/// inputs: formal_verify/claims.json, formal_verify/formal_verification_report.json
/// outputs: formal-verification.md
pub fn report_render(inputs_dir: &Path, outputs_dir: &Path) -> anyhow::Result<()> {
    let inventory: ClaimInventory = read_json(&inputs_dir.join("formal_verify/claims.json"))?;
    let report: serde_json::Value =
        read_json(&inputs_dir.join("formal_verify/formal_verification_report.json"))?;
    let report: grokrxiv_formal::report::Report = serde_json::from_value(report)?;
    std::fs::write(
        outputs_dir.join("formal-verification.md"),
        render_markdown(&report, &inventory),
    )
    .context("write formal-verification.md")
}

fn lean_version() -> Option<String> {
    let bin = std::env::var("GROKRXIV_LAKE_BIN").unwrap_or_else(|_| "lake".into());
    let out = std::process::Command::new(bin).arg("--version").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)?)
        .with_context(|| format!("write {}", path.display()))
}
```

Note: `Report` needs `Deserialize` for `report_render` — add `#[derive(serde::Deserialize)]` to `Report`, `Toolchain`, `Coverage`, `ClaimCheckResult`, and `ClaimStatus` in `grokrxiv-formal` while wiring this (one-line derives, covered by existing tests).

- [ ] **Step 4: Verify the registry test still passes and the crate builds**

Run: `cargo test -p grokrxiv-app-runtime dag_tools 2>/dev/null || cargo test -p $(grep '^name' agenthero/apps/grokrxiv/crates/orchestrator/Cargo.toml | head -1 | cut -d'"' -f2) dag_tools`
Expected: registry unit tests pass with the three new descriptors

- [ ] **Step 5: Commit**

```bash
git add agenthero/apps/grokrxiv/crates/orchestrator agenthero/apps/grokrxiv/crates/formal
git commit -m "feat(formal): register formal_verify DAG tool handlers"
```

---

### Task 10: DAG manifest, agent contracts, prompts

**Files:**
- Create: `agenthero/apps/grokrxiv/dags/paper-verify.yaml`
- Create: `agenthero/apps/grokrxiv/agents/paper-verify/claim_extractor.yaml`
- Create: `agenthero/apps/grokrxiv/agents/paper-verify/formalizer.yaml`
- Create: `agenthero/apps/grokrxiv/prompts/paper-verify/claim_extractor.md`
- Create: `agenthero/apps/grokrxiv/prompts/paper-verify/formalizer.md`

- [ ] **Step 1: Confirm agent-node output contract against code**

Read `crates/dag-runtime/src/` manifest node structs and `agenthero/apps/grokrxiv/dags/citation-validation.yaml`'s `citation_validation_adjudicator` node (an agent-backed node with explicit file `inputs`/`outputs`). Confirm whether `kind: agent` nodes accept `outputs:`; if they don't, model both agent nodes as the adjudicator does (`kind: verify`-style with declared files) and keep the same node ids. Also confirm `extractor` is a valid `AgentKind` (it is referenced in `agents/config.rs` tests at `agenthero/apps/grokrxiv/crates/orchestrator/src/agents/config.rs:579`).

- [ ] **Step 2: Write dags/paper-verify.yaml**

```yaml
id: paper-verify
version: 1
accepts:
  - extractor
concurrency: 1
tools:
  - id: claim_graph_check
    executor: rust
    handler: formal_verify::claim_graph_check
    timeout_secs: 30
  - id: lean_check
    executor: rust
    handler: formal_verify::lean_check
    timeout_secs: 1800
  - id: report_render
    executor: rust
    handler: formal_verify::report_render
    timeout_secs: 30
roles:
  - id: claim_extractor
    kind: extractor
    config: agents/paper-verify/claim_extractor.yaml
  - id: formalizer
    kind: extractor
    config: agents/paper-verify/formalizer.yaml
nodes:
  - id: claim_extractor
    kind: agent
    role: claim_extractor
    inputs: [body.md, theorem_graph.json, references.json]
    outputs: [formal_verify/claims.json]
    required: true
  - id: claim_graph_check
    kind: tool
    tool: claim_graph_check
    inputs: [formal_verify/claims.json]
    outputs: [formal_verify/claim_graph.json]
    required: true
  - id: formalizer
    kind: agent
    role: formalizer
    inputs: [formal_verify/claims.json]
    outputs: [formal_verify/formalization.json]
    required: true
  - id: lean_check
    kind: tool
    tool: lean_check
    inputs: [formal_verify/claims.json, formal_verify/formalization.json]
    outputs: [formal_verify/formal_verification_report.json]
    required: true
  - id: report_render
    kind: tool
    tool: report_render
    inputs: [formal_verify/claims.json, formal_verify/formal_verification_report.json]
    outputs: [formal-verification.md]
    required: true
edges:
  - from: claim_extractor
    to: claim_graph_check
  - from: claim_graph_check
    to: formalizer
  - from: formalizer
    to: lean_check
  - from: lean_check
    to: report_render
```

- [ ] **Step 3: Write the agent contracts** (field set mirrors `agents/paper-review/citation.yaml`, verified current)

`agents/paper-verify/claim_extractor.yaml`:

```yaml
id: claim_extractor
kind: extractor
role: "Extract every definition, lemma, theorem, proposition, corollary, and conjecture as a structured claim with dependencies."
provider: gemini
# Override with GROKRXIV_CLAIM_EXTRACTOR_MODEL or `--model-for claim_extractor=<model>`.
model: gemini-2.5-pro
runner: cli
prompt_template: prompts/paper-verify/claim_extractor.md
input_schema: schemas/paper_extract.schema.json
output_schema: schemas/claim_inventory.schema.json
verifiers:
  - json_schema
prompt_context:
  body_budget_chars: 120000
  bibliography: limited
  max_bibliography_entries: 24
max_retries: 2
timeout_secs: 600
escalation: agent
```

`agents/paper-verify/formalizer.yaml`:

```yaml
id: formalizer
kind: extractor
role: "Translate each claim into a single self-contained Lean 4 declaration ending in `:= sorry`; mark prose-only claims as not formalizable."
provider: claude
# Override with GROKRXIV_FORMALIZER_MODEL or `--model-for formalizer=<model>`.
model: claude-sonnet-4-6
runner: cli
prompt_template: prompts/paper-verify/formalizer.md
input_schema: schemas/claim_inventory.schema.json
output_schema: schemas/formalization.schema.json
verifiers:
  - json_schema
max_retries: 2
timeout_secs: 900
escalation: agent
```

- [ ] **Step 4: Write the prompts**

`prompts/paper-verify/claim_extractor.md`:

```markdown
You are the GrokRxiv claim extractor. Read the paper body and the derived
theorem graph, then produce a complete claim inventory as JSON matching the
claim_inventory schema.

Rules:
- One claim per mathematical assertion: definitions, lemmas, propositions,
  theorems, corollaries, conjectures. Number them C001, C002, ... in order
  of appearance.
- `statement_text`: a faithful, self-contained English statement. Inline any
  notation the paper defined earlier — the reader of one claim must not need
  another claim to parse it (dependencies express *logical* reliance, not
  notation).
- `dependencies`: ids of claims this claim's statement or proof relies on.
  Only ids you emitted. Definitions usually have none.
- `references`: bibliography keys the proof or statement leans on.
- `source_span`: the section heading or theorem label where the claim appears.
- Do NOT invent claims that are not asserted by the paper. Do NOT include
  remarks, examples, or motivation.

Return ONLY the JSON object. No markdown fences, no commentary.
```

`prompts/paper-verify/formalizer.md`:

```markdown
You are the GrokRxiv formalizer. For each claim in the inventory, either
produce a Lean 4 statement or mark it not formalizable, as JSON matching the
formalization schema.

Rules:
- `lean_statement` must be ONE complete, self-contained Lean 4 declaration
  (theorem/def) whose proof body is exactly `sorry`. The statement — not the
  proof — is the deliverable: it must elaborate.
- Declare every hypothesis explicitly. Do not rely on context from other
  claims; restate what you need in binders.
- `imports`: list the modules your statement needs (e.g. Mathlib.Order.Basic).
  If the workspace may lack mathlib, prefer core-Lean formulations when
  faithful.
- Set `formalizable: false` with a one-sentence `skip_reason` for claims that
  are genuinely informal (meta-mathematical remarks, empirical assertions,
  claims relying on undefined external machinery). Honesty over coverage:
  a wrong formalization is worse than a skip.
- Name declarations after the claim id (e.g. `theorem c001_group_unique_id ...`).

Return ONLY the JSON object. No markdown fences, no commentary.
```

- [ ] **Step 5: Validate manifests load, then commit**

Run the manifest validation tests (the suite that validated `citation-validation.yaml`; find with `grep -rln "citation-validation" agenthero/apps/grokrxiv/crates/orchestrator/tests/` and run that test target).
Expected: paper-verify manifest loads; roles resolve; handlers are known (registered in Task 9).

```bash
git add agenthero/apps/grokrxiv/dags/paper-verify.yaml agenthero/apps/grokrxiv/agents/paper-verify agenthero/apps/grokrxiv/prompts/paper-verify
git commit -m "feat(formal): add paper-verify DAG with claim extractor and formalizer agents"
```

---

### Task 11: app.yaml action + CLI contract

**Files:**
- Modify: `agenthero/apps/grokrxiv/app.yaml` (append to `actions:`)
- Modify: `crates/orchestrator/tests/agenthero_cli_contract.rs` (one new test)

- [ ] **Step 1: Add the action to app.yaml**

```yaml
  - id: formal-verify
    command: [formal-verify]
    dag_type: paper-verify
    description: Extract formal claims and type-check Lean formalizations for an extracted paper.
    options:
      - name: source
        kind: positional
        value_name: PAPER_ID_OR_PATH
        required: true
        description: Extracted paper id or local extract directory.
```

- [ ] **Step 2: Write the failing contract test** (append to `agenthero_cli_contract.rs`; the `agh` helper already exists in that file)

```rust
#[test]
fn app_run_formal_verify_help_renders_manifest_action() {
    let output = agh(&["app", "run", "grokrxiv", "formal-verify", "--help"]);
    assert!(
        output.status.success(),
        "formal-verify help should exit successfully: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: agh app run grokrxiv formal-verify"),
        "help should include concrete usage, got:\n{stdout}"
    );
    assert!(
        stdout.contains("PAPER_ID_OR_PATH"),
        "help should expose the source positional, got:\n{stdout}"
    );
}
```

- [ ] **Step 3: Run to verify it fails, then passes**

Run: `cargo test -p agenthero-orchestrator --test agenthero_cli_contract app_run_formal_verify`
Expected: FAIL before the app.yaml edit lands in the build, PASS after (the help renders straight from the manifest — `d6cc6fc` made app help manifest-driven, no Rust change needed).

Also run the adapter-side dispatch check: the GrokRxiv process adapter must accept `dag_type: paper-verify`. Find the dispatch:
`grep -rn "paper-review\|citation-validation" agenthero/apps/grokrxiv/rust/src/ | head`
If dag types are dispatched from a list, add `paper-verify`; if dispatch is manifest-driven, nothing to do. Verify with:
`agh app run grokrxiv formal-verify --help` (expect the styled action help, no adapter execution).

- [ ] **Step 4: Full-suite gate**

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: all green (Task 0 fixed the only pre-existing red target)

- [ ] **Step 5: Commit**

```bash
git add agenthero/apps/grokrxiv/app.yaml crates/orchestrator/tests/agenthero_cli_contract.rs agenthero/apps/grokrxiv/rust
git commit -m "feat(formal): expose formal-verify app action"
```

---

### Task 12: End-to-end smoke + operator setup docs

**Files:**
- Modify: `agenthero/apps/grokrxiv/env/` (add the two new env names where the env contract lives — find with `ls agenthero/apps/grokrxiv/env/`)

- [ ] **Step 1: Provision the Lean workspace (operator step, document as you go)**

```bash
elan default stable        # or: brew install elan-init && elan default stable
cd ~/lean && lake new grokrxiv-verify && cd grokrxiv-verify && lake build
export GROKRXIV_LEAN_WORKSPACE=~/lean/grokrxiv-verify
```

Record `GROKRXIV_LEAN_WORKSPACE` and `GROKRXIV_LAKE_BIN` in the app env contract files found above (these files are the contract; do not document env names anywhere else — docs drift).

- [ ] **Step 2: Live lean smoke (no LLM cost)**

Run: `GROKRXIV_LEAN_WORKSPACE=~/lean/grokrxiv-verify cargo test -p grokrxiv-formal -- --ignored`
Expected: `live_toolchain_smoke` passes (status `partial` — the sorry statement type-checks)

- [ ] **Step 3: Full pipeline smoke on a real extracted paper (CLI runners, $0)**

Pick an already-extracted paper id (`agh app run grokrxiv list extracted`), then:

Run: `agh app run grokrxiv formal-verify <PAPER_ID>`
Expected: DAG completes; artifacts `formal_verify/claims.json`, `formal_verify/formalization.json`, `formal_verify/formal_verification_report.json`, `formal-verification.md` appear in the run's artifact store; report coverage numbers are non-zero for a math paper.

**Validation gate (memory `cli-path-is-cost-control`):** run once with default `runner: cli` AND once with `--runner api` (or the app's equivalent override — check `agh app run grokrxiv review --help` for the exact flag) before calling the feature shipped. Both paths must produce schema-valid artifacts.

- [ ] **Step 4: Commit any env-contract/doc edits**

```bash
git add agenthero/apps/grokrxiv/env
git commit -m "feat(formal): document lean toolchain env contract"
```

- [ ] **Step 5: Finish the branch**

Use superpowers:finishing-a-development-branch — run the full suite, then merge/PR per its options.

---

## Self-Review Notes (already applied)

- **Spec coverage vs Phase 1 scope:** fetch-paper/ingest exist; claim extraction → Task 10; knowledge graph → per-paper graph in Task 4 (cross-paper DB graph deferred to Phase 2); semantic layer → `semantics` slot reserved, Phase 3; proof obligations + Lean → Tasks 6–9; citation validator/PR fixer/scoring/publisher → existing DAGs, integration in Phases 2/5.
- **Known uncertainty, stated where it lives:** the agent-node `outputs:` support (Task 10 Step 1) and the tool-handler fn signature (Task 9 Step 1) are verified against code at execution time because the repo's docs are unreliable; both tasks name the exact files to read and the fallback pattern to copy.
- **Type consistency:** `ClaimStatus` (Task 5) is reused by runner (7), report (8), glue (9); `write_claim_files` signature in Task 6 matches its call in Task 7's runner; serde derives noted in Task 9.
