# SY.timer

SY.timer gives the nodes of a hive persistent timers (one-shot and cron-like recurring) and a
few time services (current time, timezone conversion, parsing and formatting). A timer
survives restarts: it lives in a local SQLite database and fires a `TIMER_FIRED` message to its
target when due.

Source: `go/sy-timer/` (`main.go` handlers, `scheduler.go` firing loop, `persistence.go`
database). Client helpers and constants: `go/fluxbee-go-sdk/timer*.go`.

| | |
| --- | --- |
| Binary | `/usr/bin/sy-timer` (`packaging/build-deb.sh`, `GO_BINS` entry `sy-timer:go/sy-timer`) |
| Node name | `SY.timer@<hive_id>` |
| Role | motherbee and worker |
| systemd unit | `sy-timer.service` (`Restart=always`, `RestartSec=5`) |
| Listens on | the local router socket only; no TCP port |
| State | `/var/lib/fluxbee/nodes/SY/SY.timer@<hive_id>/timers.db` |

## Where it runs

`SY.timer` is in the `system_nodes.motherbee` and `system_nodes.worker` lists of
`/etc/fluxbee/hive.yaml`, and in both `wait_for` lists, so the orchestrator's boot waits for it
there. It is not in the ingress or egress lists. It needs a local identity SHM (next section),
which only motherbee and workers have.

Each hive's timers are its own: `TIMER_LIST` reads the local database only.

## How it starts

`main` in `go/sy-timer/main.go`:

1. Reads `hive_id` from `/etc/fluxbee` (`fluxbeesdk.LoadHiveID`).
2. Waits up to 30 s, polling every 250 ms, until its own ILK (`SY.timer@<hive>`) is visible in
   the identity SHM `/jsr-identity-<hive>` (`fluxbeesdk.WaitForSelfSystemIlkID`). If it never
   appears the process exits (`log.Fatalf`) and systemd restarts it.
3. Connects to the router under `/var/run/fluxbee/routers` with a persistent UUID in
   `/var/lib/fluxbee/state/nodes` (`fluxbeesdk.ConnectWithRetry`, retry every 1 s).
4. Opens `timers.db` in its instance directory (`managedNodeInstanceDir`: `/var/lib/fluxbee/nodes/<KIND>/<node name>`),
   with `busy_timeout(5000)` and WAL on every connection (`timerDSN`), and creates the `timers`
   table and its indexes (`ensureTimerSchema`, schema version 2).
5. Deletes fired and canceled timers older than 7 days (`gcHistoricalTimers`,
   `defaultHistoryRetention`). This runs once per start, not periodically.
6. Replays pending timers (`replayPending`, below), logs
   `SY.timer ready: node=... db=... pending=N gc_deleted=M`, starts the scheduler and serves
   messages.

## Timers

