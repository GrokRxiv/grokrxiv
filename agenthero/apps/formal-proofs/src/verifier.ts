import { access } from "node:fs/promises";
import { constants } from "node:fs";
import { spawn } from "node:child_process";

import type { Candidate, VerifierCommandResult, VerifierResult } from "./types.js";

const allowedExecutablesByChecker: Record<string, string[]> = {
  lean: ["lean", "lake"],
  haskell: ["ghc", "runghc", "cabal", "stack"],
  sat: ["kissat", "cadical", "z3"],
  manual: [],
  unknown: []
};

export async function verifyCandidates(candidates: Candidate[]): Promise<VerifierResult[]> {
  const results: VerifierResult[] = [];
  for (const candidate of candidates) {
    results.push(await verifyCandidate(candidate));
  }
  return results;
}

export async function verifyCandidate(candidate: Candidate): Promise<VerifierResult> {
  const commands = candidate.parameters.verifier_commands;
  if (!isCommandList(commands)) {
    return {
      candidate_id: candidate.candidate_id,
      trusted: false,
      status: "unverified",
      expected_checker: candidate.expected_checker,
      commands: [],
      evidence_path: null,
      notes: "No app-owned trusted verifier command was available for this candidate."
    };
  }

  const allowed = allowedExecutablesByChecker[candidate.expected_checker] ?? [];
  const disallowed = commands.find((command) => !allowed.includes(command[0] ?? ""));
  if (disallowed) {
    return {
      candidate_id: candidate.candidate_id,
      trusted: false,
      status: "failed",
      expected_checker: candidate.expected_checker,
      commands: [],
      evidence_path: null,
      notes: `Verifier command ${disallowed.join(" ")} is not allowed for checker ${candidate.expected_checker}.`
    };
  }

  const commandResults: VerifierCommandResult[] = [];
  for (const command of commands) {
    const executableAvailable = await commandAvailable(command[0]);
    if (!executableAvailable) {
      return {
        candidate_id: candidate.candidate_id,
        trusted: false,
        status: "checker_unavailable",
        expected_checker: candidate.expected_checker,
        commands: commandResults,
        evidence_path: null,
        notes: `Verifier executable ${command[0]} is unavailable.`
      };
    }
    commandResults.push(await runCommand(command));
  }

  const allPassed = commandResults.length > 0 && commandResults.every((result) => result.exit_code === 0);
  return {
    candidate_id: candidate.candidate_id,
    trusted: allPassed,
    status: allPassed ? "verified" : "failed",
    expected_checker: candidate.expected_checker,
    commands: commandResults,
    evidence_path: allPassed ? "VERIFIER_LOG.md" : null,
    notes: allPassed
      ? "All app-owned verifier commands exited successfully."
      : "At least one app-owned verifier command failed."
  };
}

function isCommandList(value: unknown): value is string[][] {
  return (
    Array.isArray(value) &&
    value.every(
      (command) =>
        Array.isArray(command) &&
        command.length > 0 &&
        command.every((part) => typeof part === "string" && part.length > 0)
    )
  );
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

  const result = await runCommand(["/bin/sh", "-lc", `command -v ${shellQuote(command)}`], 5000);
  return result.exit_code === 0;
}

function runCommand(command: string[], timeoutMs = 30000): Promise<VerifierCommandResult> {
  const started = Date.now();
  return new Promise((resolve) => {
    const child = spawn(command[0], command.slice(1), { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    const timeout = setTimeout(() => {
      child.kill("SIGTERM");
    }, timeoutMs);
    child.stdout.on("data", (chunk: Buffer) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderr += chunk.toString("utf8");
    });
    child.on("error", (error) => {
      clearTimeout(timeout);
      resolve({
        command,
        exit_code: null,
        stdout,
        stderr: `${stderr}${error.message}`,
        duration_ms: Date.now() - started
      });
    });
    child.on("close", (code) => {
      clearTimeout(timeout);
      resolve({
        command,
        exit_code: code,
        stdout,
        stderr,
        duration_ms: Date.now() - started
      });
    });
  });
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}
