import { readFileSync } from "node:fs";

import { runCertificateVerify } from "./certificate_verify.js";
import { runOpenProblemSearch } from "./open_problem_search.js";
import { runTheoremTriage } from "./theorem_triage.js";
import { APP_ID, APP_PROTOCOL, type AppAdapterRequest, type AppAdapterResponse, type DagExecutionReport } from "./types.js";

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
      const result = await runOpenProblemSearch(request.args ?? [], request.idempotency_key ?? "manual");
      return ok(request, result.report, {
        report: result.finalReport,
        workspace: result.workspace
      });
    }
    if (request.action === "certificate-verify") {
      const result = await runCertificateVerify(request.args ?? [], request.idempotency_key ?? "manual");
      return ok(request, result.report, {
        verifier_status: result.verifierResult.status,
        trusted: result.verifierResult.trusted,
        workspace: result.workspace
      });
    }
    if (request.action === "theorem-triage") {
      const result = await runTheoremTriage(request.args ?? [], request.idempotency_key ?? "manual");
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
  const response = await handleAdapterRequestJson(input);
  process.stdout.write(`${JSON.stringify(response)}\n`);
  if (!response.ok) {
    process.exitCode = 1;
  }
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

function placeholderReport(request: AppAdapterRequest): DagExecutionReport {
  return {
    dag_type: request.dag_type,
    status: "ok",
    nodes: [
      {
        node_id: request.action,
        kind: "prepare_inputs",
        status: "ok",
        executor: "typescript",
        inputs: [],
        outputs: [],
        warning: null,
        error: null,
        latency_ms: 0,
        trace: {}
      }
    ],
    outputs: {
      values: {},
      artifacts: {}
    }
  };
}

if (process.argv[1] && process.argv[1] === new URL(import.meta.url).pathname) {
  await main();
}
