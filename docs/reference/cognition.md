# Cognition (SY.cognition): reference

**Status:** as built in 0.1.79 (2026-10-17); each communication's last turns added on 2026-10-30
(§3.4).
**Audience:** developers of AI nodes, the router, SY.storage and anyone who reads agent memory.

This page describes what SY.cognition does today, and names the code that backs each statement.
`docs/router-stream-v1.md` §6 is the as-built design and its decisions (D14–D22).
`docs/history/12-cognition-v2.md` keeps the concepts (context, reason, scope, memory, episode), but it is
stale on keying, limits and transport; where it disagrees with this page, the code wins.

The main source is `src/bin/sy_cognition.rs`, with its two model calls in
`src/bin/sy_cognition/semantic_tagger_ai.rs` and `src/bin/sy_cognition/narrative_summarizer_ai.rs`.
A function or constant named without a path lives in `sy_cognition.rs`. The wire types are in
`crates/fluxbee_sdk/src/cognition.rs`.

---

## 1. In short

- **What it builds:** a memory per communication for each agent: what the conversation is about
  (contexts), what it seeks (reasons), the phases it goes through (scopes), a running summary
  (memories), notable moments (episodes), and its last 20 turns (what the agent received and
  sent).
- **Two paths, two timings:** the last turns are taken when the turn arrives, ahead of any model
  call, so the next message delivered to the agent carries the turn before it (§3.4). Everything
  else comes from a worker that takes the turns one at a time, with up to two model calls each,
  and lags behind when turns come faster than that (§3).
- **Who replaced what:** the agents' recent conversation comes from here, not from ai.generic's
  immediate memory, which stays off for them (FINDINGS A-133). SY.architect keeps the AI SDK's
  immediate memory.
- **A communication** is `(thread_id, agent ILK)` (`CommunicationKey`): one thread, and the
  agent that serves it. Two agents in one thread are two communications. The memory is never
  keyed by hive or by node; the node is only how the router finds the agent.
- **Where it runs:** on the motherbee and on every worker, next to the agents (`hive.yaml`
  `system_nodes.motherbee` and `system_nodes.worker`). Each one handles only the agents of its
  own hive. The ingress and the egress run none.
- **Who reads it:** ai.generic, through the memory package the router attaches to messages it
  delivers to an agent (§7). A durable copy lives in SY.storage (§5).
- **The cost:** up to two model calls per turn, charged to the agent's tenant (§3, §9).

---

## 2. Input: the agent's router copies both directions

The router where an agent node (`AI.*`) is connected copies each turn of that agent's
communications to its own stream, under the subject `cognition.turns` (`src/router/mod.rs`).

**When a turn is copied:**

- **Out:** a local agent sends a message. It is copied when it enters the router
  (the node connection loop, `append_cognition_turn`).
- **In:** the router delivers a message to a local agent. It is copied at delivery, once per
  agent destination (`deliver_locally`). That covers messages from local nodes, from the WAN and
  from the hive's other routers (`handle_message`, `handle_wan_message`, `handle_peer_message`).
- A message between two local agents is copied twice, once for each agent's communication.

**What counts as a turn** (`is_communication_turn`):

- not a `system`, `admin` or `query_response` message (`should_publish_turn`);
- it carries `meta.thread_id`;
- it is not a tap copy (`meta.via_tap`) nor a broadcast.

**The copy** (`cognition_copy`): the message as routed, with the sender's real name in
`routing.src_l2_name`, the action class filled in, and no memory package. The stream record's
`producer` is the agent's node name (`append_cognition_turn`).

**Who receives it** (`sinks_for` in `src/router/stream.rs`):

- `cognition.turns` goes only to `SY.cognition@<the same hive>`, on the motherbee and worker
  roles. On the ingress and the egress it has no sink.
- No node may append it: `subject_allowed` reserves it to the router.

**How SY.cognition takes it** (`take_stream_batch`):

- It accepts `STREAM_BATCH` only from its own hive's streams (`RT.*.stream@<hive>`). A batch from
  anywhere else, or one it cannot read, is dropped without an ack.
