# SY.timer — System Node

**Status:** v1.1 implemented / aligned with current runtime
**Date:** 2026-04-13; checked against the code on 2026-10-27 (FINDINGS A-115)
**Audience:** Node developers, workflow authors, ops/SRE

The node as it runs, file by file: [nodes/sy-timer.md](nodes/sy-timer.md).

---

## 1. Summary and Positioning

`SY.timer` is a Fluxbee system node that provides two capabilities to the rest of the hive:

1. **Persistent timers** — one-shot and recurring, fired as an L2 event toward a target node.
2. **Authoritative source of time and timezone** — `now`, conversions, parse/format, server-side resolution of IANA timezones.

It is part of the family of system nodes, together with `SY.storage`, `SY.admin`, `SY.architect`, `SY.orchestrator`, `SY.config.routes`, `SY.identity`, `SY.cognition`, `SY.opa.rules`. It is always launched when the hive starts, it is kept up by systemd through the orchestrator, and it needs no configuration from the operator beyond its existence.

**One per hive.** There is one instance on the motherbee and one on each worker. Each instance is autonomous, does not talk to other `SY.timer` instances, and delegates clock synchronization to the operating system (NTP/chrony/systemd-timesyncd). Time consistency between hives is the operator's responsibility at the host level, not Fluxbee's.

**It is not a real-time system.** `SY.timer` is designed for workflows, cognition, agents and automation. The minimum granularity of a timer is **60 seconds**, the firing precision is *best-effort* with a tolerance in the order of seconds, and the expected load is up to ~10,000 concurrent timers per hive.

**Strict ownership by L2 name for mutations.** Each node can create, cancel and reschedule its own timers. Reading (`TIMER_GET`, `TIMER_LIST`) is open: `SY.timer` checks no reader, so any node that reaches it sees every timer, payload and metadata included (§12.1). The only exception for administrative mutation is `SY.orchestrator`, which can invoke `TIMER_PURGE_OWNER` as part of the teardown of a node. The effective identity of the requester is taken from `routing.src_l2_name`, stamped authoritatively by the router when it delivers the message. `SY.timer` does not resolve ownership through SHM or through `.uuid` files.

**Delivery is once, and nothing is capped** (FINDINGS A-115). A fire is sent once and never retried: a target that is down loses it (§7.8). There is no maximum delay, no cap on the timers of an owner or of the hive, and no size check of its own beyond the 128 KiB frame (§19).

---

## 2. Design Principles

- **Autonomy per hive.** One `SY.timer` per hive, with no cross synchronization. If a host goes down, only the timers local to that hive are affected.
- **Time delegated to the OS.** `SY.timer` does not implement NTP or external synchronization. It reads the OS clock and trusts that the operator keeps it synchronized.
- **Single temporal source of truth.** Any node that needs a consistent hive-wide `now()` asks `SY.timer` for it. There is no local cache, there are no local offsets: one roundtrip per call, with bounded retry in the SDK.
- **Mutation with strict ownership.** Create/cancel/reschedule keeps isolation by L2 owner. Reading is deliberately open for hive observability.
- **A single, explicit exception.** `SY.orchestrator` can purge all the timers of an owner when it deletes a node. It is the only way a timer can be touched by someone other than its creator.
- **Descriptive and extensible JSON.** Field names are self-explanatory, enums are readable strings, errors return human messages as well as codes. All requests accept a free `metadata` field that `SY.timer` persists without interpreting it, meant so that LLM clients can annotate extra context (reasons, correlations, human reminders) without breaking the schema.
- **SDK as the abstraction boundary.** Client node authors never build JSON by hand nor resolve L2 names. They call typed SDK functions and get native types (`time.Time`, structs). If the wire format changes, the SDK changes, not the nodes.
- **Determinism and replayability.** `now()` injected through `SY.timer` lets a node replay its execution deterministically, with fake responses in tests; a node that wants it asks `SY.timer.Now()` for business logic rather than the language's `time.Now()`. The WF engine does not: its CEL `now()` is the WF node's own clock (`docs/spec-systemnode-wf-v1_1.md` §8.4).

---

## 3. System Principle: Self-Description (`HELP`)

Fluxbee adopts as a design principle that **every node must be able to describe its own capabilities in a self-describing way**, reachable through a standard `HELP` operation.

The purpose is twofold:

1. **Discovery by LLM.** A cognitive agent can ask a node "what can you do?" without depending on external documentation.
2. **Human introspection.** A developer or operator can query a live node to see which operations it exposes, which request format they accept, and which errors they can return.

`SY.timer` is the **first node to implement this principle**, in a one-off way, given that it is the first system node with direct interaction from other nodes without going through `SY.admin`. The operation is called `TIMER_HELP` for consistency with the rest of its family of verbs, but it meets the general `HELP` contract.

### 1.1 Minimal visibility through `SY.admin` in v1

In v1, `SY.admin` exposes only a **read-only** and introspection surface for `SY.timer`:

- `timer_help`
- `timer_get`
- `timer_list`
- `timer_now`
- `timer_now_in`
- `timer_convert`
- `timer_parse`
- `timer_format`

The owner-bound mutation operations stay outside `SY.admin` and remain as direct use through the SDK:

- `TIMER_SCHEDULE`
- `TIMER_SCHEDULE_RECURRING`
- `TIMER_CANCEL`
- `TIMER_RESCHEDULE`

`TIMER_GET` and `TIMER_LIST` can indeed be exposed through `SY.admin` because reading is open in v1. The reason to keep scheduling and mutations outside admin is architectural: `SY.timer` authorizes those operations by the effective identity of the requester (`routing.src_l2_name`, stamped by the router), and `SY.admin` does not act as a transparent ownership proxy.

Future nodes with direct interaction (for example `WF.*`, invocable capabilities) must implement their own equivalent `HELP` operation. The generalization of the protocol, the common fields and the naming will be formalized in a separate document when the second or third case shows up.

**The generic verb exists since 0.1.86:** `NODE_HELP` ([agent-tools-v1](agent-tools-v1.md) §3.3), which `SY.admin` exposes as `node_help` (`GET /hives/{hive}/nodes/{node_name}/help`). `ai.generic` answers it with its tool catalog. `SY.timer` keeps `TIMER_HELP`, and may answer `NODE_HELP` later.

---

## 4. Internal Architecture

### 4.1 Stack

- **Language:** Go
- **Persistence:** embedded SQLite (`modernc.org/sqlite`, pure Go), WAL and a 5 s busy timeout on every connection (`timerDSN`)
- **Timezone data:** `time/tzdata` embedded in the binary, with a fallback to the OS `/usr/share/zoneinfo`
- **Cron parser:** `github.com/robfig/cron/v3` (standard spec, known by LLMs)
- **Internal scheduler:** binary heap ordered by `fire_at_utc`, protected by a mutex
- **Runtime:** standard Fluxbee Go SDK (`fluxbee-go-sdk`), the same socket read loop as the rest of the `SY.*` nodes

### 4.2 Components

