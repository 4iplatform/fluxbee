# io-common - Technical specification
> Adaptation note for `json-router`:
> In this repo, identity flow follows Router-driven onboarding (v1.16+):
> IO performs `lookup -> provision_on_miss -> forward`.
> On provision failure/timeout, it forwards with `meta.src_ilk = null` (degraded fallback).
> Any older wording suggesting synchronous wait/buffering for identity should be treated as superseded.

> Operational note:
> If lookup logs show `EACCES` (permission denied) when reading identity SHM, this is an environment permissions issue.
> See `docs/io/README.md` ("Known Issue / Troubleshooting") for diagnosis and operational fix.



## Unified Control Plane (IO Nodes ↔ AI Nodes)

✅ **NORMATIVE**:
- IO Nodes adopt the same Control Plane pattern as AI Nodes:
  - `PING/STATUS` for minimal observability,
  - `CONFIG_GET/CONFIG_SET/CONFIG_RESPONSE` for hot configuration management.
- Control Plane messages are identified by `meta.type in {"system","admin"}` + `meta.msg`.

🧩 **TO BE SPECIFIED**:
- Global catalog of system/admin commands (when Fluxbee formalizes it in the protocol/base).



## Temporary adaptation to the core/SDK (L3 fields and identity)

> ✅ **Canonical (Fluxbee protocol):** there are dedicated carriers for identity and conversational continuity. In the current repo, `src_ilk` and `thread_id` are part of the active canonical path, while `ich` must be read as the IO node's **local/systemic channel**.  
> ⚠️ **Current state (core/SDK):** the SDK does not expose all the dedicated L3 fields, but `src_ilk` is carried canonically in `meta.src_ilk`. `meta.context` is left for additional metadata.

✅ **NORMATIVE (TODAY, compatible implementation):**
- IO Nodes **MUST** use `meta.src_ilk` as the canonical carrier of the source identity.
- IO Nodes **MUST** treat `meta.ich` as the identifier of the local channel/asset operated by the IO instance, when that field applies to the path.
- Channel-specific metadata (Slack/WhatsApp/Email) must live under `meta.context.io.*`.
- While the transition lasts, untyped L3 fields (for example `dst_ilk`) may be carried in `meta.context`.

⚠️ **WARNING:**
- Using `meta.context` as a carrier is temporary, for untyped fields. `meta.src_ilk` is already canonical and must not be duplicated in `meta.context`.
- `meta.ich` must not be used to represent the external interlocutor or its remote handle.

🧩 **Current state of the core:**
- `ctx_seq/ctx_window` are not part of the repo's canonical happy path.
- if they show up in old payloads, they must be treated as legacy/opaque metadata.



## 0. Goal

`io-common` is a library (or a set of libraries, one per language) that encapsulates the
**critical and repeatable** logic of the IO Nodes.

Its goal is to:
- avoid duplicating logic across IOs,
- reduce errors in deduplication, retries and idempotency,
- standardize operation, observability and lifecycle,
- allow evolving from a single-instance MVP to a scalable architecture.

`io-common` is **not** channel-aware (Slack, WhatsApp, email, etc.).
The specific details stay in each IO Node.

---

## 1. Design principles

- **Channel-agnostic**: it does not know external APIs.
- **No local persistence**: `io-common` does **NOT** assume DB/Redis inside the IOs. All technical state is **in memory**.
- **Best-effort per process**: deduplication, sessionization and retries work while the process is alive; after a restart they may be lost.
- **Protocol-compatible**: it assumes no fields outside `routing/meta/payload`.
- **Bounded and observable**: every buffer/cache must have a TTL, limits and metrics to avoid unbounded growth.
- **Evolvable**: the design leaves hooks to add persistence (DB/Redis) outside the IO if the system enables it in the future.

---

## 2. Responsibilities of io-common

### 2.1 Inbound reliability
- **In-memory** inbound deduplication with a TTL and limits (cap by entries/bytes).
- Helpers for the "fast ACK" pattern:
  - the ACK to the provider does **not** wait for the router's response,
  - downstream processing happens asynchronously.
- A **bounded** queue/buffer of "pending" messages (e.g. waiting for identity) with timeouts.

