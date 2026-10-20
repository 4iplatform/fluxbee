# SY.timer — System Node

**Status:** v1.1 implemented / aligned with current runtime
**Date:** 2026-04-13
**Audience:** Node developers, workflow authors, ops/SRE

---

## 1. Summary and Positioning

`SY.timer` is a Fluxbee system node that provides two capabilities to the rest of the hive:

1. **Persistent timers** — one-shot and recurring, fired as an L2 event toward a target node.
2. **Authoritative source of time and timezone** — `now`, conversions, parse/format, server-side resolution of IANA timezones.

It is part of the family of system nodes, together with `SY.storage`, `SY.admin`, `SY.architect`, `SY.orchestrator`, `SY.config.routes`, `SY.identity`, `SY.cognition`, `SY.opa.rules`. It is always launched when the hive starts, it is kept up by systemd through the orchestrator, and it needs no configuration from the operator beyond its existence.

**One per hive.** There is one instance on the motherbee and one on each worker. Each instance is autonomous, does not talk to other `SY.timer` instances, and delegates clock synchronization to the operating system (NTP/chrony/systemd-timesyncd). Time consistency between hives is the operator's responsibility at the host level, not Fluxbee's.

**It is not a real-time system.** `SY.timer` is designed for workflows, cognition, agents and automation. The minimum granularity of a timer is **60 seconds**, the firing precision is *best-effort* with a tolerance in the order of seconds, and the expected load is up to ~10,000 concurrent timers per hive.

**Strict ownership by L2 name for mutations.** Each node can create, cancel and reschedule its own timers. Reading (`TIMER_GET`, `TIMER_LIST`) is open within the hive. The only exception for administrative mutation is `SY.orchestrator`, which can invoke `TIMER_PURGE_OWNER` as part of the teardown of a node. The effective identity of the requester is taken from `routing.src_l2_name`, stamped authoritatively by the router when it delivers the message. `SY.timer` does not resolve ownership through SHM or through `.uuid` files.

---

## 2. Design Principles

- **Autonomy per hive.** One `SY.timer` per hive, with no cross synchronization. If a host goes down, only the timers local to that hive are affected.
- **Time delegated to the OS.** `SY.timer` does not implement NTP or external synchronization. It reads the OS clock and trusts that the operator keeps it synchronized.
- **Single temporal source of truth.** Any node that needs a consistent hive-wide `now()` asks `SY.timer` for it. There is no local cache, there are no local offsets: one roundtrip per call, with bounded retry in the SDK.
- **Mutation with strict ownership.** Create/cancel/reschedule keeps isolation by L2 owner. Reading is deliberately open for hive observability.
- **A single, explicit exception.** `SY.orchestrator` can purge all the timers of an owner when it deletes a node. It is the only way a timer can be touched by someone other than its creator.
- **Descriptive and extensible JSON.** Field names are self-explanatory, enums are readable strings, errors return human messages as well as codes. All requests accept a free `metadata` field that `SY.timer` persists without interpreting it, meant so that LLM clients can annotate extra context (reasons, correlations, human reminders) without breaking the schema.
- **SDK as the abstraction boundary.** Client node authors never build JSON by hand nor resolve L2 names. They call typed SDK functions and get native types (`time.Time`, structs). If the wire format changes, the SDK changes, not the nodes.
- **Determinism and replayability.** `now()` injected through `SY.timer` lets nodes like `WF` do a deterministic replay of their execution by injecting fake responses in tests. That is why it is critical that nodes do not use the language's `time.Now()` for business logic, but `SY.timer.Now()`.

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

---

## 4. Internal Architecture

### 4.1 Stack

- **Language:** Go
- **Persistence:** embedded SQLite (`modernc.org/sqlite` or `mattn/go-sqlite3`)
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
1. On boot:
   - Open SQLite at /var/lib/fluxbee/nodes/SY/SY.timer@<hive>/timers.db
   - Read ALL the timers with status = 'pending'
   - For each one:
       * If fire_at_utc < now():
           - Apply missed_policy (fire | drop | fire_if_within)
       * If fire_at_utc >= now():
           - Insert into the in-memory heap
   - Recompute the next instance of recurring timers from now()

2. Tick loop:
   loop {
       next = heap.peek()
       if next == nil:
           wait for new timer or shutdown signal
           continue
       sleep_until(next.fire_at_utc)
       for timer in heap.pop_all_due(now()):
           with lock(timer.uuid):
               if timer.status != 'pending':
                   continue  // canceled or rescheduled
               publish TIMER_FIRED to timer.target
               if timer.is_recurring:
                   compute next fire_at from cron_spec
                   update db, push back to heap
               else:
                   update db: status = 'fired'
   }
