import { access } from "node:fs/promises";
import { constants } from "node:fs";
import { spawn } from "node:child_process";

import type { Candidate } from "./types.js";

export interface ProposerOutcome {
  proposer: string;
  status: "completed" | "unavailable" | "failed";
  command: string | null;
  rawRecords: string[];
  error: string | null;
}

export async function runProposers(
  proposers: string[],
  prompt: string,
  lockedOpenProblem: string
): Promise<ProposerOutcome[]> {
  const outcomes: ProposerOutcome[] = [];
  for (const proposer of proposers) {
    if (proposer === "fixture") {
      outcomes.push(fixtureOutcome(lockedOpenProblem));
      continue;
    }
    outcomes.push(await runLiveProposer(proposer, prompt));
  }
  return outcomes;
}

export function proposerPrompt(lockedOpenProblem: string, maxRounds: number): string {
  return [
    "You are a Formal Proofs proposer for an open-problem search run.",
    "Emit JSONL only. Each line must match candidate.schema.json exactly.",
    `Locked open problem: ${lockedOpenProblem}`,
    `Maximum rounds: ${maxRounds}`,
    "For E677=>fin E255, propose either a finite countermodel search candidate or a proof/certificate target.",
    "Do not claim the open problem is solved. Only trusted verifier output can do that."
  ].join("\n");
}

function fixtureOutcome(lockedOpenProblem: string): ProposerOutcome {
  const candidate: Candidate = {
    candidate_id: "fixture-e677-e255-order1",
    lane: "E677 =>fin E255",
    locked_open_problem: lockedOpenProblem,
    claim_type: "countermodel",
    object: { operation_table: [[0]], relation: "fixture-only" },
    parameters: { order: 1 },
    claimed_improvement: "Deterministic fixture candidate; not a frontier countermodel.",
    verification_target: "finite_magma_countermodel",
    expected_checker: "haskell",
    proposer: "fixture",
    notes: "This fixture exercises artifacts and verifier gating without solving the target."
  };
  return {
    proposer: "fixture",
    status: "completed",
    command: null,
    rawRecords: [JSON.stringify(candidate), "{\"candidate_id\":\"fixture-malformed\"}"],
    error: null
  };
}

async function runLiveProposer(proposer: string, prompt: string): Promise<ProposerOutcome> {
  const command = proposerCommand(proposer);
  if (!command) {
    return {
      proposer,
      status: "unavailable",
      command: null,
      rawRecords: [],
      error: `unknown proposer ${proposer}`
    };
  }
  const [bin, ...args] = command;
  const available = await commandAvailable(bin);
  if (!available) {
    return {
      proposer,
      status: "unavailable",
      command: command.join(" "),
      rawRecords: [],
      error: `${bin} not found`
    };
  }

  const result = await spawnWithPrompt(command, prompt, 120000);
  if (result.exitCode !== 0) {
    return {
      proposer,
      status: "failed",
      command: command.join(" "),
      rawRecords: [],
      error: result.stderr || `exit ${result.exitCode}`
    };
  }
  return {
    proposer,
    status: "completed",
    command: command.join(" "),
    rawRecords: extractJsonRecords(result.stdout),
    error: null
  };
}

function proposerCommand(proposer: string): string[] | null {
  if (proposer === "claude") {
    return ["claude", "--print"];
  }
  if (proposer === "gpt-5" || proposer === "codex") {
    return ["codex", "exec", "--json"];
  }
  if (proposer === "gemini") {
    return [process.env.AGENTHERO_ANTIGRAVITY_BIN || "agy", "run", "--json"];
  }
  return null;
}

async function commandAvailable(command: string): Promise<boolean> {
  if (command.includes("/")) {
    try {
      await access(command, constants.X_OK);
      return true;
    } catch {
      return false;
    }
  }
  const result = await spawnWithPrompt(["/bin/sh", "-lc", `command -v ${shellQuote(command)}`], "", 5000);
  return result.exitCode === 0;
}

function spawnWithPrompt(
  command: string[],
  prompt: string,
  timeoutMs: number
): Promise<{ exitCode: number | null; stdout: string; stderr: string }> {
  return new Promise((resolve) => {
    const child = spawn(command[0], command.slice(1), { stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    const timeout = setTimeout(() => child.kill("SIGTERM"), timeoutMs);
    child.stdout.on("data", (chunk: Buffer) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderr += chunk.toString("utf8");
    });
    child.on("error", (error) => {
      clearTimeout(timeout);
      resolve({ exitCode: null, stdout, stderr: `${stderr}${error.message}` });
    });
    child.on("close", (code) => {
      clearTimeout(timeout);
      resolve({ exitCode: code, stdout, stderr });
    });
    child.stdin.end(prompt);
  });
}

function extractJsonRecords(stdout: string): string[] {
  const trimmed = stdout.trim();
  if (trimmed.length === 0) {
    return [];
  }
  const records = trimmed
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.startsWith("{"));
  if (records.length > 0) {
    return records;
  }
  try {
    const parsed = JSON.parse(trimmed) as unknown;
    if (Array.isArray(parsed)) {
      return parsed.map((item) => JSON.stringify(item));
    }
    if (typeof parsed === "object" && parsed !== null) {
      return [JSON.stringify(parsed)];
    }
  } catch {
    return [trimmed];
  }
  return [trimmed];
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}
