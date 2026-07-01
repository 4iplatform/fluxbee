# AI engine selection — evaluation + design (NOT yet implemented)

Status: **design note only.** Captures the current state and the agreed target so
the change can be made later without re-discovering the codebase. No code changed.

## Goal (operator's framing)

> No quiero tener configuraciones por SY. Todos los `SY.*` correrían con **un motor
> elegido por `hive.yaml`** (default del hive), y el **nodo AI que corra elige su
> motor cuando se instancia**. Las dos opciones: **OpenAI** y **Anthropic**.

So a two-tier model:

- **Tier 1 — hive default (in `hive.yaml`):** one provider+model for the whole hive.
  Every `SY.*` node that uses AI (today: `SY.cognition`'s semantic-tagger and
  narrative-summarizer) inherits it. No per-SY config.
- **Tier 2 — AI-node instance choice:** a spun-up AI node (`ai-generic` /
  `ai-frontdesk-gov`) can pick its own provider+model in its node config, overriding
  the hive default for that node.

## Current state (as of 2026-06-30)

### Provider abstraction — trait exists, only OpenAI implemented
- `crates/fluxbee_ai_sdk/src/llm.rs`: `pub trait LlmClient` (≈148–159) is
  provider-agnostic (`generate` / `generate_stream`). The **only** production impl is
  `OpenAiResponsesClient` (≈232–340), `base_url` hardcoded to
  `https://api.openai.com/v1/responses` (≈243, overridable via `with_base_url`).
  Function-calling path: `OpenAiFunctionCallingModel` (≈560–747), OpenAI Responses-API
  specific. **No Anthropic client, no `Provider` enum, no factory.**

### Keys — already reproducible, via SY.vault (Model D')
- Keys are resolved from **SY.vault**, not env/files: `ResourceType::Openai`
  (and **`ResourceType::Anthropic` already exists** in `crates/fluxbee_sdk/src/vault.rs`
  ≈40–160, but unused).
- `SY.cognition` `resolve_cognition_openai_api_key()` (`src/bin/sy_cognition.rs`
  ≈1591–1634) → `vault.resolve_resource(ResourceType::Openai, root_tenant, 5s)`.
- AI nodes `resolve_openai_api_key_with_source()`
  (`nodes/ai/ai-generic/src/bin/ai_node_runner.rs` ≈1589–1632) → same, tenant-scoped.
  **Caveat (the "reproducible" gap):** resolution needs `FLUXBEE_NODE_TENANT_ID`
  (orchestrator-injected); a node started by hand without it falls back to `Missing`.

### Model/provider selection — scattered, "openai"-only
- Hardcoded default model `"gpt-4.1-mini"`:
  `nodes/ai/ai-generic/.../ai_node_runner.rs` `default_model()` (≈369–370);
  `SY.cognition` constants `COGNITION_DEFAULT_SEMANTIC_TAGGER_{PROVIDER,MODEL}`
  (`src/bin/sy_cognition.rs` ≈64–65, = `"openai"` / `"gpt-4.1-mini"`).
- Per-AI-node config: `OpenAiChatSection.model` (`ai_node_runner.rs` ≈149–161).
- **Hard "openai"-only gates** that reject any other provider string:
  `src/bin/sy_cognition/semantic_tagger_ai.rs:37` and
  `narrative_summarizer_ai.rs:47` (`!eq_ignore_ascii_case("openai")` → error).

### hive.yaml — no AI config at all
- `config/hive.yaml`, `packaging/hive.yaml.example`, and the `HiveFile` struct
  (`src/config/mod.rs` ≈76–127) have **no** `ai`/`engine`/`model`/`provider` section.
  So there is no single source of truth for the hive's engine today.

## Target design

### 1. `hive.yaml` — one hive-level default (Tier 1)
```yaml
# Default AI engine for the whole hive. All SY.* AI usage inherits this unless a
# specific AI node overrides it at instantiation. The matching key lives in the
# vault (ResourceType::Openai / ::Anthropic); flipping provider switches both.
ai:
  provider: openai          # openai | anthropic
  model: gpt-4.1-mini       # provider-appropriate model
```
- Parse into `HiveFile` (`src/config/mod.rs`): `ai: Option<AiSection { provider, model }>`.
- A helper `hive_ai_default(&hive) -> (provider, model)` (mirrors how
  `identity_sync_auth_required` reads the hive) with a built-in fallback
  (`openai` / `gpt-4.1-mini`) so an omitted section keeps today's behavior.