```

### 4.4 Concurrency

- One global mutex for the heap structure.
- One mutex per timer (sharded map of mutexes by uuid) to serialize the corner case of "a reschedule arriving while the fire is in flight".
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
- `fired` timers (completed one-shots) can be purged by a periodic GC after N days (configurable, default 7 days) to keep the database from growing indefinitely.

---

## 6. Transport

### 6.1 Envelope

All messages follow the standard Fluxbee envelope:

```json
{
  "routing": {
    "src": "<client-node-uuid>",
    "dst": "SY.timer@<hive>",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "msg_type": "system",
    "msg": "TIMER_SCHEDULE"
  },
  "payload": { ... }
}
```

- Destination: always L2 unicast to `SY.timer@<hive-local>`. The SDK resolves the name automatically.
- `meta.msg_type`: always `"system"`.
- `meta.msg`: one of the verbs defined in section 7.
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
  "routing": { "dst": "SY.timer@<hive>", "ttl": 16, "trace_id": "..." },
  "meta": { "msg_type": "system", "msg": "TIMER_SCHEDULE" },
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
| `target_l2_name` | no | L2 name of the node to notify. Default: the requester itself, derived from `routing.src_l2_name`. |
| `client_ref` | no | Optional identifier defined by the client. If it is reused on an active timer of the same owner, `TIMER_SCHEDULE` returns the existing timer instead of creating another one. |
| `payload` | no | Arbitrary JSON that will be included in `TIMER_FIRED.payload.user_payload`. |
| `missed_policy` | no | `"fire"` (default), `"drop"`, or `"fire_if_within"`. |
| `missed_within_ms` | conditional | Required if `missed_policy="fire_if_within"`. |
| `metadata` | no | Free JSON, opaque to `SY.timer`. |

*Exactly one of `fire_at_utc_ms` / `fire_in_ms`.

**Validation:**

- `fire_in_ms >= 60000` or, equivalently, `fire_at_utc_ms - now() >= 60000`. If it fails: error `TIMER_BELOW_MINIMUM`.
- `target_l2_name` must be a syntactically valid L2 name.
- `cron_spec` must not appear (that is `TIMER_SCHEDULE_RECURRING`).

**Response OK:**

```json
{
  "meta": { "msg_type": "system", "msg": "TIMER_RESPONSE" },
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
  "meta": { "msg_type": "system", "msg": "TIMER_SCHEDULE_RECURRING" },
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
      "cron_spec": null,
      "cron_tz": null,
      "missed_policy": "fire_if_within",
      "missed_within_ms": 600000,
      "status": "pending",
      "created_at_utc_ms": 1775574000000,
      "fire_count": 0,
      "last_fired_at_utc_ms": null,
      "payload": { "ticket_id": 1234 },
      "metadata": { "created_by_reasoning": "..." }
    }
  }
}
```

Read by `timer_uuid`: open within the hive. It does not require ownership by the requester.

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
    "count": 3,
    "timers": [ { ... }, { ... }, { ... } ]
  }
}
```

`owner_l2_name` is optional. If it is omitted, `SY.timer` lists the full set of the hive, subject to `status_filter` and `limit`.

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

**Authorization:** `SY.timer` checks that `routing.src` corresponds to the L2 name `SY.orchestrator@<hive-local>`. Any other origin: error `TIMER_FORBIDDEN`.

