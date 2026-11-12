# SY.timer — System Node

**Status:** v1.7 implemented / aligned with current runtime
**Date:** 2026-04-13; checked against the code on 2026-10-27; v1.2 on 2026-11-04, in 0.1.99: a fire saved
before it goes and sent again while its target is down, caps, reads by the owner, a node's own
target (FINDINGS A-115); v1.3 for the next release: `TIMER_LIST` in pages (FINDINGS A-151); v1.4 in
0.1.127: a recurring timer SY.timer cannot schedule is canceled, and a timer SY.timer cancels says
why in `canceled_reason`; a refusal's code goes with its error, not in its text (FINDINGS A-220);
v1.5 in 0.1.131: a canceled timer is kept 7 days from its cancellation, not from its creation or its
last fire (FINDINGS A-238), and a timer's lock is kept in memory only while it is in use (FINDINGS
A-244); v1.6 in 0.1.133: the scheduler's heap holds the pending timers only, a canceled, moved or
purged timer's entry leaving it at once, a fire whose read of its timer fails is tried again in
30 s, and a start whose count of pending timers fails says so (FINDINGS A-259); a refused request
gets a line in SY.timer's own log, with its verb, its code and its sender (and the database's error
for a `TIMER_STORAGE_ERROR`), at most 10 a minute per sender, and so does a message that is no
request (FINDINGS A-258); a `TIMER_LIST` page its `limit` cuts answers `next_cursor`, as one the
frame cuts does (FINDINGS A-266); v1.7 in 0.1.139: a `TIMER_LIST` page reads its timers from an
index that holds them in the list's order, from its cursor's place on, and no further than the
first one it cannot take: what it lists and one more, whatever the hive holds (FINDINGS A-326); the
start drops `idx_timers_owner` and `idx_timers_fire_at`, which no statement reads once those
indexes are there, goes on without an index it cannot build, and says in its ready line what it
built and dropped and how long that took, and a purge's line gives the delete's time (the review of
A-326); each connection's page cache is 16 MiB, where SQLite's default is 2,000 KiB (the operator's
call of 2026-11-12, FINDINGS A-326)
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

**Strict ownership by L2 name.** Each node creates, reads, cancels and reschedules its own timers, and their fires go to it: only a SY node may schedule a timer whose fire goes to another node. `SY.admin` reads every timer, for the operator (§12.1). The only exception for administrative mutation is `SY.orchestrator`, which can invoke `TIMER_PURGE_OWNER` as part of the teardown of a node. The effective identity of the requester is taken from `routing.src_l2_name`, stamped authoritatively by the router when it delivers the message. `SY.timer` does not resolve ownership through SHM or through `.uuid` files.

**Delivery is at most once, and sent again while the target is down** (FINDINGS A-115). A fire is saved before it is sent, so it never goes twice; one whose target is not connected is sent again while it is on time, for up to an hour (§7.8). A one-shot fires at most 366 days ahead, a payload and its metadata are at most 32 KiB each, and an owner keeps at most 10,000 pending timers, the hive 100,000 (§19).

---

## 2. Design Principles

- **Autonomy per hive.** One `SY.timer` per hive, with no cross synchronization. If a host goes down, only the timers local to that hive are affected.
- **Time delegated to the OS.** `SY.timer` does not implement NTP or external synchronization. It reads the OS clock and trusts that the operator keeps it synchronized.
- **Single temporal source of truth.** Any node that needs a consistent hive-wide `now()` asks `SY.timer` for it. There is no local cache, there are no local offsets: one roundtrip per call, with bounded retry in the SDK.
- **Strict ownership.** Create, read, cancel and reschedule keep isolation by L2 owner. `SY.admin` reads every timer, for the operator's view of the hive.
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

`TIMER_GET` and `TIMER_LIST` can be exposed through `SY.admin` because `SY.timer` answers them to `SY.admin@motherbee` for any owner (§12.1). The reason to keep scheduling and mutations outside admin is architectural: `SY.timer` authorizes those operations by the effective identity of the requester (`routing.src_l2_name`, stamped by the router), and `SY.admin` does not act as a transparent ownership proxy.

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
│  │  indexes: status, and one per TIMER_LIST        │  │
│  │  filter, in its order (§5.2)                    │  │
│  └─────────────────────────────────────────────────┘  │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

### 4.3 Scheduling loop

```
1. On boot (§11):
   - Open SQLite at /var/lib/fluxbee/nodes/SY/SY.timer@<hive>/timers.db
   - Delete the fired timers whose last fire, and the canceled ones whose cancellation,
     is more than 7 days old (once per start, §5.2)
   - Read ALL the timers with status = 'pending', by fire_at_utc
   - For each recurring timer:
       * Compute its next instance from now() and insert it into the heap;
         the instances missed while the node was down never fire (§10)
       * One whose next instance cannot be computed: canceled, with its canceled_reason (§11)
   - For each one-shot:
       * If fire_at_utc > now(): insert into the in-memory heap
       * Otherwise: apply missed_policy (fire | drop | fire_if_within);
         one that fires is queued, due at once: it goes when the tick loop starts;
         one dropped is canceled, with its canceled_reason

2. Tick loop (sleeps at most one hour when the heap is empty; a new timer wakes it):
   loop {
       next = heap.peek()
       sleep_until(next.fire_at_utc)
       for entry in heap.pop_all_due(now()):
           with lock(entry.timer_uuid):
               if entry is a retry (§7.8):
                   if the timer was purged or canceled: drop it
                   else: send the same fire again, under a new trace_id
                   continue
               re-read the timer
               if the read failed (the database, not a timer gone):
                   put it back in the heap 30 s from now; continue  // nothing sent
               if it is gone or timer.status != 'pending':
                   continue  // purged, canceled
               if timer.fire_at_utc > now():
                   keep it in the heap at that time  // rescheduled later, which queued it there
                   if a read again finds it gone (purged): take it out
                   continue
               if timer.is_recurring:
                   compute next fire_at from cron_spec, from now
                   if it cannot be computed:
                       cancel it, with its canceled_reason (§11); send nothing; continue
                   put its next fire in the heap, then update db while still pending
               else:
                   update db while still pending: status = 'fired'
               if the update failed: move it in the heap to 30 s from now; continue  // nothing sent
               if the row was no longer pending (purged): take it out of the heap; continue  // nothing sent
               send TIMER_FIRED to timer.target (§7.8)
   }
```

The heap holds one entry per pending timer, the next fire it waits for, found by its uuid (`queued`), and the retries of fires on their way (§7.8), which are no timer's entry and leave at their time, within the hour of retries. `TIMER_CANCEL` takes the timer's entry out (§7.3), `TIMER_RESCHEDULE` moves it (§7.4), and `TIMER_PURGE_OWNER` takes out the entries of the owner's pending timers (§7.7). Since v1.6 (FINDINGS A-259); before, nothing took an entry out: a canceled, moved or purged timer's entry stayed until its old fire time, up to 366 days ahead, and was dropped then, and a node that looped a schedule a year ahead and its cancel grew the heap by about 100 bytes a pair, which the pending caps (§19), counting pending rows only, never saw. A fire whose read of its timer fails (the database, not a timer gone) sends nothing, puts the timer back 30 s later, and logs `timer_fire_read_failed`, as one whose save fails does (`timer_fire_persist_failed`): the entry that brought the fire was the timer's one. Before, it logged nothing and left the timer with no entry, unfired until the next start. A timer canceled or purged between its entry's pop and that failed read waits there for those 30 s, and its fire then finds it gone or no longer pending.

