import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import type { ArtifactRef, DagExecutionEvent, DagExecutionReport, DagIo, DagNodeReport } from "./types.js";

export function runtimeReport(
  dagType: string,
  nodes: DagNodeReport[],
  outputs: DagExecutionReport["outputs"],
  options: { input?: DagIo } = {}
): DagExecutionReport {
  const manifest = readDagManifest(dagType);
  const context = runtimeEventContext(dagType, manifest, options.input);
  const status = reportStatus(nodes);
  return {
    dag_type: dagType,
    manifest_version: manifest.version,
    manifest_hash: manifest.hash,
    status,
    input: normalizeInput(options.input),
    nodes,
    outputs,
    events: [
      dagStartedEvent(context),
      ...nodes.flatMap((node) => nodeEvents(node, context)),
      dagTerminalEvent(context, status, nodes.length)
    ]
  };
}

function reportStatus(nodes: DagNodeReport[]): DagExecutionReport["status"] {
  if (nodes.some((node) => node.status === "failed")) {
    return "failed";
  }
  if (nodes.some((node) => node.status === "awaiting_approval")) {
    return "awaiting_approval";
  }
  if (nodes.some((node) => node.status === "running")) {
    return "running";
  }
  if (nodes.some((node) => node.status === "pending")) {
    return "pending";
  }
  if (nodes.length > 0 && nodes.every((node) => node.status === "skipped")) {
    return "skipped";
  }
  if (nodes.some((node) => node.status === "degraded" || node.status === "skipped")) {
    return "degraded";
  }
  return "ok";
}

export function runtimeNode(
  workspace: string,
  node_id: string,
  kind: string,
  outputs: string[],
  options: Partial<DagNodeReport> = {}
): DagNodeReport {
  return {
    node_id,
    kind,
    status: options.status ?? "ok",
    attempt: 1,
    role: options.role ?? null,
    tool: options.tool ?? null,
    child_dag_type: options.child_dag_type ?? null,
    required: options.required ?? true,
    executor: options.executor ?? "typescript",
    model: options.model ?? null,
    prompt_hash: options.prompt_hash ?? null,
    command: options.command ?? null,
    exit_status: options.exit_status ?? null,
    inputs: options.inputs ?? [],
    outputs,
    input_refs: options.input_refs ?? {},
    output_refs: outputRefs(workspace, outputs),
    diagnostic_refs: options.diagnostic_refs ?? {},
    policy: options.policy ?? {},
    warning: options.warning ?? null,
    error: options.error ?? null,
    latency_ms: options.latency_ms ?? 0,
    trace: options.trace ?? {}
  };
}

function outputRefs(workspace: string, outputs: string[]): Record<string, string> {
  return Object.fromEntries(outputs.map((output) => [output, join(workspace, output)]));
}

interface RuntimeEventContext {
  dagType: string;
  manifestVersion: number;
  manifestHash: string;
  appRunId: string;
  dagRunId: string;
  artifactId: string;
  leaseId: string;
}

function runtimeEventContext(
  dagType: string,
  manifest: { version: number; hash: string },
  input?: DagIo
): RuntimeEventContext {
  return {
    dagType,
    manifestVersion: manifest.version,
    manifestHash: manifest.hash,
    appRunId: inputString(input, "app_run_id"),
    dagRunId: inputString(input, "dag_run_id"),
    artifactId: inputString(input, "artifact_id"),
    leaseId: inputString(input, "lease_id")
  };
}

function inputString(input: DagIo | undefined, key: string): string {
  const value = input?.values?.[key];
  return typeof value === "string" ? value : "";
}

function nodeEvents(node: DagNodeReport, context: RuntimeEventContext): DagExecutionEvent[] {
  return [nodeStartedEvent(node, context), nodeTerminalEvent(node, context)];
}

function dagStartedEvent(context: RuntimeEventContext): DagExecutionEvent {
  return {
    level: "info",
    event_type: "dag.started",
    node_id: null,
    message: `${context.dagType} started`,
    payload: {
      ...commonDagPayload(context),
      status: null,
      node_count: null
    }
  };
}

function dagTerminalEvent(
  context: RuntimeEventContext,
  status: DagExecutionReport["status"],
  nodeCount: number
): DagExecutionEvent {
  const event_type =
    status === "failed"
      ? "dag.failed"
      : status === "awaiting_approval"
        ? "dag.awaiting_approval"
        : status === "skipped"
          ? "dag.skipped"
          : "dag.completed";
  const level = status === "failed" ? "error" : status === "degraded" || status === "awaiting_approval" ? "warn" : "info";
  return {
    level,
    event_type,
    node_id: null,
    message: `${context.dagType} ${status}`,
    payload: {
      ...commonDagPayload(context),
      status,
      node_count: nodeCount
    }
  };
}