### 2.1.1 `text/v1` normalization + offload to blob (IO canonical)
- The enforcement point of the `text/v1` offload (inline -> `content_ref`) is in `fluxbee_sdk::NodeSender::send`.
- `io-common` **MUST** delegate that decision to the SDK and **NOT** duplicate size-based offload logic.
- IO adapters must **NOT** reimplement the `content` vs `content_ref` decision; they must only map external event -> base payload + attachments.
- The common `text/v1` validations (shape, limits, count, blob resolution) live in `io-common`.
- The effective policy of accepted MIME types must be defined by each IO adapter according to its capabilities and the external channel, and `io-common` must apply it uniformly.
- An `io-common` default can serve as a technical baseline, but it must not replace an explicit adapter decision when the channel supports more or fewer types.

Operational rule for MIME:
- if Fluxbee can carry an attachment and the channel/adapter can reasonably accept it, the adapter should include that MIME type in its effective policy
- if a MIME type is left out, it must be because of a concrete limitation of the channel/adaptation or an explicit documented decision, not an accidental implicit default

Short adoption rule for future `IO.*`:
- if the adapter produces textual user messages toward the router, it must use the canonical `text/v1` contract
- those messages must be sent to the router through `fluxbee_sdk::NodeSender::send`
- the adapter must not reimplement the `content` vs `content_ref` decision on its own
- if the adapter needs a payload type other than `text/v1`, it does not inherit the auto-offload automatically; its contract/size must be defined explicitly before assuming equivalent behavior

Current operational configuration of the enforcement in the SDK:
- The normalization in `NodeSender::send` uses the SDK's internal defaults.
- At this stage it does not depend on environment variables to decide the offload.

Operational logging (low noise):
- `debug`: normalization decision (`offload_to_blob`, estimated bytes, limit, attachments).
- `info` only when there was an offload to blob.
- `warn` on a normalization error, with the canonical code.
- Never log user content or blob bytes.

Recommended validation:
- Deterministic test of large payloads (`>64KB` default) with `io-sim`.
- In `io-slack`, use adapter and contract integration tests, taking the channel's own limits into account.

### 2.2 Outbound reliability
- **In-memory** outbox (best-effort) with:
  - a bounded queue,
  - retries with backoff,
  - timeouts and minimal states (`PENDING/SENDING/SENT/FAILED/DEAD`) for internal operation only.
- Outbound idempotency **per process** through internal in-memory keys (if required).
- An in-memory worker/dispatcher to process the outbox.
- **Note**: since there is no persistence, the outbox may be lost after a restart, and with it any outbound messages not sent yet.

### 2.2.2 Output policy toward the external channel (large content)

✅ **NORMATIVE (IO baseline):**
- IO adapters **MUST** respect the limits of the destination external channel.
- IO adapters **MUST NOT** truncate content silently.
- When it hits a channel limit, the adapter **MUST** use an explicit alternative delivery strategy (for example file/attachment/chunking) and leave a trace in logs/metrics.

🧩 **TBD per channel:**
- Each channel defines a concrete fallback strategy for long payloads:
  - direct `text`,
  - file/attachment,
  - chunking into multiple messages,
  - hybrid (summary + attachment).

✅ **NORMATIVE UX (multilingual):**
- Do not use notices hardcoded in a fixed language.
- If a notice message is sent because of a fallback, it must be configurable/localizable per node.
- Recommended default when there is no localization: deliver the full content with no fixed notice text.

### 2.2.1 Identity Resolution Client (SY.identity)

`io-common` **MUST** provide identity as a shared pipeline for all IOs:

1. `lookup(channel, external_id)` on the identity SHM (`jsr-identity-<island>`).
2. If the lookup gives a miss/error and `provision_on_miss=true`, call `ILK_PROVISION` against `SY.identity`.
3. If the provision returns an ILK, forward with `meta.src_ilk=<ilk>`.
4. If the provision fails or times out, forward with `meta.src_ilk=null` (degraded fallback).

Operational configuration:
- `ISLAND_ID`
- `IDENTITY_TARGET` (default `SY.identity@<ISLAND_ID>`)
- `IDENTITY_TIMEOUT_MS`

Required properties:
- it does not block the ACK to the external provider
- it does not keep unbounded sessions waiting for identity
- logs/counters for hit/miss/error/provision/fallback

