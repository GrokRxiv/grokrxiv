# Review Pipeline Reliability + Feedback Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the review gate trustworthy and prove the GitHub correction loop with an automated live test: push to a revision PR, webhook re-runs review, and the same GitHub feedback comment is updated until the gate passes.

**Architecture:** Centralize review-gate classification as typed code, separate DAG execution usability from publication approval, and make correction PRs contain the manuscript source that the webhook re-review path actually reads. Add a hidden, opt-in CLI smoke command for the destructive GitHub push loop so the public operator surface stays small.

**Tech Stack:** Rust orchestrator, `sqlx`, Supabase Postgres, GitHub REST via `octocrab`, `wiremock` for unit tests, local `grokrxiv` CLI for all acceptance paths, provider CLIs for live review.

---

## Current Findings

- `GITHUB_TOKEN` is available from repo `.env`; `gh repo view GrokRxiv/grokrxiv-reviews` reports `ADMIN`.
- `grokrxiv doctor --json` reports publisher OK when `.env` is loaded.
- Existing local DB has reviewed papers, but current `pr_open` examples have `github_pr_url = null`, so a live feedback-loop test needs to open a fresh real PR before pushing.
- Current `request-revisions` opens a PR containing rendered review artifacts, not the paper source.
- Current `pull_request.synchronize` re-review clones the PR head repo and tries to prepare it as a corrected `git_repo` manuscript.
- Therefore the advertised “push fixes to this PR branch” loop is not reliable until revision PRs include a real editable source snapshot or target the original source repository.
- Current gate semantics are split across `supervisor.rs`, `cli.rs`, and `routes/webhook.rs`; `minor_revision` is rejected by `approve` but treated as pass in the webhook feedback comment. That inconsistency must be removed before live trust testing.

## Current Run Addendum: Citation Trust + Agent Teams

This pass also fixes the citation/provenance issues found in the Rust audit before any full live review run. The source of truth is: LLM specialists write role judgments; deterministic verifiers write provenance; the public site must not collapse verifier unknowns into “fake citation” claims.

**Acceptance assumptions:**

- CLI-only review/publish testing: `grokrxiv ... --runner cli --extractor cli`.
- Direct provider API fallback remains disabled unless an explicit premium/API profile is selected.
- Citation verifier transient errors are `transient_unknown`/warn, not fake citations/fail.
- Publication remains human-gated; do not auto-merge publication PRs in this pass.
- Live validation target after unit checks: `https://arxiv.org/abs/2602.17480`.

**Implementation teams:**

- **Team A — Citation Verifier Integrity:** classify citation lookups as `resolved`, `unresolved`, `transient_unknown`, or `malformed`; retry transient 429/5xx/timeouts once; explicitly page/chunk arXiv `id_list` queries; keep unknowns out of the fail fraction.
- **Team B — Typed Merge + Provenance:** stop mutating schema-valid specialist JSON with verifier-only facts after validation; keep citation and novelty verifier facts in provenance fields; rerun schema validation after any remaining merge.
- **Team C — Citation Fallback + Gate Policy:** remove schema-shaped citation fallback output that looks verified without external checks; terminally fail/withdraw quorum failures instead of leaving misleading moderation rows.
- **Team D — Public API + Web Provenance:** include `verifier_notes` in public review detail APIs; render citation states as verified, missing, malformed, or unverified; rename LLM-only missing-reference claims to suggested missing prior art.
- **Team E — Local/Git Paper Metadata:** show inferred subject category for local PDF/TeX/Git papers from persisted `source_metadata.adapter.inferred_subjects` when `papers.field` is null, so review cards do not show `—`.
- **Team F — CLI JSON Robustness:** tolerate provider CLI replies that wrap otherwise valid JSON in prose or fenced code blocks; extract the last schema-valid JSON object before declaring the role failed.
- **Team G — Validation Runner:** run focused Rust unit tests, web typecheck/build checks, then a CLI-native full review smoke. If the live model/provider layer is unstable, report that separately from deterministic pipeline failures.

## File Map

