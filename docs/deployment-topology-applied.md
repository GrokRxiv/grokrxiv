# Deployment Topology — Applied Notes

GrokRxiv supports two deployment topologies that share the same orchestrator
binary and the same LiteLLM gateway contract. Only the *placement* of the
inference runtime changes.

## TL;DR

| Topology | Inference runtime         | Surrounding services      | Where you run it          |
|----------|---------------------------|---------------------------|---------------------------|
| Local    | Ollama **native on host** | Docker (LiteLLM, WebUI, Redis) | Mac M-series (M1/M2/M3/M4) |
| Cloud    | Ollama (+vLLM) in Docker  | Docker (LiteLLM, Redis, orchestrator) | Linux + NVIDIA GPU         |

## The Mac rule: Ollama runs natively, not in a container

Docker Desktop on macOS runs containers inside a Linux VM (xhyve / Apple
Hypervisor). That VM has **no passthrough to the Metal GPU**. If you start
Ollama inside a Mac container, every prompt drops to CPU inference. For a
4-bit-quantized 32B model that is roughly 1–2 tokens/sec — useless for the
review DAG. Native Ollama on the same Mac, talking to the Metal stack
directly, runs the same model at 15–35 tok/sec.

Therefore on Mac:

- **Native (host):** Ollama (`brew install ollama` or the official `.app`).
- **Containerized:** everything else — LiteLLM gateway, Open WebUI, Redis.

Containers reach the host’s native Ollama through `host.docker.internal`,
which is wired up explicitly in `infra/compose.local.yml`:

```yaml
extra_hosts:
  - "host.docker.internal:host-gateway"
```

The LiteLLM model_list in `infra/litellm/config.yaml` therefore points at
`http://host.docker.internal:11434` for every Ollama-backed model.

## The cloud rule: everything in containers, GPUs via nvidia-container-toolkit

On a Linux host with an NVIDIA card and `nvidia-container-toolkit` installed,
GPU passthrough works correctly inside containers. The cloud compose file
(`infra/compose.cloud.yml`) reserves all visible GPUs to the Ollama (and
optional vLLM) containers via the standard `deploy.resources.reservations`
syntax:

```yaml
deploy:
  resources:
    reservations:
      devices:
        - driver: nvidia
          count: all
          capabilities: [gpu]
```

The orchestrator container points its `OLLAMA_HOST` env var at
`http://ollama:11434` (the in-network service DNS name) and is otherwise
identical to the Mac build. vLLM is gated behind a compose `profiles: [vllm]`
so it only starts when explicitly requested (`docker compose --profile vllm
up`); some operators will skip vLLM entirely and route everything through
Ollama for simpler ops.

## The surrounding services

### LiteLLM gateway (port 4000)

LiteLLM is the OpenAI-compatible reverse proxy that fronts every inference
backend. The orchestrator and Open WebUI never speak to Ollama directly —
they speak OpenAI-protocol to LiteLLM, which maps the requested
`model_name` (e.g. `qwen-32b`) to the configured backend (`ollama/...` on
the right `api_base`). This gives us:

- One stable internal URL regardless of which backend is online.
- Retries and rate limits configured centrally.
- Cost accounting and JSON request logs visible at
  `http://localhost:4000/ui`.
- Trivial swap to vLLM, Together, or a frontier API by editing one config
  file — zero Rust code changes.

See `docs/litellm-gateway-applied.md` for the deeper rationale.

### Open WebUI (port 3001)

A ChatGPT-style frontend pre-wired against LiteLLM. Useful for ad-hoc
prompting of the same models the review DAG uses (`qwen-32b`, `llama-8b`,
…). `WEBUI_AUTH=False` because this is a local-only convenience UI; never
enable that flag in cloud topology.

### Redis (port 6379)

Used by LiteLLM for cross-replica routing state (request counters,
rate-limit state, simple-shuffle bookkeeping). The orchestrator does not
require Redis itself in M1; it’s here so that scaling LiteLLM horizontally
later is a no-op.

## Operator’s flow (Mac)

```sh
# 1. One-time: install Ollama natively and pull the models the config expects.
brew install ollama
brew services start ollama          # listens on 127.0.0.1:11434
ollama pull qwen2.5:32b-instruct-q4_K_M
ollama pull llama3.1:8b-instruct-q4_K_M
ollama pull deepseek-coder-v2:16b

# 2. Bring up the surrounding containers.
just up-local
#   → LiteLLM     http://localhost:4000
#   → Open WebUI  http://localhost:3001
#   → Redis       localhost:6379

# 3. Run the orchestrator on the host (it talks to LiteLLM, not Ollama).
just orch
#   or, against a specific paper:
grokrxiv app run research review 2605.12484

# 4. Tear down when done.
just down
```

## Operator’s flow (cloud / Linux + GPU)

```sh
# 1. Verify nvidia-container-toolkit is installed and `docker run --rm --gpus all
#    nvidia/cuda:12.4.1-base-ubuntu22.04 nvidia-smi` works.

# 2. Point .env at production secrets (DATABASE_URL, LITELLM_MASTER_KEY,
#    ANTHROPIC_API_KEY, etc.).

# 3. Bring up the whole stack.
just up-cloud
#   → Ollama       http://localhost:11434
#   → LiteLLM      http://localhost:4000
#   → Orchestrator http://localhost:8080

# 4. Optionally enable vLLM alongside Ollama:
docker compose -f infra/compose.cloud.yml --profile vllm up -d
```

## What is *not* in scope here

- Supabase is managed separately via `supabase start` / the Supabase CLI
  (already in Docker, via its own compose project).
- TLS termination, ingress, and DNS are the cloud provider’s responsibility
  (Fly.io, Railway, ECS, etc.). The compose files here are the *workload*
  topology, not the edge.
- Model selection per task is handled by the orchestrator’s router in Rust;
  this layer only guarantees that whichever `model_name` it asks for is
  reachable.
