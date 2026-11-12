# fluxbee-go-sdk

Go SDK for Fluxbee node/runtime integration.

Current v1 scope:

- Fluxbee message envelope types
- socket framing and router lifecycle
- node handshake helpers
- node config control-plane helpers
- default node status helpers
- direct `system` request/reply RPC helpers
- canonical `HELP` descriptor types
- base `TIMER_RESPONSE` parsing helpers
- typed `SY.timer` client surface for time operations, scheduling, reads, cancel/reschedule by `timer_uuid` or `client_ref`, and `TIMER_FIRED` parsing

First-party consumers:

- `SY.opa.rules`
- `SY.timer`
- `SY.wf-rules`
- `wf-generic`, the WF engine

## Versioning and compatibility policy

Current compatibility target:

- `fluxbee-go-sdk` is a **v1 in-repo SDK**
- it is intended for first-party nodes now and third-party Go consumers next
- wire compatibility is anchored to the current Fluxbee runtime contract and guarded by SDK tests/fixtures

Rules for v1:

- patch-level changes must be backward compatible bug fixes
- additive exported APIs are allowed in v1 as long as they do not break existing callers
- breaking wire-contract changes require:
  - explicit spec update
  - fixture/golden test updates
  - coordinated runtime + SDK migration
- breaking exported-API changes should be treated as a new major SDK phase, not as a casual refactor

Practical meaning:

- `SY.opa.rules` and `SY.timer` are the current compatibility anchors
- new first-party consumers should build on the documented surface below, not on incidental helpers

## Multiple routers per hive

The Go SDK now assumes the same runtime model that the Rust router implements:

- a node may connect to **any local router** in the hive
- the effective router is the one announced back in `ANNOUNCE.router_name`
- the router stamps `routing.src_l2_name` on delivered messages
- router SHM is **per-router**, not a hive-wide merged node table

Implications for consumers:

- do not infer identity from `gateway_name`
- do not assume one router per hive
- do not assume reading one router SHM gives a merged view of the whole hive

This is sufficient for `SY.timer` because requester ownership now comes from router-stamped `routing.src_l2_name`, not from SDK-side SHM lookup.

## Stable public surface for v1

The v1 stable surface is:

- node connection/lifecycle:
  - `ConnectWithRetry`
  - `RouterDispatcher`
  - `OperationalRouteProfile`
  - `NewOperationalRouteProfile`
  - `NodeConfig`
  - `NodeSender`
  - `NodeUuidMode`
- envelope and protocol types:
  - `Message`
  - `Routing`
  - `Destination`
  - `Meta`
- request/reply helpers:
  - `BuildSystemRequest`
  - `BuildSystemResponse`
  - `RouterDispatcher.SendSystemRPC`
  - `RouterDispatcher.SendAdminRPC`
  - `RouterDispatcher.SendWithMatcher`
  - `RouterDispatcher.TakeCommandReceiver`
  - `RouterDispatcher.Subscribe`
  - `ParseSystemResponse`
  - `ParseSystemResponseError`
  - `RpcError`: why a request failed, by `Kind`. A router's `UNREACHABLE` or `TTL_EXCEEDED` is
    `RpcErrTerminalTransport`, with what the notice says: `Notice`, `Reason` (`LINK_BUSY`,
    `NODE_NOT_FOUND`, ...), `OriginalDst` and `LastHop` (`UnreachablePayload`,
    `TTLExceededPayload`). The target's `NOT_TAKEN` is `RpcErrNotTaken`, with its `Reason` and
    `Retryable` (since 0.1.141)
  - `RouterDispatcher.CommandChannelDrops`, `RouterDispatcher.NotTakenSent`: what the command
    channels refused, and what the SDK answered (below)
- `NOT_TAKEN` (below):
  - `MSGNotTaken`, `NotTakenPayload`, `NotTakenReasonNodeBusy`
  - `BuildNotTaken`, `ParseNotTaken`
  - `IsNoticeOrReply`, `IsReplyName`: the messages no one answers
- the text/v1 offload (below): `BlobRef`, `DefaultBlobRoot`
- node control-plane helpers:
  - `CONFIG_GET`
  - `CONFIG_SET`
  - `CONFIG_RESPONSE`
  - default node status helpers
- `HELP` descriptor types/helpers
- `SY.timer` client/types/helpers
- identity SHM lookup:
  - `LookupIlkByChannel`

## Command channels and NOT_TAKEN

