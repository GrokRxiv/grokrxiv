# GrokRxiv Model Routing Architecture

> Long-term routing design for using subscription-backed CLIs, direct provider
> APIs, cloud agents, and local models without blurring the runtime boundary.

## 1. Problem

GrokRxiv now has separate concepts that are easy to mix up:

- `ReviewAgent` / extraction stage: what work needs to be done.
- `AgentRunner`: how that work is executed.
- `LLMProvider`: an HTTP provider abstraction for direct API-style backends.
- `RuntimeConfig.model_for`, `--model-for`, and `GROKRXIV_<ROLE>_MODEL`:
  operator model overrides.

The risk is treating a model id as if it fully describes execution. It does
not. A model id like `gemini-3-flash-preview` is only meaningful after the
system also knows whether it is being reached through the Gemini CLI, direct
Gemini API, a gateway, or a local OpenAI-compatible endpoint.

The durable rule is:

```text
agent role/stage
  -> model policy / route profile
  -> resolved endpoint: runner + provider + model + capabilities
  -> runner executes it
```

Per-agent env vars are useful operator overrides, but they should not become
the architecture. The architecture needs a routing layer that resolves a full
execution endpoint.

## 2. Boundary

Keep the Rust supervisor as the control plane. It owns the typed DAG,
concurrency, cache lookup, verifier gates, persistence, moderation state,
rendering, PR handoff, and publish lifecycle.

Under that, use three separate abstractions:

```text
ReviewAgent / extraction stage
  declares the task, schema, prompt, tools, and constraints

ModelResolver
  resolves runner + provider + model + capabilities from a profile

AgentRunner
  executes through CLI, API, cloud, or local inference
```

`LLMProvider` remains valuable, but it should stay on the HTTP/API-local side
of the boundary. It is the right abstraction for direct APIs, LiteLLM,
OpenAI-compatible local servers, vLLM, and similar transports.

Do not force local CLIs into `LLMProvider`. CLI execution has different
properties:

- subprocess lifecycle
- logged-in subscription auth
- API-key scrubbing
- cwd/workdir risk
- stdout/stderr parsing
- provider-specific command flags
- local tool fallback behavior
- timeout and quota signals

Those belong in `AgentRunner`, not `LLMProvider`.

## 3. Route Profiles

Introduce route profiles as the primary operator concept. A profile maps each
role or extraction stage to a full execution endpoint.

Example shape:

```toml
[profiles.cli_subscription.review.citation]
runner = "cli"
provider = "gemini"
model = "gemini-3-flash-preview"

[profiles.api.review.citation]
runner = "api"
provider = "gemini"
model = "gemini-3-flash-preview"

[profiles.local.review.citation]
runner = "local_inference"
provider = "openai_compatible"
model = "qwen3:32b"
```

This allows the same role to move across execution backends without changing
role code or pretending the backends behave the same.

Recommended top-level operator defaults:

```bash
GROKRXIV_RUNTIME_PROFILE=cli_subscription
GROKRXIV_RUNNER=cli
GROKRXIV_EXTRACTOR=cli
GROKRXIV_ALLOW_PROVIDER_API=0
```

Direct API use remains explicit:

```bash
GROKRXIV_RUNTIME_PROFILE=api
GROKRXIV_RUNNER=api
GROKRXIV_EXTRACTOR=api
GROKRXIV_ALLOW_PROVIDER_API=1
```

Local inference is its own profile:

```bash
GROKRXIV_RUNTIME_PROFILE=local
GROKRXIV_RUNNER=local_inference
GROKRXIV_EXTRACTOR=local_inference
```

## 4. Capabilities

The resolver should not only match names. It should check whether a route can
do the job.

A route can declare capabilities such as:

- `json_schema`
- `tool_calling`
- `vision`
- `long_context`
- `web_lookup`
- `local_files`
- `api_billing`
- `subscription_cli`

Review examples:

```text
summary
  requires: json_schema

technical_correctness
  requires: json_schema, long_context

novelty
  requires: json_schema, long_context

reproducibility
  requires: json_schema, local_files optional

citation
  requires: json_schema, long_context

meta_reviewer
  requires: json_schema, long_context
```

Extraction examples:

