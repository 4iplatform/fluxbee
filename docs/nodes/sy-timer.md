# SY.timer

SY.timer gives the nodes of a hive persistent timers (one-shot and cron-like recurring) and a
few time services (current time, timezone conversion, parsing and formatting). A timer
survives restarts: it lives in a local SQLite database and fires a `TIMER_FIRED` message to its
target when due.

Source: `go/sy-timer/` (`main.go` handlers, `scheduler.go` firing loop, `persistence.go`
database, `cron.go` the `cron_spec` parser, `refusals.go` the refusal log and the log of ignored
messages). Client helpers and constants:
`go/fluxbee-go-sdk/timer*.go`.

| | |
| --- | --- |
| Binary | `/usr/bin/sy-timer` (`packaging/build-deb.sh`, `GO_BINS` entry `sy-timer:go/sy-timer`) |
| Node name | `SY.timer@<hive_id>` |
| Role | motherbee and worker |
| systemd unit | `sy-timer.service` (`Restart=always`, `RestartSec=5`) |
| Listens on | the local router socket only; no TCP port |
| State | `/var/lib/fluxbee/nodes/SY/SY.timer@<hive_id>/timers.db` |

## Where it runs

`SY.timer` is in the release's lists for the motherbee and the worker (`src/release_settings.rs`),
and in both `wait_for` lists, so the orchestrator's boot waits for it
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
   with `busy_timeout(5000)`, WAL and a 16 MiB page cache on every connection (`timerDSN`; the
   cache since 1.7, the operator's call of 2026-11-12, FINDINGS A-326: SQLite's default is 2,000
   KiB, and a large purge spilled past it, below; up to about 31 MiB of memory for each of the
   pool's two connections, `docs/sy-timer.md` §5.2), and creates the `timers`
   table and its indexes (`ensureTimerSchema`, schema version 4: version 3 adds `canceled_reason`,
   FINDINGS A-220, and version 4 `canceled_at_utc`, FINDINGS A-238, to a table that lacks them,
   whatever `user_version` says; `addTimerColumn`). It builds the indexes the table lacks
   (`ensureTimerIndexes`), among them the four `TIMER_LIST` reads (`idx_timers_list*`, since 1.7,
   FINDINGS A-326): the first start of 1.7 builds them over the table it finds and then drops
   `idx_timers_owner` and `idx_timers_fire_at`, which no statement reads once the four are there,
   0.14 s with 18,000 pending timers and 1.8 to 1.9 s with 200,000 timers on a Mac. An index it
   cannot build (a disk short of room) is reported in step 6 and the start goes on without it,
   keeping the two old ones until the four are there; the next start tries again (the review of
   A-326). Before, a build that failed stopped the start.
5. Deletes the fired timers whose last fire, and the canceled timers whose cancellation, is more
   than 7 days old (`gcHistoricalTimers`, `defaultHistoryRetention`). A canceled timer with no
   cancellation time, canceled before 1.5 or by a SY.timer before it, is dated first and kept 7 days
   from this start. Before 1.5 a canceled timer went by its last fire or its creation: one canceled
   yesterday whose creation was a week old was deleted, its `canceled_reason` with it (FINDINGS
   A-238). This runs once per start, not periodically; a GC that fails is logged and changes nothing.
6. Replays pending timers (`replayPending`, below), logs
   `SY.timer ready: node=... db=... pending=N gc_deleted=M gc_stamped=S` (S: the canceled timers
   step 5 dated), starts the scheduler and serves messages. When step 5 failed, the line says
   `gc=failed` in place of both counts, and `timer_startup` gives `gc_error` (`startupGC`): the
   failed GC's counts are 0, which would read as nothing to delete or to date. A count of the
   pending timers that fails is `pending=failed` the same way, with `pending_error` in
   `timer_startup` (`startupPending`, since 1.6, FINDINGS A-259); before, the line said `pending=0`.
   A start that built or dropped an index in step 4, or could not, adds
   `indexes_built=B indexes_dropped=D indexes_ms=T` to the line (and `indexes_failed=F`), and
   `timer_startup` names them, with each failure's error, and gives `indexes_ms`, the time step 4
   took for them (`startupIndexes`, since 1.7): the first start of 1.7 on an older table says
   `indexes_built=4 indexes_dropped=2` (on a new database, `indexes_built=5`), later starts
   nothing.

## Timers

A timer row (`timerRow`, table `timers`) has: `uuid`, `owner_l2_name` (who scheduled it),
`target_l2_name` (who receives `TIMER_FIRED`; defaults to the owner), `client_ref` (optional,
the caller's own name for it), `kind` (`oneshot` or `recurring`), `fire_at_utc` (ms),
`cron_spec`, `cron_tz`, `missed_policy`, `missed_within_ms`, `payload` (JSON), `metadata`
(JSON), `status` (`pending`, `fired`, `canceled`), `created_at_utc`, `last_fired_at_utc`,
`fire_count`, `canceled_reason` (why SY.timer canceled it itself, below; none when its owner
canceled it), `canceled_at_utc` (when it was canceled, by its owner's first `TIMER_CANCEL` or by
SY.timer: the 7 days of a canceled timer count from it; kept, not shown by `TIMER_GET` or
`TIMER_LIST`).

Rules enforced in `main.go`:

- **Owner.** The owner is the router-stamped `routing.src_l2_name` of the request
  (`resolveSourceL2NameFromMsg`). A message without it is one the router made itself
  (`UNREACHABLE`, `TTL_EXCEEDED`): it gets no answer, only a log line, bounded
  (`isInfrastructureMessage`; `timer_message_ignored`, Messages below).
  Nor does a notice or a reply from a node: `HELLO`, `ANNOUNCE`, `WITHDRAW`, a message without a
  name, SY.vault's `VAULT_SECRET_CHANGED`, any `*_RESPONSE`, a `NOT_TAKEN` or an agent's
  `<msg>.error`; wf-generic's rule, name by name, and the AI nodes' but for the vault's notice and
  the last two (FINDINGS A-218, A-329, A-325).
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
  hive 100,000 (`checkPendingLimits`): both `TIMER_LIMIT_EXCEEDED`. The owner's count reads only
  its own pending timers, in `idx_timers_list_owner_status` (since 1.7, FINDINGS A-326); before,
  each new timer's count read every pending timer of the hive.
- **Cron.** Five fields (minute, hour, day of month, month, day of week), parsed with
  `robfig/cron`; `cron_tz` is an IANA zone and defaults to `UTC` (`scheduleRecurringFireAt`).
  A `cron_spec` that starts with `TZ=`, `CRON_TZ=` or `@` is `TIMER_INVALID_CRON` before the
  parser sees it (`parseCronSpec`; FINDINGS A-206): the zone goes in `cron_tz`, and v1 has no
  descriptors. The binary embeds the timezone database (`time/tzdata` import).
- **client_ref.** At most 255 characters. Unique per owner among pending timers (unique index
  `idx_timers_owner_clientref_pending`). Scheduling again with a pending `client_ref` returns the
  existing timer with `already_existed: true` instead of creating a second one. A canceled timer,
  by its owner or by SY.timer, frees it.
- **Missed policy.** `fire` (default), `drop`, or `fire_if_within` with `missed_within_ms > 0`
  (`validateMissedPolicyRaw`).

### Firing

The scheduler (`timerScheduler` in `scheduler.go`) keeps pending timers in a min-heap by fire
time and sleeps until the next one (at most one hour when the heap is empty). The heap holds one
entry per pending timer, found by its uuid (`queued`), and the retries of fires on their way (below):
`TIMER_CANCEL` takes the timer's entry out, `TIMER_RESCHEDULE` moves it, and `TIMER_PURGE_OWNER`
takes out the owner's pending timers' entries (since 1.6, FINDINGS A-259). Before, nothing took an
entry out: a canceled, moved or purged timer's stayed until its old fire time, up to 366 days ahead,
and a node that looped a schedule and its cancel grew the heap, unseen by the pending caps. When a
timer is due, `fireTimerByUUID` re-reads it, skips it unless still `pending`, and sends:

- `TIMER_FIRED`, a `SYSTEM` unicast to `target_l2_name` (TTL 16, new trace id), with payload
  `timer_uuid`, `owner_l2_name`, `kind`, `scheduled_fire_at_utc_ms`, `actual_fire_at_utc_ms`,
  `fire_count`, `is_last_fire` (true for one-shots), `user_payload` (the scheduled `payload`), and
  `client_ref` and `metadata` when set (`emitTimerFired`).

A fire is saved before it is sent (FINDINGS A-115): a one-shot becomes `fired`, a recurring timer
gets its next fire time from the cron and goes back on the heap. A stop between the two loses that
fire instead of sending it twice. A row that cannot be saved sends nothing, and the fire is tried
again in 30 s (`timer_fire_persist_failed`); so is a row that cannot be read, the database's
failure and not a timer gone (`timer_fire_read_failed`, since 1.6, the review of FINDINGS A-259):
before, that fire logged nothing, and the timer, its entry gone, waited for the next start.

A fire holds its timer's lock (`lockTimer`) from its read of the row to its save and its send.
`TIMER_CANCEL` and `TIMER_RESCHEDULE` take the same lock and read the timer again under it
(`lockOwnedTimerForMutation`, since 1.4, FINDINGS A-220's review): one that lands during a fire
waits, then sees what the fire wrote, so a one-shot that fired is `TIMER_ALREADY_FIRED` and a
recurring timer canceled after its fire stays canceled. Before, the fire's save wrote over them. The
save is made only while the row is still `pending` (`updateTimerIfStatus`): the fire of a timer
purged before its save (`TIMER_PURGE_OWNER` takes no timer lock) is not sent (`timer_fire_dropped`),
and one saved before the purge is sent. Either way the timer is not left in the heap: the fire puts
a recurring timer's next fire there before its save, and takes it out again when the save finds the
timer gone, and a purge after the save takes it out itself (since 1.6). The
scheduler keeps a timer's lock only while something holds it or waits for it (since 1.5, FINDINGS
A-244); before, it kept one for every timer that ever fired, was canceled or rescheduled, until
SY.timer stopped.

Each attempt goes under a new trace id, and is kept 30 s for the router's answer (`deliver`). An
`UNREACHABLE` with that trace id, for a target that is not connected (`NODE_NOT_FOUND`,
`PEER_UNAVAILABLE`, `WAN_UNAVAILABLE`, `GATEWAY_UNAVAILABLE`) or whose link queue is full
(`LINK_BUSY`, from the router of 0.1.129, FINDINGS A-222: it clears as the target reads), or a send
that fails, sends the same fire again after 5, 10, 20 and 40 s, then each minute (`undelivered`,
`retryLater`). So does the target's own `NOT_TAKEN` with that trace id, when it says `retryable`:
the Go SDK of a target whose queue was full (`node_busy`, a WF's, since 0.1.141, FINDINGS A-329;
`notTaken`, with a log line at each attempt queued; a fire no longer on time is lost,
`timer_fire_lost`, never said to go again). Only the fire's target, by the name its router
stamps, decides it; one not retryable is not sent again (`timer_fire_refused`). Before 0.1.141 a Go
target dropped a fire its queue (64) could not hold, and SY.timer answered a `NOT_TAKEN`
`TIMER_UNKNOWN_VERB`. The retries stop
an hour after the first attempt, sooner for a `fire_if_within` one-shot (its window from its
scheduled time) and a recurring timer (its next instance), and a `drop` one-shot gets none
(`deliveryDeadline`); then the fire is lost (`timer_fire_lost`). A fire the router refuses for
another reason (`VPN_BLOCKED`, `OPA_*`, `FRAME_TOO_LARGE`) is not sent again (`timer_fire_refused`),
nor one over the frame. A retry whose timer was purged or canceled since is dropped. The retries
live in memory: a fire being retried when SY.timer stops is lost, and its owner covers it (a WF
reconciles its timers when it starts).

### After a restart

`replayPending` runs at startup:

- a pending one-shot still in the future is re-queued;
- a pending one-shot whose time passed while the node was down follows its missed policy
  (`handleMissedTimer`): `fire` queues it, due at once; `drop` cancels it; `fire_if_within` queues
  it only if it is late by at most `missed_within_ms`, otherwise cancels it; an unknown policy
  cancels it. One canceled says why in its `canceled_reason`. A queued one goes when the scheduler
  starts and, after a hive reboot, is sent again until its owner is relaunched;
- a recurring timer is moved to its next future occurrence; missed occurrences are not fired. One
  whose next occurrence cannot be computed is canceled (`cancelUnschedulable`), with
  `canceled_reason` `its next fire cannot be computed: <the error>`, and logged once
  (`timer_replay_invalid_recurring`): its `client_ref` is free for a new timer, and it no longer
  counts toward the pending caps (FINDINGS A-220, since 1.4). Before, it stayed `pending` for good:
  scheduling again with its `client_ref` answered the dead timer, `already_existed: true`.

Its owner reads `canceled_reason` with `TIMER_GET` by `timer_uuid` and with `TIMER_LIST`
(`status_filter` `canceled` or `all`); by `client_ref` a canceled timer is `TIMER_NOT_FOUND`. The
Go SDK's `TimerInfo` carries it as `CanceledReason` (since 0.1.130, FINDINGS A-239), and the Rust
one keeps it in `extra`; SY.admin's routes, and Archi, show it.

## Messages

All requests are `SYSTEM` messages to `SY.timer@<hive>`. Every answer is `TIMER_RESPONSE`
(`MsgTimerResponse`) with `{ok: true, verb, ...}` or
`{ok: false, verb, error: {code, message}}` (`sendTimerResponse`, `sendTimerError`).

Every refusal is logged too (`sendTimerError`, `refusalLog` in `refusals.go`; since 1.6, FINDINGS
A-258): one `timer_request_refused` line with the `verb`, the `error_code`, the sender
(`src_l2_name`) and the `trace_id`, not the `message`, which can quote what the request sent
(FINDINGS A-232), except a `TIMER_STORAGE_ERROR`'s, which is the database's: the line gives it as
`error`. At most 10 lines a minute per sender, from its first refusal: the tenth carries
`not_logged_until_utc_ms`, the rest of that minute is counted, and one
`timer_request_refusals_not_logged` line gives the `count` when the minute ends (a sweep every 10 s,
or the sender's next refusal). Before, only the sender heard of a refusal. A message that is no
request (below) gets one `timer_message_ignored` line (`msg`, `src_l2_name`, empty for one the
router made, `trace_id`; `ignoredLog`), bounded the same way in windows of its own, with
`timer_messages_ignored_not_logged` for the count; before 1.6, a plain line each, with no bound.

| Verb | Payload | Who may send |
| --- | --- | --- |
| `TIMER_SCHEDULE` | `fire_at_utc_ms` or `fire_in_ms` (exactly one), optional `target_l2_name`, `client_ref`, `payload`, `missed_policy`, `missed_within_ms`, `metadata` | any node; it becomes the owner, and the target unless it is a SY node |
| `TIMER_SCHEDULE_RECURRING` | `cron_spec`, optional `cron_tz` and the same optional fields | any node; it becomes the owner, and the target unless it is a SY node |
| `TIMER_GET` | `timer_uuid`, or `client_ref` (the caller's own pending timer) | the owner, and `SY.admin@motherbee` for any timer by `timer_uuid` (`TIMER_NOT_OWNER` otherwise) |
| `TIMER_LIST` | optional `owner_l2_name`, `status_filter` (`pending` default, `fired`, `canceled`, `all`), `limit` (default 100, max 1000), `cursor` | a node lists its own (another owner is `TIMER_FORBIDDEN`); `SY.admin@motherbee` lists any owner's, or the whole hive's; ordered by fire time, then uuid. A page ends at `limit` or at one 128 KiB frame, whichever comes first, and while timers follow it answers `next_cursor`, its last timer's place, which given back as `cursor` lists the next; the page that lists the rest has none. The frame's cut since 1.3 (FINDINGS A-151), the limit's since 1.6 (FINDINGS A-266; `listTimers` reads one timer past the limit, never listed): before, a page its limit cut read as the whole list. A page reads its timers from an index that holds them in the list's order, one per filter, from its cursor's place on, and stops at the first one it cannot take (`timerListQuery`, `takeTimerPage`; since 1.7, FINDINGS A-326): it reads what it lists and one more, whatever the hive holds. Before, a page of an owner's timers in one status read every timer of the hive in that status, every owner's, and sorted the owner's, and a page read 1001 timers where about 228 of a WF's fit its frame. Both SDKs' `List` follow the pages up to their own limit. Go's `ListPage` and Rust's `list_page` list one page with its `next_cursor`, Rust's `list` hands it back when it stops at its limit, and Go's `List` returns the timers only (FINDINGS A-278); a WF's reconcile reads every page of its pending timers |
| `TIMER_CANCEL` | `timer_uuid` or `client_ref` | the owner only (`TIMER_NOT_OWNER`); a `client_ref` with no pending timer answers `ok` with `found: false`; a fired one-shot gives `TIMER_ALREADY_FIRED` |
| `TIMER_RESCHEDULE` | `timer_uuid` or `client_ref`, and `new_fire_at_utc_ms` or `new_fire_in_ms` | the owner only; pending one-shots only (`TIMER_RECURRING_NOT_RESCHEDULABLE`, `TIMER_ALREADY_FIRED`) |
| `TIMER_PURGE_OWNER` | `owner_l2_name` | only `SY.orchestrator@<same hive>` (`TIMER_FORBIDDEN` otherwise); deletes every timer of that owner, any status, in one transaction: the pending ones first, whose uuids come back for the heap, then the history, counted without being read (`deleteTimersByOwner`). The orchestrator waits 3 s for it; its time grows with the owner's timers, its history most, and with the indexes: about 0.17 s for 7,500 pending among 18,000 on a Mac, 0.89 s for 10,000 pending and 20,000 fired among 100,000, with 1.7's 16 MiB page cache (1.13 s with SQLite's default, the operator's call of 2026-11-12, FINDINGS A-326; docs/sy-timer.md §7.7) |
| `TIMER_NOW` | none | anyone |
| `TIMER_NOW_IN` | `tz` | anyone |
| `TIMER_CONVERT` | `instant_utc_ms`, `to_tz` | anyone |
| `TIMER_PARSE` | `input`, `layout` (a Go time layout), `tz` | anyone |
| `TIMER_FORMAT` | `instant_utc_ms`, `layout`, `tz` | anyone |
| `TIMER_HELP` | none | anyone; returns the verb and error catalog (`respondTimerHelp`) |
| `NODE_STATUS_GET` | | answered `HEALTHY` (`BuildDefaultNodeStatusResponse`) |

Any other verb gets `TIMER_UNKNOWN_VERB`; a notice or a reply gets no answer: one the router made,
and from a node `HELLO`, `ANNOUNCE`, `WITHDRAW`, a message without a name, SY.vault's
`VAULT_SECRET_CHANGED`, any `*_RESPONSE`, a `NOT_TAKEN` or an agent's `<msg>.error`
(`isInfrastructureMessage`, above; the last two since 0.1.141, FINDINGS A-329). The vault's notice,
which SY.vault broadcasts to every node at each change of a secret and once for each secret at its
start, since 0.1.141 too: before, each was answered `TIMER_UNKNOWN_VERB`, which SY.vault drops
(FINDINGS A-325). SY.timer reads no secret. The router does not gate `TIMER_*` verbs: they are not in
`PROTECTED_SYSTEM_ACTIONS` (`src/router/system_policy.rs`).

Error codes: `TIMER_BELOW_MINIMUM`, `TIMER_ABOVE_MAXIMUM`, `TIMER_LIMIT_EXCEEDED`,
`TIMER_INVALID_PAYLOAD`, `TIMER_INVALID_CRON`,
`TIMER_INVALID_TIMEZONE`, `TIMER_FORBIDDEN`, `TIMER_NOT_FOUND`, `TIMER_NOT_OWNER`,
`TIMER_ALREADY_FIRED`, `TIMER_RECURRING_NOT_RESCHEDULABLE`, `TIMER_STORAGE_ERROR`,
`TIMER_UNKNOWN_VERB`, `TIMER_INTERNAL`. A refusal's code goes with its error (`timerError` in
`errors.go`: `timerErrorCode`, `timerStorageOrInternalCode`), never read from its message, which
quotes what the request sent (FINDINGS A-220): a junk `cron_spec` that repeats "invalid timezone"
in its fields is `TIMER_INVALID_CRON`, not `TIMER_INVALID_TIMEZONE` as it was.

### Teardown

When the orchestrator purges a node (`kill_node` with `purge_instance`, `remove_node_instance`),
it stops the node's unit first and clears its `relaunch_on_boot` (`clear_relaunch_on_boot`), then
sends `TIMER_PURGE_OWNER` for that node to the `SY.timer` of the node's hive, with a 3 s timeout,
and reports the result in its reply as `timer_purge` (`purge_owner_timers_before_teardown` in
`src/bin/sy_orchestrator.rs`). The purge of an owner's 10,000 pending and 20,000 fired timers among
the hive's 100,000 pending took 0.89 s on a Mac with 1.7's page cache, 1.13 s with SQLite's default
(`docs/sy-timer.md` §7.7). A purge that is not `ok` or gets no answer stops the teardown there
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

A refusal from SY.timer comes back with its code as `error_code` and its message as
`error_detail`, under the HTTP status of what it says (`error_code_to_http_status`, FINDINGS
A-251): `TIMER_INVALID_PAYLOAD` (a cursor that is not one, a malformed request) and
`TIMER_INVALID_TIMEZONE` 400, `TIMER_NOT_FOUND` 404, `TIMER_STORAGE_ERROR` 502,
`TIMER_UNKNOWN_VERB` 501 (an SY.timer older than the route) and `TIMER_INTERNAL` 500.

## Config

None beyond `/etc/fluxbee/hive.yaml` (`hive_id`). Paths are constants (`configDir`,
`routerSockDir`, `uuidPersistenceDir`, `nodeRootDir`). SY.timer has no `CONFIG_SET` and no vault
secret.

## Failure behavior

- No identity SHM entry within 30 s, no router connection, or a database that cannot be opened
  or replayed: the process exits and systemd restarts it every 5 s.
- If the router channel closes, the run loop ends (`sy.timer incoming channel closed`).
- A handler error is logged as a JSON event `timer_handler_error` (the request's `verb`, its sender
  `src_l2_name`, since 1.6, and its `trace_id`) plus `handle message error`: an answer over the
  frame, answered `TIMER_INTERNAL`, is one.
- A recurring timer whose next time cannot be computed, at a start
  (`timer_replay_invalid_recurring`) or at a fire (`timer_recurring_next_fire_failed`), is
  canceled with its `canceled_reason` and logged once (`cancelUnschedulable`); the fire due then
  is not sent. A row that cannot be saved stays `pending`: at a fire it is tried again in 30 s
  (`timer_fire_persist_failed`), at a start the start fails and systemd runs it again. One kept
  before 0.1.124 whose `cron_spec` starts with a zone is such a timer (FINDINGS A-206, A-220).
- A fire whose read of its timer fails sends nothing and is tried again in 30 s
  (`timer_fire_read_failed`, since 1.6).

## Health

- `systemctl status sy-timer`; `journalctl -u sy-timer`. Besides plain lines, the node logs one
  JSON object per event (`logEvent` in `logging.go`): `timer_startup`, `timer_scheduled`,
  `timer_fired` (one per attempt), `timer_canceled`, `timer_rescheduled`, `timer_owner_purged`
  (with `took_ms`, the delete's own time, since 1.7: the orchestrator waits 3 s for the purge),
  `timer_missed_policy_applied`, `timer_handler_error`, for a refused request
  `timer_request_refused` and `timer_request_refusals_not_logged`, for a message that is no request
  `timer_message_ignored` and `timer_messages_ignored_not_logged` (Messages, above), and for the
  delivery `timer_fire_read_failed`, `timer_fire_persist_failed`, `timer_fire_emit_failed`,
  `timer_fire_undelivered`, `timer_fire_refused`, `timer_fire_lost`, `timer_fire_dropped`; for a
  recurring timer canceled because its next fire cannot be computed,
  `timer_replay_invalid_recurring` and `timer_recurring_next_fire_failed` (`outcome: canceled`,
  with its `canceled_reason`).
- `curl -s 127.0.0.1:8080/hives/<hive>/timer/now` on the motherbee proves the admin → timer
  round trip; `.../timer/timers` shows the pending timers.

## Links

- [SY.wf-rules](sy-wf-rules.md): WF nodes schedule their timers on the `SY.timer` of their own
  hive, with `client_ref` `wf:<instance_id>::<timer_key>` (`timerClientRef` in
  `go/nodes/wf/wf-generic/node/timer.go`; target set in `node/config.go`).
- [SY.identity](sy-identity.md): the SHM SY.timer waits for at startup.
- [Operations](../reference/operations.md): node teardown.