**Semantics:**
- Idempotent deletion: if there are no timers, it answers `ok` with `deleted_count: 0`.
- It deletes both `pending` and historical timers (`fired`, `canceled`) — consistent with the spirit of "the node no longer exists".
- Silent: it does not emit `TIMER_FIRED` for the deleted pending timers; they are considered canceled.

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_PURGE_OWNER",
    "owner_l2_name": "WF.onboarding.step2@motherbee",
    "deleted_count": 7
  }
}
```

### 7.8 `TIMER_FIRED` event

Emitted by `SY.timer` toward the `target_l2_name` when a timer is due.

```json
{
  "routing": {
    "src": "SY.timer@<hive>",
    "dst": "WF.onboarding.step2@motherbee",
    "ttl": 16,
    "trace_id": "<new-uuid>"
  },
  "meta": {
    "msg_type": "system",
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
- `is_last_fire` is for recurring timers that have some termination criterion (in v1 always `false` for recurring timers; reserved for v2).
- `user_payload` is exactly what the client passed in `TIMER_SCHEDULE.payload.payload`, unmodified.
- `metadata` is also returned as is, so that the consumer can have the context the creator left.
- The target must NOT answer with `TIMER_RESPONSE`. `TIMER_FIRED` is fire-and-forget; if the target is down, the event is lost. If a retry is needed, it is the timer creator's responsibility to design that semantics in its application logic.
- `SY.timer` considers the fire attempted when it hands the message to the local router; it does not wait for an application ack or for confirmation from the target.
- In v1 there is no dead-letter queue nor automatic retry of `TIMER_FIRED`. Durability still lives in the timer row and in the node logs.

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
    "local_iso": "2026-04-08T17:20:00+02:00",
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
    "resolved_iso_utc": "2026-04-08T15:20:00Z"
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

All time operations do an L2 roundtrip to `SY.timer`. The SDK implements retry with backoff (3 attempts, 100ms/300ms/1s) and fails loudly if it gets no response. **There is no local cache nor offset synchronization.** If a node needs to measure fine durations, it must use the process's local monotonic clock, not `TIMER_NOW`. The rule: *authoritative real-world time through `SY.timer`, local monotonic measurements with the process clock*.

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
    "version": "1.0",
    "description": "System node providing persistent timers and authoritative time/timezone services.",
    "operations": [
      {
        "verb": "TIMER_SCHEDULE",
        "description": "Create a one-shot timer that fires at a specific time.",
        "required_fields": ["fire_at_utc_ms OR fire_in_ms"],
        "optional_fields": ["target_l2_name", "client_ref", "payload", "missed_policy", "missed_within_ms", "metadata"],
        "constraints": ["minimum duration: 60000 ms"],
        "example_request": { "...": "..." },
        "example_response": { "...": "..." }
      },
      { "...": "..." }
    ],
    "error_codes": [
      { "code": "TIMER_BELOW_MINIMUM", "description": "Requested duration is below the minimum of 60 seconds." },
      { "...": "..." }
    ]
  }
}
```

The full content of `TIMER_HELP` is generated from the node's internal descriptors and is kept in sync with the real implementation (there is no risk of stale documentation).

---

## 10. Missed Policies Semantics

When `SY.timer` starts after a downtime, it evaluates each pending timer against `now()`:

| Policy | Overdue timer | Not yet due timer |
|---|---|---|
| `fire` | It fires immediately, in order. | It is rescheduled with the original `fire_at`. |
| `drop` | It is discarded, logged at `warn` level. | It is rescheduled normally. |
| `fire_if_within` | If `now() - fire_at_utc <= missed_within_ms`, it fires. If not, it is discarded. | It is rescheduled normally. |

For **recurring timers**, the behavior on restart is:
- The **next instance** is computed from `now()` onward according to `cron_spec`.
- The instances that should have fired during the downtime **do not fire retroactively**, regardless of the `missed_policy`. This avoids firing storms when coming back from long downtimes.
- The `missed_policy` for recurring timers is interpreted only on the **current instance** (the one that was about to fire at the moment of the outage).

---

## 11. Restart Semantics — Detail

At node startup:

1. Open `timers.db`. If it does not exist, create the schema.
2. `now = time.Now().UTC().UnixMilli()`.
3. `SELECT * FROM timers WHERE status='pending' ORDER BY fire_at_utc ASC`.
4. For each timer:
   - If `fire_at_utc <= now`:
     - Apply `missed_policy`.
     - If it fires: publish `TIMER_FIRED`, update `status` and `fire_count` in the DB, and if it is recurring, compute the next instance.
     - If it is discarded: log it, update `status='canceled'` with the reason `missed_dropped` in the logs (it is not a schema field).
   - If `fire_at_utc > now`:
     - Insert it into the in-memory heap with the same `fire_at_utc`. **The `fire_at_utc` is not modified** — it is already the absolute target instant, and the heap fires it when the OS clock reaches that value.
5. For recurring timers, recompute `fire_at_utc` from `now()` with `cron_spec` and `cron_tz`, update the DB, push to the heap.
6. Run the optional GC: `DELETE FROM timers WHERE status IN ('fired','canceled') AND last_fired_at_utc < now - retention_ms`.
7. Start the tick loop.

**Guarantee:** a timer created with `fire_at_utc = T` will fire at time `T` of the OS clock, subject to the node's availability and to its `missed_policy`. There is no accumulated drift.

---

## 12. Authorization and Ownership

### 12.1 General rule

For `TIMER_CANCEL`, `TIMER_RESCHEDULE`:
```
if timer.owner_l2_name != sender_l2_name(routing.src):
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

