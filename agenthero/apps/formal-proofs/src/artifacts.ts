import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";

import type { ArtifactRef, FinalReport, NormalizedCandidate, RejectedCandidate, VerifierResult } from "./types.js";

export const requiredArtifacts = [
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
];

export async function ensureRunWorkspace(workspace: string): Promise<void> {
  await mkdir(join(workspace, "candidates"), { recursive: true });
}

export async function writeText(workspace: string, relativePath: string, text: string): Promise<string> {
  const path = join(workspace, relativePath);
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, text.endsWith("\n") ? text : `${text}\n`, "utf8");
  return path;
}

export function jsonl(records: unknown[]): string {
  if (records.length === 0) {
    return "\n";
  }
  return `${records.map((record) => JSON.stringify(record)).join("\n")}\n`;
}

export function statusMarkdown(lockedOpenProblem: string, pipeline: string, queue: string): string {
  return [
    "# Formal Proofs Status",
    "",
    `locked_open_problem: ${lockedOpenProblem}`,
    "current_status: open",
    "status_sources:",
    `- ${pipeline}`,
    `- ${queue}`,
    `full_success_statement: Settle ${lockedOpenProblem} with Lean 4 or another explicitly trusted verifier.`,
    "toy_or_known_results_that_do_not_count:",
    "- bounded searches without a frontier countermodel",
    "- model claims without checker evidence",
    "- partial finite shards"
  ].join("\n");
}

export function verifierLogMarkdown(results: VerifierResult[]): string {
  const lines = ["# Verifier Log", ""];
  if (results.length === 0) {
    lines.push("No normalized candidates reached verifier eligibility.");
  }
  for (const result of results) {
    lines.push(`## ${result.candidate_id}`, "");
    lines.push(`status: ${result.status}`);
    lines.push(`trusted: ${result.trusted}`);
    lines.push(`expected_checker: ${result.expected_checker}`);
    lines.push(`evidence_path: ${result.evidence_path ?? "null"}`);
    lines.push(`notes: ${result.notes}`);
    lines.push("");
    if (result.commands.length === 0) {
      lines.push("- No trusted verifier command was supplied by the app.");
    } else {
      for (const command of result.commands) {
        lines.push(`- command: ${command.command.join(" ")}`);
        lines.push(`  exit_code: ${command.exit_code ?? "null"}`);
        lines.push(`  duration_ms: ${command.duration_ms}`);
      }
    }
    lines.push("");
  }
  return lines.join("\n");
}

export function fixerLogMarkdown(rejected: RejectedCandidate[]): string {
  const lines = ["# Fixer Log", ""];
  if (rejected.length === 0) {
    lines.push("No malformed candidates required repair in this fixture run.");
  }
  for (const item of rejected) {
    lines.push("- rejected candidate");
    lines.push(`  reason: ${item.reason}`);
    lines.push(`  raw: ${item.raw}`);
  }
  return lines.join("\n");
}

export function leaderboardMarkdown(candidates: NormalizedCandidate[], results: VerifierResult[]): string {
  const byId = new Map(results.map((result) => [result.candidate_id, result]));
  const lines = ["# Leaderboard", "", "| rank | candidate | proposer | verified | verifier status | score |", "|---:|---|---|---|---|---:|"];
  candidates.forEach((candidate, index) => {
    const result = byId.get(candidate.candidate_id);
    const verified = result?.trusted === true;
    const score = verified ? 1 : 0;
    lines.push(
      `| ${index + 1} | ${candidate.candidate_id} | ${candidate.proposer} | ${verified} | ${result?.status ?? "unverified"} | ${score} |`
    );
  });
  if (candidates.length === 0) {
    lines.push("| 0 | none | none | false | unverified | 0 |");
  }
  return lines.join("\n");
}

export function reportMarkdown(report: FinalReport): string {
  return [
    "# Formal Proofs Report",
    "",
    `open_problem_solved: ${report.open_problem_solved}`,
    `trusted_status: ${report.trusted_status}`,
    `locked_open_problem: ${report.locked_open_problem}`,
    `failure_reason: ${report.failure_reason ?? "null"}`,
    "",
    "## Verifier Commands",
    ...(report.verifier_commands.length > 0 ? report.verifier_commands.map((command) => `- ${command}`) : ["- none"]),
    "",
    "## Partial Artifacts",
    ...report.partial_artifacts.map((artifact) => `- ${artifact}`)
  ].join("\n");
}

export function artifactRefs(workspace: string): Record<string, ArtifactRef> {
  return Object.fromEntries(
    requiredArtifacts.map((relative) => [
      relative,
      {
        uri: join(workspace, relative),
        media_type: relative.endsWith(".jsonl") ? "application/jsonl" : "text/markdown",
        metadata: {}
      }
    ])
  );
}
