import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { adapterEventLine, adapterLifecycleEventLine, handleAdapterRequestJson } from "../src/adapter.js";
import { runtimeNode, runtimeReport } from "../src/runtime_report.js";
import { AGENTHERO_EVENT_TRACE_FIELDS, APP_ADAPTER_EVENT_PREFIX, type FinalReport } from "../src/types.js";

const protocol = "agenthero.app.v1";

function request(args: string[] = []) {
  return JSON.stringify({
    protocol,
    app: "formal-proofs",
    action: "open-problem-search",
    dag_type: "open-problem-search",
    args,
    input: {
      values: {
        app_run_id: "2d0a1d88-b9f9-4e8f-848e-605b86717330",
        fixture: "open-problem-search"
      },
      artifacts: {}
    },
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
    assert.equal(response.report?.manifest_version, 1);
    assert.match(response.report?.manifest_hash ?? "", /^sha256:[a-f0-9]{64}$/);
    assert.equal(response.report?.input.values.fixture, "open-problem-search");
    assert.equal(response.report?.events?.length, (response.report?.nodes.length ?? 0) * 2 + 2);
    assert.equal(response.report?.events?.[0]?.event_type, "dag.started");
    assert.equal(response.report?.events?.[1]?.event_type, "node.started");
    assert.equal(response.report?.events?.[2]?.event_type, "node.completed");
    assert.equal(response.report?.events?.at(-1)?.event_type, "dag.completed");
    assert.equal(response.report?.events?.[1]?.payload.node_id, response.report?.nodes[0]?.node_id);
    assert.equal(response.report?.events?.[0]?.payload.app_run_id, "2d0a1d88-b9f9-4e8f-848e-605b86717330");
    assert.equal(response.report?.events?.[0]?.payload.dag_type, "open-problem-search");
    assert.equal(response.report?.events?.[0]?.payload.manifest_version, response.report?.manifest_version);
    assert.equal(response.report?.events?.[0]?.payload.manifest_hash, response.report?.manifest_hash);
    assert.equal(response.report?.events?.[1]?.payload.node_kind, response.report?.nodes[0]?.kind);
    assert.equal(response.report?.events?.[2]?.payload.status, response.report?.nodes[0]?.status);
    assert.equal(response.report?.events?.[2]?.payload.duration_ms, response.report?.nodes[0]?.latency_ms);
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

test("adapter event lines use AgentHero stderr event prefix", () => {
  const line = adapterEventLine({
    level: "info",
    event_type: "node.started",
    node_id: "verify",
    message: "verify started",
    payload: { node_id: "verify", attempt: 1 }
  });

  assert.match(line, new RegExp(`^${APP_ADAPTER_EVENT_PREFIX}`));
  assert.match(line, /"event_type":"node.started"/);
  assert.match(line, /"node_id":"verify"/);
});

test("adapter event lines normalize mandatory AgentHero trace fields", () => {
  const line = adapterEventLine({
    level: "info",
    event_type: "node.completed",
    node_id: "verify",
    message: "verify ok",
    payload: {
      app_run_id: "2d0a1d88-b9f9-4e8f-848e-605b86717330",
      dag_run_id: "f78c57db-89e3-4b63-8c1a-2c07e3331f0c",
      kind: "verify",
      tool: "lean",
      latency_ms: 42,
      status: "ok"
    }
  });
  const event = JSON.parse(line.slice(APP_ADAPTER_EVENT_PREFIX.length));

  assert.equal(event.payload.app_run_id, "2d0a1d88-b9f9-4e8f-848e-605b86717330");
  assert.equal(event.payload.dag_run_id, "f78c57db-89e3-4b63-8c1a-2c07e3331f0c");
  assert.equal(event.payload.node_id, "verify");
  assert.equal(event.payload.node_kind, "verify");
  assert.equal(event.payload.tool_id, "lean");
  assert.equal(event.payload.duration_ms, 42);
  for (const field of [
    "app_run_id",
    "dag_run_id",
    "node_id",
    "attempt",
    "node_kind",
    "tool_id",
    "manifest_hash",
    "artifact_id",
    "lease_id",
    "status",
    "exit_status",
    "duration_ms"
  ]) {
    assert.ok(Object.hasOwn(event.payload, field), `missing ${field}`);
  }
});

test("adapter lifecycle event lines carry request identity", () => {
  const adapterRequest = JSON.parse(request(["--target", "e677-fin-e255"]));
  adapterRequest.input.values.dag_run_id = "f78c57db-89e3-4b63-8c1a-2c07e3331f0c";
  adapterRequest.input.values.lease_id = "a9353847-48b3-472e-b88e-89770fcdbf7a";

  const line = adapterLifecycleEventLine(
    adapterRequest,
    "info",
    "app_action.completed",
    "formal-proofs action completed",
    "completed",
    0,
    { node_count: 3 }
  );
  const event = JSON.parse(line.slice(APP_ADAPTER_EVENT_PREFIX.length));

  assert.equal(event.event_type, "app_action.completed");
  assert.equal(event.payload.app_run_id, "2d0a1d88-b9f9-4e8f-848e-605b86717330");
  assert.equal(event.payload.dag_run_id, "f78c57db-89e3-4b63-8c1a-2c07e3331f0c");
  assert.equal(event.payload.lease_id, "a9353847-48b3-472e-b88e-89770fcdbf7a");
  assert.equal(event.payload.app, "formal-proofs");
  assert.equal(event.payload.action, "open-problem-search");
  assert.equal(event.payload.dag_type, "open-problem-search");
  assert.equal(event.payload.status, "completed");
  assert.equal(event.payload.exit_status, 0);
  assert.equal(event.payload.args_count, 2);
  assert.equal(event.payload.node_count, 3);
});

test("adapter process emits lifecycle events with mandatory trace fields", async () => {
  const workspace = await mkdtemp(join(tmpdir(), "formal-proofs-process-"));
  try {
    const payload = JSON.parse(
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
    payload.input.values.dag_run_id = "f78c57db-89e3-4b63-8c1a-2c07e3331f0c";
    payload.input.values.lease_id = "a9353847-48b3-472e-b88e-89770fcdbf7a";

    const result = spawnSync(process.execPath, [join("dist-test", "src", "adapter.js")], {
      input: JSON.stringify(payload),
      encoding: "utf8"
    });

    assert.equal(result.status, 0, result.stderr);
    const response = JSON.parse(result.stdout);
    assert.equal(response.ok, true);

    const started = lifecycleEvent(result.stderr, "app_action.started");
    const completed = lifecycleEvent(result.stderr, "app_action.completed");
    assertLifecycleTraceFields(started);
    assertLifecycleTraceFields(completed);
    assert.equal(started.payload.app_run_id, "2d0a1d88-b9f9-4e8f-848e-605b86717330");
    assert.equal(completed.payload.dag_run_id, "f78c57db-89e3-4b63-8c1a-2c07e3331f0c");
    assert.equal(completed.payload.lease_id, "a9353847-48b3-472e-b88e-89770fcdbf7a");

    const eventTypes = adapterEvents(result.stderr).map((event) => event.event_type);
    const startedIndex = eventTypes.indexOf("app_action.started");
    const dagStartedIndex = eventTypes.indexOf("dag.started");
    const dagCompletedIndex = eventTypes.indexOf("dag.completed");
    const completedIndex = eventTypes.indexOf("app_action.completed");
    const lastNodeCompletedIndex = eventTypes.lastIndexOf("node.completed");
    assert.ok(startedIndex >= 0, `missing app_action.started in ${eventTypes.join(",")}`);
    assert.ok(dagStartedIndex >= 0, `missing dag.started in ${eventTypes.join(",")}`);
    assert.ok(dagCompletedIndex >= 0, `missing dag.completed in ${eventTypes.join(",")}`);
    assert.ok(completedIndex >= 0, `missing app_action.completed in ${eventTypes.join(",")}`);
    assert.ok(lastNodeCompletedIndex >= 0, `missing node.completed in ${eventTypes.join(",")}`);
    assert.ok(startedIndex < dagStartedIndex, `expected app_action.started before dag.started: ${eventTypes.join(",")}`);
    assert.ok(dagStartedIndex < lastNodeCompletedIndex, `expected node events after dag.started: ${eventTypes.join(",")}`);
    assert.ok(lastNodeCompletedIndex < dagCompletedIndex, `expected node.completed before dag.completed: ${eventTypes.join(",")}`);
    assert.ok(dagCompletedIndex < completedIndex, `expected dag.completed before app_action.completed: ${eventTypes.join(",")}`);
  } finally {
    await rm(workspace, { force: true, recursive: true });
  }
});

test("runtime reports derive failed status from failed nodes", async () => {
  const workspace = await mkdtemp(join(tmpdir(), "formal-proofs-runtime-report-"));
  try {
    const report = runtimeReport(
      "theorem-triage",
      [
        runtimeNode(workspace, "load_pipeline_and_queue", "prepare_inputs", [], {
          status: "ok"
        }),
        runtimeNode(workspace, "audit_current_status", "verify", [], {
          status: "failed",
          error: "status check failed"
        })
      ],
      { values: {}, artifacts: {} }
    );

    assert.equal(report.status, "failed");
    assert.equal(report.events.length, 6);
    assert.equal(report.events[0]?.event_type, "dag.started");
    assert.equal(report.events[1]?.event_type, "node.started");
    assert.equal(report.events[2]?.event_type, "node.completed");
    assert.equal(report.events[3]?.event_type, "node.started");
    assert.equal(report.events[4]?.event_type, "node.failed");
    assert.equal(report.events[4]?.payload.node_id, "audit_current_status");
    assert.equal(report.events[5]?.event_type, "dag.failed");
    assert.equal(report.events[5]?.payload.status, "failed");
  } finally {
    await rm(workspace, { force: true, recursive: true });
  }
});

function lifecycleEvent(stderr: string, eventType: string): any {
  const event = adapterEvents(stderr).find((candidate) => candidate.event_type === eventType);
  assert.ok(event, `missing lifecycle event ${eventType} in stderr:\n${stderr}`);
  return event;
}

function adapterEvents(stderr: string): any[] {
  return stderr
    .split(/\r?\n/)
    .filter((line) => line.startsWith(APP_ADAPTER_EVENT_PREFIX))
    .map((line) => JSON.parse(line.slice(APP_ADAPTER_EVENT_PREFIX.length)));
}

function assertLifecycleTraceFields(event: any): void {
  for (const field of AGENTHERO_EVENT_TRACE_FIELDS) {
    assert.ok(Object.hasOwn(event.payload, field), `missing ${field}`);
  }
}

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
        input: { values: { fixture: "certificate-verify" }, artifacts: {} },
        json: true,
        dry_run: false,
        idempotency_key: "test-cert"
      })
    );

    assert.equal(response.ok, true);
    assert.match(response.report?.manifest_hash ?? "", /^sha256:[a-f0-9]{64}$/);
    assert.equal(response.report?.input.values.fixture, "certificate-verify");
    assert.equal(response.output?.verifier_status, "unverified");
    assert.deepEqual(Object.keys(response.report?.outputs.artifacts ?? {}).sort(), [
      "REPORT.md",
      "VERIFIER_LOG.md",
      "candidate.json",
      "haskell_result.json",
      "lean_result.json",
      "sat_result.json",
      "verifier_result.json"
    ]);
    assert.match(await readFile(join(workspace, "VERIFIER_LOG.md"), "utf8"), /cert-1/);
    assert.match(await readFile(join(workspace, "haskell_result.json"), "utf8"), /cert-1/);
    assert.match(await readFile(join(workspace, "verifier_result.json"), "utf8"), /cert-1/);
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
        input: { values: { fixture: "theorem-triage" }, artifacts: {} },
        json: true,
        dry_run: false,
        idempotency_key: "test-triage"
      })
    );

    assert.equal(response.ok, true);
    assert.match(response.report?.manifest_hash ?? "", /^sha256:[a-f0-9]{64}$/);
    assert.equal(response.report?.input.values.fixture, "theorem-triage");
    assert.deepEqual(Object.keys(response.report?.outputs.artifacts ?? {}).sort(), [
      "QUEUE_UPDATE.md",
      "REPORT.md",
      "STATUS.md",
      "locked_target.json",
      "pipeline_snapshot.md",
      "queue_snapshot.md"
    ]);
    assert.equal(
      response.output?.locked_open_problem,
      "finite E677=>E255, including all finite orders or a verified countermodel at the frontier"
    );
    assert.match(await readFile(join(workspace, "pipeline_snapshot.md"), "utf8"), /Research Pipeline/);
    assert.match(await readFile(join(workspace, "queue_snapshot.md"), "utf8"), /Active Research Pipeline Queue/);
    assert.match(await readFile(join(workspace, "STATUS.md"), "utf8"), /current_status: open/);
    assert.match(await readFile(join(workspace, "REPORT.md"), "utf8"), /target: e677-fin-e255/);
  } finally {
    await rm(workspace, { force: true, recursive: true });
  }
});