```
┌─────────────────────────────────────────────────────────┐
│                       SY.timer                         │
│                                                         │
│  ┌───────────────┐   ┌────────────────────────────┐   │
│  │ L2 Dispatcher │   │      Timer Scheduler       │   │
│  │               │   │                            │   │
│  │ - gets msgs   │   │  ┌──────────────────────┐ │   │
│  │ - routes by   │   │  │ Heap in-memory       │ │   │
│  │   meta.msg    │◄──┤  │ sorted by fire_at    │ │   │
│  │ - invokes     │   │  └──────────────────────┘ │   │
│  │   handler     │   │                            │   │
│  └───────┬───────┘   │  ┌──────────────────────┐ │   │
│          │           │  │ Tick loop            │ │   │
│          │           │  │ - sleep until next   │ │   │
│          │           │  │ - fire → publish L2  │ │   │
│          │           │  │ - lock per timer     │ │   │
│          │           │  └──────────────────────┘ │   │
│          │           └────────────┬───────────────┘   │
│          │                        │                    │
│          ▼                        ▼                    │
│  ┌─────────────────────────────────────────────────┐  │
│  │              SQLite (timers.db)                 │  │
│  │  table: timers                                  │  │
│  │  indexes: owner, fire_at_utc, status            │  │
│  └─────────────────────────────────────────────────┘  │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

### 4.3 Scheduling loop

```
1. On boot (§11):
   - Open SQLite at /var/lib/fluxbee/nodes/SY/SY.timer@<hive>/timers.db
   - Delete the fired and canceled timers older than 7 days (once per start, §5.2)
   - Read ALL the timers with status = 'pending', by fire_at_utc
   - For each recurring timer:
       * Compute its next instance from now() and insert it into the heap;
         the instances missed while the node was down never fire (§10)
   - For each one-shot:
       * If fire_at_utc > now(): insert into the in-memory heap
       * Otherwise: apply missed_policy (fire | drop | fire_if_within);
         a fire is sent at once, before the node serves any request

2. Tick loop (sleeps at most one hour when the heap is empty; a new timer wakes it):
   loop {
       next = heap.peek()
       sleep_until(next.fire_at_utc)
       for timer in heap.pop_all_due(now()):
           with lock(timer.uuid):
               re-read the timer
               if timer.status != 'pending':
                   continue  // canceled
               if timer.fire_at_utc > now():
                   push back to heap; continue  // rescheduled later
               send TIMER_FIRED to timer.target, once (§7.8)
               if timer.is_recurring:
                   compute next fire_at from cron_spec, from now
                   update db, push back to heap
               else:
                   update db: status = 'fired'  // even if the send failed
   }