### 2.3 Common inbound relay / sessionization
- The inbound relay lives in `io-common`; there must be no private sessionizer per adapter.
- Its job is to consolidate several inbound fragments of the same physical turn into a single `text/v1` before calling the common inbound pipeline.
- Identity is still resolved at the relay flush, not during accumulation.
- The adapter defines the relay key and the channel hints; `io-common` only manages the buffer, limits, deadlines, flush and assembly.
- `relay_window_ms = 0` means immediate passthrough.
- `relay_window_ms > 0` means consolidation within a short window.
- The recommended formal configuration for IO adapters lives under `config.io.relay.*`.
- The first active implementation uses the `memory` store; durable is left as a later evolution, without changing the adapter contract.
- The relay leaves a trace in `meta.context.io.relay.*`.

### 2.3.1 Response contracts hop-by-hop

`io-common` also exposes common helpers for structured response contracts between hops.

Current carriers:

- `meta.context.response_envelope`
- `meta.context.final_response_contract`

Rules:

- `response_envelope` defines the expected shape of the current hop's response;
- `final_response_contract` keeps, when needed, the expected shape of the final response of a composite operation;
- `response_envelope` is not propagated automatically to intermediate hops;
- `final_response_contract` must not be imposed on the next hop as a format obligation;
- copying the whole `meta.context` without filtering those fields is considered incorrect.

Shape v1:

- `kind = "json_object_v1"`
- flat top-level object
- allowed types per property:
  - `string`
  - `boolean`
  - `integer`
  - `number`
- `enum` only for `string`
- `required` defines the mandatory fields
- a field present in `properties` but absent from `required` is optional

Important semantics rule:

- in v1, optional means "may be absent";
- optional does not mean "may come as `null`";
- if a field exists in the response, it must match the type declared in the contract.

Common responsibilities of `io-common` here:

- set/get of `response_envelope` and `final_response_contract`;
- minimal validation of the contract;
- parsing and validation of canonical structured responses over a `text` payload;
- canonical errors for:
  - invalid contract
  - invalid `meta.context`
  - invalid structured response

### 2.4 Retry and errors
- Error classification (retryable / non-retryable / rate_limited).
- Optional circuit breaker.
- Policies configurable per IO.

### 2.5 Lifecycle
- States: STARTING / READY / DRAINING / STOPPED.
- SIGTERM handling.
- Outbox flush on shutdown.

### 2.6 Observability
- Common structured logs.
- Standard metrics.
- `trace_id` propagation.
- For the relay, the initial logging policy is event-driven:
  - no periodic logs,
  - `debug` for open/buffer/flush/dedup,
  - `info` for initialization or drain milestones,
  - `warn` for capacity, expiry, drops and fallbacks.

---

## 3. Layer 3: Identity (IdentityResolver)

To keep IO adapters consistent, `io-common` exposes an
inbound-centered identity contract:
- resolve `src_ilk` by `(channel, external_id)` when possible,
- provision on a miss if enabled,
- degrade to `src_ilk=null` when it cannot be resolved.

### 3.1 Resolver interface (Contract)
The operational contract at runtime is made of:

- `IdentityResolver::lookup(channel, external_id) -> Option<src_ilk>`
- `IdentityProvisioner::provision(input) -> Option<src_ilk>`
- `InboundProcessor` as the orchestrator of the flow (lookup/provision/fallback)

### 3.2 Inbound flow (Channel -> Router)
Every time the IO Node receives a message or event from outside:
1. **Extraction:** The IO Node extracts the provider ID (e.g. `U12345`).
2. **Lookup:** `lookup(channel, external_id)`.
3. **Provision on miss (optional, by config):** on a miss/error, try `ILK_PROVISION`.
4. **Forward to the Router:** with `meta.src_ilk=<ilk>` on success, or `null` on fallback.

Important:
- the `external_id` used for the external identity is a different datum from the local `ICH`;
- the local `ICH` identifies the asset/channel operated by the IO instance;
- the remote handle of the external interlocutor must not be reinterpreted as an `ICH`.

### 3.3 Outbound
The outbound destination is resolved through `meta.context.io.reply_target` (IO Context contract).
It does not depend on an additional identity resolver in `io-common`.

For the case implemented today, `IO.api`:

- `reply_target.kind = "webhook_post"`
- `reply_target.address = integration_id`
- minimal `reply_target.params`:
  - `integration_id`
  - `tenant_id`
  - `request_id`
  - `trace_id`
  - `reply_mode = "final_only"`

Rules:

- `reply_target` must not carry `webhook.url`;
- `reply_target` must not carry secrets or signed headers;
- the real outbound sending node resolves that data at delivery time.