A timer row (`timerRow`, table `timers`) has: `uuid`, `owner_l2_name` (who scheduled it),
`target_l2_name` (who receives `TIMER_FIRED`; defaults to the owner), `client_ref` (optional,
the caller's own name for it), `kind` (`oneshot` or `recurring`), `fire_at_utc` (ms),
`cron_spec`, `cron_tz`, `missed_policy`, `missed_within_ms`, `payload` (JSON), `metadata`
(JSON), `status` (`pending`, `fired`, `canceled`), `created_at_utc`, `last_fired_at_utc`,
`fire_count`.

Rules enforced in `main.go`:

- **Owner.** The owner is the router-stamped `routing.src_l2_name` of the request
  (`resolveSourceL2NameFromMsg`). A message without it is one the router made itself
  (`UNREACHABLE`, `TTL_EXCEEDED`): it gets no answer, only a log line (`isInfrastructureMessage`).
- **Names.** `target_l2_name` and `owner_l2_name` filters must be canonical L2 names:
  `<kind>.<name>@<hive>`, with a dot before the `@` (`isCanonicalL2Name`).
- **Target.** A node schedules for itself: a `target_l2_name` that names another node is
  `TIMER_FORBIDDEN`, unless the owner is a SY node (`checkTimerTarget`; FINDINGS A-115).
- **Minimum duration: 60 s** (`MinTimerDuration` in `go/fluxbee-go-sdk/timer_schedule.go`). A
  one-shot must fire at least 60 s from now; a recurring cron must not fire twice within 60 s.
  Otherwise `TIMER_BELOW_MINIMUM`.
- **Caps** (FINDINGS A-115). A one-shot fires at most 366 days ahead, scheduled or rescheduled
  (`maxTimerDelay`, `TIMER_ABOVE_MAXIMUM`). A `payload` and a `metadata` are at most 32 KiB each
  (`checkTimerData`), and a new timer is refused once its owner has 10,000 pending timers or the
  hive 100,000 (`checkPendingLimits`): both `TIMER_LIMIT_EXCEEDED`.
- **Cron.** Five fields (minute, hour, day of month, month, day of week), parsed with
  `robfig/cron`; `cron_tz` is an IANA zone and defaults to `UTC` (`scheduleRecurringFireAt`).
  The binary embeds the timezone database (`time/tzdata` import).
- **client_ref.** At most 255 characters. Unique per owner among pending timers (unique index
  `idx_timers_owner_clientref_pending`). Scheduling again with a pending `client_ref` returns the
  existing timer with `already_existed: true` instead of creating a second one.
- **Missed policy.** `fire` (default), `drop`, or `fire_if_within` with `missed_within_ms > 0`
  (`validateMissedPolicyRaw`).

### Firing

The scheduler (`timerScheduler` in `scheduler.go`) keeps pending timers in a min-heap by fire
time and sleeps until the next one (at most one hour when the heap is empty). When a timer is
due, `fireTimerByUUID` re-reads it, skips it unless still `pending`, and sends:

- `TIMER_FIRED`, a `SYSTEM` unicast to `target_l2_name` (TTL 16, new trace id), with payload
  `timer_uuid`, `owner_l2_name`, `kind`, `scheduled_fire_at_utc_ms`, `actual_fire_at_utc_ms`,
  `fire_count`, `is_last_fire` (true for one-shots), `user_payload` (the scheduled `payload`), and
  `client_ref` and `metadata` when set (`emitTimerFired`).

A fire is saved before it is sent (FINDINGS A-115): a one-shot becomes `fired`, a recurring timer
gets its next fire time from the cron and goes back on the heap. A stop between the two loses that
fire instead of sending it twice. A row that cannot be saved sends nothing, and the fire is tried
again in 30 s (`timer_fire_persist_failed`).

Each attempt goes under a new trace id, and is kept 30 s for the router's answer (`deliver`). An
`UNREACHABLE` with that trace id, for a target that is not connected (`NODE_NOT_FOUND`,
`PEER_UNAVAILABLE`, `WAN_UNAVAILABLE`, `GATEWAY_UNAVAILABLE`), or a send that fails, sends the same
fire again after 5, 10, 20 and 40 s, then each minute (`undelivered`, `retryLater`). The retries stop
an hour after the first attempt, sooner for a `fire_if_within` one-shot (its window from its
scheduled time) and a recurring timer (its next instance), and a `drop` one-shot gets none
(`deliveryDeadline`); then the fire is lost (`timer_fire_lost`). A fire the router refuses for
another reason (`VPN_BLOCKED`, `OPA_*`) is not sent again (`timer_fire_refused`), nor one over the
frame. A retry whose timer was purged or canceled since is dropped. The retries live in memory: a
fire being retried when SY.timer stops is lost, and its owner covers it (a WF reconciles its timers
when it starts).

### After a restart

`replayPending` runs at startup:

- a pending one-shot still in the future is re-queued;
- a pending one-shot whose time passed while the node was down follows its missed policy
  (`handleMissedTimer`): `fire` queues it, due at once; `drop` cancels it; `fire_if_within` queues
  it only if it is late by at most `missed_within_ms`, otherwise cancels it; an unknown policy
  cancels it. A queued one goes when the scheduler starts and, after a hive reboot, is sent again
  until its owner is relaunched;
- a recurring timer is moved to its next future occurrence; missed occurrences are not fired.

## Messages

All requests are `SYSTEM` messages to `SY.timer@<hive>`. Every answer is `TIMER_RESPONSE`
(`MsgTimerResponse`) with `{ok: true, verb, ...}` or
`{ok: false, verb, error: {code, message}}` (`sendTimerResponse`, `sendTimerError`).

| Verb | Payload | Who may send |
| --- | --- | --- |
| `TIMER_SCHEDULE` | `fire_at_utc_ms` or `fire_in_ms` (exactly one), optional `target_l2_name`, `client_ref`, `payload`, `missed_policy`, `missed_within_ms`, `metadata` | any node; it becomes the owner, and the target unless it is a SY node |
| `TIMER_SCHEDULE_RECURRING` | `cron_spec`, optional `cron_tz` and the same optional fields | any node; it becomes the owner, and the target unless it is a SY node |
| `TIMER_GET` | `timer_uuid`, or `client_ref` (the caller's own pending timer) | the owner, and `SY.admin@motherbee` for any timer by `timer_uuid` (`TIMER_NOT_OWNER` otherwise) |
| `TIMER_LIST` | optional `owner_l2_name`, `status_filter` (`pending` default, `fired`, `canceled`, `all`), `limit` (default 100, max 1000), `cursor` | a node lists its own (another owner is `TIMER_FORBIDDEN`); `SY.admin@motherbee` lists any owner's, or the whole hive's; ordered by fire time, then uuid. A page fits one 128 KiB frame: `next_cursor`, given back as `cursor`, lists the next, and both SDKs' `List` follow the pages (since 1.3, FINDINGS A-151) |
| `TIMER_CANCEL` | `timer_uuid` or `client_ref` | the owner only (`TIMER_NOT_OWNER`); a `client_ref` with no pending timer answers `ok` with `found: false`; a fired one-shot gives `TIMER_ALREADY_FIRED` |
| `TIMER_RESCHEDULE` | `timer_uuid` or `client_ref`, and `new_fire_at_utc_ms` or `new_fire_in_ms` | the owner only; pending one-shots only (`TIMER_RECURRING_NOT_RESCHEDULABLE`, `TIMER_ALREADY_FIRED`) |
| `TIMER_PURGE_OWNER` | `owner_l2_name` | only `SY.orchestrator@<same hive>` (`TIMER_FORBIDDEN` otherwise); deletes every timer of that owner, any status |
| `TIMER_NOW` | none | anyone |
| `TIMER_NOW_IN` | `tz` | anyone |
| `TIMER_CONVERT` | `instant_utc_ms`, `to_tz` | anyone |
| `TIMER_PARSE` | `input`, `layout` (a Go time layout), `tz` | anyone |
| `TIMER_FORMAT` | `instant_utc_ms`, `layout`, `tz` | anyone |
| `TIMER_HELP` | none | anyone; returns the verb and error catalog (`respondTimerHelp`) |
| `NODE_STATUS_GET` | | answered `HEALTHY` (`BuildDefaultNodeStatusResponse`) |

Any other verb gets `TIMER_UNKNOWN_VERB`. The router does
not gate `TIMER_*` verbs: they are not in `PROTECTED_SYSTEM_ACTIONS`
(`src/router/system_policy.rs`).

Error codes: `TIMER_BELOW_MINIMUM`, `TIMER_ABOVE_MAXIMUM`, `TIMER_LIMIT_EXCEEDED`,
`TIMER_INVALID_PAYLOAD`, `TIMER_INVALID_CRON`,
`TIMER_INVALID_TIMEZONE`, `TIMER_FORBIDDEN`, `TIMER_NOT_FOUND`, `TIMER_NOT_OWNER`,
`TIMER_ALREADY_FIRED`, `TIMER_RECURRING_NOT_RESCHEDULABLE`, `TIMER_STORAGE_ERROR`,
`TIMER_UNKNOWN_VERB`, `TIMER_INTERNAL`.

### Teardown

When the orchestrator purges a node (`kill_node` with `purge_instance`, `remove_node_instance`),
it stops the node's unit first and clears its `relaunch_on_boot` (`clear_relaunch_on_boot`), then
sends `TIMER_PURGE_OWNER` for that node to the `SY.timer` of the node's hive, with a 3 s timeout,
and reports the result in its reply as `timer_purge` (`purge_owner_timers_before_teardown` in
`src/bin/sy_orchestrator.rs`). A purge that is not `ok` or gets no answer stops the teardown there
with `TIMER_PURGE_FAILED`, which reports `relaunch_on_boot: false`: the node stays stopped, with
its files and its identity, until the purge is run again. The 60 s reconcile does not launch it
again (FINDINGS A-121).

## SY.admin HTTP routes

Read-only; SY.admin forwards to `SY.timer@<hive in the URL>` (`handle_timer_rpc` in
`src/bin/sy_admin.rs`), which answers `SY.admin@motherbee` for every owner. Scheduling and
cancelling are not exposed: a timer belongs to the node that created it.

| Route | Verb |
| --- | --- |
| `GET /hives/{hive}/timer/help` | `TIMER_HELP` |
| `GET /hives/{hive}/timer/timers?owner_l2_name=&status_filter=&limit=&cursor=` | `TIMER_LIST`, one page |
| `GET /hives/{hive}/timer/timers/{timer_uuid}` | `TIMER_GET` |
| `GET /hives/{hive}/timer/now` | `TIMER_NOW` |
| `POST /hives/{hive}/timer/now-in` | `TIMER_NOW_IN` |
| `POST /hives/{hive}/timer/convert` | `TIMER_CONVERT` |
| `POST /hives/{hive}/timer/parse` | `TIMER_PARSE` |
| `POST /hives/{hive}/timer/format` | `TIMER_FORMAT` |

## Config

None beyond `/etc/fluxbee/hive.yaml` (`hive_id`). Paths are constants (`configDir`,
`routerSockDir`, `uuidPersistenceDir`, `nodeRootDir`). SY.timer has no `CONFIG_SET` and no vault
secret.

## Failure behavior

- No identity SHM entry within 30 s, no router connection, or a database that cannot be opened
  or replayed: the process exits and systemd restarts it every 5 s.
- If the router channel closes, the run loop ends (`sy.timer incoming channel closed`).
- A handler error is logged as a JSON event `timer_handler_error` plus `handle message error`.
- A recurring timer whose next time cannot be computed (`timer_recurring_next_fire_failed`) does
  not fire and is not re-queued; it stays `pending` and the next start tries again
  (`replayPending`, which skips one it still cannot compute).

## Health

- `systemctl status sy-timer`; `journalctl -u sy-timer`. Besides plain lines, the node logs one
  JSON object per event (`logEvent` in `logging.go`): `timer_startup`, `timer_scheduled`,
  `timer_fired` (one per attempt), `timer_canceled`, `timer_rescheduled`, `timer_owner_purged`,
  `timer_missed_policy_applied`, `timer_handler_error`, and for the delivery
  `timer_fire_persist_failed`, `timer_fire_emit_failed`, `timer_fire_undelivered`,
  `timer_fire_refused`, `timer_fire_lost`, `timer_fire_dropped`.
- `curl -s 127.0.0.1:8080/hives/<hive>/timer/now` on the motherbee proves the admin → timer
  round trip; `.../timer/timers` shows the pending timers.

## Links

- [SY.wf-rules](sy-wf-rules.md): WF nodes schedule their timers on the `SY.timer` of their own
  hive, with `client_ref` `wf:<instance_id>::<timer_key>` (`timerClientRef` in
  `go/nodes/wf/wf-generic/node/timer.go`; target set in `node/config.go`).
- [SY.identity](sy-identity.md): the SHM SY.timer waits for at startup.
- [Operations](../reference/operations.md): node teardown.
