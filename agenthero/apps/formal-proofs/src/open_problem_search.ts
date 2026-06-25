import { existsSync, readFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  artifactRefs,
  ensureRunWorkspace,
  fixerLogMarkdown,
  jsonl,
  leaderboardMarkdown,
  reportMarkdown,
  requiredArtifacts,
  statusMarkdown,
  verifierLogMarkdown,
  writeText
} from "./artifacts.js";
import { proposerPrompt, runProposers } from "./proposers.js";
import { runtimeNode, runtimeReport } from "./runtime_report.js";
import { verifyCandidates } from "./verifier.js";
import type {
  DagExecutionReport,
  DagIo,
  FinalReport,
  NormalizedCandidate,
  RejectedCandidate
} from "./types.js";

export interface OpenProblemSearchOptions {
  pipeline: string;
  queue: string;
  target: string;
  maxRounds: number;
  proposers: string[];
  workspace: string;
}

export interface OpenProblemSearchResult {
  report: DagExecutionReport;
  finalReport: FinalReport;
  workspace: string;
}

const targetLocks: Record<string, string> = {
  "e677-fin-e255":
    "finite E677=>E255, including all finite orders or a verified countermodel at the frontier"
};

export function lockedOpenProblemForTarget(target: string): string | null {
  return targetLocks[target] ?? null;
}

export function resolveDefaultInputPath(fileName: "RESEARCH_PIPELINE.md" | "QUEUE.md"): string {
  return join(appRoot(), "inputs", fileName);
}

export function parseOpenProblemSearchArgs(args: string[], idempotencyKey = "manual"): OpenProblemSearchOptions {
  const parsed: Partial<OpenProblemSearchOptions> = {
    pipeline: resolveDefaultInputPath("RESEARCH_PIPELINE.md"),
    queue: resolveDefaultInputPath("QUEUE.md"),
    maxRounds: 3,
    proposers: ["claude", "gpt-5", "gemini"]
  };

  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--pipeline") {
      parsed.pipeline = requireValue(args, ++index, arg);
    } else if (arg === "--queue") {
      parsed.queue = requireValue(args, ++index, arg);
    } else if (arg === "--target") {
      parsed.target = requireValue(args, ++index, arg);
    } else if (arg === "--max-rounds") {
      const value = Number.parseInt(requireValue(args, ++index, arg), 10);
      if (!Number.isFinite(value) || value < 1) {
        throw new Error("--max-rounds must be a positive integer");
      }
      parsed.maxRounds = value;
    } else if (arg === "--proposers") {
      parsed.proposers = requireValue(args, ++index, arg)
        .split(",")
        .map((value) => value.trim())
        .filter(Boolean);
    } else if (arg === "--workspace") {
      parsed.workspace = requireValue(args, ++index, arg);
    } else {
      throw new Error(`unknown open-problem-search argument ${arg}`);
    }
  }

  if (!parsed.target) {
    parsed.target = "e677-fin-e255";
  }
  if (!parsed.workspace) {
    parsed.workspace = defaultWorkspace(parsed.target, idempotencyKey);
  }
  if (!parsed.proposers || parsed.proposers.length === 0) {
    throw new Error("--proposers must name at least one proposer");
  }

  return {
    pipeline: resolve(parsed.pipeline ?? resolveDefaultInputPath("RESEARCH_PIPELINE.md")),
    queue: resolve(parsed.queue ?? resolveDefaultInputPath("QUEUE.md")),
    target: parsed.target,
    maxRounds: parsed.maxRounds ?? 3,
    proposers: parsed.proposers,
    workspace: resolve(parsed.workspace)
  };
}

export function normalizeCandidateRecords(records: string[]): {
  accepted: NormalizedCandidate[];
  rejected: RejectedCandidate[];
} {
  const accepted: NormalizedCandidate[] = [];
  const rejected: RejectedCandidate[] = [];
  for (const raw of records) {
    try {
      const parsed = JSON.parse(raw) as unknown;
      const validationError = validateCandidate(parsed);
      if (validationError) {
        rejected.push({ raw, reason: validationError });
      } else {
        accepted.push({ ...(parsed as NormalizedCandidate), verified: false });
      }
    } catch (error) {
      rejected.push({
        raw,
        reason: error instanceof Error ? error.message : "invalid JSON"
      });
    }
  }
  return { accepted, rejected };
}

