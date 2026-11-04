# IO Node - Slack (MVP Specification)
> Adaptation note for `json-router`:
> This spec is imported and kept as functional baseline.
> If a section conflicts with router v1.16+ docs in this repo, follow `docs/io/README.md` precedence.

## 1. Goal

The **Slack IO Node** is the adapter between Slack and the internal system (router + nodes).
For the MVP, its goal is:

- **Inbound:** receive the bot's mentions (`app_mention`) and the plain messages of the bound
  channel (`message`), files included, and the clicks on a WF's menu (§3.3).
- **Outbound:** send messages to Slack using the **Slack Web API** (`chat.postMessage`), with a WF's
  facts and menu (§8.4), and their files (§8.6), and show a WF's "not taken" to whoever wrote
  (§8.5).

This document **extends** the base IO Nodes specification and defines only
the **Slack-specific** decisions and needs.

---

## 2. MVP assumptions

- There is **a single active logical instance** of the Slack IO Node per `workspace_id + conversation_id` binding.
- **Each node needs a Slack app of its own**, with its own `xapp-` and `xoxb-` tokens (FINDINGS
  A-129). In Socket Mode, Slack sends each event, and each click, to one of an app's open
  connections, not to all of them. Two io.slack nodes on one app split its events between them:
  each drops the other's share as outside its binding, with an `info` line ("dropping inbound
  Slack event outside configured binding", or "dropping a click outside configured binding"), and
  the node bound there never gets it.
- Downtime is accepted if the node goes down.
- IO Nodes do **NOT** handle a database or Redis; therefore:
  - deduplication and sessionization are **in memory** (*best-effort*),
  - outbound retries/outbox are **in memory** (*best-effort*),
  - the technical state is lost after a restart.
- The one exception is `IO.slack`'s thread records and open menus (§2.4): a local SQLite file in its
  node directory, because a reply or a click can come hours later, after a restart.

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
  - `PersistentChannel(channel_type=\"slack\", entrypoint_id=workspace_id, conversation_id=conversation_id)` when there is no `thread_ts`;
  - a reply in the Slack thread of a recorded post takes the recorded thread id instead (§2.4).

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

### 2.4 Thread records

The node owns its conversations' thread ids, because it created the communication event
([wf-orchestration-v1](../wf-orchestration-v1.md) §5.3.4). It records the thread id of each Slack
place it handles, a place being one channel and one thread in it, or the channel's top level. The
records are `slack_threads.db`, next to the node's `config.json`: one row per place, with
`thread_id`, `channel_id`, `thread_ts` (empty for the top level), `created_at`, `last_used_at` and
`opened_by_node` (`nodes/io/io-slack/src/thread_records.rs`).

- **Who opened a place:** the node, with its own post at the top level, or a person: a place
  recorded on the way in, or the `reply_target` of the node's own channel.
- **In:** a reply in a Slack thread whose root is recorded takes the recorded thread id, in
  `meta.thread_id` and in `meta.context.io.conversation.thread_id`, and the place keeps its opener.
  Any other message takes the computed id (§2.2), and its place is recorded under it.
- **Out, to a place a person opened:** a message on its thread id goes there, and its
  `reply_target` is not read.
- **Out, a post the node opened serves one message** (the operator, 2026-10-30): the next message
  on its thread id opens a new post at the top level, whatever its `reply_target`. Once that post
  goes out, its `ts` is recorded, and the node forgets its older posts on the thread id. So a
  second question on a conversation, days later, shows in the channel, and a late reply under the
  first question's post gets its own thread id: it never decides the second. The open menus of
  the forgotten posts, on a post or in its Slack thread, close with them, in the same transaction
  (§8.4), so their buttons no longer send clicks on the thread id.
- **Out, an unrecorded thread id:** with a `reply_target` of the node's own channel, it is posted
  there, and recorded. Any other is posted at the top level, and the post's `ts` is recorded.
- **The marks** ([wf-orchestration-v1](../wf-orchestration-v1.md) §5.4.2): a WF marks each message
  it sends with `meta.context.wf.subject`, read only from a WF node (§8.4).
  - `new`, or no mark: as above.
  - `same`: on a thread id with the node's post, into that post's Slack thread, with no new post
    and nothing forgotten, so a case keeps one Slack thread. On an unrecorded thread id, as `new`.
  - `end`: where `same` puts it; then the subject's open menu closes (§8.4). With no text, no
    blocks and no files, it posts nothing (§8.6), and only closes the menu.
  - A place a person opened takes every message, whatever its mark. A mark the node does not know
    is `new`, with a log line.