```

### 4.4 Concurrency

- One global mutex for the heap structure.
- One mutex per timer (a map of mutexes by uuid), taken only by the fire: it serializes the fires of one timer. `TIMER_CANCEL` and `TIMER_RESCHEDULE` do not take it, so one that lands while that timer's fire is in flight can be overwritten when the fire saves the row it read before sending.
- SQLite operations serialized by the driver (SQLite in WAL mode allows multiple readers + one writer).

---

## 5. Data Model

### 5.1 File path

```
/var/lib/fluxbee/nodes/SY/SY.timer@<hive>/timers.db
```

Together with the rest of the node's persistent state (`config.json`, `status_version`, etc.), following the canonical per-node directory convention of the system.

### 5.2 Schema

```sql
CREATE TABLE timers (
    uuid                TEXT PRIMARY KEY,
    owner_l2_name       TEXT NOT NULL,
    target_l2_name      TEXT NOT NULL,
    client_ref          TEXT,
    kind                TEXT NOT NULL CHECK(kind IN ('oneshot','recurring')),

    fire_at_utc         INTEGER NOT NULL,  -- unix ms, next fire
    cron_spec           TEXT,              -- only if kind='recurring'
    cron_tz             TEXT,              -- IANA timezone of the recurring cron

    missed_policy       TEXT NOT NULL DEFAULT 'fire'
                        CHECK(missed_policy IN ('fire','drop','fire_if_within')),
    missed_within_ms    INTEGER,           -- only if missed_policy='fire_if_within'

    payload             TEXT NOT NULL,     -- JSON blob, opaque to SY.timer
    metadata            TEXT,              -- free JSON blob, opaque

    status              TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending','fired','canceled')),

    created_at_utc      INTEGER NOT NULL,  -- unix ms, from the OS clock; kept even if rescheduled
    last_fired_at_utc   INTEGER,           -- last fire (recurring)
    fire_count          INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_timers_owner ON timers(owner_l2_name);
CREATE INDEX idx_timers_fire_at ON timers(fire_at_utc) WHERE status='pending';
CREATE INDEX idx_timers_status ON timers(status);
CREATE UNIQUE INDEX idx_timers_owner_clientref_pending
ON timers(owner_l2_name, client_ref)
WHERE client_ref IS NOT NULL AND status='pending';
```

**Notes:**
- `created_at_utc` is kept forever, even after reschedules. It serves traceability and `fire_if_within` decisions based on the age of the original timer.
- `cron_tz` is persisted explicitly so that the next recurring fire can be recomputed without losing the source timezone.
- History is deleted only when `SY.timer` starts: the `fired` and `canceled` timers whose last fire (their creation, for one that never fired) is older than 7 days (`gcHistoricalTimers`, `defaultHistoryRetention`). There is no periodic GC, and the 7 days are a constant: a node that runs for weeks keeps all its history until its next start.

---

## 6. Transport

### 6.1 Envelope

All messages follow the standard Fluxbee envelope:

```json
{
  "routing": {
    "src": "<client-node-uuid>",
    "dst": "SY.timer@<hive>",
    "ttl": 1,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "TIMER_SCHEDULE",
    "action": "TIMER_SCHEDULE"
  },
  "payload": { ... }
}
```

- Destination: always L2 unicast to `SY.timer@<hive-local>`. The SDK resolves the name automatically.
- `routing.ttl`: both SDKs send their requests with TTL 1, since `SY.timer` is on the sender's own hive. `TIMER_FIRED` goes out with TTL 16.
- `routing.src_l2_name`: stamped by the router when it delivers the message; `SY.timer` takes the requester from it (§12.2).
- `meta.type`: always `"system"`. That is the wire key; the SDKs call the field `msg_type` (Rust) and `MsgType` (Go).
- `meta.msg`: one of the verbs defined in section 7. Both SDKs also set `meta.action` to the verb.
- Response: always `meta.msg = "TIMER_RESPONSE"`, with the same `trace_id` as the request.
- Only requests are answered. A message the router made itself (it has no `routing.src_l2_name`: `UNREACHABLE`, `TTL_EXCEEDED`), `ECHO_REPLY` and any `*_RESPONSE` get no answer, so a `TIMER_FIRED` for a node that is not connected ends at the router's `UNREACHABLE`. An unknown verb from a node is answered `TIMER_INTERNAL`, and so is a request whose response does not fit in one frame (128 KiB).

### 6.2 Verbs

| Verb | Purpose |
|---|---|
| `TIMER_SCHEDULE` | Create a one-shot timer |
| `TIMER_SCHEDULE_RECURRING` | Create a recurring timer (cron) |
| `TIMER_CANCEL` | Cancel one of the requester's own timers by `timer_uuid` or `client_ref` |
| `TIMER_RESCHEDULE` | Change the `fire_at` of one of the requester's own pending timers by `timer_uuid` or `client_ref` |
| `TIMER_GET` | Get a timer by `timer_uuid`, or the requester's own active timer by `client_ref` |
| `TIMER_LIST` | List the timers of the hive, optionally filtered by owner |
| `TIMER_NOW` | Get the current instant in UTC |
| `TIMER_NOW_IN` | Get the current instant in a given timezone |
| `TIMER_CONVERT` | Convert an instant between timezones |
| `TIMER_PARSE` | Parse a string into an instant with a timezone |
| `TIMER_FORMAT` | Format an instant into a string with a timezone |
| `TIMER_PURGE_OWNER` | **SY.orchestrator only.** Delete all the timers of an owner |
| `TIMER_HELP` | Introspection of capabilities |
| `TIMER_RESPONSE` | Generic response to all requests |
| `TIMER_FIRED` | Event emitted by `SY.timer` to the target when a timer is due |

---

## 7. Timer Operations

### 7.1 `TIMER_SCHEDULE`

Creates a one-shot timer. If it comes with a `client_ref` and an active timer of the same owner with that reference already exists, the operation is idempotent and returns the existing timer.

**Request:**

```json
{
  "routing": { "dst": "SY.timer@<hive>", "ttl": 1, "trace_id": "..." },
  "meta": { "type": "system", "msg": "TIMER_SCHEDULE" },
  "payload": {
    "fire_at_utc_ms": 1775577600000,
    "target_l2_name": "WF.onboarding.step2@motherbee",
    "client_ref": "escalation_for_ticket_1234",
    "payload": {
      "ticket_id": 1234,
      "reason": "no_reply_timeout"
    },
    "missed_policy": "fire_if_within",
    "missed_within_ms": 600000,
    "metadata": {
      "created_by_reasoning": "user asked to remind in 10 min"
    }
  }
}
```

**Fields:**

| Field | Required | Description |
|---|---|---|
| `fire_at_utc_ms` | yes* | Absolute UTC instant in ms. Alternative to `fire_in_ms`. |
| `fire_in_ms` | yes* | Relative duration from `now()`. Alternative to `fire_at_utc_ms`. |
| `target_l2_name` | no | L2 name of the node to notify. Default: the requester itself, derived from `routing.src_l2_name`. A WF node takes a `TIMER_FIRED` only for a timer it scheduled itself (`owner_l2_name`, since 0.1.90): a timer another node schedules for it is dropped there. |
| `client_ref` | no | Optional identifier defined by the client. If it is reused on an active timer of the same owner, `TIMER_SCHEDULE` returns the existing timer instead of creating another one. |
| `payload` | no | Arbitrary JSON that will be included in `TIMER_FIRED.payload.user_payload`. |
| `missed_policy` | no | `"fire"` (default), `"drop"`, or `"fire_if_within"`. |
| `missed_within_ms` | conditional | Required if `missed_policy="fire_if_within"`. |
| `metadata` | no | Free JSON, opaque to `SY.timer`. |

*Exactly one of `fire_at_utc_ms` / `fire_in_ms`.

**Validation:**

- `fire_in_ms >= 60000` or, equivalently, `fire_at_utc_ms - now() >= 60000`. If it fails: error `TIMER_BELOW_MINIMUM`.
- There is no maximum: any instant at least 60 s ahead is taken.
- `target_l2_name` must be a canonical L2 name, `<kind>.<name>@<hive>` (`TIMER_INVALID_PAYLOAD`).
- `client_ref`: at most 255 characters.
- Fields the verb does not know, a `cron_spec` included, are ignored: a recurring timer is `TIMER_SCHEDULE_RECURRING`.

**Response OK:**

```json
{
  "meta": { "type": "system", "msg": "TIMER_RESPONSE" },
  "payload": {
    "ok": true,
    "verb": "TIMER_SCHEDULE",
    "timer_uuid": "9d8c6f4b-2f4d-4ad4-b5f4-8d8d9d9d0001",
    "fire_at_utc_ms": 1775577600000,
    "already_existed": false
  }
}
```

### 7.2 `TIMER_SCHEDULE_RECURRING`

Creates a recurring timer with standard cron syntax.

**Request:**

```json
{
  "meta": { "type": "system", "msg": "TIMER_SCHEDULE_RECURRING" },
  "payload": {
    "cron_spec": "0 9 * * MON",
    "cron_tz": "America/Argentina/Buenos_Aires",
    "target_l2_name": "AI.weekly_reporter@motherbee",
    "client_ref": "weekly_report_monday_9am",
    "payload": { "report_type": "weekly_summary" },
    "missed_policy": "drop",
    "metadata": { "note": "lunes 9am hora de BA" }
  }
}
```

**Additional fields:**

| Field | Required | Description |
|---|---|---|
| `cron_spec` | yes | Standard cron expression (5 fields: min hr dom mon dow). |
| `cron_tz` | no | IANA timezone to interpret the cron. Default: `UTC`. |

**Validation:**
- `cron_spec` must parse correctly. Error `TIMER_INVALID_CRON`.
- The interval between fires must be `>= 60s`. Error `TIMER_BELOW_MINIMUM`.
- `cron_tz` must be a valid IANA zone. Error `TIMER_INVALID_TIMEZONE`.
- `missed_policy` is validated and stored, but a recurring timer never applies it (§10).

**Response OK:** same as `TIMER_SCHEDULE`, with the `fire_at_utc_ms` of the computed **next** fire and `already_existed` saying whether the active timer already existed for that `client_ref`.

### 7.3 `TIMER_CANCEL`

```json
{
  "meta": { "msg": "TIMER_CANCEL" },
  "payload": { "timer_uuid": "9d8c6f4b-..." }
}
```

It can also be invoked with:

```json
{
  "meta": { "msg": "TIMER_CANCEL" },
  "payload": { "client_ref": "wf:instance-123::sla_timeout" }
}
```

If both fields come, `timer_uuid` wins.

Validation: the `owner_l2_name` of the timer must match `routing.src_l2_name` for mutation verbs. If not: `TIMER_NOT_OWNER`.

If the timer has already fired (`status = 'fired'`): `TIMER_ALREADY_FIRED`.
- By `timer_uuid`, if it does not exist: `TIMER_NOT_FOUND`.
- By `client_ref`, if there is no active timer of the requester: response `ok=true, found=false`.

**Response OK:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_CANCEL",
    "timer_uuid": "9d8c6f4b-...",
    "status": "canceled",
    "found": true
  }
}
```

### 7.4 `TIMER_RESCHEDULE`

Changes the `fire_at` of a pending one-shot timer.

```json
{
  "meta": { "msg": "TIMER_RESCHEDULE" },
  "payload": {
    "timer_uuid": "9d8c6f4b-...",
    "new_fire_at_utc_ms": 1775580000000
  }
}
```

Alternatively, `new_fire_in_ms` relative to `now()`.

It can also identify the timer by the `client_ref` of the active owner:

```json
{
  "meta": { "msg": "TIMER_RESCHEDULE" },
  "payload": {
    "client_ref": "wf:instance-123::sla_timeout",
    "new_fire_in_ms": 120000
  }
}
```

Validation: the same ownership check. It only applies to timers in `status='pending'`. For recurring timers, reschedule is not allowed in v1 (cancel and recreate). Error `TIMER_RECURRING_NOT_RESCHEDULABLE`.

The 60s minimum applies to the `new_fire_at` too.

### 7.5 `TIMER_GET`

```json
{
  "meta": { "msg": "TIMER_GET" },
  "payload": { "timer_uuid": "9d8c6f4b-..." }
}
```

The requester's own active timer can also be queried by `client_ref`:

```json
{
  "meta": { "msg": "TIMER_GET" },
  "payload": { "client_ref": "wf:instance-123::sla_timeout" }
}
```

**Response OK:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_GET",
    "timer": {
      "uuid": "9d8c6f4b-...",
      "owner_l2_name": "WF.onboarding@motherbee",
      "target_l2_name": "WF.onboarding@motherbee",
      "client_ref": "escalation_for_ticket_1234",
      "kind": "oneshot",
      "fire_at_utc_ms": 1775577600000,
      "missed_policy": "fire_if_within",
      "missed_within_ms": 600000,
      "status": "pending",
      "created_at_utc_ms": 1775574000000,
      "fire_count": 0,
      "payload": { "ticket_id": 1234 },
      "metadata": { "created_by_reasoning": "..." }
    }
  }
}
```

A field without a value (`client_ref`, `cron_spec`, `cron_tz`, `missed_within_ms`, `last_fired_at_utc_ms`, `metadata`) is left out.

Read by `timer_uuid`: open to any node, payload and metadata included (§12.1). It does not require ownership by the requester.

Read by `client_ref`: owner-scoped. `SY.timer` searches only within `(routing.src_l2_name, client_ref)` and returns `TIMER_NOT_FOUND` if there is no active timer of the requester with that reference.

### 7.6 `TIMER_LIST`

Lists the timers of the local hive. No parameters = all the `pending` ones. It can be filtered by `owner_l2_name`.

```json
{
  "meta": { "msg": "TIMER_LIST" },
  "payload": {
    "owner_l2_name": "WF.onboarding@motherbee",
    "status_filter": "pending",
    "limit": 100
  }
}
```

| Field | Description |
|---|---|
| `owner_l2_name` | Optional filter by canonical L2 owner. |
| `status_filter` | `"pending"`, `"fired"`, `"canceled"`, or `"all"` (default `"pending"`). |
| `limit` | Maximum number of results. Default 100, maximum 1000. |

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_LIST",
    "owner_l2_name": "WF.onboarding@motherbee",
    "count": 3,
    "timers": [ { ... }, { ... }, { ... } ]
  }
}
```

