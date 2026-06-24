import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";

import { artifactRefs, ensureRunWorkspace, reportMarkdown, verifierLogMarkdown, writeText } from "./artifacts.js";
import { normalizeCandidateRecords } from "./open_problem_search.js";
import { verifyCandidate } from "./verifier.js";
import type { DagExecutionReport, FinalReport, NormalizedCandidate, VerifierResult } from "./types.js";

export interface CertificateVerifyOptions {
  candidate: string;
  workspace: string;
}

export interface CertificateVerifyResult {
  report: DagExecutionReport;
  verifierResult: VerifierResult;
  workspace: string;
}

export async function runCertificateVerify(
  args: string[],
  idempotencyKey = "manual"
): Promise<CertificateVerifyResult> {
  const options = parseCertificateVerifyArgs(args, idempotencyKey);
  if (!existsSync(options.candidate)) {
    throw new Error(`--candidate file does not exist: ${options.candidate}`);
  }
  await ensureRunWorkspace(options.workspace);
  const raw = await readFile(options.candidate, "utf8");
  const { accepted, rejected } = normalizeCandidateRecords([raw]);
  if (accepted.length !== 1) {
    throw new Error(`candidate failed schema validation: ${rejected[0]?.reason ?? "unknown error"}`);
  }

  const candidate = accepted[0];
  const verifierResult = await verifyCandidate(candidate);
  const finalReport: FinalReport = certificateFinalReport(candidate, verifierResult, options.workspace);
  await writeText(options.workspace, "candidate.json", JSON.stringify(candidate, null, 2));
  await writeText(options.workspace, "VERIFIER_LOG.md", verifierLogMarkdown([verifierResult]));
  await writeText(options.workspace, "REPORT.md", reportMarkdown(finalReport));

  return {
    report: dagReport(options.workspace, finalReport, verifierResult),
    verifierResult,
    workspace: options.workspace
  };
}

function parseCertificateVerifyArgs(args: string[], idempotencyKey: string): CertificateVerifyOptions {
  let candidate: string | null = null;
  let workspace: string | null = null;
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--candidate") {
      candidate = requireValue(args, ++index, arg);
    } else if (arg === "--workspace") {
      workspace = requireValue(args, ++index, arg);
    } else {
      throw new Error(`unknown certificate-verify argument ${arg}`);
    }
  }
  if (!candidate) {
    throw new Error("--candidate is required");
  }
  return {
    candidate: resolve(candidate),
    workspace: resolve(workspace ?? defaultWorkspace(idempotencyKey))
  };
}

function certificateFinalReport(
  candidate: NormalizedCandidate,
  verifierResult: VerifierResult,
  workspace: string
): FinalReport {
  return {
    open_problem_solved: false,
    trusted_status: verifierResult.trusted ? "partial" : "not_solved",
    locked_open_problem: candidate.locked_open_problem,
    verifier_commands: verifierResult.commands.map((command) => command.command.join(" ")),
    failure_reason: "Certificate verification does not by itself settle the full locked open problem.",
    partial_artifacts: ["candidate.json", "VERIFIER_LOG.md", "REPORT.md"],
    workspace
  };
}

function dagReport(
  workspace: string,
  finalReport: FinalReport,
  verifierResult: VerifierResult
): DagExecutionReport {
  return {
    dag_type: "certificate-verify",
    status: "ok",
    nodes: [
      node("load_candidate", "prepare_inputs", ["candidate.json"]),
      node("lean_check", "verify", []),
      node("haskell_check", "verify", ["VERIFIER_LOG.md"]),
      node("sat_check", "verify", []),
      node("synthesize_verifier_result", "synthesizer", ["VERIFIER_LOG.md"]),
      node("certificate_report", "render_artifacts", ["REPORT.md"])
    ],
    outputs: {
      values: {
        report: finalReport,
        verifier_status: verifierResult.status,
        workspace
      },
      artifacts: artifactRefs(workspace)
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

function requireValue(args: string[], index: number, flag: string): string {
  const value = args[index];
  if (!value || value.startsWith("--")) {
    throw new Error(`${flag} requires a value`);
  }
  return value;
}

function defaultWorkspace(idempotencyKey: string): string {
  const safeKey = idempotencyKey.replace(/[^a-zA-Z0-9._-]+/g, "-").slice(0, 40) || "manual";
  return join(process.cwd(), ".agenthero", "formal-proofs", "certificate-verify", safeKey);
}