### 12.2 The single exception

For `TIMER_PURGE_OWNER`:
```
if sender_l2_name(routing.src) != "SY.orchestrator@<local-hive>":
    return error TIMER_FORBIDDEN
```

The check is done by the L2 name of the sender, resolving `routing.src` (L1 UUID) through the Go runtime/SDK and trusting the router's L2 layer for the integrity of that identity. This is consistent with the authorization model of the rest of the system nodes.

In the current v1, that resolution in Go takes the `router_name` received in the `ANNOUNCE`, reads `state/<router_l2_name>/identity.yaml` to locate the `shm.name`, opens that router SHM in read-only mode and resolves `uuid -> L2` from the live table of nodes connected to that router.

### 12.3 Node deletion flow

```
SY.admin (HTTP request)
    │
    ├─► SY.orchestrator (RPC/L2)
    │       │
    │       ├─► SY.timer: TIMER_PURGE_OWNER { owner_l2_name: "WF.foo@hive" }
    │       │       └─► TIMER_RESPONSE { ok: true, deleted_count: N }
    │       │
    │       ├─► kill the node's process
    │       ├─► cleanup of /var/lib/fluxbee/nodes/.../
    │       └─► unregister from identity
    │
    └─◄ HTTP response
```

The orchestrator must call `TIMER_PURGE_OWNER` **before** it kills the node's process, to avoid the race condition where the node gets a `TIMER_FIRED` right while it is being terminated. If `SY.timer` is down at that moment, the orchestrator must go ahead with the deletion anyway and log the failure; the orphaned timers will be discarded naturally when their `TIMER_FIRED` events find no target (and eventually they can be cleaned up manually).

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

## 14. Go SDK

The Fluxbee Go SDK exposes a `timer` module that wraps all the interaction with `SY.timer`. The author of the client node never sees JSON.

### 14.1 Recommended direct-use pattern

For `WF.*`, `AI.*`, `IO.*` nodes and future Go nodes, the recommended pattern is:

1. connect to the router with `fluxbee-go-sdk`
2. build a `TimerClient` on top of the `NodeSender`
3. use the client's typed methods for time, scheduling and reading
4. process `TIMER_FIRED` as a normal `system` message in the node's handler

In v1 this is the primary integration path. `SY.admin` adds read-only visibility and time utilities, but the owner-bound semantics of scheduling and mutations remain direct SDK use.

**Transport note:** the `fluxbee-go-sdk` emits these direct requests as local unicast `system` with a default TTL of `1`, because the expected destination is always the `SY.timer@<hive-local>` of the same hive. The envelope examples in this spec illustrate the shape; the canonical contract for SDK clients is also fixed by the SDK's golden tests.

Minimal example:

```go
sender, receiver, err := sdk.Connect(sdk.NodeConfig{
    Name:               "WF.demo",
    RouterSocket:       "/var/run/fluxbee/routers",
    UUIDPersistenceDir: "/var/lib/fluxbee/state/nodes",
    UUIDMode:           sdk.NodeUuidPersistent,
    ConfigDir:          "/etc/fluxbee",
    Version:            "1.0",
})
if err != nil {
    return err
}

timerClient, err := sdk.NewTimerClient(sender, "")
if err != nil {
    return err
}

timerID, err := timerClient.ScheduleIn(ctx, 2*time.Minute, sdk.ScheduleOptions{
    ClientRef: "wf-demo-timeout",
    Payload: map[string]any{
        "kind": "timeout",
    },
})
if err != nil {
    return err
}

_ = timerID
_ = receiver
```

### 14.2 Main signatures