`owner_l2_name` is optional (the response echoes it, `null` when omitted). If it is omitted, `SY.timer` lists the full set of the hive, subject to `status_filter` and `limit`, by `fire_at` and to any node that asks (§12.1).

### 7.7 `TIMER_PURGE_OWNER` (exclusive use of `SY.orchestrator`)

Deletes **all the timers** of a given `owner_l2_name`.

```json
{
  "meta": { "msg": "TIMER_PURGE_OWNER" },
  "payload": {
    "owner_l2_name": "WF.onboarding.step2@motherbee"
  }
}
```

**Authorization:** `SY.timer` checks that the router-stamped `routing.src_l2_name` is `SY.orchestrator@<its own hive>`. Any other origin: error `TIMER_FORBIDDEN`.

**Semantics:**
- Idempotent deletion: if there are no timers, it answers `ok` with `deleted_count: 0`.
- It deletes both `pending` and historical timers (`fired`, `canceled`) — consistent with the spirit of "the node no longer exists".
- Silent: it does not emit `TIMER_FIRED` for the deleted pending timers; they are considered canceled.

**Response:** the count only; the owner is not repeated.
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_PURGE_OWNER",
    "deleted_count": 7
  }
}
```

### 7.8 `TIMER_FIRED` event

Emitted by `SY.timer` toward the `target_l2_name` when a timer is due.

```json
{
  "routing": {
    "src": "<uuid of SY.timer@<hive>>",
    "dst": "WF.onboarding.step2@motherbee",
    "ttl": 16,
    "trace_id": "<new-uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "TIMER_FIRED"
  },
  "payload": {
    "timer_uuid": "9d8c6f4b-...",
    "owner_l2_name": "WF.onboarding.step2@motherbee",
    "client_ref": "escalation_for_ticket_1234",
    "kind": "oneshot",
    "scheduled_fire_at_utc_ms": 1775577600000,
    "actual_fire_at_utc_ms": 1775577600123,
    "fire_count": 1,
    "is_last_fire": true,
    "user_payload": {
      "ticket_id": 1234,
      "reason": "no_reply_timeout"
    },
    "metadata": {
      "created_by_reasoning": "..."
    }
  }
}
```

**Design notes:**
- `trace_id` is new; it does not reuse the one of the original `TIMER_SCHEDULE`. This lets the fire be traced as an independent event.
- `scheduled_fire_at_utc_ms` vs `actual_fire_at_utc_ms` lets the consumer measure the real jitter.
- In v1, `actual_fire_at_utc_ms` is captured right before the attempt to send the event, not after confirming a successful socket write.
- `is_last_fire` is `true` for a one-shot; for recurring timers it is reserved for a termination criterion (in v1 always `false` for recurring timers; reserved for v2).
- `user_payload` is exactly what the client passed in `TIMER_SCHEDULE.payload.payload`, unmodified (`{}` when there was none).
- `metadata` is also returned as is, so that the consumer can have the context the creator left. `client_ref` and `metadata` are left out when the timer has none.
- The target must NOT answer with `TIMER_RESPONSE`. `TIMER_FIRED` is fire-and-forget; if the target is down, the event is lost. If a retry is needed, it is the timer creator's responsibility to design that semantics in its application logic.
- `SY.timer` considers the fire attempted when it hands the message to the local router; it does not wait for an application ack or for confirmation from the target.
- In v1 there is no dead-letter queue nor automatic retry of `TIMER_FIRED`. Durability still lives in the timer row and in the node logs.

**Delivery, as the code does it** (`fireTimerByUUID`, `emitTimerFired` in `scheduler.go`; FINDINGS A-115):
- **Once.** The fire is sent once. A target that is not connected makes the router answer `UNREACHABLE`, which `SY.timer` ignores (§6.1): the fire is lost, and a one-shot is `fired` all the same. A send that fails (logged `timer_fire_emit_failed`), say a `TIMER_FIRED` whose payload and metadata do not fit in the 128 KiB frame, is lost the same way.
- **Overdue at start.** When `SY.timer` starts, a one-shot whose time passed fires at once, under `missed_policy` `fire` or `fire_if_within` (§10), before the node serves requests. After a hive reboot that is before the hive's managed nodes are relaunched, so a fire for one of them finds no target.
- **Twice, rarely.** A one-shot is saved as `fired` after its send: if `SY.timer` stops between the two, it is still `pending` at the next start and fires again. A consumer that must not act twice keeps its own record (for example by `timer_uuid`).

---

## 8. Time Operations

### 8.1 `TIMER_NOW`

```json
{ "meta": { "msg": "TIMER_NOW" }, "payload": {} }
```

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_NOW",
    "now_utc_ms": 1775577600123,
    "now_utc_iso": "2026-04-08T15:20:00.123Z"
  }
}
```

### 8.2 `TIMER_NOW_IN`

```json
{
  "meta": { "msg": "TIMER_NOW_IN" },
  "payload": { "tz": "America/Argentina/Buenos_Aires" }
}
```

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_NOW_IN",
    "now_utc_ms": 1775577600123,
    "tz": "America/Argentina/Buenos_Aires",
    "now_local_iso": "2026-04-08T12:20:00.123-03:00",
    "tz_offset_seconds": -10800,
    "tz_abbrev": "-03"
  }
}
```

### 8.3 `TIMER_CONVERT`

Converts an instant into the local representation of a timezone.

```json
{
  "meta": { "msg": "TIMER_CONVERT" },
  "payload": {
    "instant_utc_ms": 1775577600000,
    "to_tz": "Europe/Madrid"
  }
}
```

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_CONVERT",
    "instant_utc_ms": 1775577600000,
    "to_tz": "Europe/Madrid",
    "local_iso": "2026-04-08T17:20:00.000+02:00",
    "tz_offset_seconds": 7200
  }
}
```