- A message's first post decides its place; the rest of the message (its files) goes there, after
  a new post into that post's thread. A file upload answers no `ts`, so a message with only files,
  on a new place, posts a first text that names them, which is recorded, and the files go into its
  thread (§8.6): a reply there takes the thread id, and a `same` goes into that thread.
- **The open menus** (§8.4): the same file's `slack_menus` holds one row per post that shows a WF's
  menu as buttons: `channel_id` and `ts`, `thread_ts` (the Slack thread the post is in, empty at
  the top level), `thread_id` (where a click goes), the menu and the post's blocks as JSON, and
  `created_at`. A thread id has one open menu per channel. A file from before the menus gains the
  table when it opens.
- A record unused for 30 days is deleted, at start and daily, and a menu open for 30 days is
  forgotten.
- If the records cannot open or a query fails, the node logs it and works without them: messages
  in keep the computed id, messages out go to the `reply_target` of the node's own channel, or
  else to the top level, and a menu shows no buttons.
- Marking a record used is best effort: when only that fails (a full disk, a locked or read-only
  file), the node logs it and uses the record it found.

So a WF's question, which carries the instance's thread id, is answered on that thread id when a
supervisor replies in the Slack thread of its post.

---

## 3. Inbound: receiving messages from Slack

### 3.1 Reception mechanism

The Slack IO Node **MUST** support one of the following modes:

#### Option A - Socket Mode (recommended for the MVP)
- WebSocket connection using the **App-Level Token** (`xapp-...`).
- Required scope: `connections:write`.
- It does not need a public HTTP endpoint.
- One app per node (§2): Slack sends each event to one of an app's connections.

#### Option B - Events API (HTTP)
- Public HTTPS endpoint.
- Support for `url_verification` (respond to the `challenge` in plaintext).
- Signature validation using Slack's **Signing Secret**.

The choice of mode does **NOT affect** the IO's internal contract.

---

### 3.2 Supported events: `app_mention` and `message`

The Slack IO Node relays, in the bound channel only:
- `type = "app_mention"`;
- `type = "message"` when it is a plain message from a person:
  - it has no `subtype`, or one of two that are a person's plain message all the same (FINDINGS
    A-130): `thread_broadcast`, a reply with "Also send to #channel", relayed as the reply it is,
    in its Slack thread; and `file_share`, a message that shares files, relayed with them (§7.1.b).
    Any other subtype is dropped: edits, deletions, joins, bot posts;
  - it has no `bot_id`;
  - it is not the node's own post. The node knows its bot user id from `auth.test`; while that id
    is unknown, it relays no `message` (it fails closed). So a file the node uploads, which comes
    back as the node's own `file_share`, is never relayed.

Relevant fields of the Slack event:
- `user`: ID of the user who wrote.
- `channel`: ID of the channel.
- `text`: message content.
- `ts`: message timestamp.
- `thread_ts`: thread timestamp, when the message is in a Slack thread.
- `files`: the files a message shares.
- `event_id` / `event_ts`: event identifiers.

Other events are ignored.

- **One message, two events.** A Slack app subscribed to `message.channels` (or
  `message.groups`) receives the mentions as messages too; subscribed to `app_mention` as well, it
  may get a message that mentions the bot as two events, with different `event_id`s. The node
  deduplicates by the message as well as by the event (§4), so it relays that message once.

### 3.3 Interactive envelopes: a click on a WF's menu

The node also takes the Socket Mode envelopes of type `interactive`, acked at once like any other,
each in a task of its own (`nodes/io/io-slack/src/slack_menu.rs`):

- `block_actions`: a click on a menu's button (§8.4). A click on any other block is ignored;
- `view_submission`: the submission of an option's modal (§8.4). Any other modal is ignored.

A click outside the bound channel, or the bound workspace, is dropped with an `info` line, as an
event is (§2).

- **The read loop only reads, acks and hands off** (`hand_off_envelope`):
  - the `events_api` envelopes go to one event worker, through a queue of 1,024, which takes them
    one at a time, in the order they came (`run_slack_event_worker`). A full queue drops an event
    with an `error` line: it was acked, so Slack does not send it again;
  - the `interactive` envelopes go to a task each, so a click's modal opens within the 3 seconds its
    `trigger_id` lasts, whatever the worker is busy with (a file to download, a person to
    provision);
  - any other envelope is ignored.