### 4.4 Concurrency

- One global mutex for the heap and its index of pending timers (`queued`, §4.3).
- One mutex per timer being worked on (`lockTimer`: a map by uuid, whose entry counts who holds it or waits for it and leaves with the last of them, since v1.5, FINDINGS A-244; before, the map kept a mutex for every timer that ever fired, was canceled or rescheduled, until `SY.timer` stopped), taken by the fire and its retries, and by `TIMER_CANCEL` and `TIMER_RESCHEDULE` (`lockOwnedTimerForMutation`, since v1.4, FINDINGS A-220's review). A fire holds it from its read of the row to its save and its send; a cancel or a reschedule that lands meanwhile waits, reads the timer again, and sees what the fire wrote: it wins, or loses cleanly. A one-shot that fired is `TIMER_ALREADY_FIRED`; a recurring timer canceled after a fire stays canceled. Before, they took no lock, and the fire's save wrote over them: a recurring timer its owner was told is canceled kept firing, and a one-shot rescheduled during its fire fired again at its new time.
- A fire saves its row only while it is still `pending` (`updateTimerIfStatus`): `TIMER_PURGE_OWNER`, which takes no timer lock, is not written over, and the fire of a timer purged before its save is not sent (`timer_fire_dropped`); one saved before the purge is sent. Nor is the purged timer left in the heap (since v1.6, FINDINGS A-259): the fire puts a recurring timer's next fire in the heap before its save, and keeps a timer it finds rescheduled later there before it reads it again, so either the purge takes that entry out after it, or the save or the read finds the timer gone and the fire takes the entry out itself.
- SQLite operations serialized by the driver (SQLite in WAL mode allows multiple readers + one writer).

---

## 5. Data Model

### 5.1 File path

```
/var/lib/fluxbee/nodes/SY/SY.timer@<hive>/timers.db
```

Together with the rest of the node's persistent state (`config.json`, etc.), following the canonical per-node directory convention of the system.

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
    fire_count          INTEGER NOT NULL DEFAULT 0,
    canceled_reason     TEXT,              -- why SY.timer canceled it itself (§7.5); schema 3
    canceled_at_utc     INTEGER            -- unix ms, when it was canceled, by its owner or SY.timer; schema 4
);

CREATE INDEX idx_timers_status ON timers(status);
CREATE UNIQUE INDEX idx_timers_owner_clientref_pending
ON timers(owner_l2_name, client_ref)
WHERE client_ref IS NOT NULL AND status='pending';

-- TIMER_LIST's, one per filter, in the list's order (since v1.7)
CREATE INDEX idx_timers_list_owner_status ON timers(owner_l2_name, status, fire_at_utc, uuid);
CREATE INDEX idx_timers_list_owner ON timers(owner_l2_name, fire_at_utc, uuid);
CREATE INDEX idx_timers_list_status ON timers(status, fire_at_utc, uuid);
CREATE INDEX idx_timers_list ON timers(fire_at_utc, uuid);