### 8.4 `TIMER_PARSE`

```json
{
  "meta": { "msg": "TIMER_PARSE" },
  "payload": {
    "input": "2026-04-08 17:20:00",
    "layout": "2006-01-02 15:04:05",
    "tz": "Europe/Madrid"
  }
}
```

It uses the Go layout format (`2006-01-02 15:04:05`). The SDK can provide helpers with known layouts (ISO8601, RFC3339).

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_PARSE",
    "instant_utc_ms": 1775575200000,
    "resolved_iso_utc": "2026-04-08T15:20:00.000Z"
  }
}
```

### 8.5 `TIMER_FORMAT`

```json
{
  "meta": { "msg": "TIMER_FORMAT" },
  "payload": {
    "instant_utc_ms": 1775577600000,
    "layout": "Mon 02 Jan 2006 15:04 MST",
    "tz": "America/Argentina/Buenos_Aires"
  }
}
```

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_FORMAT",
    "formatted": "Wed 08 Apr 2026 12:20 -03"
  }
}
```

### 8.6 Note on the roundtrip cost

All time operations do an L2 roundtrip to `SY.timer`. The SDKs make up to 3 attempts, which wait 100 ms, 300 ms and 1 s for the answer, and fail loudly if none comes (§14.4). **There is no local cache nor offset synchronization.** If a node needs to measure fine durations, it must use the process's local monotonic clock, not `TIMER_NOW`. The rule: *authoritative real-world time through `SY.timer`, local monotonic measurements with the process clock*.

---

## 9. `TIMER_HELP`

Returns a complete descriptor of the node's capabilities, meant for both human and LLM consumption.

```json
{ "meta": { "msg": "TIMER_HELP" }, "payload": {} }
```

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_HELP",
    "node_family": "SY",
    "node_kind": "SY.timer",
    "version": "1.1",
    "description": "System node providing persistent timers and authoritative time/timezone services.",
    "operations": [
      {
        "verb": "TIMER_SCHEDULE",
        "description": "Create a one-shot timer. If client_ref already matches an active owned timer, return the existing timer instead of creating a duplicate.",
        "required_fields": ["fire_at_utc_ms OR fire_in_ms"],
        "optional_fields": ["target_l2_name", "client_ref", "payload", "missed_policy", "missed_within_ms", "metadata"]
      },
      { "...": "..." }
    ],
    "error_codes": [
      { "code": "TIMER_BELOW_MINIMUM", "description": "Requested timer duration is below the 60 second minimum." },
      { "...": "..." }
    ]
  }
}
```

`TIMER_HELP` is a list written by hand in `respondTimerHelp` (`main.go`), not derived from the handlers, so it can drift from them. Its operations carry no constraints and no examples. Its error list omits `TIMER_STORAGE_ERROR`, which the timer verbs answer when the database fails (§15).

---

## 10. Missed Policies Semantics

When `SY.timer` starts after a downtime, it evaluates each pending one-shot against `now()` (`replayPending`, `handleMissedTimer`):

| Policy | Overdue one-shot | Not yet due one-shot |
|---|---|---|
| `fire` | It fires immediately, in order of `fire_at`, before the node serves requests (§7.8). | It is rescheduled with the original `fire_at`. |
| `drop` | It is canceled (`status = 'canceled'`), with a `timer_missed_policy_applied` event. | It is rescheduled normally. |
| `fire_if_within` | If `now() - fire_at_utc <= missed_within_ms`, it fires. If not, it is canceled. | It is rescheduled normally. |

A one-shot whose policy is unknown, or `fire_if_within` without its window, is canceled.

For **recurring timers**, `missed_policy` is stored but never applied:
- The **next instance** is computed from `now()` onward according to `cron_spec`.
- Every instance that should have fired during the downtime, the one that was due when it went down included, **does not fire**, whatever the `missed_policy`. This avoids firing storms when coming back from long downtimes.

---

## 11. Restart Semantics — Detail

At node startup:

1. Open `timers.db`. If it does not exist, create the schema.
2. Delete the history older than 7 days, once (`gcHistoricalTimers`): `DELETE FROM timers WHERE status IN ('fired','canceled') AND COALESCE(last_fired_at_utc, created_at_utc) < now - 7 days`. Nothing deletes history again until the next start.
3. `now = time.Now().UTC()`; `SELECT * FROM timers WHERE status='pending' ORDER BY fire_at_utc ASC`.
4. For each timer:
   - Recurring: recompute `fire_at_utc` from `now` with `cron_spec` and `cron_tz`, update the DB, push to the heap (§10). One whose next fire cannot be computed is skipped and stays `pending`.
   - One-shot with `fire_at_utc > now`: insert it into the in-memory heap with the same `fire_at_utc`. **The `fire_at_utc` is not modified** — it is already the absolute target instant, and the heap fires it when the OS clock reaches that value.
   - One-shot with `fire_at_utc <= now`: apply `missed_policy` (§10). If it fires: send `TIMER_FIRED`, then save `status='fired'` and `fire_count`. If it is dropped: save `status='canceled'` and log a `timer_missed_policy_applied` event whose `outcome` says why.
5. Log `SY.timer ready: ... pending=N gc_deleted=M`, start the tick loop, and serve requests.

**Guarantee:** a timer created with `fire_at_utc = T` will fire at time `T` of the OS clock, subject to the node's availability and to its `missed_policy`. There is no accumulated drift.

---

## 12. Authorization and Ownership

### 12.1 General rule

For `TIMER_CANCEL`, `TIMER_RESCHEDULE`:
```
if timer.owner_l2_name != routing.src_l2_name:
    return error TIMER_NOT_OWNER
```

For `TIMER_GET`:
```
read timer by uuid without ownership restriction
```

For `TIMER_LIST`:
```
query: optional WHERE owner_l2_name = <owner_l2_name>
```

**Reads are open** (FINDINGS A-115). `TIMER_GET` by uuid and `TIMER_LIST` answer any node, every timer of the hive with its `payload` and `metadata`: `SY.timer` checks no reader, and the router gates no `TIMER_*` verb (they are not in `PROTECTED_SYSTEM_ACTIONS`). SY.admin's `timer_get` and `timer_list` show the same to the operator and to Archi. Nothing secret belongs in a timer's payload.

### 12.2 The single exception

For `TIMER_PURGE_OWNER`:
```
if routing.src_l2_name != "SY.orchestrator@<local-hive>":
    return error TIMER_FORBIDDEN
```

The check is done by the L2 name of the sender: `routing.src_l2_name`, which the router stamps on every message a node sends and the sender cannot set (`resolveSourceL2NameFromMsg`). `SY.timer` reads no router SHM and no `.uuid` file. A message without the stamp is one the router made itself (`UNREACHABLE`, `TTL_EXCEEDED`): it gets no answer, only a log line (`isInfrastructureMessage`, §6.1). The owner of a new timer is taken the same way. This is consistent with the authorization model of the rest of the system nodes.

### 12.3 Node deletion flow

```
SY.admin: kill_node with purge_instance, or remove_node_instance
    │
    ├─► SY.orchestrator of the node's hive (a spoke's gets it forwarded)
    │       │
    │       ├─► stop the node's unit (remove_node_instance: the node must be down already)
    │       │   and clear its relaunch_on_boot, so no reconcile launches it again
    │       │
    │       ├─► SY.timer of the same hive: TIMER_PURGE_OWNER { owner_l2_name: "WF.foo@hive" }, 3 s
    │       │       └─► TIMER_RESPONSE { ok: true, deleted_count: N }
    │       │       not ok, or no answer: the teardown stops here, TIMER_PURGE_FAILED
    │       │
    │       ├─► remove /var/lib/fluxbee/nodes/.../, the node's UUID file and its ILK mapping
    │       └─► SY.admin teardown_node: its identity and its vault secrets
    │
    └─◄ the answer, with timer_purge { status, deleted_count }