### 2. SDK — make the trait actually polymorphic (both providers)
- Add `pub enum LlmProvider { OpenAi, Anthropic }` (+ `FromStr` for the config string).
- Add `AnthropicClient` implementing `LlmClient` (Anthropic Messages API:
  `https://api.anthropic.com/v1/messages`, `x-api-key` + `anthropic-version` headers,
  the `messages`/`content-blocks` request shape, and a tool-use mapping for the
  function-calling path → `AnthropicFunctionCallingModel`).
- A factory `create_client(provider, api_key, base_url_override) -> Arc<dyn LlmClient>`
  and `create_function_model(provider, ...)`. Everything downstream already speaks the
  `LlmClient` / function-model traits, so call sites become provider-agnostic.

### 3. Key loading — pick the resolver by provider (stays reproducible)
- Add `resolve_anthropic_api_key()` mirroring the OpenAI resolver
  (`vault.resolve_resource(ResourceType::Anthropic, tenant, 5s)`).
- The node resolves the key for **the selected provider** (hive default or node
  override). Still vault-only / deterministic. (Optionally also fix the
  `FLUXBEE_NODE_TENANT_ID`-missing fallback so a hand-started node degrades clearly.)

### 4. SY.* nodes — inherit the hive default (Tier 1), drop per-SY config
- `SY.cognition`: replace the `COGNITION_DEFAULT_*` constants + the `"openai"`-only
  gates (`semantic_tagger_ai.rs:37`, `narrative_summarizer_ai.rs:47`) with: read
  `(provider, model)` from the hive default, build the client via the factory,
  dispatch on `LlmProvider`. No per-cognition engine config.

### 5. AI node — choose at instantiation (Tier 2)
- Extend the AI-node config so a spun-up node can set `provider` + `model` (e.g. an
  `anthropic_chat` behavior kind alongside `openai_chat`, or a `provider` field on the
  existing chat section). When unset, it falls back to the hive default.