export async function runOpenProblemSearch(
  args: string[],
  idempotencyKey = "manual",
  input?: DagIo
): Promise<OpenProblemSearchResult> {
  const options = parseOpenProblemSearchArgs(args, idempotencyKey);
  const lockedOpenProblem = lockedOpenProblemForTarget(options.target);
  if (!lockedOpenProblem) {
    throw new Error(`unknown open-problem-search target ${options.target}`);
  }
  assertReadable(options.pipeline, "--pipeline");
  assertReadable(options.queue, "--queue");
  await ensureRunWorkspace(options.workspace);

  const pipelineText = await readFile(options.pipeline, "utf8");
  const queueText = await readFile(options.queue, "utf8");
  const prompt = proposerPrompt(lockedOpenProblem, options.maxRounds);
  const proposerOutcomes = await runProposers(options.proposers, prompt, lockedOpenProblem);
  const rawRecords = proposerOutcomes.flatMap((outcome) => outcome.rawRecords);
  const { accepted, rejected } = normalizeCandidateRecords(rawRecords);
  const verifierResults = await verifyCandidates(accepted);
  const verifierCommands = verifierResults.flatMap((result) =>
    result.commands.map((command) => command.command.join(" "))
  );
  const solved = verifierResults.some((result) => result.trusted);
  const finalReport: FinalReport = {
    open_problem_solved: solved,
    trusted_status: solved ? "solved" : "not_solved",
    locked_open_problem: lockedOpenProblem,
    verifier_commands: verifierCommands,
    failure_reason: solved
      ? null
      : "No complete trusted verifier evidence settled the locked open problem.",
    partial_artifacts: requiredArtifacts,
    workspace: options.workspace
  };

  await writeText(options.workspace, "STATUS.md", statusMarkdown(lockedOpenProblem, options.pipeline, options.queue));
  await writeText(
    options.workspace,
    "PROPOSER_PROMPTS.md",
    proposerPromptsMarkdown(prompt, proposerOutcomes, pipelineText, queueText)
  );
  await writeText(options.workspace, "candidates/raw.jsonl", rawRecords.length === 0 ? "\n" : `${rawRecords.join("\n")}\n`);
  await writeText(options.workspace, "candidates/normalized.jsonl", jsonl(accepted));
  await writeText(options.workspace, "candidates/rejected.jsonl", jsonl(rejected));
  await writeText(options.workspace, "VERIFIER_LOG.md", verifierLogMarkdown(verifierResults));
  await writeText(options.workspace, "FIXER_LOG.md", fixerLogMarkdown(rejected));
  await writeText(options.workspace, "LEADERBOARD.md", leaderboardMarkdown(accepted, verifierResults));
  await writeText(
    options.workspace,
    "ITERATION_LOG.md",
    iterationLogMarkdown(options, proposerOutcomes, accepted.length, rejected.length)
  );
  await writeText(options.workspace, "REPORT.md", reportMarkdown(finalReport));

  return {
    report: dagReport(options.workspace, finalReport, input),
    finalReport,
    workspace: options.workspace
  };
}

function appRoot(): string {
  let dir = dirname(fileURLToPath(import.meta.url));
  while (dir !== dirname(dir)) {
    const packagePath = join(dir, "package.json");
    if (existsSync(packagePath)) {
      try {
        const packageJson = JSON.parse(readFileSync(packagePath, "utf8")) as { name?: string };
        if (packageJson.name === "@agenthero/formal-proofs") {
          return dir;
        }
      } catch {
        // Keep walking; malformed package files should not hide the app root.
      }
    }
    dir = dirname(dir);
  }
  throw new Error("could not locate @agenthero/formal-proofs app root");
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
  return join(process.cwd(), ".agenthero", "formal-proofs", "runs", `${target}-${safeKey}`);
}

function assertReadable(path: string, flag: string): void {
  if (!existsSync(path)) {
    throw new Error(`${flag} file does not exist: ${path}`);
  }
}