```

The orchestrator stops the node **first** and purges its timers after (`purge_owner_timers_before_teardown` in `src/bin/sy_orchestrator.rs`): a fire that lands in between finds no target and is lost, which is harmless for a node on its way out. If `SY.timer` does not answer `ok` within 3 s, the teardown does not go ahead: the answer is `TIMER_PURGE_FAILED`, the node stays stopped with its files and its identity, and running the purge again finishes it. It stays stopped because the stop cleared its `relaunch_on_boot` before the timer purge (the answer reports `relaunch_on_boot: false`), so the orchestrator's 60 s reconcile does not launch it again (FINDINGS A-121).

### 12.4 Schema migration v1.0 → v1.1

The v1.1 extension is **fully backward compatible**. No existing columns or verbs are removed or modified. The only schema change is the addition of a partial unique index:

```sql
CREATE UNIQUE INDEX idx_timers_owner_clientref_pending
ON timers(owner_l2_name, client_ref)
WHERE client_ref IS NOT NULL AND status = 'pending';
```

This index is created automatically at startup if `PRAGMA user_version` is lower than 2. v1.0 clients that do not pass `client_ref` keep working without any change.

**Recommended pre-check before deploying v1.1 on an existing DB:**

```sql
SELECT owner_l2_name, client_ref, COUNT(*)
FROM timers
WHERE client_ref IS NOT NULL AND status = 'pending'
GROUP BY owner_l2_name, client_ref
HAVING COUNT(*) > 1;
```

If this query returns no rows, the migration proceeds without friction. If it returns rows, there are active `client_ref` duplicates that should not exist in normal v1.0 use (where `client_ref` was only decorative metadata); review them manually before continuing. The node fails with an explicit error if the index cannot be created.

---

## 13. Supported Cron Syntax

`SY.timer` uses standard 5-field cron: `minute hour day-of-month month day-of-week`.

| Field | Range | Special |
|---|---|---|
| minute | 0-59 | `*`, `,`, `-`, `/` |
| hour | 0-23 | same |
| day of month | 1-31 | same, `?` |
| month | 1-12 or JAN-DEC | same |
| day of week | 0-6 or SUN-SAT | same |

**Common examples:**

| Cron | Meaning |
|---|---|
| `*/5 * * * *` | Every 5 minutes |
| `0 * * * *` | Every hour on the hour |
| `0 9 * * MON` | Mondays at 9:00 |
| `0 0 1 * *` | First day of the month at midnight |
| `30 14 * * 1-5` | Monday to Friday at 14:30 |

**Not supported in v1:**
- Seconds fields (6-field cron)
- Special descriptors like `@reboot`, `@yearly`
- Interval expressions like `every 5m` (reserved for v2 if demand shows up)

---

## 14. SDKs

Both SDKs wrap the interaction with `SY.timer` in a typed `TimerClient`: the author of the client node never builds the JSON. Both send their requests to `SY.timer@<own hive>` with TTL 1 (§6.1), check the 60 s minimum before sending (the server checks it again), and retry only the time operations (§14.4).

### 14.1 Go: recommended direct-use pattern

For `WF.*` and other Go nodes (`go/fluxbee-go-sdk/timer*.go`), the recommended pattern is:

1. connect to the router with `fluxbee-go-sdk` (`ConnectWithRetry`, which gives the node's `RouterDispatcher`)
2. build a `TimerClient` on top of that dispatcher (`NewTimerClient`)
3. use the client's typed methods for time, scheduling and reading
4. process `TIMER_FIRED` as a normal `system` message in the node's handler (`ParseFiredEvent`)

In v1 this is the primary integration path. `SY.admin` adds read-only visibility and time utilities, but the owner-bound semantics of scheduling and mutations remain direct SDK use.

**Transport note:** the envelope examples in this spec illustrate the shape; the canonical contract for SDK clients is also fixed by the SDK's golden tests.

Minimal example, as `wf-generic` does it (`go/nodes/wf/wf-generic/node/`):

```go
dispatcher, err := sdk.ConnectWithRetry(sdk.NodeConfig{
    Name:               "WF.demo",
    RouterSocket:       "/var/run/fluxbee/routers",
    UUIDPersistenceDir: "/var/lib/fluxbee/state/nodes",
    UUIDMode:           sdk.NodeUuidPersistent,
    ConfigDir:          "/etc/fluxbee",
    Version:            "1.0",
}, time.Second, profile) // profile: the node's OperationalRouteProfile
if err != nil {
    return err
}

timers, err := sdk.NewTimerClient(dispatcher, sdk.TimerClientConfig{})
if err != nil {
    return err
}

timerID, err := timers.ScheduleIn(ctx, 2*time.Minute, sdk.ScheduleOptions{
    ClientRef: "wf-demo-timeout",
    Payload: map[string]any{
        "kind": "timeout",
    },
})
if err != nil {
    return err
}
_ = timerID
```

### 14.2 Go: main signatures

```go
package sdk

const MinTimerDuration = time.Minute
const MaxTimerListLimit = 1000

type TimerClientConfig struct {
    TimerNode         string          // default: SY.timer@<the node's hive>
    TimeRetrySchedule []time.Duration // default: 100 ms, 300 ms, 1 s (§14.4)
}

func NewTimerClient(dispatcher *RouterDispatcher, cfg TimerClientConfig) (*TimerClient, error)

// --- Time ---
func (c *TimerClient) Now(ctx context.Context) (*TimerNowResult, error)
func (c *TimerClient) NowIn(ctx context.Context, tz string) (*TimerNowInResult, error)
func (c *TimerClient) Convert(ctx context.Context, instantUTCMS int64, toTZ string) (*TimerConvertResult, error)
func (c *TimerClient) Parse(ctx context.Context, input, layout, tz string) (*TimerParseResult, error)
func (c *TimerClient) Format(ctx context.Context, instantUTCMS int64, layout, tz string) (*TimerFormatResult, error)

// --- One-shot timers ---
type ScheduleOptions struct {
    TargetL2Name   string       // optional, default: the requester
    ClientRef      string       // optional
    Payload        any          // optional, serializable to JSON
    MissedPolicy   MissedPolicy // default: MissedPolicyFire
    MissedWithinMS *int64       // required if MissedPolicy == MissedPolicyFireIfWithin
    Metadata       any          // optional
}

func (c *TimerClient) Schedule(ctx context.Context, fireAt time.Time, opts ScheduleOptions) (TimerID, error)
func (c *TimerClient) ScheduleIn(ctx context.Context, d time.Duration, opts ScheduleOptions) (TimerID, error)

// --- Recurring timers ---
type RecurringOptions struct {
    CronSpec       string
    CronTZ         string // default: "UTC"
    TargetL2Name   string
    ClientRef      string
    Payload        any
    MissedPolicy   MissedPolicy // stored, never applied (§10)
    MissedWithinMS *int64
    Metadata       any
}

func (c *TimerClient) ScheduleRecurring(ctx context.Context, opts RecurringOptions) (TimerID, error)

