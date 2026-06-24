import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";

import { ensureRunWorkspace, statusMarkdown, writeText } from "./artifacts.js";
import { lockedOpenProblemForTarget, resolveDefaultInputPath } from "./open_problem_search.js";
import type { DagExecutionReport } from "./types.js";

export interface TheoremTriageResult {
  report: DagExecutionReport;
  lockedOpenProblem: string;
  workspace: string;
}

export async function runTheoremTriage(args: string[], idempotencyKey = "manual"): Promise<TheoremTriageResult> {
  const options = parseTheoremTriageArgs(args, idempotencyKey);
  const lockedOpenProblem = lockedOpenProblemForTarget(options.target);
  if (!lockedOpenProblem) {
    throw new Error(`unknown theorem-triage target ${options.target}`);
  }
  assertReadable(options.pipeline, "--pipeline");
  assertReadable(options.queue, "--queue");
  await ensureRunWorkspace(options.workspace);
  const pipelineText = await readFile(options.pipeline, "utf8");
  const queueText = await readFile(options.queue, "utf8");

  await writeText(options.workspace, "STATUS.md", statusMarkdown(lockedOpenProblem, options.pipeline, options.queue));
  await writeText(
    options.workspace,
    "locked_target.json",
    JSON.stringify({ target: options.target, locked_open_problem: lockedOpenProblem }, null, 2)
  );
  await writeText(
    options.workspace,
    "QUEUE_UPDATE.md",
    ["# Queue Update", "", `target: ${options.target}`, `locked_open_problem: ${lockedOpenProblem}`].join("\n")
  );
  await writeText(
    options.workspace,
    "REPORT.md",
    [
      "# Theorem Triage Report",
      "",
      `target: ${options.target}`,
      `locked_open_problem: ${lockedOpenProblem}`,
      "current_status: open",
      "",
      "## Pipeline Snapshot",
      "",
      pipelineText.slice(0, 1000),
      "",
      "## Queue Snapshot",
      "",
      queueText.slice(0, 1000)
    ].join("\n")
  );

  return {
    report: dagReport(options.workspace, options.target, lockedOpenProblem),
    lockedOpenProblem,
    workspace: options.workspace
  };
}

function parseTheoremTriageArgs(args: string[], idempotencyKey: string) {
  let pipeline = resolveDefaultInputPath("RESEARCH_PIPELINE.md");
  let queue = resolveDefaultInputPath("QUEUE.md");
  let target = "e677-fin-e255";
  let workspace: string | null = null;
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--pipeline") {
      pipeline = requireValue(args, ++index, arg);
    } else if (arg === "--queue") {
      queue = requireValue(args, ++index, arg);
    } else if (arg === "--target") {
      target = requireValue(args, ++index, arg);
    } else if (arg === "--workspace") {
      workspace = requireValue(args, ++index, arg);
    } else {
      throw new Error(`unknown theorem-triage argument ${arg}`);
    }
  }
  return {
    pipeline: resolve(pipeline),
    queue: resolve(queue),
    target,
    workspace: resolve(workspace ?? defaultWorkspace(target, idempotencyKey))
  };
}

function dagReport(workspace: string, target: string, lockedOpenProblem: string): DagExecutionReport {
  return {
    dag_type: "theorem-triage",
    status: "ok",
    nodes: [
      node("load_pipeline_and_queue", "prepare_inputs", ["pipeline_snapshot.md", "queue_snapshot.md"]),
      node("audit_current_status", "verify", ["STATUS.md"]),
      node("lock_target", "synthesizer", ["locked_target.json"]),
      node("update_queue", "synthesizer", ["QUEUE_UPDATE.md"]),
      node("triage_report", "render_artifacts", ["REPORT.md"])
    ],
    outputs: {
      values: {
        target,
        locked_open_problem: lockedOpenProblem,
        workspace
      },
      artifacts: {
        "STATUS.md": { uri: join(workspace, "STATUS.md"), media_type: "text/markdown", metadata: {} },
        "locked_target.json": { uri: join(workspace, "locked_target.json"), media_type: "application/json", metadata: {} },
        "QUEUE_UPDATE.md": { uri: join(workspace, "QUEUE_UPDATE.md"), media_type: "text/markdown", metadata: {} },
        "REPORT.md": { uri: join(workspace, "REPORT.md"), media_type: "text/markdown", metadata: {} }
      }
    }
  };
}

function node(node_id: string, kind: string, outputs: string[]) {
  return {
    node_id,
    kind,
    status: "ok" as const,
    executor: "typescript",
    inputs: [],
    outputs,
    warning: null,
    error: null,
    latency_ms: 0,
    trace: {}
  };
}

function assertReadable(path: string, flag: string): void {
  if (!existsSync(path)) {
    throw new Error(`${flag} file does not exist: ${path}`);
  }
}

function requireValue(args: string[], index: number, flag: string): string {
  const value = args[index];
  if (!value || value.startsWith("--")) {
    throw new Error(`${flag} requires a value`);
  }
  return value;
}

function defaultWorkspace(target: string, idempotencyKey: string): string {
  const safeKey = idempotencyKey.replace(/[^a-zA-Z0-9._-]+/g, "-").slice(0, 40) || "manual";
  return join(process.cwd(), ".agenthero", "formal-proofs", "theorem-triage", `${target}-${safeKey}`);
}
