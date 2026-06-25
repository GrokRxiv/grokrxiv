import { readFileSync } from "node:fs";

import { runCertificateVerify } from "./certificate_verify.js";
import { runOpenProblemSearch } from "./open_problem_search.js";
import { runtimeNode, runtimeReport } from "./runtime_report.js";
import { runTheoremTriage } from "./theorem_triage.js";
import {
  APP_ADAPTER_EVENT_PREFIX,
  APP_ID,
  APP_PROTOCOL,
  AGENTHERO_EVENT_TRACE_FIELDS,
  type AppAdapterRequest,
  type AppAdapterResponse,
  type DagExecutionEvent,
  type DagExecutionReport
} from "./types.js";

export async function handleAdapterRequestJson(json: string): Promise<AppAdapterResponse> {
  let request: AppAdapterRequest;
  try {
    request = JSON.parse(json) as AppAdapterRequest;
  } catch (error) {
    return failed(baseRequest(), error instanceof Error ? error.message : "invalid adapter request JSON");
  }

  try {
    validateRequest(request);
    if (request.action === "open-problem-search") {
      const result = await runOpenProblemSearch(
        request.args ?? [],
        request.idempotency_key ?? "manual",
        request.input
      );
      return ok(request, result.report, {
        report: result.finalReport,
        workspace: result.workspace
      });
    }
    if (request.action === "certificate-verify") {
      const result = await runCertificateVerify(
        request.args ?? [],
        request.idempotency_key ?? "manual",
        request.input
      );
      return ok(request, result.report, {
        verifier_status: result.verifierResult.status,
        trusted: result.verifierResult.trusted,
        workspace: result.workspace
      });
    }
    if (request.action === "theorem-triage") {
      const result = await runTheoremTriage(
        request.args ?? [],
        request.idempotency_key ?? "manual",
        request.input
      );
      return ok(request, result.report, {
        locked_open_problem: result.lockedOpenProblem,
        workspace: result.workspace
      });
    }
    throw new Error(`unsupported formal-proofs action \`${request.action}\``);
  } catch (error) {
    return failed(request, error instanceof Error ? error.message : String(error));
  }
}

export async function main(): Promise<void> {
  const input = readFileSync(0, "utf8");
  const lifecycleRequest = lifecycleRequestFromJson(input);
  process.stderr.write(`${adapterLifecycleEventLine(
    lifecycleRequest,
    "info",
    "app_action.started",
    `formal-proofs action \`${lifecycleRequest.action}\` started`,
    "running",
    null
  )}\n`);
  const response = await handleAdapterRequestJson(input);
  emitAdapterReportEvents(response.report);
  if (response.ok) {
    process.stderr.write(`${adapterLifecycleEventLine(
      lifecycleRequest,
      "info",
      "app_action.completed",
      `formal-proofs action \`${response.action}\` completed`,
      "completed",
      0,
      { node_count: response.report?.nodes.length ?? 0 }
    )}\n`);
  } else {
    process.stderr.write(`${adapterLifecycleEventLine(
      lifecycleRequest,
      "error",
      "app_action.failed",
      `formal-proofs action \`${response.action}\` failed: ${response.error ?? "unknown error"}`,
      "failed",
      1,
      { error: response.error ?? "unknown error" }
    )}\n`);
  }
  process.stdout.write(`${JSON.stringify(response)}\n`);
  if (!response.ok) {
    process.exitCode = 1;
  }
}

export function adapterLifecycleEventLine(
  request: AppAdapterRequest,
  level: DagExecutionEvent["level"],
  eventType: string,
  message: string,
  status: string,
  exitStatus: number | null,
  extra: Record<string, unknown> = {}
): string {
  const inputValues = request.input?.values ?? {};
  return adapterEventLine({
    level,
    event_type: eventType,
    node_id: null,
    message,
    payload: {
      app: request.app,
      action: request.action,
      dag_type: request.dag_type,
      adapter_protocol: request.protocol,
      args_count: request.args?.length ?? 0,
      dry_run: request.dry_run ?? false,
      json: request.json ?? false,
      idempotency_key: request.idempotency_key ?? "",
      app_run_id: stringOrNull(inputValues.app_run_id),
      dag_run_id: stringOrNull(inputValues.dag_run_id),
      lease_id: stringOrNull(inputValues.lease_id),
      status,
      exit_status: exitStatus,
      ...extra
    }
  });
}

export function adapterEventLine(event: DagExecutionEvent): string {
  return `${APP_ADAPTER_EVENT_PREFIX}${JSON.stringify(normalizeAdapterEvent(event))}`;
}

export function emitAdapterReportEvents(report: DagExecutionReport | undefined): void {
  for (const event of report?.events ?? []) {
    process.stderr.write(`${adapterEventLine(event)}\n`);
  }
}

export function normalizeAdapterEvent(event: DagExecutionEvent): DagExecutionEvent {
  const payload = { ...(event.payload ?? {}) };
  payload.app_run_id = stringOrNull(payload.app_run_id);
  payload.node_id = event.node_id ?? stringOrNull(payload.node_id);
  payload.node_kind = payload.node_kind ?? payload.kind ?? null;
  payload.tool_id = payload.tool_id ?? payload.tool ?? null;
  payload.duration_ms = payload.duration_ms ?? payload.latency_ms ?? null;
  for (const field of AGENTHERO_EVENT_TRACE_FIELDS) {
    if (!Object.hasOwn(payload, field)) {
      payload[field] = null;
    }
  }
  return {
    ...event,
    payload
  };
}

function stringOrNull(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function validateRequest(request: AppAdapterRequest): void {
  if (request.protocol !== APP_PROTOCOL) {
    throw new Error(`unsupported adapter protocol \`${request.protocol}\``);
  }
  if (request.app !== APP_ID) {
    throw new Error(`formal-proofs adapter received app \`${request.app}\``);
  }
  if (!["open-problem-search", "certificate-verify", "theorem-triage"].includes(request.action)) {
    throw new Error(`unsupported formal-proofs action \`${request.action}\``);
  }
  if (request.dag_type !== request.action) {
    throw new Error(`formal-proofs adapter received dag_type \`${request.dag_type}\` for action \`${request.action}\``);
  }
}

function ok(
  request: AppAdapterRequest,
  report: DagExecutionReport,
  output: Record<string, unknown>
): AppAdapterResponse {
  return {
    protocol: APP_PROTOCOL,
    app: request.app,
    action: request.action,
    dag_type: request.dag_type,
    ok: true,
    report,
    output,
    error: null
  };
}

function failed(request: AppAdapterRequest, error: string): AppAdapterResponse {
  return {
    protocol: APP_PROTOCOL,
    app: request.app,
    action: request.action,
    dag_type: request.dag_type,
    ok: false,
    error
  };
}

function baseRequest(): AppAdapterRequest {
  return {
    protocol: APP_PROTOCOL,
    app: APP_ID,
    action: "unknown",
    dag_type: "unknown",
    args: []
  };
}

function lifecycleRequestFromJson(json: string): AppAdapterRequest {
  try {
    return JSON.parse(json) as AppAdapterRequest;
  } catch {
    return baseRequest();
  }
}

function placeholderReport(request: AppAdapterRequest): DagExecutionReport {
  return runtimeReport(
    request.dag_type,
    [runtimeNode(process.cwd(), request.action, "prepare_inputs", [])],
    {
      values: {},
      artifacts: {}
    }
  );
}

if (process.argv[1] && process.argv[1] === new URL(import.meta.url).pathname) {
  await main();
}