## Decisions still open (for when we build it)
- **Per-provider default model in hive.yaml?** i.e. keep a `model` per provider so
  flipping `provider` alone picks a sane model — vs. a single `model` the operator must
  change in tandem. (Leaning: single `provider`+`model` pair for simplicity, per "no
  complicar"; revisit if switching becomes frequent.)
- **Where the hive default reaches the nodes:** push it into each AI/cognition node's
  config at orchestrator bootstrap (like other hive-derived config), vs. each node
  reading `hive.yaml` directly. (Leaning: bootstrap push, consistent with how SY nodes
  already get hive-derived settings.)
- **Anthropic model pinning:** which default model id (e.g. a current Claude Sonnet).

## Why this is documented, not coded
The operator wants the *option* (both engines, hive-default + per-AI-node choice),
not a switch flipped now. This note is the actionable plan; implementing it is a
contained follow-up: SDK (`AnthropicClient` + factory) → vault resolver → `hive.yaml`
`ai:` section + `HiveFile` → swap the `SY.cognition` constants/gates for the factory →
AI-node override. See also the audit/lab state in
`.../memory/fluxbee-audit-lab-state.md`.

## Code-review findings to fold into implementation

Added 2026-06-30 after reviewing this design against the current code. These do not
invalidate the two-tier target, but they should be handled before treating this note
as implementation-ready.

### 1. `SY.architect` is missing from the Tier 1 scope

The current note says the only `SY.*` AI consumer is `SY.cognition`, but
`SY.architect` already runs AI directly:

- It has its own `HiveFile` and `ArchitectNodeConfigFile` with
  `ai_providers.openai` (`src/bin/sy_architect.rs:200-241`).
- `build_architect_ai_runtime()` resolves the OpenAI key from Vault, picks a model
  from merged config/hive settings, and constructs `OpenAiResponsesClient`
  (`src/bin/sy_architect.rs:6121-6164`).
- Multiple architect flows build OpenAI function-calling models directly from that
  runtime (`src/bin/sy_architect.rs:2044-2048`, `src/bin/sy_architect.rs:10202-10206`).

Implementation implication: Tier 1 must explicitly include `SY.architect`, otherwise
the hive-level provider switch would leave a core `SY.*` node pinned to OpenAI.

### 2. Existing per-SY AI config needs a migration/deprecation plan

The target says "no per-SY config", but current code exposes per-SY AI settings:

- `SY.cognition` accepts `config.semantic_tagger.provider/model` and rejects anything
  except `openai` today (`src/bin/sy_cognition.rs:2728-2750`).
- `SY.architect` exposes `config.ai_providers.openai.default_model/max_tokens/
  temperature/top_p` in its local config contract (`src/bin/sy_architect.rs:11345-11368`).

Implementation implication: define whether these fields are removed, ignored, migrated
to the hive default, or kept as temporary compatibility aliases. If they stay, they
conflict with the operator goal of a single hive-level `SY.*` default.

### 3. `hive.yaml` has no canonical AI config, but there is legacy local parsing

The global `config/hive.yaml`, `packaging/hive.yaml.example`, and the shared
`src/config/mod.rs::HiveFile` still have no canonical `ai:` section. However,
`SY.architect` has a separate local `HiveFile` that already reads
`ai_providers.openai` from hive config and merges it with node config
(`src/bin/sy_architect.rs:200-229`, `src/bin/sy_architect.rs:6167-6189`).

Implementation implication: the new `ai: { provider, model }` section should either
replace or explicitly supersede the older `ai_providers.openai` shape. Update any
architect docs/contracts that still mention the legacy shape so operators do not have
two competing hive-level AI config formats.

### 4. Tier 2 should distinguish dynamic `AI.*` nodes from `SY.frontdesk.gov`

The note groups `ai-generic` and `ai-frontdesk-gov` as "AI-node instance choice".
The current code treats them differently:

- `ai-generic` is the dynamic AI runner; its effective config contains an optional
  `behavior.provider`, but the runtime dispatch still keys only on
  `behavior.kind == "openai_chat"` (`nodes/ai/ai-generic/src/bin/ai_node_runner.rs:248-266`,
  `nodes/ai/ai-generic/src/bin/ai_node_runner.rs:4963-5008`).
- `ai-frontdesk-gov` documents itself as `SY.frontdesk.gov`, a system node listed in
  `hive.yaml`, not a dynamic spawn using `FLUXBEE_NODE_ILK_ID`
  (`nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs:456-465`).
- `ai-frontdesk-gov` resolves OpenAI from the root/system tenant, not a dynamic
  per-node tenant (`nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs:1613-1630`).

Implementation implication: Tier 2 should apply to dynamic `AI.*` instances. If
`SY.frontdesk.gov` remains a system node, it should inherit Tier 1 unless/until there
is an explicit decision to make it a configurable AI runtime instance.

### 5. Anthropic support is not just `LlmClient`; multimodal/input parts are OpenAI-specific

The SDK has a provider-agnostic `LlmClient` trait and `FunctionCallingModel` trait,
but current input shaping and call sites still use OpenAI wire shapes:

- `build_openai_user_content_parts()` emits `input_text`, `input_image`, and
  `input_file` parts for OpenAI Responses (`crates/fluxbee_ai_sdk/src/text_payload.rs:93-172`).
- `SY.architect`, `ai-generic`, and `ai-frontdesk-gov` call that helper before sending
  multimodal turns (`src/bin/sy_architect.rs:10236-10240`,
  `nodes/ai/ai-generic/src/bin/ai_node_runner.rs:1138-1159`,
  `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs:948-966`).
- `OpenAiFunctionCallingModel` maps tool calls to OpenAI Responses-specific fields
  (`crates/fluxbee_ai_sdk/src/llm.rs:560-690`).

Implementation implication: add provider-neutral content/input abstractions, or a
provider-specific builder behind the same factory. Otherwise Anthropic text-only may
work while attachments/tool paths still fail or silently use OpenAI payload shapes.

### 6. Status, config contracts, and Vault refresh must become provider-aware

`ResourceType::Anthropic` already exists in the vault SDK
(`crates/fluxbee_sdk/src/vault.rs:89-105`, `crates/fluxbee_sdk/src/vault.rs:140-155`),
but current consumers and contracts report/listen for OpenAI-specific resources:

- `SY.cognition` listens for `VAULT_SECRET_CHANGED` with `resource_type=openai` and
  reports OpenAI-specific status/contract fields.
- `SY.architect` reports `resource_type=openai` in local config get and refreshes
  OpenAI-specific runtime state.
- AI node contracts still document allowed behavior as `echo` / `openai_chat` and
  required resource `openai` (`docs/node-config-control-plane-spec.md:171-245`).

Implementation implication: the selected effective provider must drive:

- Vault resource type (`openai` vs `anthropic`).
- `CONFIG_GET` resource lists and health/readiness.
- `VAULT_SECRET_CHANGED` interests.
- Runtime error parsing and user-facing missing-key/provider-error payloads.