A command channel (`TakeCommandReceiver`) hands its consumer a queue of two lanes (since 0.1.141,
FINDINGS A-329): `system` messages and every other kind. The consumer gets the oldest system
message first, then the oldest of the rest, each lane in the order it came: a `NODE_STATUS_GET`, a
`CONFIG_SET` or a `TIMER_FIRED` never waits behind a backlog of user messages, only behind the one
the consumer is working on (and, at most, the one handed over as it came). Each lane holds 1,024
messages or 8 MiB, whichever comes first: the router's own bound for a node's link
(`LINK_QUEUE_MAX_FRAMES`, `LINK_QUEUE_MAX_BYTES`), about 34 s of a WF that creates 30
conversations a second, and two and a half times the backlog A-329 was seen with. Before 0.1.141 a
command channel held 64 messages and dropped the rest, telling no one.

A message that does not fit its lane is never waited for: the dispatcher answers its sender
`NOT_TAKEN` at once, a `system` message to the sender's uuid on the message's `trace_id` and
`thread_id`, with `{"ok": false, "error": "not_taken", "reason": "node_busy", "detail": "...",
"retryable": true}` (`BuildNotTaken`). A notice or a reply is never answered (`IsNoticeOrReply`:
one the router made, `HELLO`, `ANNOUNCE`, `WITHDRAW`, `UNREACHABLE`, `TTL_EXCEEDED`, a system message
without a name, any `*_RESPONSE`, a `NOT_TAKEN`, an `<msg>.error`), so two nodes never bounce one;
a person's text, a user message without a name, is answered. Each refusal is counted
(`CommandChannelDrops`, by channel; `NotTakenSent`: answered, unanswered, or whose `NOT_TAKEN`
could not be sent) and logged: the first at once, then at most a line every 10 s per channel,
counting what came since:

```text
fluxbee-go-sdk: command channel "incoming" full (0 system and 1024 non-system messages waiting): 344 message(s) not taken since 09:39:41.120Z: 344 answered NOT_TAKEN node_busy, 0 not answered (a notice or a reply), 0 whose NOT_TAKEN could not be sent; the last: msg_type="user" msg="" from "IO.gate@worker1" trace_id=...
```

Retrying is the sender's (the operator's principle of 2026-11-12: retry and log on the node that
fails, never a global action). SY.timer sends a fire again; a WF takes SY.timer's `NOT_TAKEN` for a
timer request as its refusal, and the timer goes again; the request helpers end with
`RpcErrNotTaken`. A broadcast channel (`Subscribe`) keeps 64 messages per subscriber and drops what
a slow one cannot hold, counted under `broadcast:<name>`: it carries replies, which no one answers.

The connection's reader waits for room in the receiver's queue (256) instead of dropping a frame
(since 0.1.141): the dispatcher never waits, so the wait is short, and meanwhile the frames wait in
the router's queue for the link, which answers `LINK_BUSY` past its bound
([protocol](../../docs/reference/protocol.md) §6.10, §10.2, §11).

## A text over the frame goes as a blob

`NodeSender.Send` (and `SendTraced`) sends a message whose frame is over 128 KiB, which it refused
before (`MessageTooLargeError`), with the text of its text/v1 payload (`{"type": "text", "content":
...}`) in a blob of the hive's blob store (`/var/lib/fluxbee/blob`) and the blob's ref in its place,
`content_ref` (`BlobRef`), when that makes it fit (since 0.1.141, FINDINGS A-264). A message that
fits a frame goes as it is, byte for byte, its text inline however long.

That is the operator's call of 2026-11-12, option (b): the Rust SDK sends a text over about 62 KiB
as a blob, the Go SDK only past the frame. A reader on another hive gets a blob through Syncthing,
about 10 s or more after the message, and today no reader waits that long: ai.generic reads a
`content_ref` at once, io.slack and io.wapp wait 5 s (`read_blob_with_retry`), and a WF, io.api,
io.web and io.linkedhelper read none. So what fit inline before 0.1.141 still goes inline and
reaches every reader, and only a text that could not be sent at all goes as a blob: a reader on the
sender's hive reads it at once, and one on another hive may not find it yet. Getting the blob to
the reader's hive in time is a topic of its own. A text of about 95 to 128 KiB still reaches its
node but not the router stream's turns (FINDINGS A-253), as before
([protocol](../../docs/reference/protocol.md) §8).

