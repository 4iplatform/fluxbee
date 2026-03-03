# Fluxbee AI Nodes - Living Spec

Status: draft-v0  
Last update: 2026-03-03

## 1) Scope and constraints

This document defines the implementation path for `AI.*` nodes in Fluxbee.

Current assumptions:
- Thread (`thread_id`) is the concrete conversation unit and comes from IO.
- Cognition concepts are in transition; do not hard-couple AI node runtime to any current cognition internals.
- AI node core must work without context/memory/episodes enrichment.
- Router protocol and routing semantics remain unchanged.

Out of scope for MVP:
- tool calling with waiting state
- handoffs with state carry-over
- cognition-specific storage contracts

## 2) Repo placement

AI node development is split in two layers:

1. `crates/fluxbee_ai_sdk/`
- Rust SDK for `AI.*` nodes.
- Reuses `fluxbee_sdk` socket/protocol primitives.
- Contains: node trait, runtime loop, message helpers, text payload helpers, LLM client abstraction, minimal Agent abstraction.

2. `crates/fluxbee_ai_nodes/src/bin/*.rs` (bootstrap) -> `ai_node_runner` (target)
- Runtime executables layer (currently `crates/fluxbee_ai_nodes/src/bin/`).
- Target state is one generic runner binary (`ai_node_runner`) instantiated with different configs.

Implemented now:
- `ai_node_runner` exists and loads node behavior from YAML config.
- dedicated `ai_echo` binary removed; echo now runs via `behavior.kind: echo` profile.
- `ai_local_probe` exists for direct OpenAI smoke tests without router/IO.
- Startup command contract supports one process with one or many configs:
  - `ai_node_runner --config node-a.yaml`
  - `ai_node_runner --config node-a.yaml --config node-b.yaml`
  - each config => one logical `AI.*` node instance
  - duplicate `node.name` values are rejected at startup
- Bootstrap sample config: `docs/onworking/ai_node_runner_config.example.yaml`.

Rationale:
- Keeps router core and AI runtime concerns separate.
- Allows external repos to consume the same AI SDK crate.
- Lets us iterate on AI features without touching router internals.
- Aligns with Fluxbee operational model: many `AI.*` logical nodes, one runner implementation.

## 3) Minimal architecture (Stage 1)

### 3.1 Runtime contract

Loop:
1. read message from router
2. call `AiNode::on_message`
3. if response exists, write response to router

Rules:
- async-only
- stateless by default
- no hard dependency on cognition storage
- no routing mutation beyond normal reply behavior

Runtime configuration (implemented):
- `read_timeout`
- `handler_timeout`
- `write_timeout`
- `queue_capacity` (bounded internal queue)
- `worker_pool_size` (bounded concurrent processing)
- `retry_policy.max_attempts`
- `retry_policy.initial_backoff`
- `retry_policy.max_backoff`
- `metrics_log_interval`

Error policy (implemented):
- Recoverable errors:
  - socket disconnections/timeouts
  - transient I/O errors
  - transient HTTP/connectivity errors
- Fatal errors:
  - protocol contract errors
  - payload/schema decode errors
  - invalid local configuration/identity errors

Runtime behavior:
- Recoverable errors are retried with exponential backoff up to `max_attempts`.
- If retries are exhausted, message processing is dropped and runtime continues.
- Fatal errors fail fast and stop runtime.

Observability (implemented):
- structured lifecycle logs (`runtime started`, `runtime stopped`)
- structured error/retry logs by stage (`handler`, `write`)
- periodic metrics logs with counters:
  - `read_messages`
  - `enqueued_messages`
  - `processed_messages`
  - `responses_sent`
  - `recoverable_exhausted`
  - `fatal_errors`
  - `retry_attempts`

Contract tests (implemented):
- message JSON roundtrip (`Message`, `Routing`, `Meta`)
- reply routing/message builders
- `text/v1` response builder and extractor behavior

Platform note:
- If you want to remove Windows compatibility stubs and run Linux-only end-to-end (including tests),
  follow `docs/onworking/ai_sdk_linux_only_migration.md`.

### 3.2 Core interfaces

`AiNode`:
- async message handler with optional response

`RouterClient`:
- connect/read/write wrapper over `fluxbee_sdk`

`LlmClient`:
- provider-agnostic generation interface
- initial implementation: OpenAI Responses API
- deterministic mock implementation available for tests (`MockLlmClient`)
- optional streaming pathway via `generate_stream(...)`

`Agent`:
- minimal unit with instructions + model + llm client
- one-shot `run_text` entry point
- optional `run_text_stream` entry point

`ai_node_runner` config schema (v0):
- `node`:
  - `name`, `version`, `router_socket`, `uuid_persistence_dir`, `config_dir`
- `runtime`:
  - `read_timeout_ms`, `handler_timeout_ms`, `write_timeout_ms`
  - `queue_capacity`, `worker_pool_size`
  - `retry_max_attempts`, `retry_initial_backoff_ms`, `retry_max_backoff_ms`
  - `metrics_log_interval_ms`
- `behavior`:
  - `kind: echo`
  - `kind: openai_chat` with `model`, `instructions`, `api_key_env`, optional `base_url`
  - optional `model_settings`:
    - `temperature`
    - `top_p`
    - `max_output_tokens`
  - `instructions` loading strategies:
    - short form: `instructions: "inline text"`
    - structured form:
      - `source: inline` + `value: "..."`
      - `source: file` + `value: "/path/to/instructions.txt"`
      - `source: env` + `value: "ENV_VAR_NAME"`
      - `source: none`
      - optional `trim: true|false` (default `true`)

### 3.3 Payload contract

MVP standard payload:
- `text/v1` compatible payload (`type=text`, content, attachments)
- helper functions:
  - `extract_text(payload)`
  - `build_text_response(content)`

## 4) Alignment with openai-agents-python

We take only the minimal and stable ideas from `openai/openai-agents-python`:
- `Agent` as explicit object
- runtime loop separated from agent declaration
- model provider abstraction (`LlmClient`)
- incremental feature expansion (tools, handoffs later)

We intentionally skip for now:
- guardrails framework
- run state persistence
- multi-turn tool orchestration
- streaming event framework

## 5) Incremental roadmap

Phase A (now):
- crate skeleton (`fluxbee_ai_sdk`)
- `AiNode`, `RouterClient`, `NodeRuntime`
- `LlmClient` + OpenAI client
- bootstrap completed with generic runner + echo/openai_chat profiles

Phase B:
- timeouts + worker pool in runtime
- structured logging and metrics hooks
- stricter message validation and error taxonomy

Phase C:
- generic `ai_node_runner` with per-instance config
- startup command contract for multiple node instances
- deprecate dedicated per-node bins

Phase D:
- tool-calling fire-and-forget
- correlation by `trace_id`

Phase E:
- handoffs + advanced orchestration

## 6) Cognition compatibility rule

AI node runtime must not assume a fixed cognition schema.

If cognition provides context/memory/episodes metadata, AI nodes consume it as optional message metadata.
If cognition changes shape, only adapter/parsing layers should change, not the core runtime loop.