// --- Management ---
func (c *TimerClient) Cancel(ctx context.Context, id TimerID) error
func (c *TimerClient) CancelByClientRef(ctx context.Context, clientRef string) error
func (c *TimerClient) Reschedule(ctx context.Context, id TimerID, newFireAt time.Time) error
func (c *TimerClient) RescheduleByClientRef(ctx context.Context, clientRef string, newFireAt time.Time) error
func (c *TimerClient) Get(ctx context.Context, id TimerID) (*TimerInfo, error)
func (c *TimerClient) GetByClientRef(ctx context.Context, clientRef string) (*TimerInfo, error)
func (c *TimerClient) List(ctx context.Context, filter ListFilter) ([]TimerInfo, error)
func (c *TimerClient) ListMine(ctx context.Context, filter ListFilter) ([]TimerInfo, error)

// --- Introspection ---
func (c *TimerClient) Help(ctx context.Context) (*HelpDescriptor, error)

// --- TIMER_FIRED ---
func ParseFiredEvent(msg Message) (*FiredEvent, error)
```

The Go client has no `TIMER_PURGE_OWNER`: only `SY.orchestrator` sends it (§7.7).

### 14.3 Rust

`fluxbee_sdk::timer` (`crates/fluxbee_sdk/src/timer.rs`) has the same surface: a `TimerClient` built on the node's `RouterDispatcher` with `TimerClient::new_with_dispatcher`, with the time helpers (`now`, `now_in`, `convert`, `parse`, `format`, `help`), scheduling (`schedule`, `schedule_in`, `schedule_recurring`), reading (`get`, `get_by_client_ref`, `list`, `list_mine`), `cancel`, `reschedule` and their `_by_client_ref` forms, and `purge_owner`. `parse_timer_response` and `parse_timer_fired_event` read `TIMER_RESPONSE` and `TIMER_FIRED` (`FiredEvent`). `ai.generic` asks `TIMER_NOW_IN` through it for its `get_current_time` tool ([agent-tools-v1](agent-tools-v1.md) §6.1).

### 14.4 Retry of time operations

The time operations (`Now`, `NowIn`, `Convert`, `Parse`, `Format`; in Rust `now`, `now_in`, `convert`, `parse`, `format`) make up to three attempts, each waiting for its answer 100 ms, 300 ms and 1 s in turn (`TimeRetrySchedule`, `time_retry_schedule`), about 1.4 s in all. When none answers, the Go client fails with `TIMER_UNREACHABLE`, and the Rust client with its last attempt's error (a timeout, or the router's `UNREACHABLE` or `TTL_EXCEEDED`). An answer with an error is not retried. Scheduling, reading and mutations are sent once, with the call's own timeout (5 s by default).

### 14.5 Handling `TIMER_FIRED` in the client node

`TIMER_FIRED` arrives as a `system` message on the node's own handler; the SDK parses it:

```go
type FiredEvent struct {
    TimerUUID            string          `json:"timer_uuid"`
    OwnerL2Name          string          `json:"owner_l2_name"`
    ClientRef            *string         `json:"client_ref,omitempty"`
    Kind                 string          `json:"kind"`
    ScheduledFireAtUTCMS int64           `json:"scheduled_fire_at_utc_ms"`
    ActualFireAtUTCMS    int64           `json:"actual_fire_at_utc_ms"`
    FireCount            int64           `json:"fire_count"`
    IsLastFire           bool            `json:"is_last_fire"`
    UserPayload          json.RawMessage `json:"user_payload,omitempty"`
    Metadata             json.RawMessage `json:"metadata,omitempty"`
}

