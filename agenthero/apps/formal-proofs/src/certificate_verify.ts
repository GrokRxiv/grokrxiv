import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";

import { ensureRunWorkspace, reportMarkdown, verifierLogMarkdown, writeText } from "./artifacts.js";
import { normalizeCandidateRecords } from "./open_problem_search.js";
import { artifactOutput, runtimeNode, runtimeReport } from "./runtime_report.js";
import { verifyCandidate } from "./verifier.js";
import type { Checker, DagExecutionReport, DagIo, FinalReport, NormalizedCandidate, VerifierResult } from "./types.js";

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
  idempotencyKey = "manual",
  input?: DagIo
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
  await writeText(options.workspace, "lean_result.json", JSON.stringify(checkerResult("lean", verifierResult), null, 2));
  await writeText(
    options.workspace,
    "haskell_result.json",
    JSON.stringify(checkerResult("haskell", verifierResult), null, 2)
  );
  await writeText(options.workspace, "sat_result.json", JSON.stringify(checkerResult("sat", verifierResult), null, 2));
  await writeText(options.workspace, "verifier_result.json", JSON.stringify(verifierResult, null, 2));
  await writeText(options.workspace, "VERIFIER_LOG.md", verifierLogMarkdown([verifierResult]));
  await writeText(options.workspace, "REPORT.md", reportMarkdown(finalReport));

  return {
    report: dagReport(options.workspace, finalReport, verifierResult, input),
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
    partial_artifacts: [
      "candidate.json",
      "lean_result.json",
      "haskell_result.json",
      "sat_result.json",
      "verifier_result.json",
      "VERIFIER_LOG.md",
      "REPORT.md"
    ],
    workspace
  };
}

function dagReport(
  workspace: string,
  finalReport: FinalReport,
  verifierResult: VerifierResult,
  input?: DagIo
): DagExecutionReport {
  const checkerCommand = (checker: Checker): { command: string[] | null; exit_status: number | null } => {
    if (verifierResult.expected_checker !== checker) {
      return { command: null, exit_status: null };
    }
    return {
      command: verifierResult.commands.at(0)?.command ?? null,
      exit_status: verifierResult.commands.at(0)?.exit_code ?? null
    };
  };

  return runtimeReport(
    "certificate-verify",
    [
      runtimeNode(workspace, "load_candidate", "prepare_inputs", ["candidate.json"], {
        role: "candidate_loader"
      }),
      runtimeNode(workspace, "lean_check", "verify", ["lean_result.json"], {
        role: "lean_checker",
        ...checkerCommand("lean")
      }),
      runtimeNode(workspace, "haskell_check", "verify", ["haskell_result.json"], {
        role: "haskell_checker",
        ...checkerCommand("haskell")
      }),
      runtimeNode(workspace, "sat_check", "verify", ["sat_result.json"], {
        role: "sat_checker",
        ...checkerCommand("sat")
      }),
      runtimeNode(workspace, "synthesize_verifier_result", "synthesizer", ["verifier_result.json"], {
        role: "verification_synthesizer"
      }),
      runtimeNode(workspace, "certificate_report", "render_artifacts", ["REPORT.md"], {
        role: "certificate_reporter"
      })
    ],
    {
      values: {
        report: finalReport,
        verifier_status: verifierResult.status,
        workspace
      },
      artifacts: artifactOutput(workspace, [
        "candidate.json",
        "lean_result.json",
        "haskell_result.json",
        "sat_result.json",
        "verifier_result.json",
        "VERIFIER_LOG.md",
        "REPORT.md"
      ])
    },
    { input }
  );
}

function checkerResult(checker: Checker, verifierResult: VerifierResult): Record<string, unknown> {
  const selected = verifierResult.expected_checker === checker;
  return {
    checker,
    candidate_id: verifierResult.candidate_id,
    selected,
    status: selected ? verifierResult.status : "checker_unavailable",
    trusted: selected ? verifierResult.trusted : false,
    commands: selected ? verifierResult.commands : [],
    evidence_path: selected ? verifierResult.evidence_path : null,
    notes: selected
      ? verifierResult.notes
      : `Candidate requested ${verifierResult.expected_checker}; ${checker} was not invoked.`
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