---

## Out of scope for io-common

- Validation of provider signatures.
- OAuth authentication.
- Extraction of channel-specific IDs.
- Routing or OPA decisions.
- Secret management.

---

## 4. Internal architecture

### 4.1 Main interfaces

**InboundAdapter (per channel)**
- parse_event(raw) -> InboundEvent
- dedup_key(event) -> DedupKey
- relay_key(event) -> RelayKey
- to_relay_fragment(event) -> RelayFragment

**OutboundAdapter (per channel)**
- build_outbound(message) -> OutboxItem
- send(outbox_item) -> SendResult

**ErrorClassifier**
- classify(error) -> retryable | non_retryable | rate_limited | auth_error

**Storage**
- dedup_store
- outbox_store
- relay_store

### 4.2 Common relay (`io-common`)

Current minimal contract:

- `RelayPolicy`
- `RelayStore`
- `InMemoryRelayStore`
- `RelayFragment`
- `RelaySession`
- `AssembledTurn`
- `RelayDecision`
- `FlushReason`
- `RelayBuffer`

Normative flow:

1. the adapter parses the external event;
2. it builds a `RelayFragment` with `relay_key`, `fragment_id`, `io_context` and `identity_input`;
3. `RelayBuffer` decides `Hold`, `FlushNow`, `DropDuplicate`, `RejectCapacity` or `DropExpired`;
4. on `FlushNow`, `InboundProcessor` resolves the identity and builds the final message to the router.

---

---

## 4.X IO metadata contract (meta.context.io (TENTATIVE))

`io-common` **MUST** provide utilities to build and validate the standardized block
`meta.context.io (TENTATIVE)` (see also the IO Nodes spec). The goal is that all IOs produce/consume
homogeneous metadata, minimizing per-channel exceptions.

Key elements:
- `entrypoint`: the business identity/account (e.g. number A vs B).
- `reply_target`: minimal parameters to reply (key contract).
- `message.id`: external ID for best-effort dedup.
- `conversation`: external identifier (and thread, if it applies).

`io-common` **MAY** provide helpers such as:
- `build_io_context_inbound(...)`
- `extract_reply_target(outbound_message)`
- `propagate_io_context(inbound -> outbound)`

> Note: the exact per-channel content is modeled through `reply_target.kind` and `reply_target.params`.

Case implemented in the repo today:

- `webhook_post`
  - `address = integration_id`
  - `params.integration_id`
  - `params.tenant_id`
  - `params.request_id`
  - `params.trace_id`
  - `params.reply_mode = "final_only"`

Semantics:

- `reply_target` identifies the delivery channel and the minimal correlation;
- it does not replace the channel's real configuration;
- it must not copy secrets or sensitive configuration into the message's data plane.


## 5. State and storage (inside the IO)

Under the current directives, IO Nodes do **NOT** handle a database or Redis.
Therefore `io-common` implements **in-memory** storage for:

- `dedup_cache` (TTL + cap)
- `relay_store` / `relay_buffer` (windows + caps)
- `outbox_queue` (queue + caps)

### 5.1 Mandatory bounding rules

`io-common` **MUST** expose configuration to limit memory and avoid unbounded growth:

- `dedup.ttl_ms`, `dedup.max_entries`, `dedup.max_bytes`
- `relay.window_ms`, `relay.max_open_sessions`, `relay.max_fragments_per_session`, `relay.max_bytes_per_session`
- `outbox.max_pending`, `outbox.max_inflight`, `outbox.max_age_ms`
- eviction policies and forced flush

`io-common` **MUST** expose metrics for:
- evictions,
- drops (because of limits),
- current size of caches/queues,
- timeouts.

---

## 6. Future options: persistence and acceleration outside the IO

If the system allows persistence for IO in the future, there are two typical paths:

- **Postgres** (source of truth) for dedup/outbox/mappings (idempotency) with auditing.
- **Redis** as an accelerator (TTL cache, temporary buffers, rate limiting).

These options are **NOT** enabled inside the IO under the current directives, but the design
of `io-common` must keep extension points to add them (for example, a `StorageBackend` interface).
 (post-MVP)

### 6.1 Recommended uses
- Dedup cache (SETNX + TTL).
- Temporary sessionization buffer.
- Distributed rate limiting.

### 6.2 Not recommended as a source of truth
- Durable outbox.
- Final audit.