```go
package timer

// SDK constants
const MinDuration = 60 * time.Second

// --- Time ---
func Now(ctx context.Context) (time.Time, error)
func NowIn(ctx context.Context, tz string) (time.Time, error)
func Convert(ctx context.Context, instant time.Time, toTz string) (time.Time, error)
func Parse(ctx context.Context, input, layout, tz string) (time.Time, error)
func Format(ctx context.Context, instant time.Time, layout, tz string) (string, error)

// --- One-shot timers ---
type ScheduleOptions struct {
    Target       string          // optional, default: self
    ClientRef    string          // optional
    Payload      any             // optional, serializable to JSON
    MissedPolicy MissedPolicy    // default: Fire
    MissedWithin time.Duration   // required if MissedPolicy == FireIfWithin
    Metadata     map[string]any  // optional
}

func Schedule(ctx context.Context, fireAt time.Time, opts ScheduleOptions) (TimerID, error)
func ScheduleIn(ctx context.Context, d time.Duration, opts ScheduleOptions) (TimerID, error)

// --- Recurring timers ---
type RecurringOptions struct {
    CronSpec     string
    CronTZ       string          // default: "UTC"
    Target       string
    ClientRef    string
    Payload      any
    MissedPolicy MissedPolicy
    MissedWithin time.Duration
    Metadata     map[string]any
}

func ScheduleRecurring(ctx context.Context, opts RecurringOptions) (TimerID, error)

### 14.3 Rust SDK status

In the current state of the runtime:

- the rich, typed surface of `SY.timer` is implemented in `fluxbee-go-sdk`
- `fluxbee_sdk` (Rust) does **not** yet expose an equivalent `timer` module

That is not a design accident nor a long-term decision: it is an explicit SDK parity debt.

The right goal for the Rust SDK is to replicate:

- typed time helpers (`now`, `now_in`, `convert`, `parse`, `format`, `help`)
- a typed scheduling client (`schedule`, `schedule_in`, `schedule_recurring`, `get`, `list`, `cancel`, `reschedule`, `purge_owner`)
- parsing of `TIMER_RESPONSE`
- parsing of `TIMER_FIRED`
- errors and client-side validation comparable to the Go SDK

The formal backlog of that work is in [sdk_tasks.md](history/onworking-coa/sdk_tasks.md) under the `RUST-TIMER-SDK` section.

**Scope decision:** this Rust SDK parity does not imply that `SY.orchestrator` must use `SY.timer` for its base operation in v1. The work is first about SDK capability and support for new Rust consumers, not about re-architecting the control-plane.

// --- Management ---
func Cancel(ctx context.Context, id TimerID) error
func Reschedule(ctx context.Context, id TimerID, newFireAt time.Time) error
func Get(ctx context.Context, id TimerID) (*TimerInfo, error)
func ListMine(ctx context.Context, filter ListFilter) ([]TimerInfo, error)

// --- Introspection ---
func Help(ctx context.Context) (*HelpDescriptor, error)
```

### 14.3 Client-side validation

```go
func ScheduleIn(ctx context.Context, d time.Duration, opts ScheduleOptions) (TimerID, error) {
    if d < MinDuration {
        return "", ErrBelowMinimum
    }
    // ... resolves the L2 name of SY.timer in the local hive
    // ... builds TIMER_SCHEDULE
    // ... sends and parses the response
}
```

The 60s minimum is validated **client-side** before sending the request, which avoids a roundtrip. It is also validated server-side as defense in depth.

### 14.4 Retry of time operations

```go
func Now(ctx context.Context) (time.Time, error) {
    for attempt, backoff := range []time.Duration{100*time.Millisecond, 300*time.Millisecond, 1*time.Second} {
        t, err := doNowRequest(ctx)
        if err == nil {
            return t, nil
        }
        if attempt < 2 {
            time.Sleep(backoff)
        }
    }
    return time.Time{}, ErrTimerUnreachable
}
```

### 14.5 Handling `TIMER_FIRED` in the client node

The SDK exposes a typed handler:

```go
type FiredEvent struct {
    TimerUUID    TimerID
    ClientRef    string
    Kind         TimerKind
    ScheduledAt  time.Time
    ActualAt     time.Time
    FireCount    int
    UserPayload  json.RawMessage
    Metadata     map[string]any
}

// In the runtime of the client node:
node.OnSystemMessage("TIMER_FIRED", func(msg Message) error {
    evt, err := timer.ParseFiredEvent(msg)
    if err != nil {
        return err
    }
    // application logic
    return nil
})
```

---

## 15. Error Codes

