import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";

import { ensureRunWorkspace, statusMarkdown, writeText } from "./artifacts.js";
import { lockedOpenProblemForTarget, resolveDefaultInputPath } from "./open_problem_search.js";
import { artifactOutput, runtimeNode, runtimeReport } from "./runtime_report.js";
import type { DagExecutionReport, DagIo } from "./types.js";

export interface TheoremTriageResult {
  report: DagExecutionReport;
  lockedOpenProblem: string;
  workspace: string;
}

export async function runTheoremTriage(
  args: string[],
  idempotencyKey = "manual",
  input?: DagIo
): Promise<TheoremTriageResult> {
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

  await writeText(options.workspace, "pipeline_snapshot.md", pipelineText);
  await writeText(options.workspace, "queue_snapshot.md", queueText);
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
    report: dagReport(options.workspace, options.target, lockedOpenProblem, input),
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

function dagReport(workspace: string, target: string, lockedOpenProblem: string, input?: DagIo): DagExecutionReport {
  return runtimeReport(
    "theorem-triage",
    [
      runtimeNode(workspace, "load_pipeline_and_queue", "prepare_inputs", [
        "pipeline_snapshot.md",
        "queue_snapshot.md"
      ], {
        role: "pipeline_reader"
      }),
      runtimeNode(workspace, "audit_current_status", "verify", ["STATUS.md"], {
        role: "status_auditor"
      }),
      runtimeNode(workspace, "lock_target", "synthesizer", ["locked_target.json"], {
        role: "target_locker"
      }),
      runtimeNode(workspace, "update_queue", "synthesizer", ["QUEUE_UPDATE.md"], {
        role: "queue_writer"
      }),
      runtimeNode(workspace, "triage_report", "render_artifacts", ["REPORT.md"], {
        role: "triage_reporter"
      })
    ],
    {
      values: {
        target,
        locked_open_problem: lockedOpenProblem,
        workspace
      },
      artifacts: artifactOutput(workspace, [
        "pipeline_snapshot.md",
        "queue_snapshot.md",
        "STATUS.md",
        "locked_target.json",
        "QUEUE_UPDATE.md",
        "REPORT.md"
      ])
    },
    { input }
  );
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