// In the node's handler, for a system message whose meta.msg is TIMER_FIRED:
evt, err := sdk.ParseFiredEvent(msg)
if err != nil {
    return err
}
// application logic
```

A node should check that a `TIMER_FIRED` comes from `SY.timer@<its hive>` (the router-stamped `routing.src_l2_name`) before it acts on one.

---

## 15. Error Codes

| Code | Description |
|---|---|
| `TIMER_BELOW_MINIMUM` | Requested duration lower than 60 seconds. |
| `TIMER_INVALID_CRON` | `cron_spec` does not parse correctly. |
| `TIMER_INVALID_TIMEZONE` | `tz` / `cron_tz` is not a valid IANA zone. |
| `TIMER_INVALID_PAYLOAD` | The request `payload` is not valid JSON or required fields are missing. |
| `TIMER_NOT_FOUND` | No timer has that `timer_uuid`; or, by `client_ref` (`TIMER_GET`, `TIMER_RESCHEDULE`), the requester has no pending timer with it. |
| `TIMER_NOT_OWNER` | The requester is not the owner of the timer, for mutation verbs (`TIMER_CANCEL`, `TIMER_RESCHEDULE`). |
| `TIMER_ALREADY_FIRED` | A fired one-shot cannot be canceled; a timer that is not pending (fired or canceled) cannot be rescheduled. |
| `TIMER_RECURRING_NOT_RESCHEDULABLE` | Reschedule is not allowed on recurring timers in v1. |
| `TIMER_FORBIDDEN` | Restricted operation (e.g. `TIMER_PURGE_OWNER` from a non-orchestrator node). |
| `TIMER_STORAGE_ERROR` | Internal SQLite error. Not in `TIMER_HELP`'s list (§9). |
| `TIMER_INTERNAL` | An unknown verb, a response that does not fit in one 128 KiB frame, or another internal error. A message without the router's `routing.src_l2_name` gets no answer at all (§6.1). |
| `TIMER_UNREACHABLE` | (Go client) `SY.timer` does not answer after the retries of a time operation (§14.4). |

Format of an error response:

```json
{
  "meta": { "type": "system", "msg": "TIMER_RESPONSE" },
  "payload": {
    "ok": false,
    "verb": "TIMER_SCHEDULE",
    "error": {
      "code": "TIMER_BELOW_MINIMUM",
      "message": "minimum timer duration is 60 seconds"
    }
  }
}
```

---

## 16. Operation

### 16.1 Systemd

`SY.timer` is a core singleton per hive, launched and kept up by `SY.orchestrator` through the same operational model as the rest of the system's `SY.*` nodes. It has an installed core binary, a copy in `dist/core/bin`, a systemd unit file and supervision/restart by the orchestrator.

Binary: `/usr/bin/sy-timer` (installed by the `.deb` on the motherbee; `add_hive` and the core
update copy it to the spokes).
Distribution copy: `/var/lib/fluxbee/dist/core/bin/sy-timer`.
Unit: `/lib/systemd/system/sy-timer.service` on the motherbee (the `.deb`),
`/etc/systemd/system/sy-timer.service` on a spoke (written by the orchestrator).

### 16.2 Runtime paths

| Path | Purpose |
|---|---|
| `/var/lib/fluxbee/nodes/SY/SY.timer@<hive>/timers.db` | SQLite with the persisted timers |
| `/var/lib/fluxbee/nodes/SY/SY.timer@<hive>/config.json` | Config managed by the orchestrator |
| `/var/lib/fluxbee/nodes/SY/SY.timer@<hive>/state.json` | Optional node state |
| `/var/run/fluxbee/routers/` | Socket for communication with the local router |

### 16.3 Backup

The `timers.db` database should be included in the backup snapshots of the `/var/lib/fluxbee/nodes/` directory. SQLite supports hot backup with `.backup` or with `VACUUM INTO`. For v1 no specific tool is provided: a backup of the directory with `SY.timer` stopped, or with SQLite in WAL mode, works.

### 16.4 Logs

`SY.timer` logs on stdout (captured by systemd), with no levels: plain lines, and one JSON object per event (`logEvent`). Among the events: `timer_startup` (with `pending_count` and `gc_deleted`), `timer_scheduled`, `timer_fired`, `timer_canceled`, `timer_rescheduled`, `timer_owner_purged`, `timer_missed_policy_applied` (with its `outcome`), `timer_fire_emit_failed`, `timer_handler_error`, and for recurring timers `timer_replay_invalid_recurring`, `timer_recurring_next_fire_failed` and `timer_recurring_persist_failed`. The router's `UNREACHABLE` for a fire is only logged as an ignored message (§7.8).

### 16.5 Metrics (future)

Reserved for v2. Candidates: `timers_pending_gauge`, `timers_fired_counter`, `timer_fire_latency_histogram`, `timer_requests_counter` per verb.

---

## 17. Expected Use Cases

The Go snippets use the client `timers` of §14.1.

### 17.1 WF — Step timeout

A workflow schedules a timer when it enters a state that waits for an external response:

```go
timerID, _ := timers.ScheduleIn(ctx, 30*time.Minute, sdk.ScheduleOptions{
    ClientRef:    fmt.Sprintf("wf_%s_step_%s_timeout", wfInstanceID, stepName),
    Payload:      map[string]any{"wf_instance": wfInstanceID, "step": stepName, "reason": "step_timeout"},
    MissedPolicy: sdk.MissedPolicyFire,
})
```

When the WF gets the expected response, it cancels the timer. If not, it gets `TIMER_FIRED` and transitions to an escalation state.

### 17.2 Cognition — Deferred reconsideration

A cognitive node can schedule a reminder to review a conclusion with fresh data:

```go
timers.ScheduleIn(ctx, 24*time.Hour, sdk.ScheduleOptions{
    ClientRef:    "reconsider_hypothesis_42",
    Payload:      map[string]any{"hypothesis_id": 42, "action": "reconsider"},
    MissedPolicy: sdk.MissedPolicyDrop,
    Metadata: map[string]any{
        "reasoning": "low confidence conclusion, may benefit from fresh data",
    },
})
```

### 17.3 Recurring reports

```go
timers.ScheduleRecurring(ctx, sdk.RecurringOptions{
    CronSpec:  "0 9 * * MON",
    CronTZ:    "America/Argentina/Buenos_Aires",
    ClientRef: "weekly_summary",
    Payload:   map[string]any{"report_type": "weekly"},
})
```

A Monday missed while `SY.timer` was down is skipped (§10).

### 17.4 Authoritative time for AI nodes

```go
now, _ := timers.Now(ctx)
prompt := fmt.Sprintf("Current date and time: %s\nUser query: %s", now.NowUTCISO, userQuery)
```

`ai.generic` does this through the Rust client: its `get_current_time` tool asks `TIMER_NOW_IN` (§14.3).

### 17.5 Time formatting by user timezone

```go
formatted, _ := timers.Format(ctx, eventTime.UnixMilli(), "02/01/2006 15:04", userTimezone)
responseToUser := fmt.Sprintf("The event is on %s", formatted.Formatted)
```

---

## 18. Design Decisions

| Decision | Reason |
|---|---|
| One per hive, autonomous | Simplicity, no distributed synchronization, delegates to the OS |
| No NTP synchronization of its own | The OS solves it better and it is already installed |
| Server-side time, always with a roundtrip | No complicated local resynchronization, guaranteed consistency |
| 60-second minimum | Removes fine reactive use cases, aligns with cron granularity |
| Embedded SQLite | Consistency with other nodes, simple, transactional, trivial backup |
| Strict ownership by L2 name | Isolation between nodes, no admin override, no complications |
| `TIMER_PURGE_OWNER` only for the orchestrator | A single, traceable exception for node teardown |
| Standard cron syntax | Known by LLMs with no extra learning |
| Generic `TIMER_RESPONSE` response | Consistency with `CONFIG_RESPONSE` and the rest of the control plane |
| `TIMER_FIRED` does not wait for an ack | Fire-and-forget, simplicity; retry is the creator's responsibility |
| `metadata` as a free field | Extensibility without breaking the schema, useful for LLM clients |
| `client_ref` as a top-level field | Common enough use (debug, display, search) to promote it |
| UUIDs generated by `SY.timer` | Avoids collisions, simplifies the client, authoritative |
| Recurring: no retroactive fire | Avoids storms when coming back from a long downtime |
| Embedded `TIMER_HELP` | Meets the system's self-description principle |
| No reschedule of recurring timers in v1 | Rare edge case; cancel+recreate works |
| Embedded `time/tzdata` | Self-contained node, does not depend on the OS `tzdata` (simpler) |

---

## 19. System Limits

| Limit | Value | Notes |
|---|---|---|
| Minimum duration | 60 s | Enforced (`TIMER_BELOW_MINIMUM`); a cron may not fire twice within 60 s. Constant, not configurable |
| Maximum delay | none | Any instant at least 60 s ahead is taken |
| Pending timers | no cap | Not per owner, not per hive. About 10,000 per hive is the load it was designed for, not a limit it enforces |
| `TIMER_LIST` maximum | 1000 results | Per request; default 100, and a larger `limit` is cut to 1000 |
| Retention of fired/canceled timers | 7 days | Deleted when `SY.timer` starts, never in between (§5.2). A constant |
| Firing precision | ±5 s | Best-effort, subject to load |
| `payload` and `metadata` size | no limit of its own | Only the router's 128 KiB frame: a request over it cannot be sent, a response over it is answered `TIMER_INTERNAL`, and a `TIMER_FIRED` over it is lost (§7.8) |
| Time operations' retries | ~1.4 s total | SDK side: 3 attempts that wait 100 ms, 300 ms and 1 s (§14.4) |

---

## 20. Pending / v2

- Structured metrics (Prometheus-compatible).
- Node HA (replication of `timers.db` to a standby instance).
- Administrative backup/restore through a capability.
- Reschedule of recurring timers.
- Optional retroactive firing of recurring timers (for cases where the "missed" ones do matter).
- Extended cron descriptors (`@hourly`, `@daily`, `@weekly`).
- Configurable retention per owner.
- Dead-letter for undelivered `TIMER_FIRED`.
- Delivery retries, and caps per owner and in total (FINDINGS A-115). Until then a lost fire is its owner's to cover. An agent owns no timers: it is a pure responder, and timers live in a WF ([agent-tools-v1](agent-tools-v1.md) §6.2–§6.3). A WF reconciles its timers with `SY.timer` when it starts and runs each one whose time has passed (`Recover`, `go/nodes/wf/wf-generic/node/recover.go`), so a fire it loses while it runs waits for its next start ([wf-orchestration-v1](wf-orchestration-v1.md) §2, §4.4).

---

## 21. Cross References

| Topic | Document |
|---|---|
| General Fluxbee architecture | `01-arquitectura.md` |
| L2 protocol and messages | `02-protocolo.md` |
| Shared memory (for context on other SY nodes) | `03-shm.md` |
| Routing and L2 names | `04-routing.md` |
| Intra/inter-island connectivity | `05-conectividad.md` |
| Config control plane | `node-config-control-plane-spec.md` |
| Node runtime rollout | `14-runtime-rollout-motherbee.md` |
| Glossary and global decisions | `08-apendices.md` |
| (Future) WF nodes | `wf-spec.md` — pending |
| (Future) General self-description protocol | pending |