| Code | Description |
|---|---|
| `TIMER_BELOW_MINIMUM` | Requested duration lower than 60 seconds. |
| `TIMER_INVALID_CRON` | `cron_spec` does not parse correctly. |
| `TIMER_INVALID_TIMEZONE` | `tz` / `cron_tz` is not a valid IANA zone. |
| `TIMER_INVALID_PAYLOAD` | The request `payload` is not valid JSON or required fields are missing. |
| `TIMER_NOT_FOUND` | `timer_uuid` does not exist. |
| `TIMER_NOT_OWNER` | The requester is not the owner of the timer, for mutation verbs (`TIMER_CANCEL`, `TIMER_RESCHEDULE`). |
| `TIMER_ALREADY_FIRED` | Timer already fired; it cannot be canceled/rescheduled. |
| `TIMER_RECURRING_NOT_RESCHEDULABLE` | Reschedule is not allowed on recurring timers in v1. |
| `TIMER_FORBIDDEN` | Restricted operation (e.g. `TIMER_PURGE_OWNER` from a non-orchestrator node). |
| `TIMER_STORAGE_ERROR` | Internal SQLite error. |
| `TIMER_INTERNAL` | Unclassified internal error. |
| `TIMER_UNREACHABLE` | (Client-side) `SY.timer` does not answer after retries. |

Format of an error response:

```json
{
  "meta": { "msg_type": "system", "msg": "TIMER_RESPONSE" },
  "payload": {
    "ok": false,
    "verb": "TIMER_SCHEDULE",
    "error": {
      "code": "TIMER_BELOW_MINIMUM",
      "message": "minimum timer duration is 60 seconds, got 30000 ms"
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

`SY.timer` emits structured logs on stdout (captured by systemd / orchestrator):

- `INFO`: startup, number of timers loaded, GC, fires (optional: verbose).
- `WARN`: timers discarded by `missed_policy`, unreachable targets.
- `ERROR`: SQLite errors, invalid crons in the DB, integrity violations.

### 16.5 Metrics (future)

Reserved for v2. Candidates: `timers_pending_gauge`, `timers_fired_counter`, `timer_fire_latency_histogram`, `timer_requests_counter` per verb.

---

## 17. Expected Use Cases

### 17.1 WF — Step timeout

A workflow schedules a timer when it enters a state that waits for an external response:

```go
timerID, _ := timer.ScheduleIn(ctx, 30*time.Minute, timer.ScheduleOptions{
    ClientRef: fmt.Sprintf("wf_%s_step_%s_timeout", wfInstanceID, stepName),
    Payload:   map[string]any{"wf_instance": wfInstanceID, "step": stepName, "reason": "step_timeout"},
    MissedPolicy: timer.Fire,
})
```

When the WF gets the expected response, it cancels the timer. If not, it gets `TIMER_FIRED` and transitions to an escalation state.

### 17.2 Cognition — Deferred reconsideration

A cognitive node can schedule a reminder to review a conclusion with fresh data:

```go
timer.ScheduleIn(ctx, 24*time.Hour, timer.ScheduleOptions{
    ClientRef: "reconsider_hypothesis_42",
    Payload:   map[string]any{"hypothesis_id": 42, "action": "reconsider"},
    MissedPolicy: timer.Drop,
    Metadata: map[string]any{
        "reasoning": "low confidence conclusion, may benefit from fresh data",
    },
})
```

### 17.3 Recurring reports

```go
timer.ScheduleRecurring(ctx, timer.RecurringOptions{
    CronSpec:  "0 9 * * MON",
    CronTZ:    "America/Argentina/Buenos_Aires",
    ClientRef: "weekly_summary",
    Payload:   map[string]any{"report_type": "weekly"},
    MissedPolicy: timer.Drop,
})
```

### 17.4 Authoritative time for AI nodes

```go
now, _ := timer.Now(ctx)
prompt := fmt.Sprintf("Current date and time: %s\nUser query: %s", now.Format(time.RFC3339), userQuery)
```

### 17.5 Time formatting by user timezone

```go
formatted, _ := timer.Format(ctx, eventTime, "02/01/2006 15:04", userTimezone)
responseToUser := fmt.Sprintf("El evento es el %s", formatted)
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
| Minimum duration | 60 s | Constant, not configurable |
| Maximum pending timers | ~10,000 | Per hive, soft limit |
| `TIMER_LIST` maximum | 1000 results | Per request |
| Retention of fired/canceled timers | 7 days | Configurable in v2 |
| Firing precision | ±5 s | Best-effort, subject to load |
| Maximum `payload` size | 64 KB | Aligned with inline L2 messages |
| Maximum `metadata` size | 16 KB | Recommended, not enforced in v1 |
| `TIMER_NOW` retry timeout | ~1.4 s total | 3 attempts with backoff |

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
