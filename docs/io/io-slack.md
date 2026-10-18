# IO Node - Slack (MVP Specification)
> Adaptation note for `json-router`:
> This spec is imported and kept as functional baseline.
> If a section conflicts with router v1.16+ docs in this repo, follow `docs/io/README.md` precedence.

## 1. Goal

The **Slack IO Node** is the adapter between Slack and the internal system (router + nodes).
For the MVP, its goal is:

- **Inbound:** receive messages only through **mentions of the bot** (`app_mention`).
- **Outbound:** send messages to Slack using the **Slack Web API** (`chat.postMessage`).

This document **extends** the base IO Nodes specification and defines only
the **Slack-specific** decisions and needs.

---

## 2. MVP assumptions

- There is **a single active logical instance** of the Slack IO Node per `workspace_id + conversation_id` binding.
- Downtime is accepted if the node goes down.
- IO Nodes do **NOT** handle a database or Redis; therefore:
  - deduplication and sessionization are **in memory** (*best-effort*),
  - outbound retries/outbox are **in memory** (*best-effort*),
  - the technical state is lost after a restart.

> Note: to avoid inconsistencies with future durability needs, the IO's internal logic should use a **persistence interface** (MVP: InMemory), as defined in `io-common.md`.

### 2.1 Local ICH and minimal operability

For `IO.slack`, the `ICH` must be read as the local binding operated by the instance for `workspace_id + conversation_id`, not as the remote Slack user and not as the workspace in the abstract.

Minimal operational rule:
- the node can stay alive for `PING/STATUS/CONFIG_*`;
- if it does not have enough material to operate its local channel (for example valid credentials or a minimal effective configuration), it must not process real operational Slack traffic;
- in the current state of the repo, that non-operability is expressed through `UNCONFIGURED` or `FAILED_CONFIG`, not through a separate `ICH`-specific state.

### 2.2 Current state of the runtime regarding the internal ILK and ICH

Reading adjusted to the real current state of the repo:

- `IO.slack` already reads `self_ilk_id` and `self_tenant_id` from env at boot;
- unlike other IO nodes, today those values are used operationally to resolve `slack` credentials in `SY.vault` and for the refresh loop;
- in addition, they are now used to ensure that the `ICH` owned by the `workspace_id + conversation_id` binding is registered when Slack traffic comes in;
- `IoContext.entrypoint` no longer represents the workspace in the abstract, but the local `workspace_id + conversation_id` binding;
- the current `thread_id` calculation already uses local channel material:
  - `NativeThread(channel_type=\"slack\", entrypoint_id=workspace_id, native_thread_id=thread_ts)` when there is a native thread;
  - `PersistentChannel(channel_type=\"slack\", entrypoint_id=workspace_id, conversation_id=conversation_id)` when there is no `thread_ts`.

Practical conclusion:

- conversational continuity already correctly includes local material from the `workspace_id + conversation_id` binding;
- the runtime already uses `self_ilk_id/self_tenant_id` effectively for its operation;
- `meta.ich` is already aligned with the local binding;
- an explicit registration of the binding's own `ICH` against identity is already attempted, using `self_ilk_id + (workspace_id + conversation_id)`.

### 2.3 Current state of the registration of the own ICH

The current operational path for `IO.slack` is:

- use the `self_ilk_id` and `self_tenant_id` injected at boot;
- require `config.io.workspace_id` and `config.io.conversation_id` as the minimal operational configuration;
- compute a stable `ich_id` from `channel_type + address + tenant`;
- request `ILK_ADD_CHANNEL` from `SY.identity` with:
  - `channel_type = "slack_binding"`
  - `address = "<workspace_id>:<conversation_id>"`
  - `ilk_id = self_ilk_id`

If the registration fails:

- the node does not process that inbound event;
- it logs a `warn` with the configured binding;
- and it waits for the next event to retry.

In addition, the current runtime already tries to ensure its own `ICH`:

- at boot, if there is a valid `effective_config`;
- and on each successful `CONFIG_SET`, before leaving the node in `Configured`.

Residual gap:

- the local validation of `SY.identity` depends on the available build environment;
- the homogeneous resolution for other IO nodes is still pending.