function validateCandidate(value: unknown): string | null {
  if (!isRecord(value)) {
    return "candidate must be an object";
  }
  const required = [
    "candidate_id",
    "lane",
    "locked_open_problem",
    "claim_type",
    "object",
    "parameters",
    "claimed_improvement",
    "verification_target",
    "expected_checker",
    "proposer",
    "notes"
  ];
  for (const key of required) {
    if (!(key in value)) {
      return `missing required field ${key}`;
    }
  }
  for (const key of [
    "candidate_id",
    "lane",
    "locked_open_problem",
    "claim_type",
    "claimed_improvement",
    "verification_target",
    "expected_checker",
    "proposer",
    "notes"
  ]) {
    if (typeof value[key] !== "string") {
      return `${key} must be a string`;
    }
  }
  const claimType = value.claim_type;
  if (
    typeof claimType !== "string" ||
    !["construction", "countermodel", "lower_bound", "upper_bound", "identity", "proof_sketch"].includes(claimType)
  ) {
    return "claim_type is not allowed";
  }
  const expectedChecker = value.expected_checker;
  if (
    typeof expectedChecker !== "string" ||
    !["lean", "haskell", "sat", "manual", "unknown"].includes(expectedChecker)
  ) {
    return "expected_checker is not allowed";
  }
  if (!isRecord(value.object)) {
    return "object must be an object";
  }
  if (!isRecord(value.parameters)) {
    return "parameters must be an object";
  }
  return null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function proposerPromptsMarkdown(
  prompt: string,
  outcomes: { proposer: string; status: string; command: string | null; error: string | null }[],
  pipelineText: string,
  queueText: string
): string {
  return [
    "# Proposer Prompts",
    "",
    "## Shared Prompt",
    "",
    "```text",
    prompt,
    "```",
    "",
    "## Proposer Availability",
    "",
    ...outcomes.map(
      (outcome) =>
        `- ${outcome.proposer}: ${outcome.status}${outcome.command ? ` (${outcome.command})` : ""}${outcome.error ? ` - ${outcome.error}` : ""}`
    ),
    "",
    "## Pipeline Snapshot",
    "",
    "```markdown",
    pipelineText.slice(0, 4000),
    "```",
    "",
    "## Queue Snapshot",
    "",
    "```markdown",
    queueText.slice(0, 4000),
    "```"
  ].join("\n");
}

function iterationLogMarkdown(
  options: OpenProblemSearchOptions,
  outcomes: { proposer: string; status: string; command: string | null; error: string | null }[],
  acceptedCount: number,
  rejectedCount: number
): string {
  return [
    "# Iteration Log",
    "",
    `target: ${options.target}`,
    `max_rounds: ${options.maxRounds}`,
    `pipeline: ${options.pipeline}`,
    `queue: ${options.queue}`,
    "",
    "## Round 1",
    "",
    ...outcomes.map(
      (outcome) =>
        `- proposer ${outcome.proposer}: ${outcome.status}${outcome.error ? ` (${outcome.error})` : ""}`
    ),
    `- accepted_candidates: ${acceptedCount}`,
    `- rejected_candidates: ${rejectedCount}`,
    "- loop_continue: false"
  ].join("\n");
}

function dagReport(workspace: string, finalReport: FinalReport, input?: DagIo): DagExecutionReport {
  const nodes: Array<[string, string, string, string[]]> = [
    ["status_lock", "prepare_inputs", "status_auditor", ["STATUS.md"]],
    ["proposer_claude", "agent", "proposer_claude", ["candidates/raw.jsonl"]],
    ["proposer_gpt5", "agent", "proposer_gpt5", ["candidates/raw.jsonl"]],
    ["proposer_gemini", "agent", "proposer_gemini", ["candidates/raw.jsonl"]],
    ["normalize_candidates", "synthesizer", "candidate_normalizer", ["candidates/normalized.jsonl", "candidates/rejected.jsonl"]],
    ["search_verify_fix_loop", "loop", "lean_or_sat_verifier", ["VERIFIER_LOG.md", "FIXER_LOG.md", "ITERATION_LOG.md"]],
    ["leaderboard", "synthesizer", "leaderboard_agent", ["LEADERBOARD.md"]],
    ["report", "render_artifacts", "reporter_agent", ["REPORT.md"]]
  ];
  return runtimeReport(
    "open-problem-search",
    nodes.map(([node_id, kind, role, outputs]) => runtimeNode(workspace, node_id, kind, outputs, { role })),
    {
      values: {
        report: finalReport,
        workspace
      },
      artifacts: artifactRefs(workspace)
    },
    { input }
  );
}
