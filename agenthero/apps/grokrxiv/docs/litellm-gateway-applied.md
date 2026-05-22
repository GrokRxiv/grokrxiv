# LiteLLM Gateway — Applied Notes

## Why a gateway at all?

The GrokRxiv review DAG can target several backends from one run:

- Ollama (native on Mac, containerized on Linux) for local 8B/16B/32B models.
- vLLM, when an operator wants higher throughput on Linux+GPU.
- Frontier APIs (Anthropic, OpenAI, Gemini) for the API runner.
- Future: Together, Fireworks, Groq, Cerebras — same OpenAI protocol.

If the orchestrator spoke each backend's native dialect directly, every
backend swap would be a Rust code change, every retry policy would have to
be re-implemented, and every cost-accounting hook would have to be wired
six times. We don’t want that.

LiteLLM is a thin OpenAI-protocol reverse proxy that abstracts the
backends behind one URL. The orchestrator only ever knows
`http://litellm:4000/v1` (or `http://localhost:4000/v1` on Mac).

## What LiteLLM gives us, concretely

1. **One stable internal URL.** Containers in either topology call
   `http://litellm:4000/v1/chat/completions` regardless of whether the
   actual model is on host-Ollama, in-cluster Ollama, vLLM, or Anthropic.
   Backend swaps are a YAML edit, not a redeploy.

2. **Centralized retries and rate limiting.** Configured once in
   `router_settings`. The Rust runners no longer need to implement
   per-backend retry policies; they get exponential backoff and circuit
   breaking for free.

3. **Observability via `/ui`.** LiteLLM ships a live UI at
   `http://localhost:4000/ui` showing per-request latency, token counts,
   error rates, and cost. This is enormous for debugging slow review runs
   where you want to know *which* model was the bottleneck.

4. **Cost accounting.** LiteLLM tracks token usage per model and (for
   commercial backends) computes USD cost from its built-in price tables.
   For a research-grade review pipeline this is the difference between
   "this run cost $0.37" and "we have no idea".

5. **vLLM / Together / frontier pass-through without code changes.** The
   same Rust runner that targets `model_name: qwen-32b` can be redirected
   from Ollama to vLLM (which is also OpenAI-compatible) or to Together's
   hosted Qwen2.5 by editing one config line. The orchestrator does not
   care.

6. **Master-key auth.** A single `LITELLM_MASTER_KEY` env var is the bearer
   token. This is enough for our threat model (local Mac dev, single-VPC
   cloud), and the gateway can issue scoped child keys later if we
   multi-tenant.

## What LiteLLM is *not*

- It is **not** a model router in the sense of "pick the best model for
  this prompt". That logic stays in the GrokRxiv orchestrator's
  `runners::router`. LiteLLM just resolves a *named* model to a backend.
- It is **not** required at runtime for the API runner — that runner can
  speak directly to Anthropic/OpenAI. LiteLLM is included as a pass-through
  for the frontier APIs only so that all traffic appears in one cost
  ledger; an operator can disable that and let the Rust client talk
  directly to Anthropic if they prefer.

## File layout

- `infra/litellm/config.yaml` — Mac local topology. Every Ollama
  `api_base` is `http://host.docker.internal:11434` because Ollama runs on
  the Mac host, not in a container.
- `infra/litellm/config.cloud.yaml` — cloud topology. Same `model_list`
  shape, but `api_base` is `http://ollama:11434` (in-VPC service DNS).
- Both files share identical `litellm_settings`, `router_settings`, and
  `general_settings`. The router strategy is `simple-shuffle`, which is
  fine while we have one replica per model; swap to
  `least-busy` or `usage-based-routing` when scaling out.

## Operator checklist

- Set `LITELLM_MASTER_KEY` in `.env` for both topologies (any random
  string; this is the bearer token clients use).
- For the cloud topology, also set `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`,
  and `GOOGLE_GENERATIVE_AI_API_KEY` if the frontier pass-throughs are in
  use. LiteLLM reads them from the environment via the `os.environ/...`
  syntax in `config.yaml`.
- The UI at `/ui` is unauthenticated in dev. Behind any cloud topology,
  put it behind a private network ingress.