-- Before v1.7, dropped by its start once the four above are there
-- CREATE INDEX idx_timers_owner ON timers(owner_l2_name);
-- CREATE INDEX idx_timers_fire_at ON timers(fire_at_utc) WHERE status='pending';
```

**Notes:**
- The four `idx_timers_list*` indexes (since v1.7, FINDINGS A-326) hold the timers in `TIMER_LIST`'s order, fire time then uuid, after what a page filters on: an owner and a status, an owner (`status_filter` `all`), a status (`SY.admin`'s list of the whole hive), or nothing (the whole hive, `all`). A page reads one of them from its cursor's place on (§7.6). The owner's count of pending timers, which each new timer's cap check runs (§7.1), and `TIMER_PURGE_OWNER` (§7.7) find the owner's timers in them too. `idx_timers_status` stays for the hive's count of pending timers, which the same check runs: its entries are narrower than `idx_timers_list_status`'s, and the count reads them faster: 8 to 11 ms with 100,000 pending timers on a busy Mac, where `idx_timers_list_status` took 13 to 16. The start's history GC reads it too.
- Every start builds the indexes the table lacks (`ensureTimerIndexes`), whatever `user_version` says, before it serves a request: the first start of v1.7 builds the four `idx_timers_list*` over the table it finds, and then, once it has all four, drops `idx_timers_owner` and `idx_timers_fire_at`. No statement reads those two then: `idx_timers_list_owner` serves the purge's delete of an owner's timers as `idx_timers_owner` did, and `idx_timers_list_status` holds the pending timers by fire time as `idx_timers_fire_at` did; kept, they cost every write of a timer, and a purge most (§7.7; the review of A-326). On a Mac the first start's index step took 0.14 s with 18,000 pending timers and 1.8 to 1.9 s with 200,000 timers (the hive's cap of 100,000 pending, and as many fired). An index only speeds reads up, so a start that cannot build one, say on a disk short of room, goes on without it, its reads slower, and keeps the two old ones while one of the four is missing; the next start tries again. Before the review a build that failed stopped the start, and systemd started `SY.timer` again every 5 s. The start says what it did in its ready line and its `timer_startup` event, when it built or dropped an index or could not: `indexes_built`, `indexes_dropped`, `indexes_ms` (the step's time, apart from the router's connection, the history GC and the replay), and `indexes_failed`, each index with its error (`startupIndexes`, §11, §16.4).
- The indexes take room: the pages in use went from 8.1 to 12.0 MB at 18,000 pending timers, and from 81 to 126 MB at 200,000 timers. The two dropped indexes' pages stay in the file as free pages, which `SQLite` reuses, so the file does not shrink. A timer's writes update every index: on the builder's Mac, with 18,000 timers in the table and v1.7's indexes as first built, the two old ones still there, an insert took 376 µs where it took 289 µs on v1.6, and a fire's read and save 462 µs where 358 µs; since the review each write updates those two fewer.
- Going back is safe: an older `SY.timer` started on the table builds `idx_timers_owner` and `idx_timers_fire_at` again, at its start, and leaves the four as they are, and its lists read them too (an older page's query searches them with nothing to sort). The next start of v1.7 drops the two again.
- **The page cache** (since v1.7, the operator's call of 2026-11-12, FINDINGS A-326): every connection opens with `PRAGMA cache_size` at 16 MiB (`timerDSN`, `timerPageCacheKiB`), where SQLite's default is 2,000 KiB. A large `TIMER_PURGE_OWNER` changes more pages than the default holds, and spills them to the WAL as it goes: the purge of an owner's 10,000 pending and 20,000 fired timers among 100,000 pending takes 0.89 s with it, 1.13 s with the default (§7.7). **Its memory:** a connection takes a page of the cache only when a statement reads or writes it, and keeps it until the connection closes. In `SY.timer`'s SQLite (modernc's, in Go) a cached page of 4 KiB holds about 8 KiB, so a full cache is about 31 MiB resident, the default's about 4 MiB (measured on a Mac after a read of every page of an 82 MB `timers.db`: 31.0 MiB more with 16 MiB, 3.8 MiB with the default, freed once the connection closed). The pool keeps two connections (`database/sql`'s default idle ones; `SY.timer` serves one request at a time beside its scheduler), so up to about 62 MiB on a hive whose `timers.db` outgrows the cache, some 25,000 timers, where the default kept up to about 8 MiB; on a smaller one, what its pages take. After the purge above the process held 43 MiB with the connection idle, 16 MiB with the default, and 12 MiB once it closed, either way.
- `created_at_utc` is kept forever, even after reschedules. It serves traceability and `fire_if_within` decisions based on the age of the original timer.
- `cron_tz` is persisted explicitly so that the next recurring fire can be recomputed without losing the source timezone.
- `canceled_reason` is schema version 3 (`PRAGMA user_version`, since 0.1.127, FINDINGS A-220). `ensureTimerSchema` adds it to a table that lacks it, whatever `user_version` says: a `SY.timer` before it writes `user_version` 2 back over a table that has it, and reads and writes its rows by naming the other columns, so going back to it and forward again is safe.
- `canceled_at_utc` is schema version 4 (since v1.5, FINDINGS A-238), added the same way (`addTimerColumn`): v1.4 writes `user_version` 3 back over a table that has it. A timer gets it when it is canceled: by its owner's first `TIMER_CANCEL` (§7.3; a cancel again keeps it), or by `SY.timer` (§10, §11). v1.4 names its columns too, so going back to it and forward again is safe: back on v1.4, its GC ages a canceled timer by its last fire or its creation again, and the timers it cancels get no `canceled_at_utc`, until the next start of v1.5 dates them. The time is kept, not shown: `TIMER_GET` and `TIMER_LIST` answer the SDKs' `TimerInfo`, which has no field for it.
- History is deleted only when `SY.timer` starts (`gcHistoricalTimers`, `defaultHistoryRetention`): the `fired` timers whose last fire (their creation, for one that never fired) is older than 7 days, and the `canceled` ones canceled more than 7 days ago. A canceled timer with no `canceled_at_utc`, canceled before v1.5 or by a v1.4 after a rollback, is dated at that start first, in the same transaction, and so kept 7 days from it: the start logs how many as `gc_stamped` (§11, §16.4). Before v1.5 a canceled timer went by its last fire or its creation, so one canceled yesterday whose creation was a week old was deleted at the next start, its `canceled_reason` with it (FINDINGS A-238). There is no periodic GC, and the 7 days are a constant: a node that runs for weeks keeps all its history until its next start.

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
- Only requests are answered. A message the router made itself (it has no `routing.src_l2_name`: `UNREACHABLE`, `TTL_EXCEEDED`), a notice from a node (`HELLO`, `ANNOUNCE`, `WITHDRAW`, a message without a name, or SY.vault's `VAULT_SECRET_CHANGED`) and a reply (any `*_RESPONSE`, a `NOT_TAKEN`, an agent's `<msg>.error`) get no answer: wf-generic's rule and the AI nodes', name by name, but for `VAULT_SECRET_CHANGED`, which a WF drops unanswered with every system message that is none of its verbs and ai.generic still answers, and, for the AI nodes, `NOT_TAKEN` and `<msg>.error` (`isInfrastructureMessage`, FINDINGS A-218, A-329, A-325; before 0.1.141 a `NOT_TAKEN` was answered `TIMER_UNKNOWN_VERB`, and so was each `VAULT_SECRET_CHANGED`, the notice SY.vault broadcasts to every node at each change of a secret and once for each secret at its start, which SY.vault drops: `SY.timer` reads no secret). Each is logged, at most 10 lines a minute per sender (`timer_message_ignored`, §16.4, since v1.6). The router's `UNREACHABLE` for a `TIMER_FIRED` (its `trace_id`), and its target's retryable `NOT_TAKEN`, make `SY.timer` send that fire again (§7.8). An unknown verb from a node is answered `TIMER_UNKNOWN_VERB`, and so is a request whose response does not fit in one frame (128 KiB).

### 6.2 Verbs

| Verb | Purpose |
|---|---|
| `TIMER_SCHEDULE` | Create a one-shot timer |
| `TIMER_SCHEDULE_RECURRING` | Create a recurring timer (cron) |
| `TIMER_CANCEL` | Cancel one of the requester's own timers by `timer_uuid` or `client_ref` |
| `TIMER_RESCHEDULE` | Change the `fire_at` of one of the requester's own pending timers by `timer_uuid` or `client_ref` |
| `TIMER_GET` | Get one of the requester's timers by `timer_uuid`, or its active timer by `client_ref`; `SY.admin` gets any by `timer_uuid` |
| `TIMER_LIST` | List the requester's timers; `SY.admin` lists any owner's, or the hive's |
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
| `target_l2_name` | no | L2 name of the node to notify. Default: the requester itself, derived from `routing.src_l2_name`. Another node's name is refused (`TIMER_FORBIDDEN`) unless the requester is a SY node: a node schedules for itself (since 0.1.99). A WF node also takes a `TIMER_FIRED` only for a timer it scheduled itself (`owner_l2_name`, since 0.1.90). |
| `client_ref` | no | Optional identifier defined by the client. If it is reused on an active timer of the same owner, `TIMER_SCHEDULE` returns the existing timer instead of creating another one. |
| `payload` | no | Arbitrary JSON that will be included in `TIMER_FIRED.payload.user_payload`. At most 32 KiB. |
| `missed_policy` | no | `"fire"` (default), `"drop"`, or `"fire_if_within"`. |
| `missed_within_ms` | conditional | Required if `missed_policy="fire_if_within"`. |
| `metadata` | no | Free JSON, opaque to `SY.timer`. At most 32 KiB. |

*Exactly one of `fire_at_utc_ms` / `fire_in_ms`.

**Validation:**

- `fire_in_ms >= 60000` or, equivalently, `fire_at_utc_ms - now() >= 60000`. If it fails: error `TIMER_BELOW_MINIMUM`.
- At most 366 days ahead (`maxTimerDelay`). If it fails: error `TIMER_ABOVE_MAXIMUM`.
- `target_l2_name` must be a canonical L2 name, `<kind>.<name>@<hive>` (`TIMER_INVALID_PAYLOAD`), and the requester's own unless the requester is a SY node (`TIMER_FORBIDDEN`, `checkTimerTarget`).
- `payload` and `metadata` at most 32 KiB each (`TIMER_LIMIT_EXCEEDED`, `checkTimerData`).
- A new timer is refused when its owner has 10,000 pending timers, or the hive 100,000 (`TIMER_LIMIT_EXCEEDED`, `checkPendingLimits`). A `client_ref` that names a pending timer of the requester is still answered with it. The owner's count reads only its own pending timers, in `idx_timers_list_owner_status` (§5.2; since v1.7, FINDINGS A-326): on the builder's Mac 1.8 ms with 100,000 pending timers in the hive, where before it read every one of them, 58 ms. The hive's count reads `idx_timers_status`'s pending entries, as before, about 10 ms at 100,000 there.
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
| `cron_spec` | yes | Standard cron expression (5 fields: min hr dom mon dow), with nothing before them: its zone goes in `cron_tz` (§13). |
| `cron_tz` | no | IANA timezone to interpret the cron. Default: `UTC`. |

**Validation:**
- `cron_spec` must parse correctly. One that starts with `TZ=` or `CRON_TZ=` (a zone, which goes in `cron_tz`) or with `@` (a descriptor) is refused before it is parsed (§13). Error `TIMER_INVALID_CRON`.
- The interval between fires must be `>= 60s`. Error `TIMER_BELOW_MINIMUM`.
- `cron_tz` must be a valid IANA zone. Error `TIMER_INVALID_TIMEZONE`.
- `missed_policy` is validated and stored, but a recurring timer never applies it (§10).
- `target_l2_name`, the sizes of `payload` and `metadata`, and the pending caps: as `TIMER_SCHEDULE`. A recurring timer has no maximum delay.

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

A cancel dates the timer (`canceled_at_utc`, since v1.5, FINDINGS A-238): it is kept 7 days from then (§5.2). A cancel by `timer_uuid` of a timer canceled already answers the same, and keeps its first date.

A cancel takes the timer out of the scheduler's heap at once (§4.3, since v1.6, FINDINGS A-259); before, its entry stayed there until its old fire time. A fire of a recurring timer that is being sent again (§7.8) is dropped at its next attempt, as before.

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

The 60 s minimum and the 366-day maximum apply to the `new_fire_at` too.

The timer's entry in the scheduler's heap moves to the new time: a timer is in the heap once (§4.3, since v1.6, FINDINGS A-259). Before, each reschedule added an entry, and the old one stayed until its time.

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

A field without a value (`client_ref`, `cron_spec`, `cron_tz`, `missed_within_ms`, `last_fired_at_utc_ms`, `metadata`, `canceled_reason`) is left out.

`canceled_reason` (since v1.4, FINDINGS A-220) says why `SY.timer` canceled the timer itself: a one-shot its missed policy dropped at a start (§10), or a recurring timer whose next fire cannot be computed (§11), as in `"canceled_reason": "its next fire cannot be computed: invalid cron: cron_spec starts with CRON_TZ=: ..."`. A timer its owner canceled has none. Such a timer is no longer active: by `client_ref` it is `TIMER_NOT_FOUND`, and its owner reads it by `timer_uuid`, or lists its `canceled` timers (§7.6). Like any canceled timer, it is deleted at the first start more than 7 days after it was canceled (§5.2; before v1.5, 7 days after its last fire or its creation, FINDINGS A-238). The Go SDK's `TimerInfo` carries it as `CanceledReason` (since 0.1.130, FINDINGS A-239), and the Rust one keeps it in `extra`; a raw read, `SY.admin`'s routes and Archi show it.

Read by `timer_uuid`: by the owner, and by `SY.admin@motherbee` for any timer (§12.1). Another node gets `TIMER_NOT_OWNER`.

Read by `client_ref`: owner-scoped. `SY.timer` searches only within `(routing.src_l2_name, client_ref)` and returns `TIMER_NOT_FOUND` if there is no active timer of the requester with that reference.

### 7.6 `TIMER_LIST`

Lists the requester's timers; `SY.admin` lists any owner's, or the whole hive's. No parameters = the requester's `pending` ones (the hive's, for `SY.admin`).

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
| `owner_l2_name` | Optional. A node may name only itself (`TIMER_FORBIDDEN` otherwise); `SY.admin`, any owner. |
| `status_filter` | `"pending"`, `"fired"`, `"canceled"`, or `"all"` (default `"pending"`). |
| `limit` | Maximum number of results. Default 100, maximum 1000. |
| `cursor` | Optional. The `next_cursor` of the page before: where this page starts. |

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_LIST",
    "owner_l2_name": "WF.onboarding@motherbee",
    "count": 100,
    "timers": [ { ... }, { ... }, ... ],
    "next_cursor": "1775577600000:9d8c6f4b-2f4d-4ad4-b5f4-8d8d9d9d0001"
  }
}
```

