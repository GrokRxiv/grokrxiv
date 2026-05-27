# Report: Elevating AgentHero to Commercial-Grade DAFOps

This report reviews the current orchestration abstractions in `agent-runtime` and outlines the technical roadmap to elevate the platform to a commercial-grade **AgentHero Runtime** (DAFOps).

## 1. Current State Assessment

The current `agent-runtime` crate serves as a **contract layer**. It defines the "neutral" interface between the orchestrator and the execution backends.

### Key Abstractions:
*   **`AgentRunner` Trait**: A pure execution interface for one-shot LLM or CLI calls.
*   **`AgentRunnerKind`**: Limited to `Api` (direct LLM) and `Cli` (local subprocess).
*   **`AppAdapterProtocol (v1)`**: A JSON-based stdin/stdout protocol for communicating with process-backed DAG apps.
*   **Supervisor Model**: Logic for side-effects (DB, Cache, PRs) is separated from the "pure" reasoning/execution of agents.
*   **`AgentSpec` / `AgentInput`**: Primarily focused on single-role execution rather than holistic DAG lifecycle management.

---

## 2. Gaps to Commercial-Grade DAFOps

To move from a "contract library" to a "durable runtime," the following gaps must be addressed:

### A. Global Installs & Application Lifecycle
*   **Current**: Relies on local binaries (`claude`, `gemini`) being in the `PATH` or manually configured in YAML. There is no concept of an "Installed App."
*   **Needed**: A `DAGApp` bundle format. The CLI should support `agenthero install <app-slug>` which fetches the bundle, validates contracts, and registers it in a global/user-scoped registry.

### B. Hardcoded Variables & Provider Registry
*   **Current**: `CliRunner` selection logic and provider mapping are often driven by ad-hoc environment variables or specific YAML fields.
*   **Needed**: A formal **Provider Registry**. Instead of hardcoding "claude" -> "binary X", the runtime should use a discovery mechanism that maps `AgentSpec.provider` to a registered capability (Local CLI, WASM, or Remote API).

### C. Durable DAG Execution
*   **Current**: Execution is "pure-ish" but volatile. If the process crashes mid-DAG, there is no built-in checkpointing in the `agent-runtime` contracts.
*   **Needed**: A **Tokio-based Durable Executor**. The core loop must persist `AgentRun` results to a state store *between nodes*.
    *   **Checkpoint Engine**: Ability to resume a DAG from the last successful node.
    *   **Retry Engine**: Exponential backoff and jitter for transient LLM/Network failures, persisted across restarts.

### D. Multi-Language Backend Support
*   **Current**: Limited to `Api` and `Cli`.
*   **Needed**: Expansion of `AgentRunnerKind` to include:
    *   **WASM (Wasmtime)**: For portable, sandboxed, high-performance node execution.
    *   **Python (PyO3/Container)**: For data-science/LLM-native tasks.
    *   **JS/TS (Deno/Subprocess)**: For web-heavy agentic workflows.

---

## 3. The AgentHero Runtime Vision (DAFOps)

### Architecture: The Tokio-based DAG Engine
The runtime should be reimagined as a durable execution engine built on Tokio, moving the "Supervisor" logic into a formal **DAG Scheduler**.

```rust
// The vision for the core execution loop
while let Some(node) = ready_queue.next().await {
    tokio::spawn(async move {
        let input = state_store.load_inputs(node).await?;
        contract_validator::validate_input(node, &input)?;
        
        let output = runner_factory.get(node.runner_kind)
            .run(node.spec, input)
            .await?;
            
        contract_validator::validate_output(node, &output)?;
        state_store.persist_result(node, output).await?;
        scheduler.mark_dependents_ready(node).await?;
    });
}
```

### Strategic Requirements for CLI & Ops:

1.  **Contract-First DAGApps**:
    *   Every node must have an `input.schema.json` and `output.schema.json`.
    *   **Capability Policies**: A manifest defining what a DAGApp is allowed to do (Network access, File system access, LLM usage limits).

2.  **Distributed Worker Pool**:
    *   The `agent-runtime` should support a `Remote` runner kind that dispatches tasks to gRPC/HTTP workers, enabling the DAG to span multiple machines.

3.  **Unified Entry Point**:
    *   `agenthero run <app>` should not just "execute a script" but "instantiate a DAG in the runtime," providing a unique `run_id` for monitoring and debugging via a standard event bus.

4.  **Artifact & State Store**:
    *   Transition from local `workdir` to a formal **Artifact Store** abstraction that supports S3/Local/Memory backends, ensuring that data passed between DAG nodes is durable and auditable.

## 4. Summary of CLI Perspective Elevators

| Feature | Current (Library) | Commercial (Runtime) |
| :--- | :--- | :--- |
| **Installation** | Manual binary placement | `agenthero install <app>` |
| **Config** | Env vars / Local YAML | Global Config + Secret Store |
| **Execution** | Volatile subprocesses | Durable Tokio-based Tasks |
| **Sandboxing** | Opt-in Container | Default WASM/Container isolation |
| **Monitoring** | Stdout logs | Event Bus + Execution Dashboard |
| **State** | Filesystem side-effects | Versioned Artifact Store |

---
**Technical Description**: A Rust/Tokio durable execution engine for typed, distributed, agentic DAG applications.