- **A click takes no lock an event holds:** it goes to the mesh through an inbound processor of its
  own, so it never waits on an event's provisioning, and a first clicker's never holds up the
  events.
- **Clicks reach the node only when Interactivity is on** in the Slack app, under "Interactivity &
  Shortcuts". Over Socket Mode it needs no Request URL. The buttons show without it, but a click
  then shows Slack's own error to the person, and no envelope reaches the node. The bot token's
  scopes cover the calls: `chat:write` for `chat.update` and `chat.postEphemeral`, and `views.open`
  with a click's `trigger_id`.

---

## 4. Inbound deduplication (Slack)

Slack may resend events if:
- it receives no ACK,
- the ACK is slow,
- or there are transient errors.

Slack may also deliver one message as two events, with different `event_id`s: an `app_mention`
and a `message` (§3.2).

### Deduplication keys
The node deduplicates by both (FINDINGS A-130):
1. the event, `event_id` (or the message's `ts` when there is none): the inbound processor, and
   the relay's fragments (§5);
2. the message, its channel and `ts`, in the event worker: the second event of one message is
   dropped with a `debug` line. An event with no `ts` is never taken for another's.

### Implementation (no DB/Redis)
- In-memory caches with a TTL and a size limit: `DEDUP_TTL_MS` (10 minutes) and
  `DEDUP_MAX_ENTRIES` (50,000) each.
- Eviction: drop-oldest past the size limit.
- Metrics: dedup_hits, dedup_misses (the inbound processor's).

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
    "entrypoint": { "kind": "slack_binding", "id": "<team_id>:<channel>" },
    "sender": { "kind": "slack_user", "id": "<user>" },
    "conversation": { "kind": "slack_channel", "id": "<channel>", "thread_id": "<computed from the channel and the Slack thread, or the recorded one (§2.4)>" },
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

> Note: the message carries no context window or sequence (`meta.ctx*` left the protocol). The Slack IO keeps no history: the router copies each turn into its stream, and only an `AI.*` destination receives memory (`meta.memory_package`), so the Slack IO never has internal memory to forward to Slack.

### payload
```json
{
  "type": "text",
  "content": "<message text without the bot mention>",
  "raw": { "slack_event": "..." }
}
```

- `content` is the person's words as written. Slack sends a person's `&`, `<` and `>` as `&amp;`,
  `&lt;` and `&gt;`, and the node decodes those three, `&amp;` last, so `&amp;lt;` stays `&lt;`.
- Slack's own markup stays as it is: a `<…>` with raw angle brackets, such as a mention `<@U…>` or
  a link `<https://…|label>`.
- On the way out the text is escaped again (§8.1), so a person's `Q&A <x>` goes back as Slack
  sent it.

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

A shared file is downloaded with the bot token only from Slack's file store, `https://files.slack.com` (0.1.95, the review of WF Case 5). Since 0.1.99 (FINDINGS A-85) the download has a 10 s connect timeout and a 120 s deadline, follows a redirect only on the file store's own origin (three at most), and reads at most what the file may take: `max_attachment_bytes`, or what `max_total_attachment_bytes` leaves after the message's files before it. A file announced bigger is not read, and one that grows past it is dropped as it comes, with `attachment skipped` and its size (`download_private_file`). A file shared from elsewhere names its own host: an external file, such as a Google Drive one, or one an app added with `files.remote.add`. Such a file is left out with a `warn` line and never downloaded, since its download would hand that host the token.

Current effective allowlist of `IO.slack`:

- `image/jpeg`
- `image/png`
- `image/webp`
- `image/gif`
- `application/pdf`
- `text/plain`
- `text/markdown`
- `text/html`
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
- the same list governs outbound: a file outside it is left out of the message, and the text names it (§8.6)

Current state:

- `png/jpg/webp/gif`: arrive as attachments
- `pdf`: arrives as an attachment
- `text/plain`, `text/markdown`, `application/json`: arrive as attachments
- `text/html`: goes out as a file (an agent's `generate_html_artifact` page, FINDINGS A-108), and arrives as an attachment
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
- **The text is escaped:** `&`, `<` and `>` are posted as `&amp;`, `&lt;` and `&gt;`, as Slack's
  formatting rules ask of text that is not markup. A payload's text is what a person or an agent
  wrote, so a `<!channel>`, `<!here>`, `<@U…>`, `<!subteam^…>` or `<https://evil.example|label>` in
  it shows as written: it pings no one and shows no false link label. A bare URL still links.
  `blocks` go as they are. A person's text, decoded on the way in (§7), goes back escaped once.

### 8.2 Destination resolution

The node posts only in its bound channel (`config.io.conversation_id`). Where in it is the thread
records' call (§2.4):

- the place of the message's `meta.thread_id`, when a person opened it;
- when the node opened its places, the Slack thread of its post for a message marked `same` or
  `end`; else a new post at the top level, recorded: each of its posts serves one message;
- else its `reply_target`, when that is the node's own channel: its `thread_ts`, or the top level;
- else a new post at the top level, recorded.

A `reply_target` of another channel is never read: an agent's answer carries the one of the message
it answers, which can be another node's.

### 8.3 Long content policy (current state)

✅ **Implemented today:**
- If the inbound payload to `io-slack` carries a text `content_ref`, `io-slack` resolves the blob and sends it as text through `chat.postMessage`.
- The payload's `attachments[]` are sent as files using the external Slack files API flow, each alone (§8.6).

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

### 8.4 A WF's facts, and its menu for people

A WF's block, `meta.context.wf` ([wf-orchestration-v1](../wf-orchestration-v1.md) §5.4.3 and
§5.4.4), adds to a post (`nodes/io/io-slack/src/slack_menu.rs`):

- **Only from a WF:** the node reads the block, its mark included (§2.4), only when the message's
  sender is a `WF.*` node, by the name its router stamps (`routing.src_l2_name`). Any other
  sender's block is ignored, with a `warn` line: a node that knows a thread id could otherwise close
  a case's menu with `end`, or plant buttons whose clicks go on that thread id.
- **The facts:** `vars`, when it is a non-empty object, is a context line under the text, of
  `key: value` pairs: escaped, each value on one line and cut at 200 characters, 10 keys at most.
  A key whose value is empty (`""`, a blank text, `null`, `[]` or `{}`) is left out: a WF's var
  the person's context has no field for is `""` (FINDINGS A-130). Facts alone make no post.
- **The menu:** the same menu an agent answers with an object is, here, a row of buttons under the
  post, one per option, labeled with its name (`_` as a space, the first letter capitalized). The
  menu's `help`, when it has one, is a context line above them.
  - The post is recorded as its thread id's open menu (§2.4). A new one on the thread id closes the
    older.
  - The menu shows no buttons, with a log line, when the message has no thread id, when the records
    are closed, when the message is marked `end`, or when it does not read with wfcel's limits.
- **A post with facts or a menu is blocks:** the text in sections, escaped and `verbatim`, so Slack
  turns no `@channel`, mention, channel or URL in it into markup; then the facts, the help and the
  buttons. A payload's own `blocks` take the text's place. A post with neither is as before.
  - **A post holds Slack's 50 blocks.** A text's sections always leave room for the facts, the help
    and the buttons; a payload's own blocks may not. Then the room goes to the buttons first, then
    to the facts, and what does not fit is left out, with a `warn` line. A post with no room for
    the buttons records no open menu. Own blocks past 50 go as they are, and Slack refuses them.
  - The buttons' block id is `fluxbee_wf_menu`: a payload's own block with any other is never taken
    for them.
- **Closing a menu**, by a new one on its thread id, by `end`, or by a new post that makes the node
  forget the menu's post (§2.4), updates its post (`chat.update`): the same blocks without the
  buttons, and a context line "Closed.".
- **A click on a button** (§3.3):
  - an option without params sends the click message at once;
  - an option with params opens a modal (`views.open`, with the click's `trigger_id`), with one
    required input per param: a string is multiline text, an integer or a number a number input, a
    boolean a yes/no select, and an enum a select of its values. Slack checks the inputs. The
    submission is checked again against the types, and dropped with a log line when it does not
    match;
  - when the modal does not open (Slack refuses a `trigger_id` older than 3 seconds,
    `expired_trigger_id`), the clicker gets "Could not open the form; click again.", which only
    they see, where they clicked, and nothing goes;
  - a click on a post with no open menu gets "This is no longer open.", which only the clicker
    sees (`chat.postEphemeral`), and nothing goes.
- **The click message** is a `user` message built as a relayed Slack message (§6, §7): from the
  person who clicked, resolved or provisioned as for a message, with the post's place in
  `meta.context.io`, on the menu's thread id (`meta.thread_id`).
  - `meta.context.evaluation` is `{"option": "<name>", "params": {...}}`, the shape of an agent's
    answer, so the WF's guards read both. Each param is typed as the menu says: a string, an
    integer as a JSON integer, a number, a boolean, or one of an enum's values.
  - Its text is the option's name, then one line per param, `name: value`.

### 8.5 "Not taken"

When a WF takes a message nowhere, it answers `NOT_TAKEN`
([wf-orchestration-v1](../wf-orchestration-v1.md) §5.4.6): a `system` message with the message's
`trace_id`, and a `reason` in its payload (`nodes/io/io-slack/src/not_taken.rs`).

- The node remembers each message and click it relays, for 15 minutes and 2,000 at most: its
  `trace_id`, its channel, its Slack thread, its Slack user, and the node it was relayed to (its
  `routing.dst`: the node's `io.dst_node` then).
- A `NOT_TAKEN` counts only from the node the message was relayed to, when that node is a `WF.*`,
  by the name its router stamps (`routing.src_l2_name`). One from any other sender is dropped with
  a `warn` line, and takes nothing, so the WF's own still shows. A message relayed with no
  `io.dst_node` (the router resolved it) shows none.
- A `NOT_TAKEN` for a remembered `trace_id` posts "Not taken: " and the reason's text, which only
  that user sees, where they wrote (`chat.postEphemeral`):
  - `no_conversation`: "no conversation here takes this message";
  - `not_expected`: "the conversation does not expect a message from you now";
  - `invalid_input`: "the message is not valid here";
  - `no_thread`: "the message has no conversation";
  - a reason the node does not know: "Not taken.".
- The text is fixed by the reason: the answer's `detail` is never posted. An answer is shown once,
  and one for a message not remembered is dropped with a log line.
- Any other system message stays ignored, and never reaches the channel.

### 8.6 Files, and a message with nothing to post

A message's text goes first, then its files, each uploaded alone (`files.getUploadURLExternal`,
then `files.completeUploadExternal` into the text's place).

- **A file the node cannot send is left out alone** (FINDINGS A-108): the text and the other files
  still go, and the text names it, after a blank line: "Not sent: setup.exe, terms.pdf". A file is
  left out, with a `warn` line and its code, when it is past the first `max_attachments`, of a type
  outside the list (§7.1.b), past `max_attachment_bytes`, past `max_total_attachment_bytes` with
  the files before it, or when its blob does not come (`io.blob.resolve_retry`) or does not read.
  The files are read before the text goes.
- **A file Slack refuses**, once the text went, is named the same way by a post after the files, in
  the same place.
- **A payload that does not read** (no `text/v1`, or a `content_ref` that does not resolve) still
  fails the whole message: an error line with its code is posted.
- **Only files, on a new place** (FINDINGS A-130): a message with no text and no blocks, on a thread
  id with no place (§2.4), posts a first text that names its files ("File: report.pdf", or "Files:
  report.pdf, chart.png"), whose `ts` is recorded, and the files go into its thread. A file upload
  answers no `ts`: without that post, each file would be a top-level post of its own, the place
  would never be recorded, and a reply, or the thread id's next message, would get another place.
  On a place the node knows, and without the records, the files go with no such post.
- **Nothing to post** (FINDINGS A-130): a message with no text, no blocks and no files, such as an
  agent's empty reply a WF forwards, posts nothing, with an `info` line. There is no fallback text.
  A WF's facts alone make no post; its menu alone does, as buttons.

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



### Clarification - meta.thread_id vs meta.context

- `meta.thread_id` carries the communication: `NativeThread` inside a Slack thread, `PersistentChannel` outside one (§2.2), or the recorded thread id for a reply in the Slack thread of a recorded post (§2.4)
- `meta.context` is free JSON for the receiver and the OPA policy; the Slack facts go in `meta.context.io`
- There is no `meta.ctx`: the router drops every key that is not a field of `Meta` ([protocol](../reference/protocol.md) §3.1)

`meta.context` must **NOT** be used to store history, turns or conversational context.
`meta.context.thread_id` was removed from the IO pipeline; the valid carrier is `meta.thread_id`.



## Retries and deduplication

✅ **NORMATIVE (TODAY, Socket Mode)**:
- General deduplication still uses a stable `message_id` (preferably `event_id`; fallback `ts`).
- In addition, the event worker deduplicates by the message, its channel and `ts`, so one message
  delivered as two events is relayed once (§4).
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