---

## 3. Inbound: receiving messages from Slack

### 3.1 Reception mechanism

The Slack IO Node **MUST** support one of the following modes:

#### Option A - Socket Mode (recommended for the MVP)
- WebSocket connection using the **App-Level Token** (`xapp-...`).
- Required scope: `connections:write`.
- It does not need a public HTTP endpoint.

#### Option B - Events API (HTTP)
- Public HTTPS endpoint.
- Support for `url_verification` (respond to the `challenge` in plaintext).
- Signature validation using Slack's **Signing Secret**.

The choice of mode does **NOT affect** the IO's internal contract.

---

### 3.2 Supported event: `app_mention`

The Slack IO Node **ONLY** processes events:
- `type = "app_mention"`

Relevant fields of the Slack event:
- `user`: ID of the user who mentions the bot.
- `channel`: ID of the channel where the mention happened.
- `text`: message content.
- `ts`: message timestamp.
- `thread_ts`: thread timestamp (if it applies).
- `event_id` / `event_ts`: event identifiers.

Other events **MUST** be ignored.

---

## 4. Inbound deduplication (Slack)

Slack may resend events if:
- it receives no ACK,
- the ACK is slow,
- or there are transient errors.

### Recommended deduplication key (in order of preference)
1. `event_id` (if present)
2. `(workspace_id, conversation_id, ts)` as a stable surrogate

### Implementation (no DB/Redis)
- In-memory cache with a TTL.
- Size limit: `max_entries`/`max_bytes`.
- Eviction (TTL-LRU or drop-oldest).
- Metrics: dedup_hits, dedup_misses, evictions.

### Operational rule
- The ACK to Slack **MUST** be sent as soon as possible, without waiting for a response from the router or from identity.

> Note: after a node restart, the dedup state is lost and duplicates may reach the router.

---

## 5. Inbound relay (Slack)

### Goal
Group several inbound fragments from the same user in the same channel/thread
into a single logical turn before sending it to the router.

### MVP rules
- Slack no longer keeps a private `SlackSessionizer`.
- The relay/sessionization mechanism lives in `io-common`, and `IO.slack` only provides the channel semantics.
- Suggested key:
```
(channel, user, thread_ts || channel)
```
- Derivation implemented today:
```
slack:<workspace_id>:<conversation_id>:<thread_ts||conversation_id>:<user>
```
- Configurable time window.
- `relay_window_ms = 0` means immediate passthrough.
- `relay_window_ms > 0` means consolidation within the configured window.
- The formal configuration surface of the relay is `config.io.relay.*`.
- `IO.slack` takes that config from the spawn config/bootstrap and can update it hot through `CONFIG_SET`.
- If the node restarts during the window, the `memory` mode may lose the grouping and emit separate messages.
- The relay flush produces a single `text/v1`, and only then does the common identity/forward pipeline run.

---

## 6. Identity (L3) and dispatch to the router

### 6.1 Requirement
The Slack IO Node **MUST** use the shared `io-common` flow for identity:

1. `lookup` through SHM (`jsr-identity-*`)
2. on a miss/error: `provision_on_miss` via `SY.identity` (`ILK_PROVISION`)
3. if the provision fails or times out: forward with `meta.src_ilk = null`

- On a hit: include `meta.src_ilk`.
- On a miss with a successful provision: include the provisioned `meta.src_ilk`.
- On a miss with an unsuccessful provision: **send anyway** with `meta.src_ilk = null`.

This implements the recommended L3 strategy: **Diversion to Onboarding (Reprocessing)**.

### 6.2 Integration with identity (non-blocking ACK)
The Slack IO Node uses `io-common::FluxbeeIdentityProvisioner` for the provision step.
Operational rule:

- It must **ALWAYS** ACK Slack first.
- After the ACK it may run lookup/provision in the inbound flow.
- There is no waiting for an “identity session”; if there is no `src_ilk`, forward with `null`.

Flow control (wait/reinjection) is the responsibility of `WF.onboarding` and the Router.

---

## 7. Normalization toward the router