The blob is `content_<the first 16 hex digits of its SHA-256>.txt`, `text/plain` (`text/html` or
`image/svg+xml` when its first bytes are such a document, so it is served sandboxed), written to
`staging/<prefix>/` and moved to `active/<prefix>/` with the Rust SDK's layout and checks (no link
under the blob root followed; the blob given to the owner of `active/`, whom Syncthing runs as, so
it reaches the other hives). The payload keeps its other fields; one whose `content` is null or no
string, or that has a `content_ref` already, is not offloaded. A message over the frame with the
ref too (a large `meta.context`) is refused, and no blob is written for it; so is one whose blob
cannot be written, with a log line. The node writes the blob itself: a WF runs as root, as every
managed node.

## SY.timer client patterns

Recommended usage in v1.1:

- schedule with an optional deterministic `client_ref`
- operate later by `client_ref` instead of waiting for the schedule response UUID
- keep UUID-based methods when the caller already has a concrete timer id

Available helper methods:

- `Schedule`
- `ScheduleIn`
- `ScheduleRecurring`
- `Get`
- `GetByClientRef`
- `Cancel`
- `CancelByClientRef`
- `Reschedule`
- `RescheduleByClientRef`
- `List`
- `ListPage`
- `ListMine`
- `Help`

Typical async pattern:

```go
clientRef := "wf:instance-123::sla_timeout"

_, err := timerClient.ScheduleIn(ctx, time.Hour, sdk.ScheduleOptions{
    ClientRef: clientRef,
})
if err != nil {
    return err
}

// Later, without needing the timer UUID:
if err := timerClient.CancelByClientRef(ctx, clientRef); err != nil {
    return err
}
```

Listing every timer, in pages (FINDINGS A-278). `List` follows SY.timer's pages up to the filter's
`Limit` (at most `MaxTimerListLimit`, 1000) and returns the timers only, nothing of those past it.
`ListPage` is one `TIMER_LIST`: a page of SY.timer's, from the filter's `Cursor`, and its
`NextCursor`, which is empty once the page lists the rest. A WF's reconcile reads its pending timers
this way, with a bound on the pages ([SY.timer §7.6](../../docs/sy-timer.md#76-timer_list)):

```go
filter := sdk.ListFilter{StatusFilter: "pending", Limit: sdk.MaxTimerListLimit}
for {
    page, err := timerClient.ListPage(ctx, filter)
    if err != nil {
        return err
    }
    handle(page.Timers)
    if page.NextCursor == "" {
        break // the rest of the list
    }
    filter.Cursor = page.NextCursor
}
```

A list in pages is no snapshot: a timer whose fire time moves between two pages can come again, or
not at all. SY.timer before 1.6 (0.1.133) answers no `next_cursor` when the limit, not the frame, ends
a page; at the limit 1000 that never happens, since a frame holds about 400 timers at the most.

## Identity SHM lookup

IO nodes can resolve a `(hiveID, tenantID, channelType, address)` tuple to an ILK ID directly from the identity shared memory region — no socket round-trip required:

```go
ilkID, found, err := sdk.LookupIlkByChannel(hiveID, tenantID, channelType, address)
if err != nil {
    // Fatal I/O or mapping failure — treat as unavailable.
}
if !found {
    // ILK not yet provisioned for this channel.
    // Provisioning (ILK_PROVISION wire message) is Rust-only today;
    // the Go SDK does not emit ILK_PROVISION. See the Rust SDK
    // (`fluxbee_sdk::identity::provision_ilk`) for the provisioning path.
}
// ilkID is in "ilk:<uuid>" format, ready for meta.src_ilk.
```

Parameters:

- `hiveID` — raw hive identifier (e.g. `"prod-hive-1"`), no prefix
- `tenantID` — `"tnt:<uuid>"` format; pass `""` to match the default tenant (zero bytes)
- `channelType` / `address` — normalized to lowercase before lookup (matching writer behavior)

Requirements:

- Requires `IDENTITY_VERSION=4` in the SHM region. Older regions return `(_, false, nil)` without error.
- The SHM file must exist at `/dev/shm/jsr-identity-<hiveID>`. A missing file is treated as not-found (`false, nil`), not an error.
- Uses a 50 ms seqlock timeout. If the writer holds the lock for longer, the call returns `(_, false, nil)` rather than blocking.

### Scope of this SDK regarding identity

The Go SDK exposes **only** identity SHM lookup. It does not currently construct or send
the identity system verbs such as `ILK_PROVISION`, `TNT_CREATE`, `TNT_UPDATE`, or
`TNT_SET_SPONSOR`. All first-party identity mutation helpers today live in the Rust SDK.
If a Go-based node needs to call `SY.identity@<hive>`, the wire message must still be
assembled manually against the live `SY.identity` contract.

Everything else should be treated as implementation support unless it is later documented here as part of the stable surface.