`owner_l2_name` is optional, and the response echoes the owner it listed: the requester when a node leaves it out, `null` when `SY.admin` lists the whole hive. Either way by `fire_at`, then by uuid, subject to `status_filter` and `limit` (§12.1). Each timer is as `TIMER_GET` answers it, `canceled_reason` included (§7.5). `next_cursor` is there only while timers follow the page's last one, as here, where the `limit` of 100 cut the list (Pages, below).

**Pages** (since v1.3, FINDINGS A-151; the limit's since v1.6, FINDINGS A-266). A page ends at the request's `limit` or where the frame does, whichever comes first: a response fits one 128 KiB frame, so a page holds at most 96 KiB of timers, encoded (`maxTimerListPageBytes`).
- While timers follow the page's last one, `next_cursor` says where the next page starts (`"<fire_at_utc_ms>:<uuid>"` of the page's last timer), whichever cut ended the page. `SY.timer` reads one timer past the `limit` to know, and never lists it.
- The same request with `cursor` set to it lists the next page: the timers right after that one, in the same order, under the same filters. A caller that wants `limit` timers in all lowers `limit` by what came.
- The page that lists the rest of the list has no `next_cursor`, even when it holds exactly `limit` timers.

Before v1.6 only the frame's cut gave `next_cursor`: a page its `limit` cut, 100 timers without one, read as the whole list. A list in pages is no snapshot: a timer whose fire time moves between two pages (a recurring timer's fire, a `TIMER_RESCHEDULE`) takes its new place in the order, so it can come again on a later page, or not at all. A response has no `total`. The cursor is a place in the order, never a timer `SY.timer` looks up: the timer it names canceled or deleted since, the next page still starts right after its place, and a well-formed cursor `SY.timer` never gave lists the timers after the place it names.

**What a page reads** (since v1.7, FINDINGS A-326). A page reads its timers from the index that holds its filters' timers in the list's order (§5.2), from its cursor's place on, and stops at the first timer it cannot take, past its `limit` or past its frame, which says more follow: it reads the timers it lists and one more, whatever the hive holds. On the builder's Mac (SQLite and the encoding, no router) a page took 1.5 to 2.7 ms (medians) whatever its filter, with 15,000 or 90,000 pending timers of other owners in the hive. Before v1.7 a page of an owner's timers in one status read every timer of the hive in that status, every owner's, and sorted the owner's: 15 to 22 ms a page with 15,000 pending timers of other owners, 47 ms with 90,000 (medians); a page of `all` read and sorted every timer of the owner, its history included; `SY.admin`'s pages of the whole hive, every timer in the status, or the whole table; and a page read 1001 timers when its `limit` was 1000, where about 228 of a WF's fit its frame. `SY.timer` serves one request at a time, so each page held up the other nodes' requests that long.

