# AI Nodes Tasks (ordered)

Status: active  
Owner: AI nodes initiative  
Last update: 2026-03-03

Design note (mandatory):
- Target model is a single generic AI node runner that can spawn N logical `AI.*` nodes by config/command.
- "One binary per AI node" is allowed only as temporary bootstrap for smoke testing.

## T0 - Baseline and boundaries

- [x] T0.1 Confirm project constraints from docs and isolate deprecated cognition assumptions.
- [x] T0.2 Define repo location and layer split for AI node work.
- [x] T0.3 Start a living AI nodes spec document.

## T1 - MVP runtime (must-have)

- [x] T1.1 Create `crates/fluxbee_ai_sdk`.
- [x] T1.2 Add `AiNode` trait and minimal `NodeRuntime` loop.
- [x] T1.3 Add `RouterClient` wrapper over `fluxbee_sdk`.
- [x] T1.4 Add `text/v1` helper functions for extraction/response building.
- [x] T1.5 Add minimal `Agent` abstraction.
- [x] T1.6 Add provider-agnostic `LlmClient` and first OpenAI client.
- [x] T1.7 Add smoke node (`ai_echo`) as temporary bootstrap.

## T2 - Hardening

- [x] T2.1 Add runtime timeouts and bounded internal queues.
- [x] T2.2 Add worker-pool mode for concurrent request handling.
- [x] T2.3 Define recoverable vs fatal errors and retry policy.
- [x] T2.4 Add structured logs and basic metrics.
- [x] T2.5 Add contract tests for message in/out and text payload helpers.

## T3 - Agent behavior (incremental)

- [x] T3.0 Build generic `ai_node_runner` (single binary, node behavior by config/args).
- [x] T3.0.1 Define node config schema (`name`, `model`, `instructions`, timeouts/retries, optional capabilities).
- [x] T3.0.2 Add startup command contract to run multiple instances with different configs.
- [x] T3.0.3 Move `ai_echo` behavior to generic runner profile and deprecate dedicated per-node bins.
- [x] T3.1 Add configurable prompt/instructions loading strategy.
- [x] T3.2 Add model settings object (temperature, max tokens, etc).
- [x] T3.3 Add mock llm provider for deterministic tests.
- [x] T3.4 Add optional streaming output pathway.

## T4 - Tool and handoff primitives

- [ ] T4.1 Define tool-call message schema for Fluxbee.
- [ ] T4.2 Implement fire-and-forget tool-call path.
- [ ] T4.3 Implement `trace_id` correlation skeleton for async tool results.
- [ ] T4.4 Define and implement minimal handoff envelope.

## T5 - Cognition adapters (optional and decoupled)

- [ ] T5.1 Define optional metadata adapters for context/memory/episodes injection.
- [ ] T5.2 Ensure adapters are isolated from core runtime.
- [ ] T5.3 Add compatibility tests for schema drift in cognition metadata.