### 6.3 Hybrid model
- Redis fast-path + Postgres truth.
- Fallback to Postgres on misses.

---

## 7. Open decisions

### 7.1 DB per IO vs schema per IO
- DB per IO: more isolation, more ops.
- Schema per IO: fewer ops, less isolation.

### 7.2 Durable backend for the relay
- Keep `memory` as a simple, predictable baseline.
- Define `durable` without turning the relay into a historical backlog.

### 7.3 Outbox claiming strategy
- SELECT FOR UPDATE SKIP LOCKED.
- Leases with lock_until.
- External broker.

### 7.4 Outbound idempotency
- Internal key.
- Native channel support (if it exists).

---

## 8. If io-common is not used

Each IO Node must implement:
- durable deduplication,
- outbox and retries,
- backoff,
- lifecycle and draining,
- metrics and logging,
- inbound relay/sessionization (if it applies).

Risks:
- inconsistency across channels,
- duplication bugs,
- high technical debt.

Minimal mitigation:
- conformance checklist + contract tests.

---

## 9. io-common deliverables

- Versioned SQL migrations.
- Stable public API.
- Standard metrics.
- Integration documentation.


---

## Annex A - Suggested schema if Postgres is enabled (post-MVP)

> This annex is **informative** and **does not apply** while the IOs cannot use DB/Redis.

### A.1 Table `io_dedup` (inbound)

- `dedup_key TEXT PRIMARY KEY`
- `first_seen_at TIMESTAMPTZ NOT NULL DEFAULT now()`
- `expires_at TIMESTAMPTZ NOT NULL`
- `meta JSONB NULL`

Operation: `INSERT ... ON CONFLICT DO NOTHING` (conflict => DUPLICATE).

### A.2 Table `io_outbox` (outbound)

- `id UUID PRIMARY KEY`
- `state TEXT NOT NULL` (`PENDING/SENDING/SENT/FAILED/DEAD`)
- `attempts INT NOT NULL DEFAULT 0`
- `next_attempt_at TIMESTAMPTZ NOT NULL`
- `locked_by TEXT NULL`
- `lock_until TIMESTAMPTZ NULL`
- `idempotency_key TEXT UNIQUE NOT NULL`
- `destination JSONB NOT NULL`
- `payload JSONB NOT NULL`
- `provider_msg_id TEXT NULL`
- `last_error TEXT NULL`
- `created_at TIMESTAMPTZ NOT NULL DEFAULT now()`
- `updated_at TIMESTAMPTZ NOT NULL DEFAULT now()`

Indexes: `(state, next_attempt_at)`, `(lock_until)`.

### A.3 Table `io_session_buffer` (sessionization)

- `session_key TEXT PRIMARY KEY`
- `buffer JSONB NOT NULL`
- `window_deadline TIMESTAMPTZ NOT NULL`
- `updated_at TIMESTAMPTZ NOT NULL`


> **Outbox (clarification):** The outbox of the IO nodes **is not durable**. Its state lives only in memory and can be lost on restarts. However, **the established mechanism of TTL, retries and maximum lifetime window is kept**; after that window, pending messages are dropped.

---

## Clarifications v1.16+ - Alignment with Fluxbee

### L3 identity

The IO Node must not block the inbound ACK while waiting for identity.

Normative pipeline:
1. lookup in SHM (`jsr-identity-<island>`)
2. on a miss/error: `ILK_PROVISION` against `SY.identity` (provision_on_miss)
3. if the provision fails/times out: degraded forward with `meta.src_ilk = null`

`src_ilk` must go in `meta.src_ilk`; `meta.context` is left for additional metadata (including `meta.context.io.*`).
`thread_id` must go in `meta.thread_id` (canonical v2 carrier).
`meta.context.thread_id` was removed from the IO pipeline and must not be used.

---

### Context (`ctx_window`)

`ctx_window` is injected by the Router. IO may receive it, but must not forward it to the external channel.

---

### Protocol (`meta.ctx` vs `meta.context`)

- `meta.ctx`: conversational context
- `meta.context`: additional metadata (OPA + temporary carrier for untyped fields)
- IO channel metadata under `meta.context.io.*`
- `thread_id`: only in `meta.thread_id` (not in `meta.context.thread_id`)

---

### Persistence in the IO Node

The MVP uses memory for local technical state (dedup/sessions/outbox).
IO is not the source of truth; canonical persistence stays in the core (Router/Storage/Identity).