Both SDKs' `List` follow the pages up to their own limit, lowering it by what came, from the filter's `cursor` on (§14). Each SDK also says where the timers past its list start (FINDINGS A-278, 0.1.138): the Go SDK's `ListPage` lists one page, one `TIMER_LIST`, with its `NextCursor`, where Go's `List` returns the timers only; the Rust SDK's `list` hands back `next_cursor` when it stops at its limit with timers following (before, it dropped it), and its `list_page` lists one page. A caller that wants every timer asks again from that cursor until a page has none. So does a WF's reconcile: it reads every page of its pending timers ([spec-systemnode-wf-v1_1](spec-systemnode-wf-v1_1.md) §11.4). Until 0.1.137 it read 1000 at the most, and took the ones of its index past the 1000th as missing from `SY.timer`, though `SY.timer`'s answer said more followed. `SY.admin`'s `timer_list` answers one page, its `next_cursor` included, and takes `cursor` for the next. A cursor that is not one is refused, `TIMER_INVALID_PAYLOAD`, which SY.admin's route answers as HTTP 400 (FINDINGS A-251).

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
- The deleted pending timers leave the scheduler's heap at once (§4.3, since v1.6, FINDINGS A-259): the deletion is one transaction, the owner's pending timers first, whose uuids come back (`DELETE ... AND status='pending' RETURNING uuid`), then the rest, its history, deleted and counted without being read; `deleted_count` is both. The entries of the pending ones are taken out after it. It takes no timer lock: a fire of one of them that has not saved yet is not sent; one saved before the purge is sent; either way no entry is left behind (§4.4). Before, their entries stayed until their old fire times.
- **Its time.** The orchestrator waits 3 s for the answer, and a purge that misses them stops the node's removal (`TIMER_PURGE_FAILED`, §12.3). The delete takes each of the owner's timers out of the table and out of every index (§5.2), so it grows with the owner's timers, its history included, and with the indexes; the pages it changes past the page cache spill to the WAL as it goes, so it grows with the cache's shortfall too. Measured on a Mac with `SY.timer`'s SQLite (the medians of 10 runs, two series of 5 taken in turn, on 2026-11-12 with a load of about 2.7 on 16 cores; its timers and the others' scheduled ten at a time, interleaved over time):

  | The owner's timers, among the hive's pending | v1.6 | v1.7 as first built | v1.7, SQLite's default cache | v1.7 |
  |---|---|---|---|---|
  | 7,500 pending, among 18,000 | 116 ms | 202 ms | 176 ms | 166 ms |
  | 7,500 pending and 20,000 fired, among 18,000 | 288 ms | 836 ms | 630 ms | 424 ms |
  | 10,000 pending, among 100,000 | 384 ms | 697 ms | 635 ms | 554 ms |
  | 10,000 pending and 20,000 fired, among 100,000 | 652 ms | 1,368 ms | 1,128 ms | 892 ms |

  "v1.7 as first built" kept `idx_timers_owner` and `idx_timers_fire_at`; v1.7 drops them (§5.2), and its connections have a 16 MiB page cache (§5.2, the operator's call of 2026-11-12, FINDINGS A-326), where v1.6 and the two columns before v1.7 ran with SQLite's default of 2,000 KiB. The slowest run of the last case took 1.20 s on v1.7 and 1.18 s with the default cache. The review of A-326 measured the same cases on a busier Mac: the last one's median 0.91 s on v1.6, 1.98 s as first built and 1.50 s with the default cache, its slowest run 2.06 s; and with a load of 12 on 16 cores single runs at 100,000 pending took 3.1 to 5.1 s, on v1.7 and on v1.7 as first built, and none over 1.6 s on v1.6. The owner's history counts most: `SY.timer` deletes it only at a start (§5.2), so an owner that fired many timers since the last start purges them all. `timer_owner_purged` gives the delete's own time, `took_ms` (since v1.7, §16.4).

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
- `actual_fire_at_utc_ms` is captured right before each attempt to send the event; a retry carries its own.
- `is_last_fire` is `true` for a one-shot; for recurring timers it is reserved for a termination criterion (in v1 always `false` for recurring timers; reserved for v2).
- `user_payload` is exactly what the client passed in `TIMER_SCHEDULE.payload.payload`, unmodified (`{}` when there was none).
- `metadata` is also returned as is, so that the consumer can have the context the creator left. `client_ref` and `metadata` are left out when the timer has none.
- The target must NOT answer with `TIMER_RESPONSE`: `TIMER_FIRED` waits for no ack from the target. What tells `SY.timer` that a fire did not arrive is the router's `UNREACHABLE`, or the target's `NOT_TAKEN` (below).
- `SY.timer` takes a fire as delivered when neither the router has answered it `UNREACHABLE` nor its target `NOT_TAKEN` within 30 s.
- There is no dead-letter queue: a fire that is not delivered in time is logged `timer_fire_lost`, and its owner covers it (a WF reconciles its timers when it starts).

**Delivery, as the code does it** (`fireTimerByUUID`, `deliver`, `undelivered`, `retryLater` in `scheduler.go`; FINDINGS A-115, since 0.1.99):
- **Saved first, at most once.** A one-shot is saved `fired`, and a recurring timer its next instance, before the fire is sent: a stop between the two loses that fire instead of sending it twice. A row that cannot be saved sends nothing, and the fire is tried again in 30 s (`timer_fire_persist_failed`); so is one that cannot be read (`timer_fire_read_failed`, since v1.6, §4.3).
- **Sent again while the target is down.** Each attempt goes under a new `trace_id`. When the router answers it `UNREACHABLE` because the target is not connected (`NODE_NOT_FOUND`, `PEER_UNAVAILABLE`, `WAN_UNAVAILABLE`, `GATEWAY_UNAVAILABLE`), or because its link queue is full (`LINK_BUSY`, the router from 0.1.129, FINDINGS A-222: a refusal that clears as the target reads; SY.timer retries it since v1.4, 0.1.127), or the send itself fails, the same fire (its `timer_uuid`, `scheduled_fire_at_utc_ms` and `fire_count`) is sent again after 5, 10, 20 and 40 s, then each minute (`timer_fire_undelivered`). So is a fire its target answers `NOT_TAKEN` with `retryable` true: the Go SDK of a target whose queue for system messages is full (`node_busy`, since 0.1.141, FINDINGS A-329), with a log line at each attempt queued (a fire no longer on time is lost, `timer_fire_lost`); only the fire's target decides it, by the name its router stamps, and a `NOT_TAKEN` that is not retryable is not sent again (`timer_fire_refused`). Before 0.1.141 a Go target (a WF) dropped a fire its queue of 64 could not hold, and nothing came back. A fire the router refuses for another reason (`VPN_BLOCKED`, `OPA_*`, `FRAME_TOO_LARGE`) is not (`timer_fire_refused`), nor one over the 128 KiB frame. A retry goes only after the router or the target said the earlier attempt did not arrive, so a node gets a fire at most once.
- **While it is on time.** The retries stop an hour after the first attempt; sooner for a `fire_if_within` one-shot (its window from its scheduled time) and for a recurring timer (its next instance). A `drop` one-shot is not sent again. A fire past its time is logged `timer_fire_lost`. A retry whose timer was purged or canceled since is dropped (`timer_fire_dropped`).
- **In memory.** The retries are not saved: a fire `SY.timer` is retrying when it stops is lost. A WF covers a fire it lost when it starts: it reconciles its timers with `SY.timer` and runs each one whose time passed (`Recover`, `go/nodes/wf/wf-generic/node/recover.go`).
- **Overdue at start.** When `SY.timer` starts, a one-shot whose time passed under `missed_policy` `fire` or `fire_if_within` (§10) is queued, due at once. It goes when the scheduler starts and, after a hive reboot, is sent again until its owner is relaunched.

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
    "version": "1.7",
    "description": "System node providing persistent timers and authoritative time/timezone services.",
    "operations": [
      {
        "verb": "TIMER_SCHEDULE",
        "description": "Create a one-shot timer, 60 seconds to 366 days ahead. Its fire goes to the requester; only a SY node may name another target_l2_name. If client_ref already matches an active owned timer, return the existing timer instead of creating a duplicate.",
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
| `fire` | It is queued, due at once: it fires when the scheduler starts, in order of `fire_at`, and is sent again until its target takes it (§7.8). | It is rescheduled with the original `fire_at`. |
| `drop` | It is canceled (`status = 'canceled'`), with a `canceled_reason` and a `timer_missed_policy_applied` event. | It is rescheduled normally. |
| `fire_if_within` | If `now() - fire_at_utc <= missed_within_ms`, it fires. If not, it is canceled, with a `canceled_reason`. | It is rescheduled normally. |

A one-shot whose policy is unknown, or `fire_if_within` without its window, is canceled. Each one canceled says why in its `canceled_reason` (§7.5), since v1.4: `missed: its fire time passed while SY.timer was down, and its missed_policy is drop`, `... more than its missed_within_ms (600000 ms) ago`, and so on.

The same policy bounds how long a fire whose target is down is sent again (§7.8): `fire` up to an hour, `fire_if_within` within its window, `drop` not at all.

For **recurring timers**, `missed_policy` is stored but never applied:
- The **next instance** is computed from `now()` onward according to `cron_spec`.
- Every instance that should have fired during the downtime, the one that was due when it went down included, **does not fire**, whatever the `missed_policy`. This avoids firing storms when coming back from long downtimes.
- One whose next instance cannot be computed is canceled, with its `canceled_reason` (§11).

---

## 11. Restart Semantics — Detail

At node startup:

1. Open `timers.db`. If it does not exist, create the schema. Create the indexes the table lacks (§5.2): the first start of v1.7 builds the four `idx_timers_list*` over the table it finds (FINDINGS A-326), and then drops `idx_timers_owner` and `idx_timers_fire_at` (the review of A-326). An index it cannot build is reported, and the start goes on without it (step 5).
2. Delete the history older than 7 days, once (`gcHistoricalTimers`), in one transaction: first `UPDATE timers SET canceled_at_utc = now WHERE status='canceled' AND canceled_at_utc IS NULL` (a timer canceled before v1.5, or by v1.4 after a rollback, is dated at this start, §5.2), then `DELETE FROM timers WHERE (status='fired' AND COALESCE(last_fired_at_utc, created_at_utc) < now - 7 days) OR (status='canceled' AND canceled_at_utc < now - 7 days)`. Before v1.5 a canceled timer went by its last fire or its creation too (FINDINGS A-238). A GC that fails is logged, changes nothing, and the start goes on. Nothing deletes history again until the next start.
3. `now = time.Now().UTC()`; `SELECT <timerColumns> FROM timers WHERE status='pending' ORDER BY fire_at_utc ASC` (`listPendingTimers`; every read and write of a timer names its columns, §5.2).
4. For each timer:
   - Recurring: recompute `fire_at_utc` from `now` with `cron_spec` and `cron_tz`, update the DB, push to the heap (§10). One whose next fire cannot be computed, such as one kept before 0.1.124 whose `cron_spec` starts with a zone (§13), is canceled (`cancelUnschedulable`): `status='canceled'`, with `canceled_reason` `its next fire cannot be computed: <the error>`, logged once as `timer_replay_invalid_recurring` with `outcome: canceled`. Its `client_ref` is free for a new timer, and it no longer counts toward its owner's and the hive's pending timers (§19). Since v1.4 (FINDINGS A-220); before, it stayed `pending` and out of the queue: a `TIMER_SCHEDULE_RECURRING` with its `client_ref` answered the dead timer with `already_existed: true` and kept nothing of the new spec, and the replay logged it at every start.
   - One-shot with `fire_at_utc > now`: insert it into the in-memory heap with the same `fire_at_utc`. **The `fire_at_utc` is not modified** — it is already the absolute target instant, and the heap fires it when the OS clock reaches that value.
   - One-shot with `fire_at_utc <= now`: apply `missed_policy` (§10). If it fires: it is queued, due at once, and fires as any due timer (§4.3, §7.8). If it is dropped: save `status='canceled'` with its `canceled_reason`, and log a `timer_missed_policy_applied` event whose `outcome` says why.

A recurring timer whose next fire cannot be computed when it fires is canceled the same way, logged once as `timer_recurring_next_fire_failed`, and that fire is not sent; a row that cannot be saved stays `pending` and is tried again in 30 s (`timer_fire_persist_failed`). A save that fails at the replay stops the start, as any other there: systemd starts `SY.timer` again.
5. Log `SY.timer ready: ... pending=N gc_deleted=M gc_stamped=S` (S: the canceled timers step 2 dated, none once every one has its date), start the tick loop, and serve requests. When step 2 failed, the line says `gc=failed` in place of both counts (`startupGC`, since v1.5): a failed GC returns 0 for both, which would read as one that found nothing to delete or to date. When the count of pending timers fails (`countPendingTimers`, after the replay), the line says `pending=failed`, and `timer_startup` gives `pending_error` in place of `pending_count` (`startupPending`, since v1.6, FINDINGS A-259): before, it said `pending=0`, next to a separate error line. A start that built or dropped an index in step 1, or could not, says so after the counts: `indexes_built=B indexes_dropped=D indexes_ms=T`, and `indexes_failed=F` when it could not build or drop F of them; `timer_startup` names them, `indexes_built`, `indexes_dropped` and `indexes_failed` with each one's error, and gives `indexes_ms`, step 1's time for them (`startupIndexes`, since v1.7, the review of A-326). The first start of v1.7 on an older table says `indexes_built=4 indexes_dropped=2`, and the first start on a new database `indexes_built=5 indexes_dropped=0`; a start that did none of it, as most do, says nothing of the indexes.

**Guarantee:** a timer created with `fire_at_utc = T` will fire at time `T` of the OS clock, subject to the node's availability and to its `missed_policy`. There is no accumulated drift.

---

## 12. Authorization and Ownership

### 12.1 General rule

For `TIMER_SCHEDULE`, `TIMER_SCHEDULE_RECURRING`:
```
if target_l2_name != routing.src_l2_name and routing.src_l2_name is not SY.*:
    return error TIMER_FORBIDDEN
```

For `TIMER_CANCEL`, `TIMER_RESCHEDULE`, `TIMER_GET`:
```
if timer.owner_l2_name != routing.src_l2_name:
    return error TIMER_NOT_OWNER   // TIMER_GET: unless the requester is SY.admin@motherbee
```

For `TIMER_LIST`:
```
if routing.src_l2_name != SY.admin@motherbee:
    owner_l2_name defaults to routing.src_l2_name; another owner: TIMER_FORBIDDEN
query: WHERE owner_l2_name = <owner_l2_name>, or the whole hive for SY.admin
```

**Reads are the owner's, and SY.admin's** (FINDINGS A-115, since 0.1.99; `mayRead`). A node reads and lists its own timers, payload and metadata included. `SY.admin@motherbee` (`PrimaryAdminNode`) reads every timer, for the operator and Archi (`timer_get`, `timer_list`). The router gates no `TIMER_*` verb (they are not in `PROTECTED_SYSTEM_ACTIONS`): `SY.timer` checks the router-stamped name itself.

### 12.2 The single exception

For `TIMER_PURGE_OWNER`:
```
if routing.src_l2_name != "SY.orchestrator@<local-hive>":
    return error TIMER_FORBIDDEN
```

The check is done by the L2 name of the sender: `routing.src_l2_name`, which the router stamps on every message a node sends and the sender cannot set (`resolveSourceL2NameFromMsg`). `SY.timer` reads no router SHM and no `.uuid` file. A message without the stamp is one the router made itself (`UNREACHABLE`, `TTL_EXCEEDED`): it gets no answer, only a log line, bounded (`isInfrastructureMessage`, §6.1; `timer_message_ignored`, §16.4), and an `UNREACHABLE` for a fire in flight sends that fire again (§7.8). The owner of a new timer is taken the same way. This is consistent with the authorization model of the rest of the system nodes.

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
- Special descriptors like `@reboot`, `@yearly`: a `cron_spec` that starts with `@` is `TIMER_INVALID_CRON`
- Interval expressions like `every 5m` (reserved for v2 if demand shows up)
- A zone before the fields, `TZ=<zone>` or `CRON_TZ=<zone>`, which robfig/cron reads: the zone goes in `cron_tz`, and a `cron_spec` that starts with `TZ=` or `CRON_TZ=` is `TIMER_INVALID_CRON` before it is parsed (`parseCronSpec`; FINDINGS A-206, since 0.1.124). The parser took such a zone over `cron_tz`, which the timer still recorded, and panicked, stopping `SY.timer`, when no space followed it. A recurring timer kept with one before 0.1.124 is canceled at the first start of v1.4 (0.1.127) or later, with its `canceled_reason` (§11, FINDINGS A-220); before, it was skipped at every start and stayed `pending`.

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

// --- Lists (§7.6) ---
type ListFilter struct {
    OwnerL2Name  string // optional: a node lists its own timers only
    StatusFilter string // "pending" (default), "fired", "canceled", "all"
    Limit        int    // default 100, at most MaxTimerListLimit
    Cursor       string // optional: a NextCursor given back, where the list starts (FINDINGS A-278)
}

type TimerListPage struct {
    Timers     []TimerInfo
    NextCursor string // while timers follow the page's last one; "" once it lists the rest
}

// List follows SY.timer's pages up to filter.Limit and returns the timers only.
func (c *TimerClient) List(ctx context.Context, filter ListFilter) ([]TimerInfo, error)
// ListPage is one TIMER_LIST, one page as SY.timer answers it, and where the next page starts.
func (c *TimerClient) ListPage(ctx context.Context, filter ListFilter) (TimerListPage, error)
func (c *TimerClient) ListMine(ctx context.Context, filter ListFilter) ([]TimerInfo, error)

// --- Introspection ---
func (c *TimerClient) Help(ctx context.Context) (*HelpDescriptor, error)

// --- TIMER_FIRED ---
func ParseFiredEvent(msg Message) (*FiredEvent, error)
```

The Go client has no `TIMER_PURGE_OWNER`: only `SY.orchestrator` sends it (§7.7).

### 14.3 Rust

`fluxbee_sdk::timer` (`crates/fluxbee_sdk/src/timer.rs`) has the same surface: a `TimerClient` built on the node's `RouterDispatcher` with `TimerClient::new_with_dispatcher`, with the time helpers (`now`, `now_in`, `convert`, `parse`, `format`, `help`), scheduling (`schedule`, `schedule_in`, `schedule_recurring`), reading (`get`, `get_by_client_ref`, `list`, `list_mine`, `list_page`), `cancel`, `reschedule` and their `_by_client_ref` forms, and `purge_owner`. `list` hands back `next_cursor` when it stops at its limit with timers following, and `list_page` lists one page, `next_cursor` included, as Go's `ListPage` (FINDINGS A-278, §7.6). `parse_timer_response` and `parse_timer_fired_event` read `TIMER_RESPONSE` and `TIMER_FIRED` (`FiredEvent`). `ai.generic` asks `TIMER_NOW_IN` through it for its `get_current_time` tool ([agent-tools-v1](agent-tools-v1.md) §6.1).

### 14.4 Retry of time operations

The time operations (`Now`, `NowIn`, `Convert`, `Parse`, `Format`; in Rust `now`, `now_in`, `convert`, `parse`, `format`) make up to three attempts, each waiting for its answer 100 ms, 300 ms and 1 s in turn (`TimeRetrySchedule`, `time_retry_schedule`), about 1.4 s in all. When none answers, the Go client fails with `TIMER_UNREACHABLE`, whose message names its last attempt's error (the router's reason with it since 0.1.130, FINDINGS A-228), and the Rust client with its last attempt's error (a timeout, or the router's `UNREACHABLE` or `TTL_EXCEEDED`). An answer with an error is not retried. Scheduling, reading and mutations are sent once, with the call's own timeout (5 s by default).

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

A node should check that a `TIMER_FIRED` comes from `SY.timer@<its hive>` (the router-stamped `routing.src_l2_name`) before it acts on one. A fire comes at most once: a retry carries the same `timer_uuid` and `fire_count`, and goes only when the router said the earlier attempt did not reach the node, or the node's own SDK answered it `NOT_TAKEN` (§7.8).

---

## 15. Error Codes

| Code | Description |
|---|---|
| `TIMER_BELOW_MINIMUM` | Requested duration lower than 60 seconds. |
| `TIMER_ABOVE_MAXIMUM` | A one-shot more than 366 days ahead, scheduled or rescheduled. |
| `TIMER_LIMIT_EXCEEDED` | A `payload` or `metadata` over 32 KiB; or a new timer when its owner has 10,000 pending timers, or the hive 100,000. |
| `TIMER_INVALID_CRON` | `cron_spec` does not parse correctly, or starts with `TZ=`, `CRON_TZ=` or `@`: a zone goes in `cron_tz`, and v1 has no descriptors (§13). |
| `TIMER_INVALID_TIMEZONE` | `tz` / `cron_tz` is not a valid IANA zone. |
| `TIMER_INVALID_PAYLOAD` | The request `payload` is not valid JSON or required fields are missing. |
| `TIMER_NOT_FOUND` | No timer has that `timer_uuid`; or, by `client_ref` (`TIMER_GET`, `TIMER_RESCHEDULE`), the requester has no pending timer with it. |
| `TIMER_NOT_OWNER` | The requester is not the owner of the timer it reads (`TIMER_GET`, unless it is `SY.admin`), cancels or reschedules. |
| `TIMER_ALREADY_FIRED` | A fired one-shot cannot be canceled; a timer that is not pending (fired or canceled) cannot be rescheduled. |
| `TIMER_RECURRING_NOT_RESCHEDULABLE` | Reschedule is not allowed on recurring timers in v1. |
| `TIMER_FORBIDDEN` | Restricted operation: another node's `target_l2_name` from a requester that is not a SY node, another owner's `TIMER_LIST` from a node that is not `SY.admin`, or `TIMER_PURGE_OWNER` from a node that is not the orchestrator. |
| `TIMER_STORAGE_ERROR` | Internal SQLite error. Not in `TIMER_HELP`'s list (§9). |
| `TIMER_UNKNOWN_VERB` | A verb SY.timer does not know. |
| `TIMER_INTERNAL` | A response that does not fit in one 128 KiB frame, or another internal error. A message without the router's `routing.src_l2_name` gets no answer at all (§6.1). |
| `TIMER_UNREACHABLE` | (Go client) `SY.timer` does not answer after the retries of a time operation (§14.4). |

Every refusal is logged in `SY.timer`'s own log too, besides the answer its sender gets (`timer_request_refused`, §16.4; since v1.6, FINDINGS A-258): its verb, its code and its sender, and for a `TIMER_STORAGE_ERROR` the database's error, at most 10 lines a minute per sender. Before, only the sender heard of it.

A refusal's code goes with its error, not in its text (`timerError` in `go/sy-timer/errors.go`; FINDINGS A-220, since v1.4): the `message` quotes what the request sent, a `cron_spec` or a zone, and what it quotes never changes the code. Before, the code was read from the message: a junk `cron_spec` that repeated "invalid timezone" in its fields came back `TIMER_INVALID_TIMEZONE`, and a `cron_tz` that read "minimum timer duration", `TIMER_BELOW_MINIMUM`. They are `TIMER_INVALID_CRON` and `TIMER_INVALID_TIMEZONE` now, in that order.

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

SY.admin's read-only timer routes (docs/nodes/sy-timer.md, "SY.admin HTTP routes") answer SY.timer's refusals by what they say (`error_code_to_http_status`, FINDINGS A-251): `TIMER_INVALID_PAYLOAD` and `TIMER_INVALID_TIMEZONE` 400, `TIMER_NOT_FOUND` 404, `TIMER_STORAGE_ERROR` 502, `TIMER_UNKNOWN_VERB` 501 (an SY.timer older than the route), and `TIMER_INTERNAL` 500, each with SY.timer's code as `error_code` and its message as `error_detail`.

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

`SY.timer` logs on stdout (captured by systemd), with no levels: plain lines, and one JSON object per event (`logEvent`). Among the events: `timer_startup` (with `pending_count`, or `pending_error` when the start could not count the pending timers; and `gc_deleted` and `gc_stamped`, the canceled timers the start dated, §5.2, or, when the start's GC failed, `gc_error` in place of both counts; and, when the start built or dropped an index or could not, `indexes_built`, `indexes_dropped`, `indexes_ms` and `indexes_failed`, since v1.7; §11), `timer_scheduled`, `timer_fired`, `timer_canceled`, `timer_rescheduled`, `timer_owner_purged` (with `deleted_count`, and `took_ms`, the delete's own time, since v1.7, §7.7), `timer_missed_policy_applied` (with its `outcome`), `timer_handler_error` (with the request's `verb`, its sender `src_l2_name`, since v1.6, and its `trace_id`); for the delivery (§7.8) `timer_fire_read_failed` (since v1.6, §4.3), `timer_fire_persist_failed`, `timer_fire_emit_failed`, `timer_fire_undelivered`, `timer_fire_refused`, `timer_fire_lost` and `timer_fire_dropped`; and for recurring timers `timer_recurring_requeued`, `timer_replay_invalid_recurring` and `timer_recurring_next_fire_failed`; the last two are a timer whose next fire cannot be computed, at a start or at a fire, canceled: logged once, with `outcome: canceled`, its `owner_l2_name`, its `client_ref` and its `canceled_reason` (§11). The router's `UNREACHABLE` for a fire sends it again (§7.8), and so does its target's retryable `NOT_TAKEN` (since 0.1.141, with a plain line once the next attempt is queued, *"fire of <uuid> not taken by <target> (node_busy): sent again at <hh:mm:ss.mmmZ>"*; a fire no longer on time, a `drop` one-shot's at once, is lost instead, *"fire of <uuid> lost: ..."* and `timer_fire_lost`); one for no fire in flight is logged as an ignored message (`timer_message_ignored`, below).

Every request `SY.timer` refuses gets one `timer_request_refused` line (`refusalLog` in `go/sy-timer/refusals.go`; since v1.6, FINDINGS A-258; the operator's call on A-253: logs for everything): its `verb`, its `error_code`, its sender (`src_l2_name`, the router's stamp) and its `trace_id`. Not its `message`, which can quote what the request sent (FINDINGS A-232), except a `TIMER_STORAGE_ERROR`'s: its text is the database's (a full disk, a read-only file, a busy database), never the request's, and the line gives it as `error`, the one place besides the requester that says why `SY.timer` could not serve it (the review of A-258). An unknown verb is cut to 64 bytes. The lines are bounded per sender, so a node that loops a refused request cannot flood the journal: at most 10 in a window of a minute from the sender's first refusal, the tenth with `not_logged_until_utc_ms`, the window's end. The sender's other refusals in that window are counted, and one `timer_request_refusals_not_logged` line gives them when the window ends (a sweep every 10 s, or the sender's next refusal): `src_l2_name`, `count`, `since_utc_ms`, `until_utc_ms`, and the `last_verb` and `last_error_code` of the last one. A sender gets at most 10 refusal lines a minute and that count; the windows of senders refused no more leave memory with their count. A response that cannot go at all (over the frame) is answered `TIMER_INTERNAL` and logged as `timer_handler_error`, as before, with its sender since v1.6. Before v1.6 a refusal reached its sender only.

A message that is no request (§6.1: a notice, a reply, or one the router made) gets no answer and one `timer_message_ignored` line (`ignoredLog`, the same file): its name (`msg`, cut to 64 bytes), its sender (`src_l2_name`, empty for a message the router made: those share one window) and its `trace_id`. The lines are bounded the same way, in windows of their own: at most 10 a minute per sender, then one `timer_messages_ignored_not_logged` line with the `count` and the `last_msg`. Before v1.6 each got a plain line, with no bound: a node that looped notices or replies at `SY.timer` wrote one line per message.

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
| Strict ownership by L2 name | Isolation between nodes; `SY.admin` only reads |
| `TIMER_PURGE_OWNER` only for the orchestrator | A single, traceable exception for node teardown |
| Standard cron syntax | Known by LLMs with no extra learning |
| Generic `TIMER_RESPONSE` response | Consistency with `CONFIG_RESPONSE` and the rest of the control plane |
| `TIMER_FIRED` does not wait for an ack | No ack from the target: the router's `UNREACHABLE`, or the target's retryable `NOT_TAKEN` (since 0.1.141), is what sends a fire again, while it is on time |
| A fire is saved before it is sent | It never goes twice; a stop in between loses it, and its owner reconciles |
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
| Maximum delay | 366 days | One-shots (`TIMER_ABOVE_MAXIMUM`); a recurring timer has none |
| Pending timers | 10,000 per owner, 100,000 per hive | A new timer past either is `TIMER_LIMIT_EXCEEDED`. About 10,000 per hive is the load it was designed for |
| `TIMER_LIST` maximum | 1000 results | Per request; default 100, and a larger `limit` is cut to 1000. A page ends at the `limit` or at what fits in one 128 KiB frame, 96 KiB of timers, and either way `next_cursor` lists the next while timers follow (§7.6, FINDINGS A-151, A-266) |
| Retention of fired/canceled timers | 7 days | From a fired timer's last fire, and a canceled one's cancellation (since v1.5, FINDINGS A-238). Deleted when `SY.timer` starts, never in between (§5.2). A constant |
| Firing precision | ±5 s | Best-effort, subject to load |
| `payload` and `metadata` size | 32 KiB each | `TIMER_LIMIT_EXCEEDED`; with both, a `TIMER_FIRED` fits in the router's 128 KiB frame |
| Delivery retries | up to 1 hour | 5, 10, 20 and 40 s, then each minute; within its window under `fire_if_within`, none under `drop` (§7.8) |
| Refusal log lines, and ignored messages' | 10 per sender per minute, each | Then the rest are counted, and one line gives their number when the minute ends (§16.4, FINDINGS A-258). Constants |
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
- Retries that survive a restart of `SY.timer`: they live in memory (§7.8). A lost fire is its owner's to cover. An agent owns no timers: it is a pure responder, and timers live in a WF ([agent-tools-v1](agent-tools-v1.md) §6.2–§6.3). A WF reconciles its timers with `SY.timer` when it starts and runs each one whose time has passed (`Recover`, `go/nodes/wf/wf-generic/node/recover.go`; [wf-orchestration-v1](wf-orchestration-v1.md) §2, §4.4).

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