- It skips any record at or below the last seq it took from that stream and epoch
  (`stream_seen`, `stream_repeats_skipped_total`).
- It takes each new `cognition.turns` record into its communication's last turns, rewrites the
  memory region once for the batch, and appends the thread rows of the communications it touched
  (the intake, §3.4).
- Then it hands each record to a queue of 256 turns (`COGNITION_TURN_QUEUE`). A full queue drops
  the turn and counts it (`dropped_turns_queue_full_total`); the turn stays in its communication's
  last turns.
- It acks the batch with the batch's last seq. The ack waits for the intake, never for the
  worker: what was acked and not processed is lost if SY.cognition stops, except its last turns.
- One task takes the queued turns one at a time, in order (`run_turn_worker`).

The router never waits for any of this: the copy goes to a bounded queue of its stream
(`docs/reference/storage.md`, §3).

---

## 3. A turn

The worker's `handle_turn`, step by step, after the intake took the turn's text (§3.4). A
failure in steps 1 to 6 skips the turn; the message itself was already delivered, and its text
stays in the last turns.

1. **Thread:** `meta.thread_id` must be present (`invalid_turns_total` otherwise).
2. **Agent:** the record's producer is the agent node. SY.cognition maps it to the agent's ILK and
   tenant through its hive's identity SHM, by `handler_node` (`agent_identity_of`,
   `find_ilk_by_handler_node_from_hive_id`).
   - The answer is cached while the SHM's `seq` is unchanged; when the `seq` cannot be read, for
     60 s (`COGNITION_AGENT_ILK_TTL`).
   - An agent node without an ILK in the SHM gets no memory (`no_agent_ilk_total`).
3. **State:** a communication this cognition has not loaded is loaded from SY.storage first
   (§6), even when the intake already holds its last turns (`adopt_stored_state` merges them).
   If the load fails, the turn is skipped (`state_fetch_failures_total`). One held but idle for
   5 minutes is checked against SY.storage again.
4. **Text:** the message's text content (`TextV1Payload`, `extract_turn_text`). A message
   without text gets an empty tag set, and no tagger call.
5. **AI key:** resolved from SY.vault on every turn (§8). Without it the turn is skipped and
   the node reports `degraded_no_ai_provider`.
6. **Tagger:** one model call (§3.1). A failure skips the turn.
7. **Number:** the communication's turn count goes up by one (`turn_count`, in
   `update_thread_state_and_build_envelopes`). That number orders the communication: it is in
   every envelope (`CognitionCommunicationRef.turn`), it marks where scope instances start and
   end, and it guards the durable rows (§5). A skipped turn gets no number.
8. **Derivations:** the thread's first and last seen, contexts, reasons, co-occurrences, scope
   (§4); the thread row goes out in step 10. Steps 7 to 9 run on a copy of the communication's
   state, without the lock the intake takes, so the intake never waits for a model call.
9. **Summarizer:** one model call when the communication has an active scope (§3.2). A failure
   skips only the memory and episode updates of this turn.
10. **Outputs:** the turn's envelopes go to SY.storage through the stream (§5). The copy goes back
    into what is held (`write_back_turn`): its derivations replace the held ones, and the last
    turns stay as the intake left them, with those it took meanwhile. Then the memory region is
    rewritten (§7) and the thread row goes out, built from what is held (`sync_held_state`).

### 3.1 The tagger

`run_semantic_tagger_ai` in `semantic_tagger_ai.rs`:

- **Input:** the turn's text, `max_tags`, `max_reason_signals`, the eight canonical signals, and
  as hints `src_ilk`, `dst_ilk` and `ich`.
- **Output:** a JSON object with `tags`, `reason_signals_canonical` and `reason_signals_extra`.
  Labels are normalized and capped; canonical signals outside the list are dropped.
- **Canonical signals** (`COGNITION_REASON_CANONICAL_SIGNALS`): `resolve`, `inform`, `protect`,
  `connect`, `challenge`, `confirm`, `request`, `abandon`.