function nodeStartedEvent(node: DagNodeReport, context: RuntimeEventContext): DagExecutionEvent {
  return {
    level: "info",
    event_type: "node.started",
    node_id: node.node_id,
    message: `${node.node_id} started`,
    payload: {
      ...commonNodePayload(node, context),
      node_id: node.node_id,
      kind: node.kind,
      attempt: node.attempt ?? 1
    }
  };
}

function nodeTerminalEvent(node: DagNodeReport, context: RuntimeEventContext): DagExecutionEvent {
  const failed = node.status === "failed";
  const waiting = node.status === "awaiting_approval";
  const skipped = node.status === "skipped";
  return {
    level: failed ? "error" : waiting ? "warn" : "info",
    event_type: failed
      ? "node.failed"
      : waiting
        ? "node.awaiting_approval"
        : skipped
          ? "node.skipped"
          : "node.completed",
    node_id: node.node_id,
    message: node.error ?? node.warning ?? `${node.node_id} ${node.status}`,
    payload: {
      ...commonNodePayload(node, context),
      node_id: node.node_id,
      status: node.status,
      kind: node.kind,
      attempt: node.attempt ?? 1,
      latency_ms: node.latency_ms ?? 0,
      duration_ms: node.latency_ms ?? 0,
      command: node.command ?? null,
      exit_status: node.exit_status ?? null,
      model: node.model ?? null,
      prompt_hash: node.prompt_hash ?? null,
      input_refs: node.input_refs ?? {},
      output_refs: node.output_refs ?? {},
      diagnostic_refs: node.diagnostic_refs ?? {},
      error: node.error ?? null,
      warning: node.warning ?? null
    }
  };
}

function commonNodePayload(
  node: DagNodeReport,
  context: RuntimeEventContext
): Record<string, unknown> {
  return {
    app_run_id: context.appRunId,
    dag_run_id: context.dagRunId,
    dag_type: context.dagType,
    manifest_version: context.manifestVersion,
    manifest_hash: context.manifestHash,
    node_id: node.node_id,
    node_kind: node.kind,
    kind: node.kind,
    attempt: node.attempt ?? 1,
    tool_id: node.tool ?? "",
    tool: node.tool ?? "",
    role: node.role ?? "",
    child_dag_type: node.child_dag_type ?? "",
    required: node.required ?? true,
    artifact_id: context.artifactId,
    lease_id: context.leaseId,
    executor: node.executor ?? ""
  };
}

function commonDagPayload(context: RuntimeEventContext): Record<string, unknown> {
  return {
    app_run_id: context.appRunId,
    dag_run_id: context.dagRunId,
    dag_type: context.dagType,
    manifest_version: context.manifestVersion,
    manifest_hash: context.manifestHash,
    node_id: null,
    node_kind: null,
    attempt: null,
    tool_id: null,
    artifact_id: context.artifactId,
    lease_id: context.leaseId,
    exit_status: null,
    duration_ms: null
  };
}

export function artifactOutput(workspace: string, names: string[]): Record<string, ArtifactRef> {
  return Object.fromEntries(
    names.map((name) => [
      name,
      {
        uri: join(workspace, name),
        media_type: mediaType(name),
        metadata: {}
      }
    ])
  );
}

function normalizeInput(input?: DagIo): DagExecutionReport["input"] {
  return {
    values: input?.values ?? {},
    artifacts: input?.artifacts ?? {}
  };
}

function readDagManifest(dagType: string): { version: number; hash: string } {
  const path = join(appRoot(), "dags", `${dagType}.yaml`);
  if (!existsSync(path)) {
    throw new Error(`formal-proofs DAG manifest not found: ${path}`);
  }
  const bytes = readFileSync(path);
  const text = bytes.toString("utf8");
  const version = Number.parseInt(/^version:\s*(\d+)\s*$/m.exec(text)?.[1] ?? "1", 10);
  return {
    version: Number.isFinite(version) ? version : 1,
    hash: `sha256:${createHash("sha256").update(bytes).digest("hex")}`
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

function mediaType(name: string): string {
  if (name.endsWith(".json") || name.endsWith(".jsonl")) {
    return "application/json";
  }
  return "text/markdown";
}