- Create `crates/orchestrator/src/review_gate.rs`: typed review-gate verdicts, recommendation policy, specialist verifier policy, GitHub feedback body inputs.
- Modify `crates/orchestrator/src/lib.rs`: expose `review_gate`.
- Modify `crates/orchestrator/src/supervisor.rs`: use `review_gate` for quorum and synthetic failure payloads.
- Modify `crates/orchestrator/src/cli.rs`: use `review_gate` in `approve`, `request-revisions`, hidden feedback-loop smoke command, and help tests.
- Modify `crates/orchestrator/src/routes/webhook.rs`: use `review_gate` in re-review comment updates; use source snapshot markers when present.
- Modify `crates/orchestrator/src/db.rs`: add typed row helpers for GitHub review thread lookup and source snapshot metadata.
- Modify `crates/publisher/src/github.rs`: add revision PR source files and stable PR body markers.
- Modify `migrations/20260519000001_source_review_abstraction.sql`: add optional correction source path metadata if not already persisted cleanly.
- Add `tests/fixtures/feedback-loop-paper/paper.tex`: tiny deterministic manuscript for local source-loop testing.
- Add tests under existing crate test modules; no public shell test should bypass the `grokrxiv` CLI for review actions.

---

### Task 1: Centralize Review-Gate Semantics

**Files:**
- Create: `crates/orchestrator/src/review_gate.rs`
- Modify: `crates/orchestrator/src/lib.rs`

- [ ] **Step 1: Write failing gate policy tests**