- **Limits:** `semantic_tagger.timeout_ms` (default 8000, `COGNITION_DEFAULT_SEMANTIC_TAGGER_TIMEOUT_MS`);
  1024 output tokens (`SEMANTIC_TAGGER_MAX_OUTPUT_TOKENS`); no temperature.
- An answer it cannot parse as that JSON object counts as an invalid output
  (`semantic_tagger_invalid_outputs_total`); any other error, a timeout included, as a failure
  (`semantic_tagger_failures_total`).

### 3.2 The narrative summarizer

`run_narrative_summarizer_ai` in `narrative_summarizer_ai.rs`, called by
`update_memories_and_episodes_for_thread`:

- **When:** the communication has an active scope, with its dominant context and reason.
- **Input:** the dominant context (label, tags), the dominant reason (label, signals), the extra
  signals, the previous memory summary of the scope, and the episode candidate if there is one
  (affect and title, with the previous episode text). It never sees the message text.
- **Output:** `memory_summary`, and `episode_summary` with `episode_reason` exactly when an
  episode candidate was sent. An empty memory summary, or a missing episode text, is an invalid
  output.
- **Limits:** the same `semantic_tagger.timeout_ms`, provider and model as the tagger; 1024
  output tokens (`NARRATIVE_SUMMARIZER_MAX_OUTPUT_TOKENS`).

### 3.3 Failures: fail-open

The policy is published in `STATUS` and `CONFIG_GET` (`degraded_semantics_policy` in
`build_status_payload`):

- no AI key, or a tagger failure: `skip_semantic_derivation_fail_open`. The turn is not
  processed;
- a summarizer failure: `skip_narrative_update_fail_open`. The turn keeps its thread, context,
  reason, co-occurrence and scope updates, without memory or episode;
- there is no fallback without a model, and nothing in the message path waits for cognition;
- none of these touch the last turns, which need no model (§3.4).

### 3.4 The intake: each communication's last turns

`take_stream_batch` takes every turn of a batch into its communication's last turns before the
turn goes to the worker's queue (`intake_turn`, `recent_turn_of`, `take_recent_turns`).

- **When:** as the batch arrives, ahead of the worker and its model calls (about 5 s a turn). The
  region is rewritten and the thread rows go to SY.storage before the batch is acked, so the next
  message the router delivers to the agent, a second later, carries the turn before it, whatever
  the worker's backlog: the stream sends a record to its sinks as soon as it takes it. The message
  being delivered is not among its own turns in practice: the router reads its package right after
  it appends its copy (`deliver_locally`), long before the copy reaches SY.cognition.
- **Which turns:** those with a thread id and an agent ILK, found as the worker finds them
  (`agent_identity_of`, cached); the worker counts the others. A turn with neither text nor files
  is left out: the package would show it empty.