Besides `meta.src_ilk` (or `null`), the Slack IO **MUST** attach the standardized `meta.context.io` block (IO Context contract), including `entrypoint` (the `workspace_id + conversation_id` binding), `conversation` (channel/thread) and `reply_target` (the Slack thread inside the node's own `conversation_id`).

If the message went through the relay, it **MUST** also attach `meta.context.io.relay.*` with traceability metadata of the assembly.

For each valid event (`app_mention`), the Slack IO Node **MUST** build an
internal-protocol `Message` with:

### routing
- `routing.dst = null`
- `routing.ttl`: the system's standard value
- `routing.trace_id`: new or propagated

Operational interpretation:
- `routing.dst = null` means `resolve` in the router (production baseline).
- A fixed unicast destination must only be used as an override for tests/controlled cases.

### meta
- `meta.type = "user"`
- `meta.src_ilk`: ILK representing the Slack user (external human) **or null**
- `meta.thread_id`: canonical v2 thread for router/cognition
- `meta.context.io`:
```json
{
  "io": {
    "channel": "slack",
    "entrypoint": { "kind": "slack_workspace", "id": "<team_id>" },
    "sender": { "kind": "slack_user", "id": "<user>" },
    "conversation": { "kind": "slack_channel", "id": "<channel>", "thread_id": "<thread_ts if present>" },
    "message": { "id": "<event_id|ts>" },
    "reply_target": {
      "kind": "slack_post",
      "address": "<channel>",
      "params": { "thread_ts": "<thread_ts if present>", "workspace_id": "<team_id>" }
    },
    "relay": {
      "applied": true,
      "reason": "window_elapsed|explicit_final|max_fragments|...",
      "parts": 3,
      "window_ms": 2500
    }
  }
}
```

> Note: if the Router injects `meta.ctx_window`, the Slack IO **MUST** accept it but must **NEVER** serialize/forward it to Slack (it is internal information).

### payload
```json
{
  "type": "text",
  "content": "<message text without the bot mention>",
  "raw": { "slack_event": "..." }
}
```

### 7.1 Size-based offload rule (delegated to the SDK `send`)
- `IO.slack` builds the base `text/v1` payload (`content` + `attachments` when applicable).
- The decision to keep it inline or convert it to `content_ref` because of the size limit **is made by `fluxbee_sdk::NodeSender::send`**.
- Relevant configuration of the SDK enforcement:
  - the SDK's internal defaults (`max_message_bytes=64KB`, `message_overhead_bytes=2048`).
- Goal: avoid divergence between IO adapters and keep a single behavior for `text/v1`.

Operational note:
- `io.blob.*` in the node's control-plane can still be useful for adapter validations/policies, but on its own it does not govern the auto-offload in `NodeSender::send`.

### 7.1.b Current state of accepted inbound attachments

In the current `IO.slack` -> `io-common` -> router path, inbound attachments are only normalized to `attachments[]` when their MIME type is in the current `io-common` allowlist.

Current effective allowlist of `IO.slack`:

- `image/jpeg`
- `image/png`
- `image/webp`
- `image/gif`
- `application/pdf`
- `text/plain`
- `text/markdown`
- `application/json`
- `text/csv`
- `application/msword`
- `application/vnd.ms-excel`
- `application/vnd.ms-powerpoint`
- `application/vnd.openxmlformats-officedocument.wordprocessingml.document`
- `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`
- `application/vnd.openxmlformats-officedocument.presentationml.presentation`

Applied design:

- `io-common` keeps the common `text/v1` validation and enforcement
- `IO.slack` defines its effective policy of accepted MIME types
- the `IO_SLACK_ALLOWED_MIMES` / `io.blob.allowed_mimes_csv` override still takes precedence to adjust the set at deploy time

Exact scope of this list:

- this list describes what `IO.slack` accepts today in this repo
- it does not imply that `IO.slack` accepts every file format Slack can carry
- any MIME type outside this list is still rejected by the adapter until it is added explicitly and its E2E behavior is validated

Current operational consequence:

- if an attachment is outside this effective list, it is dropped inbound with `unsupported_attachment_mime`
- those files do not reach the router or `AI.*`
- so if a file type is supported on the `AI` side but is not in this effective policy, the real E2E limit is still `IO.slack`

Current state:

- `png/jpg/webp/gif`: arrive as attachments
- `pdf`: arrives as an attachment
- `text/plain`, `text/markdown`, `application/json`: arrive as attachments
- `csv`, `doc/docx`, `xls/xlsx`, `ppt/pptx`: accepted by the current effective policy of `IO.slack`

What is still to settle per type:

- there is no documented guarantee that `IO.slack` accepts every format Slack accepts today
- recorded E2E tests are missing for additional MIME families, for example audio and other binaries outside office/PDF
- if more types are to be added, the current criterion is: add them to the adapter's effective policy and validate them end to end

### 7.2 Validation state and the Slack channel limit
- The canonical offload mechanism (`content` -> `content_ref` when over the limit) is validated deterministically with `io-sim`, because it does not depend on the external provider's limits.
- In `IO.slack`, the adapter's end-to-end integration is validated (input/output, attachments, `text/v1` contract), but it is not a reliable channel to test extreme text payloads.
- Official Slack reference (`chat.postMessage`): Slack recommends short texts and **truncates messages above 40,000 characters**.
  - Source: https://docs.slack.dev/reference/methods/chat.postMessage/ (section "Truncating content").

---

## 8. Outbound: sending messages to Slack

### 8.1 API used
- Slack Web API: `chat.postMessage`
- Required token: **Bot Token** (`xoxb-...`)
- Minimal scope: `chat:write`

### 8.2 Destination resolution

The Slack IO Node **MUST**:
- Send the message to the indicated `channel`.
- If `thread_ts` is present, reply in the same thread.
- Otherwise, post in the channel.

### 8.3 Long content policy (current state)

✅ **Implemented today:**
- If the inbound payload to `io-slack` carries a text `content_ref`, `io-slack` resolves the blob and sends it as text through `chat.postMessage`.
- The payload's `attachments[]` are sent as files using the external Slack files API flow.

⚠️ **Known limitation (today):**
- For very long text, this behavior can hit the `chat.postMessage` limit (Slack truncates above 40,000 characters).
- As a result, if an upstream node (for example AI) returns very large text via `content_ref`, today `io-slack` may end up posting truncated text unless it also comes as an explicit attachment.

🧩 **TBD / to be standardized for Slack:**
- Define the canonical fallback when `resolved.text` exceeds Slack's limit:
  - upload the full text as a file/snippet and do not truncate,
  - optionally add a configurable/localizable notice,
  - or controlled chunking if that strategy is adopted.

Official limit reference:
- https://docs.slack.dev/reference/methods/chat.postMessage/ ("Truncating content").

---

## 9. Outbox and outbound idempotency

### MVP model (no DB/Redis)
- In-memory outbox/queue with states:
  - `PENDING`, `SENDING`, `SENT`, `FAILED`, `DEAD`
- Retries with backoff and a configurable limit.
- Mandatory limits: `max_pending`, `max_inflight`, `max_age_ms`.

### Idempotency (Slack)
For the MVP:
- Outbound idempotency is guaranteed by in-memory tracking of the outbox.
- A message marked as `SENT` must not be resent while the process is alive.
- When in doubt (timeout without a response), retry respecting the backoff.

> Note: after a restart, the outbox is lost, and outbound messages not sent yet may be lost.

---

## 10. Handling Slack-specific errors

Common errors to handle:
- `rate_limited`
- `channel_not_found`
- `not_in_channel`
- `invalid_auth` / `token_revoked`

Rules:
- Rate limit errors -> retry with backoff.
- Permission/configuration errors -> `DEAD` + audit.

---

## 11. Minimal required configuration

- `SLACK_APP_TOKEN` (xapp)
- `SLACK_BOT_TOKEN` (xoxb)
- `SLACK_SIGNING_SECRET` (if the Events API is used)

Spawn/orchestrator mode (compliance path):
- The node can read configuration from the spawn `config.json`:
  - canonical path: `/var/lib/fluxbee/nodes/IO/<node_name>/config.json`
- Tokens supported in config:
  - `slack.app_token`, `slack.bot_token`
  - `slack.app_token_ref`, `slack.bot_token_ref` (`env:VAR`)
- Precedence:
  - explicit env (`SLACK_APP_TOKEN`/`SLACK_BOT_TOKEN`) over the spawn config.
- Active spawn contract:
  - requires `FLUXBEE_NODE_NAME` injected by the orchestrator
  - no legacy `NODE_NAME`/`ISLAND_ID` fallback
  - no legacy path override for the spawn config

## 12. Known gaps (current state)

- Boundary between core and the node control-plane:
  - `PUT .../config` (core) maintains the `config.json` managed by the orchestrator.
  - the runtime/business hot reload of `IO.slack` is applied through `CONFIG_SET` (the node's control-plane).
  - `CONFIG_CHANGED` is treated as an informational infrastructure signal; it does not replace `CONFIG_SET` in v1.

- Configuration fallback and process resilience:
  - today, if credentials are missing (`slack.app_token`/`slack.bot_token`), the process ends with `exit 1`.
  - expected behavior (pending): no crash; report the state/config error and keep serving the control-plane.

- Operational blocker (high priority):
  - today the runtime treats missing config/secrets as a fatal startup error (`exit 1`), which causes a restart loop in systemd.
  - `IO.slack` responsibility: this case must move to a degraded/not-ready mode, without ending the process.
  - expected state (pending): bring up the runtime, publish a config error state and stay controllable by the orchestrator/admin.



### Clarification - L3 identity (v1.16+)

The Slack IO Node does not implement a session protocol with Identity.
It uses lookup through SHM + provision on miss in the shared `io-common` pipeline.

The primary resolution is through SHM (`jsr-identity-*`).
On a miss/error, the provisioner may send `ILK_PROVISION` to `SY.identity`.

If no `src_ilk` is obtained (failure/timeout), the IO **MUST** send to the Router with
`meta.src_ilk = null` for controlled degradation/onboarding.

This definition replaces any legacy mode or ad-hoc identity logic in the Slack node.



### Clarification - meta.ctx vs meta.context

According to the Router v1.16 specification:

- `meta.ctx` represents the **Conversational Context (CTX)**
- `meta.context` is reserved exclusively for **additional data evaluated by OPA**

`meta.context` must **NOT** be used to store history, turns or conversational context.
`meta.context.thread_id` was removed from the IO pipeline; the valid carrier is `meta.thread_id`.



## Retries and deduplication

✅ **NORMATIVE (TODAY, Socket Mode)**:
- General deduplication still uses a stable `message_id` (preferably `event_id`; fallback `ts`).
- In addition, the common relay does best-effort deduplication by `fragment_id` within each open session.
- There is no dependency on `X-Slack-Retry-*` headers (specific to the HTTP Events API), since the active mode is Socket Mode.

🧩 **TO BE SPECIFIED**:
- If ingestion through the HTTP Events API is enabled in the future, explicit handling of `X-Slack-Retry-*` and a consistent dedupe policy will have to be added.



## Hot configuration (no restart)

✅ **NORMATIVE**:
- The Slack node **must** be able to update credentials/config without depending on an orchestrator restart, using the Control Plane (`CONFIG_SET`).
- Suggested precedence:
  1) In-memory override received through `CONFIG_SET` (if any)
  2) Local config (YAML/env)
- On restart, the in-memory override is lost; if there is no local config, the node may stay `FAILED_CONFIG (missing_secret)` until it receives `CONFIG_SET` again.

Current relay surface in `CONFIG_SET` / effective config:

- `config.io.relay.window_ms`
- `config.io.relay.max_open_sessions`
- `config.io.relay.max_fragments_per_session`
- `config.io.relay.max_bytes_per_session`

## Operational logging of the relay

Current policy:

- do not use periodic logs or constant snapshots;
- use logs per relevant relay event;
- `debug` for session open, buffering, flush and dedup;
- `warn` for capacity, expiry, drops or fail-open fallback;
- `info` for the relay policy initialization at startup.

Deploy note:

- the published `start.sh` sets `RUST_LOG` including `io_common=debug`, so the relay logs should show up in `journalctl`.
