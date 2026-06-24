import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { handleAdapterRequestJson } from "../src/adapter.js";
import type { FinalReport } from "../src/types.js";

const protocol = "agenthero.app.v1";

function request(args: string[] = []) {
  return JSON.stringify({
    protocol,
    app: "formal-proofs",
    action: "open-problem-search",
    dag_type: "open-problem-search",
    args,
    input: { values: {}, artifacts: {} },
    json: true,
    dry_run: false,
    idempotency_key: "test-key"
  });
}

test("open-problem-search fixture run writes required artifacts and remains unsolved", async () => {
  const workspace = await mkdtemp(join(tmpdir(), "formal-proofs-test-"));
  try {
    const response = await handleAdapterRequestJson(
      request([
        "--workspace",
        workspace,
        "--target",
        "e677-fin-e255",
        "--max-rounds",
        "1",
        "--proposers",
        "fixture"
      ])
    );

    assert.equal(response.ok, true);
    assert.equal(response.app, "formal-proofs");
    assert.equal(response.action, "open-problem-search");
    assert.equal(response.dag_type, "open-problem-search");
    const report = response.output?.report as FinalReport;
    assert.equal(report.open_problem_solved, false);
    assert.equal(report.trusted_status, "not_solved");
    assert.equal(
      report.locked_open_problem,
      "finite E677=>E255, including all finite orders or a verified countermodel at the frontier"
    );

    for (const relative of [
      "STATUS.md",
      "PROPOSER_PROMPTS.md",
      "candidates/raw.jsonl",
      "candidates/normalized.jsonl",
      "candidates/rejected.jsonl",
      "VERIFIER_LOG.md",
      "FIXER_LOG.md",
      "LEADERBOARD.md",
      "ITERATION_LOG.md",
      "REPORT.md"
    ]) {
      const contents = await readFile(join(workspace, relative), "utf8");
      assert.notEqual(contents.trim(), "", `${relative} should not be empty`);
    }
  } finally {
    await rm(workspace, { force: true, recursive: true });
  }
});

test("adapter rejects app identity mismatch and invalid action", async () => {
  const wrongApp = JSON.parse(request());
  wrongApp.app = "grokrxiv";
  const appResponse = await handleAdapterRequestJson(JSON.stringify(wrongApp));
  assert.equal(appResponse.ok, false);
  assert.match(appResponse.error ?? "", /formal-proofs adapter received app `grokrxiv`/);

  const wrongAction = JSON.parse(request());
  wrongAction.action = "review";
  const actionResponse = await handleAdapterRequestJson(JSON.stringify(wrongAction));
  assert.equal(actionResponse.ok, false);
  assert.match(actionResponse.error ?? "", /unsupported formal-proofs action `review`/);
});

test("certificate-verify writes verifier artifacts for one candidate", async () => {
  const workspace = await mkdtemp(join(tmpdir(), "formal-proofs-cert-"));
  const candidatePath = join(workspace, "candidate.json");
  await writeFile(
    candidatePath,
    JSON.stringify({
      candidate_id: "cert-1",
      lane: "E677 =>fin E255",
      locked_open_problem:
        "finite E677=>E255, including all finite orders or a verified countermodel at the frontier",
      claim_type: "countermodel",
      object: { operation_table: [[0]] },
      parameters: { order: 1 },
      claimed_improvement: "fixture only",
      verification_target: "finite_magma_countermodel",
      expected_checker: "haskell",
      proposer: "fixture",
      notes: "syntactic fixture"
    }),
    "utf8"
  );

  try {
    const response = await handleAdapterRequestJson(
      JSON.stringify({
        protocol,
        app: "formal-proofs",
        action: "certificate-verify",
        dag_type: "certificate-verify",
        args: ["--candidate", candidatePath, "--workspace", workspace],
        input: { values: {}, artifacts: {} },
        json: true,
        dry_run: false,
        idempotency_key: "test-cert"
      })
    );

    assert.equal(response.ok, true);
    assert.equal(response.output?.verifier_status, "unverified");
    assert.match(await readFile(join(workspace, "VERIFIER_LOG.md"), "utf8"), /cert-1/);
    assert.match(await readFile(join(workspace, "REPORT.md"), "utf8"), /open_problem_solved: false/);
  } finally {
    await rm(workspace, { force: true, recursive: true });
  }
});

test("theorem-triage locks the default copied E677 target", async () => {
  const workspace = await mkdtemp(join(tmpdir(), "formal-proofs-triage-"));
  try {
    const response = await handleAdapterRequestJson(
      JSON.stringify({
        protocol,
        app: "formal-proofs",
        action: "theorem-triage",
        dag_type: "theorem-triage",
        args: ["--target", "e677-fin-e255", "--workspace", workspace],
        input: { values: {}, artifacts: {} },
        json: true,
        dry_run: false,
        idempotency_key: "test-triage"
      })
    );

    assert.equal(response.ok, true);
    assert.equal(
      response.output?.locked_open_problem,
      "finite E677=>E255, including all finite orders or a verified countermodel at the frontier"
    );
    assert.match(await readFile(join(workspace, "STATUS.md"), "utf8"), /current_status: open/);
    assert.match(await readFile(join(workspace, "REPORT.md"), "utf8"), /target: e677-fin-e255/);
  } finally {
    await rm(workspace, { force: true, recursive: true });
  }
});