- **What a turn keeps** (`MemoryTurn` in `crates/fluxbee_sdk/src/protocol.rs`):
  - `direction`: `out` when its router names the agent node as the sender, `in` otherwise;
  - `src_ilk`: the sender's ILK, when the message had one;
  - `via`, on `in` only: the node it came in through. For a message a WF forwards, the first node
    of `meta.context.wf.trail`, the IO node it came from; otherwise its sender's L2 name;
  - `text`: `extract_turn_text`, cut at 2,000 characters (`COGNITION_TURN_TEXT_MAX_CHARS`), the
    last one an ellipsis when cut. Empty for a turn with files alone: it is kept, so the agent's
    answer to it keeps its question;
  - `attachments`: how many files it carried; a text that came as a file (`content_ref`) counts as
    one;
  - `ts`: when its router took it (the stream record's time), RFC 3339 in UTC.

  `src_ilk` and `via` come from the message's meta and are cut at 256 characters.
- **How many:** the last 20, oldest first (`COGNITION_RECENT_TURNS`); the 21st pushes out the
  oldest. They also stay under 48 KiB serialized (`COGNITION_RECENT_TURNS_MAX_BYTES`), the oldest
  going first, never the newest: 20 turns of 2,000 characters of Latin text fit; a script of more
  bytes a character keeps fewer. The bound keeps a package and the message it rides on under a
  frame (128 KiB, of which an inline text message takes at most 64 KiB) and a thread row under the
  stream's record limit. A turn the communication holds already (a batch sent again) is not added.
- **A communication not loaded yet** (new, after a restart, or after its agent moved here): the
  intake starts it from the turns the region had for it, if any, and the region keeps the package
  it had, with the new turns, until the worker loads it (§7). The worker's load merges SY.storage's
  turns with the intake's: each once, in the order they were taken (`merge_recent_turns`).
- **Who writes what:** the intake owns the last turns and the last turn's ILKs and channel; the
  worker owns the rest. Both change the held state and publish it in `sync_held_state`, under the
  region's lock and then the state's, so neither loses the other's writes. The worker derives on a
  copy and writes it back keeping the intake's turns (step 10 above).

---

## 4. The entities

Most ids are a SHA-256 of a prefix and their parts (`stable_entity_id`), so the live path and a
rebuild produce the same ids. Those ids include the thread and the agent ILK, never the hive or
the node. Scopes and scope instances get a random UUID when they open.

| Entity | Id (`stable_entity_id` parts) | One per |
| --- | --- | --- |
| Thread | `thread`: thread, agent | communication |
| Context | `context`: thread, agent, label | tag |
| Reason | `reason`: thread, agent, label | reason label |
| Co-occurrence | `cooccurrence`: thread, agent, context label, reason label | context and reason pair |
| Scope | `scope:<uuid v4>` | phase of the conversation |
| Scope instance | `scope_instance:<uuid v4>` | opening of a scope |
| Memory | `memory`: thread, agent, scope id | scope |
| Episode | `episode`: thread, agent, scope instance id, affect | scope instance and affect |

**Thread** (`build_thread_envelope_body`): first and last seen, the turn count, the last
turn's `src_ilk`, `dst_ilk` and `ich`, and the last turns (§3.4). The worker sets the first
three, the intake the last two.

**Contexts** (`update_contexts_for_thread`):

- Each tag the tagger returns is a candidate with score 1.0. A candidate opens or reinforces
  its context: `weight = weight × 0.85 + 1.0` (`COGNITION_CONTEXT_DECAY_FACTOR`).
- An open context the turn does not name decays by 0.85, and closes under
  `thresholds.context_close` (default 0.25).
- Each one also keeps a cumulative average, an EMA (alpha 0.25) and per-ILK weights: the sender
  adds 1.0, the receiver 0.5 (`apply_ilk_participation`).

**Reasons** (`build_reason_candidates`, `update_reasons_for_thread`):

- Each canonical signal maps to a label, such as `resolve` to "seeking resolution".
- Some pairs make a label of their own:
  - `resolve` + `challenge`: "seeking urgent resolution";
  - `inform` + `confirm`: "information verification";
  - `request` + `resolve` without `challenge`: "seeking assistance".
- Same mechanics as contexts: decay 0.75, EMA alpha 0.30, close under `thresholds.reason_close`
  (default 0.25).

**Co-occurrences** (`update_cooccurrences_for_thread`): one per context and reason named in the
same turn. Decay 0.80, EMA alpha 0.35. They close under the average of the two close
thresholds.

**Scope** (`update_scope_binding_for_thread`), the phase of a conversation:

- The dominant context and reason are the open ones with the highest weight; on a tie, the
  scope's own keep it (`select_dominant_context`, `select_dominant_reason`).
- The first turn with both opens a scope and its instance, starting at that turn.
- Each later turn computes a binding: the Jaccard similarity of context tags, times that of
  reason signals, times the cosine of the ILK weights. Both participants weigh 1.0 here
  (`current_turn_ilk_weights`). The scope keeps an EMA of it (alpha 0.25).
- When the dominants change and the EMA is under 0.35 (`COGNITION_SCOPE_UNBIND_THRESHOLD`) for
  two turns in a row (`COGNITION_SCOPE_SUSTAIN_COUNT`), the scope and its instance close and a new
  one opens. From the second turn on only.
- A change that binds well relabels the scope instead.

**Memory** (`update_memories_for_thread`): one per scope.

- Its summary is the summarizer's. Its weight is `weight × 0.97 + max(binding EMA, 0.25)`.
- The communication's other memories decay by 0.97 (`COGNITION_MEMORY_DECAY_FACTOR`).

**Episode** (`build_episode_candidate`, `update_episodes_for_thread`): a fixed gate on the
turn's signals.

| Affect | Extra signal | Canonical signals |
| --- | --- | --- |
| `anger` | `frustration` | `challenge` or `resolve` |
| `escalation` | `escalation` | `protect` or `challenge` |
| `urgency` | `urgency` | `resolve` and `request` |

- An episode needs intensity 7 and evidence 8 (`COGNITION_EPISODE_MIN_INTENSITY`,
  `COGNITION_EPISODE_MIN_EVIDENCE_STRENGTH`). Its summary and reason are the summarizer's.
- One episode per scope instance and affect; a repeat raises its intensity and adds evidence.

---

## 5. Durable copy in SY.storage

**The envelope** (`CognitionDurableEnvelope` in `crates/fluxbee_sdk/src/cognition.rs`):

- `schema_version` 2 (`COGNITION_DURABLE_SCHEMA_VERSION`), `entity_version`, `entity`, `op`;
- `entity_id`, `thread_id`, `agent_ilk`, `turn` (the communication's turn number);
- `hive`, `writer` (the node name), `ts`, and `data` (the entity's fields).

The op is `upsert` or `close`. SY.cognition never sends `delete`.

**The way out** (`append_envelopes`, `pack_envelopes`):

- A turn's envelopes are packed into records of at most 64 KB (`COGNITION_APPEND_MAX_BYTES`),
  under the subject `storage.cognition.batch` (`CognitionEnvelopeBatch`).
- Each pack is a `STREAM_APPEND` to SY.cognition's own router (`fluxbee_sdk::stream::append`).
- The router takes `storage.cognition.*` only from `SY.cognition@<its hive>` (`subject_allowed`),
  and sends it to `SY.storage@motherbee`.
- **The thread row** has two writers: the intake, at every batch, for each communication it took
  a turn of, and the worker, after each turn (`thread_rows`). Both build it from what is held
  when it goes out, numbered with the worker's turn count, and append it before they let go of the
  held state's lock (`sync_held_state`). Rows of an equal turn reach SY.storage in the order they
  were built, and the guard below takes an equal turn, so the stored row never ends on fewer turns
  than the last one sent. A communication the worker has not loaded yet writes turn 0, which the
  guard refuses over a stored row; the worker's load merges those turns back (§6).
- The last turns live only in the thread row's `payload`; `cognition_threads` has no column for
  them. 48 KiB at most (§3.4), so the row fits a pack.

**In SY.storage** (`src/bin/sy_storage.rs`, `stream_rows`):

- Each packed envelope becomes one inbox row under `storage.cognition.<entity>`, as if it had come
  alone. Records of other producers are dropped.
- Each entity has its table: `cognition_threads`, `cognition_contexts`, `cognition_reasons`,
  `cognition_cooccurrences`, `cognition_scopes`, `cognition_scope_instances`,
  `cognition_memories`, `cognition_episodes`.
- Every row but a scope's carries `thread_id`, `agent_ilk` and `source_turn`. A thread row is
  keyed by its communication (`communication_id`). A scope row keeps `source_turn` only.
- **The guard:** an upsert replaces a row only when its turn is equal or later
  (`WHERE ... source_turn <= EXCLUDED.source_turn`). A late retry, or a cognition that restarted
  empty, never rolls a row back.
- A `close` is stored with status `closed` (`normalized_status`).
- The envelope's schema version must be 2, or the row fails (`parse_cognition_envelope`).

Delivery is best effort: what waits longer than the stream's window (120 s) is lost and
counted. `docs/reference/storage.md` has the inbox, the retries and the table columns.

---

## 6. Rebuild over the mesh

SY.cognition never reads PostgreSQL. It asks SY.storage for a communication's state
(`fetch_communication_state`, `fetch_communication_envelopes`).

**The request:** `COGNITION_STATE_GET {thread_id, agent_ilk, after}` to `SY.storage@motherbee`,
5 s per page (`COGNITION_STATE_PAGE_TIMEOUT`).

**The answer** (`answer_cognition_state`, `cognition_state` in `sy_storage.rs`):

- every row of the communication, closed ones included, as the envelopes it wrote;
- the scopes come through their instances, since a scope row has no `thread_id`;
- order: thread, contexts, reasons, co-occurrences, scopes, scope instances, memories, episodes,
  then by id (`COGNITION_STATE_ORDER`);
- pages of up to 64 KB (`COGNITION_STATE_PAGE_BYTES`); `next` points to the last
  `(entity, entity_id)` of the page;
- an empty first page means the communication has no state;
- only a caller named `SY.cognition@*`, as its router stamped it, is answered.

**When it runs:**

- **At start, before the first turn** (`reload_communications_from_shared_memory`): every
  communication the memory region holds, four at a time (`COGNITION_RELOAD_CONCURRENCY`). The
  main loop keeps acking batches meanwhile, and the intake keeps taking their last turns; turns
  wait in the worker's queue.
- **On the first turn of a communication it has not loaded:** after a restart, or when the agent
  moved to this hive. The intake may hold its last turns already.
- **After 5 minutes idle** (`COGNITION_HELD_STALE_AFTER`): a held communication is checked
  before its next turn, because its agent may have moved away and back. The stored state replaces
  the held one when its turn count is higher.

**How it rebuilds** (`build_communication_state`): with the same keys as the live path, so the
next turn finds what it rebuilt.

- Contexts and reasons are keyed by label; co-occurrences by the two labels.
- Memories are keyed by scope; episodes by scope instance and affect.
- The active scope is the open instance with the latest start turn.
- The last turns come from the thread row's payload.

**The last turns at a load** (`adopt_stored_state`, `merge_recent_turns`): the stored turns and
those held (the intake's) or in the region (at the start reload) are merged, each once, in the
order their routers took them, and bounded as at intake. The region is written ahead of SY.storage,
so after a restart its turns may be the newer ones: the reload keeps them and writes the thread row
again when they add to the stored ones. After a host reset the region is gone, and the turns come
from SY.storage alone.

A communication SY.storage has no state for yet stays in the region as it was: its last
envelopes may still be on their way.

---

## 7. The memory region and the memory package

**The region:** `/jsr-memory-<hive>` (`memory_shm_name_for_hive` in `src/shm/mod.rs`).

- Magic `JSMM`, version 1, 4 MiB of data (`MEMORY_MAX_DATA_SIZE`).
- SY.cognition is the only writer (`MemoryRegionWriter`) and refreshes its heartbeat every 5 s.
  The router is the reader (`MemoryRegionReader`).
- It holds a JSON snapshot (`MemoryShmSnapshot`, `schema_version` 2). Each entry is one
  communication: `thread_id`, `agent_ilk`, `agent_node` and its `package`
  (`MemoryShmThreadEntry`).

**The hot set** (`build_memory_hot_set_snapshot`, `retain_memory_hot_set`), rewritten at every
intake batch, after every worker turn and after the reload (`sync_held_state`):

- The communications SY.cognition holds are ranked and kept while they fit the 4 MiB.
- The ranking puts first the communications just updated (the batch's, or the worker's turn),
  which are never dropped. Then: most recent turn (the intake's last, or the worker's
  `last_seen_at`), with an active scope, most live entities, highest turn count.
- A communication the intake holds but the worker has not loaded yet shows the package the region
  had for it, with the turns taken since; one the region had none for shows its turns alone.
- Next come the entries the region had for communications not held, in their previous order. A
  restart never blanks what the agents read.
- A communication that did not fit is dropped from memory too. It reloads from SY.storage if it
  comes back (§6).
- The last turns make the entries bigger: a communication of 20 short turns (about 300 characters
  each) takes some 8 KB more, one of 20 turns of 2,000 characters about 45 KB, so a full region
  holds fewer communications than before (some 90 of the largest).

**The package** (`build_memory_package_for_thread`; `MemoryPackage` in
`crates/fluxbee_sdk/src/protocol.rs`):

- `package_version` 3 (`MEMORY_PACKAGE_VERSION`) and the `thread_id`.
- The dominant context and reason: the open ones with the highest weight.
- Up to 6 open contexts and 6 open reasons, by weight.
- Up to 6 memories by weight, and 4 episodes by intensity.
- `turns`: the last turns, oldest first, as §3.4 keeps them (`MemoryTurn`); left out when there
  are none. The message the package rides on is not among them.
- `truncated` counts the labels, memories and episodes left out, when any were.
- There is no byte budget and no summary cut beyond those counts; the turns are bounded at intake
  (48 KiB).

**What the router does** (`deliver_locally`, `package_for`, `src/router/mod.rs`):

- It attaches a package only to messages delivered to an agent node (`AI.*`) that carry a
  `thread_id` and are not `system`, `admin` or `query_response`.
- The lookup is by `thread_id` and the destination node's name (`agent_node`), one per
  destination.
- A package the sender set is removed for every destination (`prepare_local_delivery`).
- It reads the region at delivery, and reopens it when its heartbeat is older than 30 s
  (`HEARTBEAT_STALE_MS`) or a read fails. When it still cannot read it, the message goes without
  a package.
- If SY.cognition stops, the region keeps its last snapshot, and the router keeps serving it.

**ai.generic** renders the package into a "Conversation memory" block ahead of the user's text,
before a WF's block when the message carries one (`render_memory_package_prompt_block`,
`inject_prompt_blocks_into_text_input` in `nodes/ai/ai-generic/src/bin/ai_node_runner.rs`).

- The block ends with the recent conversation, oldest first: one line per turn, with its time
  (UTC, to the minute) and who spoke. The agent's own turns read "you"; another's read its
  sender's ILK and the node it came through, so a person and a supervisor in one thread read
  apart (`render_memory_turn`).
- The AI SDK cuts nothing of what the model reads ahead of the message: it cuts only the
  immediate memory's history (FINDINGS A-71), which the agents run without. With 20 turns of
  2,000 characters the block passes 40,000 characters (some 10,000 tokens a call, and again on
  each turn of a tool loop), and the WF's block and the message still follow it whole.

---

## 8. Configuration

**Provider and model** come from `hive.yaml` `ai` (`HiveAiConfig::effective`). Without that
section the default is OpenAI with `gpt-5.5` (`HiveAiConfig::fallback`,
`DEFAULT_HIVE_OPENAI_MODEL` in `crates/fluxbee_ai_sdk/src/llm.rs`). `CONFIG_SET` cannot change
them.

**The AI key:** SY.vault's general key for that provider in the root tenant's pool
(`resolve_cognition_ai_api_key`, `resolve_cognition_resource`).

- The vault runs on the motherbee; a worker's cognition asks it there
  (`VaultClient::for_primary`).
- At start it waits for the vault up to `VAULT_BOOT_WAIT`; on each turn it asks once.
- A `VAULT_SECRET_CHANGED` for that provider makes it check the key again
  (`handle_vault_secret_changed_cognition`).
- The key is the root tenant's, but each call is charged to the agent's tenant (§9).

**`CONFIG_SET`** (`apply_cognition_config_set`). The operator sends it through SY.admin
(`node_control_config_set`, `POST /hives/{hive}/nodes/{node_name}/control/config-set`):

- `node_name` must be the node's full name; `apply_mode`, when present, must be `replace`.
- `config.semantic_tagger.timeout_ms`, `max_tags`, `max_reason_signals`: each must be above 0.
  Defaults: 8000 ms, 12 tags, 4 signals.
- `config.thresholds.context_close`, `reason_close`: numbers from 0 to 1, default 0.25
  (`parse_cognition_thresholds`).
  - Any other key is refused. The retired `context_open` and `reason_open` are refused by name.
  - Nothing gates opening: every tag opens a context.
- Refused: `semantic_tagger.provider` or `model`, `ai` or `ai_providers`, and secret fields
  (`reject_cognition_secret_fields`).
- Each accepted `CONFIG_SET` raises `config_version` by one and is saved in the node's managed
  config file (`persist_cognition_config_state`). The saved values are read back at start.

**`CONFIG_GET`** answers the config, the runtime counters and the contract
(`build_cognition_config_get_payload`). **`STATUS`** and **`PING`** answer the state
(`ready_semantic_ai` or `degraded_no_ai_provider`), the counters and the region's use
(`build_status_payload`).

---

## 9. LLM usage records

Every model call SY.cognition makes leaves a usage record (`docs/router-stream-v1.md` §7).

**The account** (`handle_turn`; `AiAccount` in `crates/fluxbee_ai_sdk/src/usage.rs`):

- `AiAccount::charge` with the agent's tenant and `TenantSource::Agent`, recorded as `agent`;
- `node_ilk` is SY.cognition's own ILK; `counterpart_ilk` is the agent's ILK;
- `trace_id` and `thread_id` are the turn's.
- An agent without a tenant in the SHM is charged to the root tenant, as `fallback_root`.

**The record:**

- One per call answered with a 2xx. A call dropped before its answer, such as one cut by the
  timeout, is recorded as `abandoned` (`CallGuard`). A transport error or a non-2xx leaves no
  record.
- `UsageRecorder` appends it to the node's router stream under `storage.usage`. Its queue holds
  1024; past it a record is dropped and counted.
- The router takes `storage.usage` from SY.cognition only on the roles that run it
  (`usage_producer_allowed` in `src/router/stream.rs`).

**SY.storage decides the tenant** (`decide_usage_tenant`, `check_usage_record`):

- A record from SY.cognition says `agent` with a `counterpart_ilk`, or `fallback_root` with the
  root tenant. Anything else is refused (`wrong_source`, `not_root`).
- For `agent`, the tenant charged is the agent ILK's tenant in the motherbee's identity SHM,
  whatever the record claims. If identity cannot answer by the inbox row's last attempt, the
  claimed tenant is kept with `verified = false`.
- For `fallback_root`, the root tenant is charged.

The records land in the `llm_usage` table (`docs/reference/storage.md`).

---

## 10. Counters

`STATUS` reports, under `turns_consumer`:

- the flow: `processed_turns_total`, `invalid_turns_total`, `dropped_turns_queue_full_total`,
  `stream_repeats_skipped_total`, `no_agent_ilk_total`, and `intake_turns_total` (turns the
  intake added to their communications' last turns, §3.4);
- the state loads: `state_fetches_total`, `state_fetch_failures_total`, and the `rebuild_*`
  counters of the start reload;
- the model calls: `semantic_tagger_*` and `narrative_summarizer_*` calls, failures and invalid
  outputs;
- the outputs: `published_entities_total`, `publish_errors_total`;
- the live state: active threads, open contexts, reasons and co-occurrences, active scopes,
  memories and episodes.

Under `shm`: the capacity, the communications kept and pruned, the payload bytes and the last
sync status.

---

## 11. Known gaps

- **FINDINGS A-72:** agents with a definition only work on the motherbee, since their assets do
  not reach the workers (`docs/reference/identity.md` §4). Cognition itself runs on the workers.
- **Acked but not processed:** SY.cognition acks a batch before the worker processes its turns.
  Turns queued when it stops lose their labels, memories and episodes; the stream does not resend
  them. Their text is in the last turns already.
- **A frame over 128 KiB:** the router skips a delivery whose message, with its package, is over
  a frame (`MAX_FRAME_SIZE`), and the agent never gets it. The last turns are bounded (48 KiB) so
  that a package and an inline text message (at most 64 KiB) fit, but a message at that size with
  long memory summaries and a full window of turns can still pass it.
- **The region is read whole:** the router parses the whole snapshot at every delivery to an
  agent (`read_memory_snapshot_for_delivery`), and SY.cognition rewrites it whole at every batch
  and every worker turn. The last turns make it fuller, so each read and write takes longer.
- **`memory.lance`:** `STATUS` and `CONFIG_GET` report a `memory_lance` path
  (`ensure_runtime_paths`), but nothing in SY.cognition uses it.