```text
macros
  requires: tool_calling, local_files

equations
  requires: tool_calling, local_files

theorems
  requires: tool_calling, local_files, long_context

citations
  requires: tool_calling, local_files

vlm
  requires: vision, local_files
```

If a profile maps a stage to a backend that lacks a required capability, the
CLI should fail before running the paper:

```text
route profile local cannot run extraction.vlm:
runner local_inference provider openai_compatible model qwen3:32b lacks vision
```

That is better than a half-run that fails after spending 20 minutes.

## 5. Override Policy

The existing public overrides should stay, but they should be understood as
last-mile operator controls:

```bash
--model-for citation=gemini-3-flash-preview
GROKRXIV_CITATION_MODEL=gemini-3-flash-preview
```

Those override only the model field. They should not silently change runner or
provider. If the chosen model is incompatible with the resolved provider or
runner, fail clearly.

The long-term precedence should be:

```text
CLI explicit flags
  > GROKRXIV_<ROLE>_MODEL and related env overrides
  > selected route profile
  > default profile
  > agent YAML fallback
```

The important distinction:

```text
route profile = normal architecture
per-agent env = operator override
agent YAML    = fallback/default declaration
```

## 6. Why Not One Bridge For Everything?

A single bridge that hides CLI, API, and local behind one provider trait looks
clean, but it would encode false sameness.

Direct API and local OpenAI-compatible servers are request/response transports.
They fit `LLMProvider`.

CLIs are process runtimes. They need command construction, auth discovery,
environment scrubbing, cwd isolation, stdout unwrapping, and provider-specific
failure classification. They fit `AgentRunner`.

The right unification point is not the transport. It is the resolved endpoint:

```rust
struct ResolvedAgentRoute {
    role_or_stage: RouteTarget,
    runner: AgentRunnerKind,
    provider: String,
    model: String,
    capabilities: CapabilitySet,
    source: RouteSource,
}
```

Every backend receives the same resolved route, but each runner executes it in
the way that matches its runtime.

## 7. Implementation Direction

The next implementation should add a `ModelResolver` or `RouteResolver` layer
before agent registry construction.

It should:

- load route profiles from TOML/env/defaults
- resolve review roles and extraction stages separately
- apply `GROKRXIV_<ROLE>_MODEL` and `--model-for` as model-only overrides
- validate required capabilities before a run starts
- expose the resolved route map in `grokrxiv config --json`
- record resolved runner/provider/model in review and extraction provenance

The current per-agent model env work is still useful. It becomes the override
surface consumed by the resolver instead of being the resolver itself.

## 8. Target Operator Experience

Normal subscription-backed run:

```bash
grokrxiv --profile cli_subscription ingest 2605.00561 --status --no-cache
```

Direct provider API run:

```bash
grokrxiv --profile api ingest 2605.00561 --status --no-cache
```

Local model run:

```bash
grokrxiv --profile local ingest 2605.00561 --status --no-cache
```

One-off model override:

```bash
GROKRXIV_CITATION_MODEL=gemini-3-flash-preview \
grokrxiv --profile cli_subscription review-extracted 2605.00561
```

The CLI should print or expose the route map so the operator can see exactly
what will run before a long paper starts:

```text
summary: runner=cli provider=claude model=claude-haiku-4-5
technical_correctness: runner=cli provider=claude model=claude-opus-4-7
novelty: runner=cli provider=gemini model=gemini-3-flash-preview
reproducibility: runner=cli provider=openai model=gpt-5.5
citation: runner=cli provider=gemini model=gemini-3-flash-preview
meta_reviewer: runner=cli provider=claude model=claude-sonnet-4-6
```

## 9. Decision

Use one abstraction for route resolution and separate abstractions for
execution.

Do:

- keep `ReviewAgent` and extraction stages focused on task contracts
- add a route resolver that chooses runner/provider/model/capabilities
- keep `AgentRunner` as the execution abstraction
- keep `LLMProvider` for API/local HTTP providers
- use per-agent env vars as overrides

Do not:

- add `GROKRXIV_GEMINI_MODEL`
- make model ids imply runner choice
- force CLI subprocesses into `LLMProvider`
- let extraction and review share routing without checking capabilities
- silently fall back to API billing when the CLI profile was selected