Add this test module to the new file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use grokrxiv_schemas::{AgentRole, VerifierStatus};

    #[test]
    fn specialist_gate_distinguishes_usable_from_publishable() {
        let statuses = vec![
            (AgentRole::Summary, Some(VerifierStatus::Pass)),
            (AgentRole::TechnicalCorrectness, Some(VerifierStatus::Warn)),
            (AgentRole::Novelty, Some(VerifierStatus::Pass)),
            (AgentRole::Reproducibility, Some(VerifierStatus::Fail)),
            (AgentRole::Citation, None),
        ];
        let gate = SpecialistGate::evaluate(&statuses, 3, 5);
        assert!(gate.meta_can_run);
        assert!(!gate.publishable_without_force);
        assert_eq!(gate.usable_roles, vec!["summary", "technical_correctness", "novelty"]);
        assert_eq!(gate.warning_roles, vec!["technical_correctness"]);
        assert_eq!(gate.blocked_roles, vec!["reproducibility", "citation"]);
    }

    #[test]
    fn publication_gate_only_passes_clean_accept() {
        let clean = PublicationGateInput {
            recommendation: Some("accept"),
            specialist_gate: SpecialistGate {
                meta_can_run: true,
                publishable_without_force: true,
                usable_roles: vec!["summary", "technical_correctness", "novelty", "reproducibility", "citation"],
                warning_roles: vec![],
                blocked_roles: vec![],
                min_usable: 3,
                expected_total: 5,
            },
        };
        assert_eq!(PublicationGate::evaluate(clean).verdict, GateVerdict::Pass);

        let minor = PublicationGateInput {
            recommendation: Some("minor_revision"),
            specialist_gate: SpecialistGate::all_pass_for_test(),
        };
        assert_eq!(PublicationGate::evaluate(minor).verdict, GateVerdict::Fail);

        let warned = PublicationGateInput {
            recommendation: Some("accept"),
            specialist_gate: SpecialistGate {
                warning_roles: vec!["citation"],
                publishable_without_force: false,
                ..SpecialistGate::all_pass_for_test()
            },
        };
        assert_eq!(PublicationGate::evaluate(warned).verdict, GateVerdict::Warn);
    }
}
```

- [ ] **Step 2: Run the failing tests**

Run:

```bash
cargo test -p agenthero-orchestrator --lib review_gate -- --nocapture
```

Expected: compile failure because `review_gate` types do not exist.

- [ ] **Step 3: Implement the gate module**

Create `crates/orchestrator/src/review_gate.rs`:

```rust
use grokrxiv_schemas::{AgentRole, VerifierStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateVerdict {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialistGate {
    pub meta_can_run: bool,
    pub publishable_without_force: bool,
    pub usable_roles: Vec<&'static str>,
    pub warning_roles: Vec<&'static str>,
    pub blocked_roles: Vec<&'static str>,
    pub min_usable: usize,
    pub expected_total: usize,
}

impl SpecialistGate {
    pub fn evaluate(
        statuses: &[(AgentRole, Option<VerifierStatus>)],
        min_usable: usize,
        expected_total: usize,
    ) -> Self {
        let mut usable_roles = Vec::new();
        let mut warning_roles = Vec::new();
        let mut blocked_roles = Vec::new();
        for (role, status) in statuses {
            let slug = role_slug(*role);
            match status {
                Some(VerifierStatus::Pass) => usable_roles.push(slug),
                Some(VerifierStatus::Warn) => {
                    usable_roles.push(slug);
                    warning_roles.push(slug);
                }
                Some(VerifierStatus::Fail) | None => blocked_roles.push(slug),
            }
        }
        let meta_can_run = usable_roles.len() >= min_usable;
        let publishable_without_force =
            usable_roles.len() == expected_total && warning_roles.is_empty() && blocked_roles.is_empty();
        Self {
            meta_can_run,
            publishable_without_force,
            usable_roles,
            warning_roles,
            blocked_roles,
            min_usable,
            expected_total,
        }
    }

    #[cfg(test)]
    pub fn all_pass_for_test() -> Self {
        Self {
            meta_can_run: true,
            publishable_without_force: true,
            usable_roles: vec!["summary", "technical_correctness", "novelty", "reproducibility", "citation"],
            warning_roles: vec![],
            blocked_roles: vec![],
            min_usable: 3,
            expected_total: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationGateInput<'a> {
    pub recommendation: Option<&'a str>,
    pub specialist_gate: SpecialistGate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationGate {
    pub verdict: GateVerdict,
    pub reason: String,
    pub recommendation: String,
}

impl PublicationGate {
    pub fn evaluate(input: PublicationGateInput<'_>) -> Self {
        let recommendation = input.recommendation.unwrap_or("missing").to_string();
        if !input.specialist_gate.meta_can_run {
            return Self {
                verdict: GateVerdict::Fail,
                reason: format!(
                    "Only {} of {} specialist outputs were usable; need at least {}.",
                    input.specialist_gate.usable_roles.len(),
                    input.specialist_gate.expected_total,
                    input.specialist_gate.min_usable,
                ),
                recommendation,
            };
        }
        if recommendation != "accept" {
            return Self {
                verdict: GateVerdict::Fail,
                reason: format!("Meta-review recommendation is `{recommendation}`, not `accept`."),
                recommendation,
            };
        }
        if !input.specialist_gate.publishable_without_force {
            return Self {
                verdict: GateVerdict::Warn,
                reason: format!(
                    "Meta-review accepted, but verifier warnings or blocked roles remain. warnings={:?}; blocked={:?}",
                    input.specialist_gate.warning_roles,
                    input.specialist_gate.blocked_roles,
                ),
                recommendation,
            };
        }
        Self {
            verdict: GateVerdict::Pass,
            reason: "Meta-review accepted and all specialist verifier statuses passed.".to_string(),
            recommendation,
        }
    }
}

pub fn role_slug(role: AgentRole) -> &'static str {
    crate::review_dag::role_slug(role)
}
```

Modify `crates/orchestrator/src/lib.rs`:

```rust
pub(crate) mod review_gate;
```

- [ ] **Step 4: Verify gate tests pass**

Run:

```bash
cargo test -p agenthero-orchestrator --lib review_gate -- --nocapture
```

Expected: all `review_gate` tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/orchestrator/src/review_gate.rs crates/orchestrator/src/lib.rs
git commit -m "refactor: centralize review gate policy"
```

---

### Task 2: Wire Gate Policy Into Review, Approve, and Re-review Feedback

**Files:**
- Modify: `crates/orchestrator/src/supervisor.rs`
- Modify: `crates/orchestrator/src/cli.rs`
- Modify: `crates/orchestrator/src/routes/webhook.rs`

- [ ] **Step 1: Write failing tests for recommendation consistency**

Add tests that prove `minor_revision` is not considered a passed automated gate:

```rust
#[test]
fn rereview_feedback_treats_minor_revision_as_failed_gate() {
    let body = crate::routes::webhook::rereview_gate_comment_body_for_test(
        uuid::Uuid::nil(),
        Some("minor_revision"),
        Some(&serde_json::json!({
            "summary": "Needs small fixes.",
            "weaknesses": ["citation context missing"],
            "questions": []
        })),
    );
    assert!(body.contains("Automated Review Gate: Failed"), "{body}");
    assert!(body.contains("minor_revision"), "{body}");
}
```

If the route helper is not public today, add it as `pub(crate)` inside `routes/webhook.rs`.

- [ ] **Step 2: Run failing test**

Run:

```bash
cargo test -p agenthero-orchestrator --lib minor_revision -- --nocapture
```

Expected: fails because current webhook treats `minor_revision` as pass.

- [ ] **Step 3: Replace duplicated gate decisions**

In `routes/webhook.rs`, replace:

```rust
"accept" | "minor_revision" => {
    crate::github_feedback::gate_pass_comment_body(new_review_id, recommendation)
}
```

with:

```rust
let gate = crate::review_gate::PublicationGate::evaluate(
    crate::review_gate::PublicationGateInput {
        recommendation: Some(recommendation),
        specialist_gate: load_specialist_gate_for_review(pool, new_review_id)
            .await
            .unwrap_or_else(|_| crate::review_gate::SpecialistGate {
                meta_can_run: true,
                publishable_without_force: false,
                usable_roles: vec![],
                warning_roles: vec![],
                blocked_roles: vec![],
                min_usable: crate::review_dag::DEFAULT_MIN_SPECIALIST_QUORUM,
                expected_total: crate::review_dag::canonical_specialist_roles().len(),
            }),
    },
);
match gate.verdict {
    crate::review_gate::GateVerdict::Pass => {
        crate::github_feedback::gate_pass_comment_body(new_review_id, &gate.recommendation)
    }
    crate::review_gate::GateVerdict::Warn | crate::review_gate::GateVerdict::Fail => {
        let failure = crate::github_feedback::gate_failure_from_meta(
            new_review_id,
            &gate.recommendation,
            meta.as_ref(),
        );
        let _ = crate::github_feedback::record_gate_failure(state, new_review_id, &failure).await;
        crate::github_feedback::gate_failure_comment_body(new_review_id, &gate.recommendation, &failure)
    }
}
```

Add `load_specialist_gate_for_review` as a typed helper in `db.rs` or a private route helper. It should read `review_agents.role, review_agents.verifier_status` for the review and call `SpecialistGate::evaluate`.

- [ ] **Step 4: Apply the same policy in `approve`**

In `cli.rs::approve_impl`, keep human `--force`, but make non-force approval require:

```rust
if gate.verdict != crate::review_gate::GateVerdict::Pass {
    anyhow::bail!(
        "review {review_id} is not cleanly publishable: {}. Use `agh grokrxiv request-revisions {review_id}` or `agh grokrxiv approve --force {review_id}`.",
        gate.reason
    );
}
```

- [ ] **Step 5: Verify**

Run:

```bash
cargo test -p agenthero-orchestrator --lib review_gate minor_revision approve -- --nocapture
```

Expected: tests pass and `approve_help_is_pr_handoff_not_publish` still passes.

- [ ] **Step 6: Commit**

```bash
git add crates/orchestrator/src/supervisor.rs crates/orchestrator/src/cli.rs crates/orchestrator/src/routes/webhook.rs crates/orchestrator/src/db.rs
git commit -m "fix: use one review gate policy across approve and feedback"
```

---

### Task 3: Make Revision PRs Re-review the Actual Manuscript Source

**Files:**
- Modify: `crates/orchestrator/src/cli.rs`
- Modify: `crates/orchestrator/src/routes/webhook.rs`
- Modify: `crates/publisher/src/github.rs`
- Modify: `crates/orchestrator/src/db.rs`

- [ ] **Step 1: Write failing source-loop test**

Add a unit test for PR body markers:

```rust
#[test]
fn revision_pr_body_contains_review_and_correction_source_markers() {
    let review_id = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let body = grokrxiv_publisher::build_revision_pr_body_for_test(
        review_id,
        "source/paper.tex",
        "Fix theorem statement.",
    );
    assert!(body.contains("grokrxiv-review-id: 11111111-1111-1111-1111-111111111111"));
    assert!(body.contains("grokrxiv-correction-source-path: source/paper.tex"));
}
```

- [ ] **Step 2: Run failing test**

Run:

```bash
cargo test -p grokrxiv-publisher revision_pr_body_contains -- --nocapture
```

Expected: fails because revision-specific markers do not exist.

- [ ] **Step 3: Include source snapshot in `request-revisions` PRs**

In `request_revisions_impl`, after loading review artifacts, load the original manuscript source:

```rust
let correction_source = load_correction_source_snapshot(pool, paper_id).await?;
if let Some(source) = correction_source {
    files.push((source.repo_path.clone(), source.bytes));
}
```

Implement `load_correction_source_snapshot` so:

- For `source_kind = 'git_repo'`, clone/read from `source_metadata.adapter.repo`, `source_metadata.adapter.rev`, and `source_metadata.adapter.paper_path`.
- Store the editable file at a safe PR path such as `corrections/<source_id>/<paper_filename>`.
- Return the PR path so the body can include `grokrxiv-correction-source-path: <path>`.
- If the source cannot be loaded, `request-revisions` must fail instead of advertising an impossible push loop.

- [ ] **Step 4: Teach webhook to prefer correction source marker**

In `extract_review_id_from_body`, add a sibling helper:

```rust
fn extract_correction_source_path_from_body(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.trim().strip_prefix("grokrxiv-correction-source-path:") {
            let path = rest.trim();
            if !path.is_empty() && !path.starts_with('/') && !path.contains("..") {
                return Some(path.to_string());
            }
        }
    }
    None
}
```

Pass this path into `prepare_git_correction_extract`; if present, use it instead of the original adapter `paper_path`.

- [ ] **Step 5: Adjust user-facing PR text**

Revision PR body should say:

```text
Edit the manuscript snapshot at `<path>` on this PR branch, commit, and push.
Each push triggers GrokRxiv automated re-review.
```

For arXiv-only papers where no source snapshot can be created, do not claim push-triggered correction support. The command should fail with:

```text
review <id> cannot open a correction-loop PR because no editable manuscript source is available; submit a revised PDF/TeX or use --force to publish despite the gate.
```

- [ ] **Step 6: Verify**

Run:

```bash
cargo test -p grokrxiv-publisher revision_pr_body -- --nocapture
cargo test -p agenthero-orchestrator --lib correction_source -- --nocapture
```

Expected: tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/orchestrator/src/cli.rs crates/orchestrator/src/routes/webhook.rs crates/orchestrator/src/db.rs crates/publisher/src/github.rs
git commit -m "fix: make revision PRs carry editable manuscript source"
```

---

### Task 4: Add Hidden CLI Feedback-Loop Smoke Command

**Files:**
- Modify: `crates/orchestrator/src/cli.rs`
- Modify: `crates/orchestrator/src/db.rs`

- [ ] **Step 1: Write CLI parse test**

Add a hidden command parse test:

```rust
#[test]
fn cli_parses_hidden_feedback_loop_smoke() {
    let review_id = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let cli = Cli::try_parse_from([
        "grokrxiv",
        "feedback-loop-smoke",
        &review_id.to_string(),
        "--max-wait-secs",
        "3600",
    ]).expect("hidden smoke command parses");
    match cli.command {
        Command::FeedbackLoopSmoke { review_id: parsed, max_wait_secs } => {
            assert_eq!(parsed, review_id);
            assert_eq!(max_wait_secs, 3600);
        }
        other => panic!("expected FeedbackLoopSmoke, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run failing parse test**

Run:

```bash
cargo test -p agenthero-orchestrator --lib feedback_loop_smoke -- --nocapture
```

Expected: fails because the hidden command does not exist.

- [ ] **Step 3: Implement hidden command**

Add hidden command:

```rust
#[command(hide = true)]
FeedbackLoopSmoke {
    review_id: Uuid,
    #[arg(long, default_value_t = 3600)]
    max_wait_secs: u64,
}
```

Implementation rules:

- Refuse unless `GROKRXIV_E2E_ALLOW_GITHUB_PUSH=1`.
- Load `.env` through existing startup path.
- Require `GITHUB_TOKEN`, `GITHUB_WEBHOOK_SECRET`, `DATABASE_URL`.
- If review has no real `github_pr_url`, run `grokrxiv request-revisions <review_id> --json` internally through the same implementation path, not by duplicating publisher logic.
- Clone the PR head branch with `gh pr checkout <number>` or `git clone` + `git checkout`.
- Edit only the correction source path from PR body marker.
- Commit a harmless TeX comment:

```tex
% GrokRxiv feedback-loop smoke correction <timestamp>
```

- Push to the PR branch.
- Poll local DB for `rereview_requests` with the prior review id and new commit SHA.
- Poll until `state in ('done','failed')`.
- If done, verify `new_review_id` exists and the GitHub feedback comment body contains exactly one `<!-- grokrxiv:gate-feedback:review-<prior_review_id> -->` marker.
- Print JSON with `prior_review_id`, `new_review_id`, `request_id`, `pr_url`, `commit_sha`, and `gate_comment_url`.

- [ ] **Step 4: Verify hidden command remains hidden from public help**

Run:

```bash
agenthero --help | grep feedback-loop-smoke && exit 1 || true
cargo test -p agenthero-orchestrator --lib default_help_shows_operator_surface_only feedback_loop_smoke -- --nocapture
```

Expected: command parses but is absent from normal help.

- [ ] **Step 5: Commit**

```bash
git add crates/orchestrator/src/cli.rs crates/orchestrator/src/db.rs
git commit -m "test: add hidden GitHub feedback-loop smoke command"
```

---

### Task 5: Add Webhook and Stable Comment Regression Tests

**Files:**
- Modify: `crates/orchestrator/src/routes/webhook.rs`
- Modify: `crates/publisher/src/github.rs`

- [ ] **Step 1: Add webhook payload unit tests**

Add tests for:

- `pull_request.synchronize` ignores non-review branches.
- duplicate GitHub deliveries do not enqueue two requests.
- missing `head.sha` returns ignored.
- correction source path marker rejects absolute paths and `..`.
- marker extraction returns both review id and correction path.

Use pure helper tests where DB is not needed.

- [ ] **Step 2: Add publisher stable-comment tests**

`crates/publisher/src/github.rs` already has `wiremock` coverage for creating/updating gate comments. Add one test that creates a failure comment then updates it to pass using the same marker and asserts only one comment is present in the mock interaction sequence.

- [ ] **Step 3: Run tests**

```bash
cargo test -p grokrxiv-publisher post_or_update_gate_feedback -- --nocapture
cargo test -p agenthero-orchestrator --lib webhook correction_source marker -- --nocapture
```

Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add crates/orchestrator/src/routes/webhook.rs crates/publisher/src/github.rs
git commit -m "test: cover webhook correction-loop comment updates"
```

---

### Task 6: Run Full Local Test Matrix Before Live Push

**Files:**
- No code changes expected.

- [ ] **Step 1: Load repo `.env`**

```bash
set -a
source .env
set +a
```

- [ ] **Step 2: Verify config guard**

```bash
agh --runner cli --extractor cli --no-cache config --json \
  | jq '{runner:.runtime.default_runner, extractor:.runtime.extractor, direct_provider_api_allowed:.runtime.direct_provider_api_allowed, model_for:.runtime.model_for}'
```

Expected:

- `runner = "Cli"`
- `extractor = "cli"`
- `direct_provider_api_allowed = false`
- citation and novelty resolve to `gemini-3-flash-preview`

- [ ] **Step 3: Run crate tests**

```bash
cargo test -p grokrxiv-llm-adapter --all-features
cargo test -p grokrxiv-ingest
cargo test -p grokrxiv-storage
cargo test -p grokrxiv-verifier
cargo test -p grokrxiv-render
cargo test -p grokrxiv-publisher
cargo test -p agenthero-orchestrator --lib
node --test grokrxiv-skills/tests/*.test.js
```

Expected: all pass. If Claude service is degraded, these unit tests should still pass because they do not require live Claude review.

- [ ] **Step 4: Commit only if any test-support files changed**

```bash
git status --short
```

Expected: no new generated prompt/research/search-index dirt from the test matrix.

---

### Task 7: Run Live Feedback-Loop E2E on an Already Reviewed Paper

**Files:**
- No code changes expected; this is live acceptance.

- [ ] **Step 1: Start only the webhook receiver needed for the smoke**

Run the orchestrator with the scheduler disabled so the live smoke does not trigger unrelated arXiv backfill work:

```bash
GROKRXIV_DISABLE_SCHEDULER=1 \
agh --runner cli --extractor cli --status serve
```

Confirm `localhost:8080` is listening and the public tunnel points to `/webhook/github`.

- [ ] **Step 2: Confirm Claude status before spending time**

Use the operator-visible status page. If Claude Opus/Sonnet is degraded, do not treat a live review failure as a repo regression.

- [ ] **Step 3: Choose a reviewed git-source paper**

```bash
set -a
source .env
set +a
grokrxiv list reviews --json \
  | jq -r '.[] | select(.status=="pr_open" or .status=="awaiting_moderation") | [.id,.arxiv_id,.status,.github_pr_url] | @tsv'
```

Pick a review whose paper source is `git_repo` and whose artifacts exist under `artifacts/<review_id>`.

- [ ] **Step 4: Open or reuse a real revision PR**

```bash
grokrxiv request-revisions <review_id> --json
```

Expected JSON contains a real GitHub PR URL under `https://github.com/GrokRxiv/grokrxiv-reviews/pull/<n>`, not `SIMULATED`.

- [ ] **Step 5: Run hidden push-loop smoke**

```bash
GROKRXIV_E2E_ALLOW_GITHUB_PUSH=1 \
grokrxiv feedback-loop-smoke <review_id> --max-wait-secs 7200 --json
```

Expected:

- command clones/checks out the PR branch,
- commits one harmless source comment,
- pushes,
- GitHub webhook creates one `rereview_requests` row,
- re-review creates a new review id,
- stable GitHub feedback comment is updated in place,
- JSON includes `new_review_id` and `gate_comment_url`.

- [ ] **Step 6: Continue loop if gate fails**

If the feedback comment says failed, repeat:

```bash
GROKRXIV_E2E_ALLOW_GITHUB_PUSH=1 \
grokrxiv feedback-loop-smoke <review_id> --max-wait-secs 7200 --json
```

Expected: same feedback comment URL, updated body. Stop when verdict is `pass`, or stop with the exact failed reason if the paper genuinely cannot pass.

- [ ] **Step 7: Verify DB and GitHub state**

```bash
grokrxiv show <new_review_id>
gh pr view <pr_number> --repo GrokRxiv/grokrxiv-reviews --json comments,headRefName,url
```

Expected:

- `grokrxiv show` lists six agents, verifier statuses, and gate decision.
- GitHub comments contain exactly one `grokrxiv:gate-feedback:review-<prior_review_id>` marker.
- Latest comment body references `<new_review_id>`.

---

## Acceptance Criteria

- A clean `accept` with all specialist verifier statuses `pass` is the only automatic `pass`.
- `warn` is usable for meta-review but does not silently become a clean pass.
- `minor_revision`, `major_revision`, `reject`, missing recommendation, and unknown recommendation all fail the automated gate.
- Revision PRs do not advertise push-triggered correction unless the PR branch contains an editable manuscript source path.
- Each PR push creates at most one `rereview_requests` row per commit SHA.
- The same GitHub gate-feedback comment is updated, not duplicated.
- The live feedback-loop smoke test uses repo `.env`, `grokrxiv` CLI, GitHub push, and the real webhook path.
- No PR is merged by the smoke test.

## Spec Coverage Check

- Deep review pipeline trust: Tasks 1, 2, and 6.
- Pass/fail/warn semantics: Tasks 1 and 2.
- Automated GitHub feedback loop: Tasks 3, 4, 5, and 7.
- Clone/change/push/wait test: Task 4 implementation and Task 7 live run.
- Use existing reviewed papers: Task 7 starts from local reviewed DB rows.
- Avoid expanding public command surface: Task 4 command is hidden and opt-in gated.

## Verified Rust Pipeline Fix Backlog

This backlog synthesizes the 2026-05-19 Rust ingest/review audit into actionable follow-up work. It is intentionally narrower than the raw notes: several issues are real, several are tuning or cleanup, and a few were overstated. Implement these after the live feedback-loop repair unless they block validation.

### Tier 1: Correctness Bugs

- [ ] **Quorum failure lifecycle is wrong.** If fewer than the required specialist outputs are usable, the supervisor records a synthetic gate failure but can leave the new review visible as if it reached moderation. Fix by transitioning the review and any partial specialist rows to the terminal failure state used by the pipeline, and add tests for one specialist fail, two specialists fail, and moderator visibility.
- [ ] **Citation verifier treats transient network failures like fake citations.** Crossref/arXiv 429/5xx/timeouts should become `unknown` or retryable verifier notes, not hard `exists=false`. Fix classification, retry once on transient errors, and exclude unknowns from fake-citation thresholds.
- [ ] **arXiv citation lookup is under-batched.** The bug is real, but not exactly a "100 citation" cap: the current verifier makes one arXiv API `id_list` request and relies on API defaults. Chunk requests and page results explicitly.
- [ ] **Citation/repro fact merges must not mutate error outputs.** If a specialist output contains an `error` object, skip verifier-fact overlay instead of appending real data onto an error payload.
- [ ] **Citation merge fabricates LLM relevance.** Verifier-only entries get `"relevance": "medium"`, which looks like an LLM judgment. Persist verifier-only findings separately or use an explicit non-scored sentinel the meta-reviewer cannot treat as a specialist rating.
- [ ] **Reproducibility URL findings need dedupe and severity by URL kind.** Deduplicate repeated broken URLs and grade code/dataset URLs higher than vanity or documentation links.
- [ ] **Moderator notes DB errors are too quiet.** If request-change notes cannot be fetched for a re-review, log the DB error and surface enough context to avoid silently dropping moderator instructions.
- [ ] **Cache hashing should fail loud on serialization failure.** Replace `unwrap_or_default()` style fallbacks with explicit errors so a broken serialization path cannot collapse distinct cache inputs.
- [ ] **Ingest fallback can skip artifact persistence.** If staged ingest fails and the legacy fallback succeeds, paper artifact rows can be incomplete. Delete the fallback or write the minimal artifact/storage rows required for re-runs to converge.

### Tier 2: Cost And Latency

- [ ] **Parallelize reproducibility URL checks.** URL reachability and GitHub metadata checks run serially. Use bounded concurrency, for example `buffer_unordered(8)`, with per-URL timeout and clear result classification.
- [ ] **Redesign `html_quality` output.** The current shape can require the model to echo the full rendered HTML. Replace with a patch-list schema such as `{kind, old, new, reason}` and apply patches in Rust.
- [ ] **Move blocking temp-file writes off the async executor.** Review workdir setup uses synchronous file I/O on async paths. Use `tokio::fs` where practical.
- [ ] **Cache JSON schema validators.** `JsonSchemaVerifier` should build schemas once at construction time instead of rebuilding on every verify call.
- [ ] **Avoid duplicating review input in workdirs.** `prepare_review_workdir` writes both `prompt.md` and `review_input.json` with overlapping paper payloads. Keep one canonical channel plus small file references.
- [ ] **Calibrate body budgets per model.** `technical_correctness` uses a very large body budget; reserve context for schema/system/citation material and output, and make this model-aware.

### Tier 3: Contract Drift And Cleanup

- [ ] **Make the declared review DAG executable or rename it as metadata.** `review_dag.rs` builds topology and validates it, but execution still walks roles manually in `supervisor.rs`. Either drive execution from the DAG or explicitly scope the DAG as policy metadata.
- [ ] **Move role slugs onto `AgentRole`.** Role slug mapping is duplicated across modules.
- [ ] **Narrow `MAX_RETRIES` cleanup.** It is not dead code; it is used in the channel retry path. The actual issue is that several CLI execution paths bypass that retry policy.
- [ ] **Deprecate or rename `apply_revisions`.** It returns a hardcoded simulated PR URL and should not look like a real publication path.
- [ ] **Remove personal default paths.** Do not default to `/Users/mlong/...` for data repo paths. Prefer a loud missing-config error or a relative development default.
- [ ] **Centralize arXiv id validation.** Metadata and citation verifier code have different rules; consolidate in `grokrxiv-schemas`.
- [ ] **Use `url::form_urlencoded`.** Replace hand-rolled URL encoders.
- [ ] **Make YAML runtime fields truthful.** `agents/*.yaml` declares fields such as `verifiers` and `concurrency` that are not runtime sources of truth today. Either honor them or remove them.
- [ ] **Fix `html_quality.yaml` schema drift.** The declared input schema does not match what the runner passes.
- [ ] **Remove unused parameter trails.** Clean `let _ = state`, `let _ = paper_id`, and similar markers when touching those modules.

### Tier 4: Verifier Rungs Need Hardening

- [ ] **Make `SupportVerifier` role-aware or remove it.** Passing on any output with one long string is too weak to imply support.
- [ ] **Run `ToneVerifier` over text fields, not stringified JSON.** Current stringification can flag keys or legitimate quoted terminology.
- [ ] **Move `RenderVerifier` to render-stage artifacts.** Running it against specialist JSON is mostly vacuous.

### Tier 5: Prompt And Retrieval Drift

- [ ] **Rewrite citation prompt contract.** `prompts/citation.md` tells the model to fill fields that the verifier later overlays. The prompt should say which fields the LLM owns and which fields are verifier-owned.
- [ ] **Pick one meta-reviewer policy source.** Do not duplicate proof-as-code gate language in both prompt templates and role-system prompt construction.
- [ ] **Improve Semantic Scholar candidate retrieval.** Title-only search is noisy for generic titles. Prefer exact-title or DOI/arXiv-id lookup, then fuzzy fallback with confidence labels.

### Tier 6: Larger Refactors

- [ ] **Split `supervisor.rs` by responsibility.** Target modules: `dag.rs`, `prompts.rs`, `merges.rs`, `cache.rs`, `verify.rs`, and orchestration glue.
- [ ] **Split `cli.rs` by command family.** Separate operator commands, review commands, moderation/publish commands, and hidden smoke commands.
- [ ] **Decide the extraction-agent future.** Tool-loop extraction code remains large and mostly disabled unless `GROKRXIV_FORCE_AGENT_EXTRACTION=1`. Either restore it as a supported path or delete/deprecate it in favor of deterministic extraction.
- [ ] **Prefer `pdftotext -layout` when available.** Use system PDF text extraction for better fidelity, with `pdf-extract` as fallback.

### Tier 7: Regression Tests

- [ ] Add tests for one specialist fail where quorum still passes.
- [ ] Add tests for two specialist fails where quorum fails and no zombie moderator-visible rows remain.
- [ ] Add cache-hit short-circuit tests.
- [ ] Add `html_quality` timeout tests proving finalization does not fail solely because HTML cleanup timed out.
- [ ] Add CLI-only acceptance tests for request-changes, re-review, approve, reject, withdraw, correct, and publish handoff paths.

### Explicitly Out Of Scope For This Backlog

- Web app auth/pricing/quota work.
- Stripe.
- Bulk backfills.
- Provider-account quota monitoring.
