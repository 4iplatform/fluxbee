# Cognition (SY.cognition): reference

**Status:** as built in 0.1.79 (2026-10-17); each communication's last turns added on 2026-10-30
(§3.4); its state in its own file since 0.1.103 (§5, docs/cognition-hands-v1.md §11); the people's
relays, the communications with no agent and the episodes, public news, since 0.1.104 (§2, §4.1,
docs/cognition-hands-v1.md §12).
**Audience:** developers of AI nodes, the router, SY.storage and anyone who reads agent memory.

This page describes what SY.cognition does today, and names the code that backs each statement.
`docs/router-stream-v1.md` §6 is the as-built design and its decisions (D14–D22).
`docs/history/12-cognition-v2.md` keeps the concepts (context, reason, scope, memory, episode), but it is
stale on keying, limits and transport; where it disagrees with this page, the code wins.

The main source is `src/bin/sy_cognition.rs`, with its two model calls in
`src/bin/sy_cognition/semantic_tagger_ai.rs` and `src/bin/sy_cognition/narrative_summarizer_ai.rs`.
A function or constant named without a path lives in `sy_cognition.rs`. Its file is
`src/bin/sy_cognition/state_store.rs`, and the memory package's types are in
`crates/fluxbee_sdk/src/protocol.rs`.

---

## 1. In short

- **What it builds:** a memory per communication for each agent: what the conversation is about
  (contexts), what it seeks (reasons), the phases it goes through (scopes), a running summary
  (memories), the feelings of each person in it (§4), and its last 20 turns (what the agent
  received and sent).
- **The episodes:** social events between people, public like the news (§4.1). The cognition
  whose turn showed one makes it and sends it to every hive's cognition; an agent gets those
  about the people of its conversation, and they fade with time.
- **Two paths, two timings:** the last turns are taken when the turn arrives, ahead of any model
  call, so the next message delivered to the agent carries the turn before it (§3.4). Everything
  else comes from a worker that takes the turns one at a time, with up to two model calls each,
  and lags behind when turns come faster than that (§3).
- **Who replaced what:** the agents' recent conversation comes from here: ai.generic no longer has
  an immediate memory of its own (FINDINGS A-133). SY.architect keeps the AI SDK's immediate
  memory.
- **A communication** is `(thread_id, agent ILK)` (`CommunicationKey`): one thread, and the
  agent that serves it. Two agents in one thread are two communications. The memory is never
  keyed by hive or by node; the node is only how the router finds the agent. What people say to
  each other through a WF on a thread no agent served on this hive is the communication with no
  agent, `(thread_id, none)` (§2).
- **Where it runs:** on the motherbee and on every worker, next to the agents (`hive.yaml`
  `system_nodes.motherbee` and `system_nodes.worker`). Each one handles only the agents of its
  own hive. The ingress and the egress run none.
- **Who reads it:** ai.generic, through the memory package the router attaches to messages it
  delivers to an agent (§7). Its state lives in its own file on its hive, which only it reads
  (§5); SY.storage keeps the messages, not cognition's state.
- **The cost:** up to two model calls per turn, charged to the tenant of whoever holds the
  result: the agent's, or the writer's in a communication with no agent (§3, §9).

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
- **Relay:** a local WF sends a message to an IO node, which is how a WF passes what one person
  says to another (`copied_for_cognition_at_entry`; docs/cognition-hands-v1.md §12.4). It is
  copied when it enters the router, with the WF as the record's producer. What a WF sends an agent
  is copied at the agent's delivery, as any turn the agent gets. A solution's WF runs on its
  agents' hive (SY.wf-rules refuses any other definition), so the relay reaches its agents'
  cognition.

**What counts as a turn** (`is_communication_turn`):

- not a `system`, `admin` or `query_response` message (`should_publish_turn`);
- it carries `meta.thread_id`;
- it is not a tap copy (`meta.via_tap`) nor a broadcast.

**The copy** (`cognition_copy`): the message as routed, with the sender's real name in
`routing.src_l2_name`, the action class filled in, and no memory package. The stream record's
`producer` is the agent's node name, or the WF's for a relay (`append_cognition_turn`).

**Who receives it** (`sinks_for` in `src/router/stream.rs`):

- `cognition.turns` goes only to `SY.cognition@<the same hive>`, on the motherbee and worker
  roles. On the ingress and the egress it has no sink.
- No node may append it: `subject_allowed` reserves it to the router.

**How SY.cognition takes it** (`take_stream_batch`):

- It accepts `STREAM_BATCH` only from its own hive's streams (`RT.*.stream@<hive>`). A batch from
  anywhere else, or one it cannot read, is dropped without an ack.
- It skips any record at or below the last seq it took from that stream and epoch
  (`stream_seen`, `stream_repeats_skipped_total`).
- **Whose communication** (`holder_of`): the producer's, for an agent; for a relay, the
  communication whose agent served the thread last on this hive, held or in the file
  (`last_agent_on_thread`), or else the thread's communication with no agent.
  - *Served last* is the newest last turn, as its router took it (`last_turn_epoch_ms`), whatever
    when the worker analyzed it.
  - The relays of a batch find theirs after the batch's other turns are held. The router sends a
    batch as soon as it has a record, one at a time (`run_sink`, src/router/stream.rs), so the two
    copies of one message a WF sends to an agent and to a person can come in one batch or in two:
    - in one batch, in either order, the relay joins that agent's communication, even on a new
      thread;
    - in two, on a thread the agent served, the relay joins its communication too, and the agent's
      copy, whichever comes second, is the same turn;
    - in two, on a new thread, the relay first goes to the thread's communication with no agent and
      the agent's copy to the agent's: two communications, each analyzed (cognition-hands-v1
      §12.6).
- It takes each new `cognition.turns` record into its communication's last turns, writes each
  communication it touched to its file and rewrites the memory region once for the batch (the
  intake, §3.4).
- **One message, one turn:** a turn's key is its cycle (trace id), its writer and its text
  (`turn_key`), and a communication keeps the keys of its last turns (`turn_keys`). A message the
  agent sent, which the WF then relays to the person, comes twice with one key: the second is
  left out, and not analyzed (`duplicate_turns_skipped_total`). An answer of the agent that a WF
  relays in a later cycle, under another trace, is left out too: the agent wrote it, and its
  communication holds an answer with its text (`echoes_own_answer`). When the agent's own copy of
  a message comes after its relay, the turn was said to the agent too: it no longer names where
  else it went (`said_to_the_agent_too`), and the worker's write-back keeps it so
  (`merge_recent_turns` keeps the held copy of a turn, `same_turn`).
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
2. **Holder:** whose communication the turn is (`holder_of`, §2). An agent node is mapped to its
   ILK and tenant through its hive's identity SHM, by `handler_node` (`agent_identity_of`,
   `find_ilk_by_handler_node_from_hive_id`).
   - The answer is cached while the SHM's `seq` is unchanged; when the `seq` cannot be read, for
     60 s (`COGNITION_AGENT_ILK_TTL`).
   - An agent node without an ILK in the SHM gets no memory (`no_agent_ilk_total`).
   - **Who pays** (`payer_of`): the agent's tenant (`TenantSource::Agent`) or, in a communication
     with no agent, the tenant of the turn's writer, by their ILK (`TenantSource::Writer`). A
     turn with neither is not analyzed. The ILK that holds the result pays
     (docs/cognition-hands-v1.md §12.3).
3. **A person, then the state:**
   - **Only a turn with a person at its other end is analyzed** (`person_at_other_end`,
     [cognition-hands-v1](../cognition-hands-v1.md) §6): one the agent got, unless its writer
     (`meta.context.writer`, else its `src_ilk`) is known not to be a person, an ILK the identity
     names `agent` or `system`, or a writer with no ILK, a node; an answer the agent sent, unless
     its `dst_ilk` is. An ILK the identity does not know yet counts as a person's. Any other turn
     stops here: it stays among the last turns, which the intake wrote to the file, unanalyzed
     and unnumbered (`turns_not_analyzed_total`), with no model call.
   - **The state:** a communication this cognition does not hold is loaded from its file (§6),
     where the intake wrote its turn (`held_or_loaded`). If the file cannot be read, the turn is
     skipped (`store_load_failures_total`).
4. **Text:** the message's text content (`TextV1Payload`, `extract_turn_text`). A message
   without text gets an empty tag set, and no tagger call.
5. **AI key:** resolved from SY.vault on every turn (§8). Without it the turn is skipped and
   the node reports `degraded_no_ai_provider`.
6. **Tagger:** one model call (§3.1). A failure skips the turn.
7. **Number:** the communication's turn count goes up by one (`turn_count`, in
   `update_thread_state`): how many of its turns were analyzed. A skipped turn gets no number.
8. **Derivations:** the communication's first and last seen, contexts, reasons, co-occurrences,
   scope (§4). Steps 7 to 9 run on a copy of the communication's state, without the lock the
   intake takes, so the intake never waits for a model call. Until the turn is done, every region
   write keeps the communication held (`worker_turn`), even when it does not fit the region, so
   no intake batch lets it go meanwhile.
9. **Summarizer:** one model call when the communication has an active scope (§3.2). A failure
   skips only the memory of this turn and the text of its feelings: the feelings still move, and
   one opened by this turn has no text yet (§5).
10. **Outputs:** the copy goes back into what is held (`write_back_turn`): its derivations replace
    the held ones, and its turns are merged with those the intake took meanwhile. Then the
    communication is written whole to its file (§5) and the memory region is rewritten (§7), both
    from what is held (`sync_held_state`).
11. **Episode:** an episode the summarizer proposed goes through the gate, and one that passes is
    kept and sent to every hive's cognition (`publish_episode`, §4.1).

### 3.1 The tagger

`run_semantic_tagger_ai` in `semantic_tagger_ai.rs`:

- **Input:** the turn's text, `max_tags`, `max_reason_signals`, the eight canonical signals, and
  as hints `src_ilk`, `dst_ilk`, `ich` and, for a relay, `to`: whom it was written to, the person
  the message names with the IO node it went to, or the node alone (`relay_addressee`). Its rule:
  what the text says to "you" is said to them, never to an assistant (cognition-hands-v1 §12.9).
- **Output:** a JSON object with `tags`, `reason_signals_canonical`, `reason_signals_extra` and
  `affect_signals`. Labels are normalized and capped; canonical and affect signals outside their
  sets are dropped.
- **Canonical signals** (`COGNITION_REASON_CANONICAL_SIGNALS`): `resolve`, `inform`, `protect`,
  `connect`, `challenge`, `confirm`, `request`, `abandon`.
- **Affect signals** (`COGNITION_AFFECT_SIGNALS`, FINDINGS A-157): the writer's own affect, from
  `frustration`, `escalation` and `urgency`, each defined in the prompt; the words a model may vary
  (`angry`, `deadline`) are mapped to them (`normalize_affect_signal_label`). The free extra hints
  never named them, so before 0.1.102 no feeling was ever made.
- **Limits:** `semantic_tagger.timeout_ms` (default 8000, `COGNITION_DEFAULT_SEMANTIC_TAGGER_TIMEOUT_MS`);
  1024 output tokens (`SEMANTIC_TAGGER_MAX_OUTPUT_TOKENS`); no temperature.
- An answer it cannot parse as that JSON object counts as an invalid output
  (`semantic_tagger_invalid_outputs_total`); any other error, a timeout included, as a failure
  (`semantic_tagger_failures_total`).

### 3.2 The narrative summarizer

`run_narrative_summarizer_ai` in `narrative_summarizer_ai.rs`, called by
`update_memories_and_feelings_for_thread`:

- **When:** the communication has an active scope, with its dominant context and reason.
- **Input:** who spoke the turn (`narrator_speaker`: `person Ana Pérez (ilk:…)`, or `the agent,
  answering person Ana Pérez (ilk:…)`), the dominant context (label, tags), the dominant reason
  (label, signals), the extra signals, the previous memory summary of the scope, and the affects
  the turn shows, with their titles and the text so far of the heaviest of them. It never sees
  the message text. The memory names each person as the speaker names them
  (docs/cognition-hands-v1.md §10.3). For an episode, it also reads whom the turn is to
  (`episode_people`: the agent; for its answer, whom it answered; for a relay, the person the
  message names, else the last other person who wrote through the IO node it went to, else that
  node, which names no one), the communication's two leading contexts
  (`leading_contexts`) and how many live episodes of each lens the turn's people already have
  (§4.1). Its rule: the counterpart is whom the turn was written to: the agent, the person the
  agent answered, or, in a turn between people that the agent took no part in, the other person
  or the node they are reached through (cognition-hands-v1 §12.9).
- **Output:** `memory_summary`, and `feeling_summary` with `feeling_reason` exactly when the
  turn shows an affect: one text for the moment, which each feeling the turn shows takes. An empty
  memory summary, or a missing feeling text, is an invalid output. And `episode`, null or a
  proposal: its lens, title, summary, state, base intensity, novelty and evidence; one that does
  not read is dropped, the memory kept (`episode_of_answer`).
- **Limits:** the same `semantic_tagger.timeout_ms`, provider and model as the tagger; 1536
  output tokens (`NARRATIVE_SUMMARIZER_MAX_OUTPUT_TOKENS`).

### 3.3 Failures: fail-open

The policy is published in `STATUS` and `CONFIG_GET` (`degraded_semantics_policy` in
`build_status_payload`):

- no AI key, or a tagger failure: `skip_semantic_derivation_fail_open`. The turn is not
  processed;
- a summarizer failure: `skip_narrative_update_fail_open`. The turn keeps its thread, context,
  reason, co-occurrence, scope and feeling updates, without the memory or the feelings' text;
- there is no fallback without a model, and nothing in the message path waits for cognition;
- none of these touch the last turns, which need no model (§3.4).

### 3.4 The intake: each communication's last turns

`take_stream_batch` takes every turn of a batch into its communication's last turns before the
turn goes to the worker's queue (`intake_turn`, `recent_turn_of`, `take_recent_turns`).

- **When:** as the batch arrives, ahead of the worker and its model calls (about 5 s a turn). Each
  communication it touched is written to the file and the region is rewritten before the batch is
  acked, so the next message the router delivers to the agent, a second later,
  carries the turn before it, whatever the worker's backlog: the stream sends a record to its
  sinks as soon as it takes it. The message being delivered is never among its own turns: the
  router reads its package before it appends its copy (`deliver_locally`, §7).
- **Which turns:** those with a thread id and an agent ILK, found as the worker finds them
  (`agent_identity_of`, cached); the worker counts the others. A turn with neither text, files
  nor a menu choice is left out: the package would show it empty.
- **What a turn keeps** (`MemoryTurn` in `crates/fluxbee_sdk/src/protocol.rs`):
  - `direction`: `out` when its router names the agent node as the sender, `in` otherwise;
  - `src_ilk`: the sender's ILK, when the message had one;
  - `via`, on `in` only: the node it came in through: where its cycle started, the first entry of
    `meta.context.trace` (docs/message-trace-v1.md), the IO node a WF forwards it from; otherwise
    its sender's L2 name;
  - `writer`, on `in` only: who wrote it, its `meta.context.writer`, else its `src_ilk`; and `to`,
    on `out`: whom the agent answered, its `dst_ilk` ([cognition-hands-v1](../cognition-hands-v1.md)
    §6). Each is `{ilk, name, kind}`: the name the message gives, else the identity's display
    name, and the kind the identity's type gives (`person`, `agent`, `workflow` or `system`, none
    for an ILK it does not know), read in the identity memory of the cognition's hive and kept a
    minute (`message_writer::ilk_facts`);
  - `to_via`, on a relay only: the IO node the WF sent it to, its router's destination
    (`relay_destination`), with `to` the person its `dst_ilk` names, when it names one. A relay was
    said to whoever is there, not to the agent (cognition-hands-v1 §12.9, A-161). A WF's
    `send_message` names a person only toward the IO node of whoever started it, and a `forward`
    there sets that person; a `forward` toward any other IO node keeps the `dst_ilk` its message
    had, none for a person's message (A-139);
  - `text`: `extract_turn_text`, cut at 2,000 characters (`COGNITION_TURN_TEXT_MAX_CHARS`), the
    last one an ellipsis when cut. Empty for a turn with files alone: it is kept, so the agent's
    answer to it keeps its question;
  - `attachments`: how many files it carried; a text that came as a file (`content_ref`) counts as
    one;
  - `choice`, on `out` only: what the agent chose from a WF's menu with this answer, from its
    `meta.context.evaluation` (`menu_choice`): the option, and its params as compact JSON, cut at
    2,000 characters. The turn is kept for it even when the reply is blank: the WF may not have
    forwarded the reply, and the choice says what the answer did;
  - `ts`: when its router took it (the stream record's time), RFC 3339 in UTC.

  `src_ilk`, `via` and the option come from the message and are cut at 256 characters. U+0000
  leaves every field (`turn_field`).
- **How many:** the last 20, oldest first (`COGNITION_RECENT_TURNS`); the 21st pushes out the
  oldest. They also stay under 48 KiB serialized (`COGNITION_RECENT_TURNS_MAX_BYTES`), the oldest
  going first, never the newest: 20 turns of 2,000 characters of Latin text fit; a script of more
  bytes a character keeps fewer. The bound keeps a package and the message it rides on under a
  frame (128 KiB, of which an inline text message takes at most 64 KiB). A turn the communication
  holds already (a batch sent again) is not added.
- **A communication not held** (new, after a restart, or let go from a full region, §7): the
  intake loads it from the file, or starts it new, and adds the turn (`held_or_loaded`). When the
  file cannot be read, the turn is kept out (`store_load_failures_total`): a new one would write
  over what the file keeps.
- **Who writes what:** the intake owns the last turns and the last turn's ILKs and channel; the
  worker owns the rest. Both change the held state and write it out in `sync_held_state`: each
  communication changed is written whole to the file, then the region is rewritten, under the
  region's lock and then the state's, so neither loses the other's writes. The worker derives on a
  copy and writes it back merging the intake's turns (step 10 above).

---

## 4. The entities

A communication's state (`ThreadCognitionState`) holds its open items: an item that closes leaves
it (docs/cognition-hands-v1.md §11.2). Most ids are a SHA-256 of a prefix and their parts
(`stable_entity_id`), so an item that comes back gets the id it had. Those ids include the thread
and the agent ILK, never the hive or the node. A scope gets a random UUID when it opens.

| Entity | Id (`stable_entity_id` parts) | One per |
| --- | --- | --- |
| Context | `context`: thread, agent, label | tag |
| Reason | `reason`: thread, agent, label | reason label |
| Co-occurrence | `cooccurrence`: thread, agent, context label, reason label | context and reason pair |
| Scope | `scope:<uuid v4>` | phase of the conversation |
| Memory | `memory`: thread, agent, scope id | scope |
| Feeling | `feeling`: thread, agent, owner ILK, affect, context label | owner, affect and context |

**The communication** (`ThreadCognitionState`): first and last seen, the turn count, the last
turn's `src_ilk`, `dst_ilk` and `ich`, and the last turns (§3.4). The worker sets the first
three, the intake the last two.

**Contexts** (`update_contexts_for_thread`):

- Each tag the tagger returns is a candidate with score 1.0. A candidate opens or reinforces
  its context: `weight = weight × 0.85 + 1.0` (`COGNITION_CONTEXT_DECAY_FACTOR`).
- A context the turn does not name decays by 0.85, and closes under `thresholds.context_close`
  (default 0.25): it leaves the state. One that comes back starts again from its score.
- Each one also keeps a cumulative average and an EMA (alpha 0.25) of its weight, over every turn
  it is open, the turns that only decay it included (`sample_weight`, FINDINGS A-156); and per-ILK
  weights: the sender adds 1.0, the receiver 0.5 (`apply_ilk_participation`).
- `last_seen_at` is the last turn that named it; a turn that only decays it leaves it.

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
- The first turn with both opens a scope. It keeps its pair's labels, tags and signals, which its
  memory and its feelings read, whether or not the pair's items are still held.
- Each later turn computes a binding: the Jaccard similarity of context tags, times that of
  reason signals, times the cosine of the ILK weights. Both participants weigh 1.0 here
  (`current_turn_ilk_weights`). The scope keeps an EMA of it (alpha 0.25).
- When the dominants change and the EMA is under 0.35 (`COGNITION_SCOPE_UNBIND_THRESHOLD`) for
  two turns in a row (`COGNITION_SCOPE_SUSTAIN_COUNT`), the scope closes and a new one opens. From
  the second turn on only.
- A change that binds well relabels the scope instead.

**Memory** (`update_memories_for_thread`): one per scope.

- Its summary is the summarizer's. Its weight is `weight × 0.97 + max(binding EMA, 0.25)`.
- The communication's other memories decay by 0.97 (`COGNITION_MEMORY_DECAY_FACTOR`). Memories
  never close.

**A person's feeling** (`build_feeling_candidates`, `update_feelings_for_turn`;
docs/cognition-hands-v1.md §10.3):

- **Who:** a feeling is its owner's, the person who wrote the turns that show it
  (`feeling_owner`): a turn the agent got whose writer is a person, or an ILK the identity does
  not know yet. The agent's own answers show and move none; neither does a WF's or another
  agent's text, which cognition does not analyze.
- **The gate**, fixed, on the turn's signals, with the active scope's dominant context. Each
  affect has its own, so one turn may show several (`build_episode_candidates`):

| Affect | Affect signal | Canonical signals | Base intensity |
| --- | --- | --- | --- |
| `anger` | `frustration` | `challenge` or `resolve` | 8 |
| `escalation` | `escalation` | `protect` or `challenge` | 8 |
| `urgency` | `urgency` | `resolve` and `request` | 7 |

  It needs intensity 7 and evidence 8 (`COGNITION_EPISODE_MIN_INTENSITY`,
  `COGNITION_EPISODE_MIN_EVIDENCE_STRENGTH`).
- **One per owner, affect and context.** A turn that shows it makes its weight
  `weight × d + s`, the narrator's text replacing its own. Each other turn of the same owner makes
  each of the owner's other open feelings `weight × d`. Other people's turns move none: each
  person's turns are their own feelings' clock.
- **`s`** is the base intensity over 4 (`feeling_step`): 2 for anger and escalation, 1.75 for
  urgency, where a context adds 1.
- **`d`** is a context's decay (0.85) to the root of the affect's inertia
  (`COGNITION_FEELING_INERTIA`, `feeling_decay`), so its half-life is that many times a context's:

| Affect | Inertia | d | Half-life (owner's turns) | Shown once, closed on its owner's turn without it |
| --- | --- | --- | --- | --- |
| `urgency` | ×2 | 0.922 | 8.5 | 24th |
| `anger` | ×5 | 0.968 | 21.3 | 64th |
| `escalation` | ×10 | 0.984 | 42.7 | 128th |

  The operator set two to ten times by feeling (2026-11-05); the lab's numbers are to be checked
  against its code.
- **It closes** under 0.25 (`COGNITION_FEELING_CLOSE_THRESHOLD`) and leaves the state. One that
  comes back starts again from its step.
- **Its averages** follow its weight over its owner's turns, with a context's EMA alpha slowed by
  the inertia (`feeling_ema_alpha`).
- **Its level,** as the agent reads it (`feeling_level`): `strong` from three steps up, `present`
  from one, `faint` under one.

### 4.1 The episodes, public news

Not a communication's: an episode is a social event between people, public like the news, which
belongs to the people it is about (`src/bin/sy_cognition/episodes.rs`; docs/cognition-hands-v1.md
§12, from the lab's POC).

- **Made** in the summarizer's call of any analyzed turn, with no call of its own: it proposes one
  or none (§3.2), and the gate decides (`gate_episode`):
  - a known lens: `friction`, `discomfort`, `appreciation` or `shame` (`EPISODE_LENSES`);
  - a final intensity of 7 or more out of 10 (`EPISODE_MIN_INTENSITY`): the base intensity, a
    point off for every two live episodes of that lens among the same people (the habit, up to
    five), and the novelty, from -2 to 2;
  - evidence of 8 or more (`EPISODE_MIN_EVIDENCE`), anchored in one of the communication's two
    leading contexts;
  - and it must be about a person: an ILK the identity knows as no one else's, or does not know.
  A refused proposal counts in `episodes.gated_out_total`, and SY.cognition's log says at info
  which condition it missed (`refused`: `not_about_a_person`, `unknown_lens`, `no_title`,
  `no_summary`, `no_evidence_signals`, `weak_evidence`, `off_the_leading_contexts` or
  `below_intensity`) with the gate's numbers: the base intensity, the novelty, the habit, the
  final intensity, the evidence's strength and contexts, and the communication's leading
  contexts. Never its title or summary.
- **Its fields** (`Episode`): an id, the lens, a title, a summary (a paragraph: what happened,
  between whom and how it stood, never what someone feels), its state (`resolved`, `pending`,
  `follow_up` or `inconclusive`), its intensity, its birth time, the ILKs of its people (the
  turn's writer and whom the turn was to, `episode_people`), and where it came from.
- **It travels once** (`publish_episode`): kept here, then sent as a system broadcast,
  `COGNITION_EPISODE`, to `SY.cognition@*` with TTL 16, the router's broadcast that crosses
  hives. Each cognition takes it only from a cognition, as its router names the sender, checks
  it (`Episode::checked`) and keeps it once (`take_episode`; `episodes.received_total`,
  `episodes.refused_total`). A hive added later starts with none.
- **It fades with time, not turns:** its weight is `intensity × 0.5^(age / 7 days)`
  (`Episode::weight_at`), and it leaves under 2 (`EPISODE_FADED_BELOW`, `fades_at_ms`). Each
  region write sweeps the faded ones from memory and the file.
- **Kept** in memory (`EpisodeBook`) and in the file's table of episodes (§5), read back at start
  (`load_live_episodes`).
- **Read:** each package carries up to 5 (`PACKAGE_MAX_NEWS`), as `news`: those about the people
  of its communication (`people_of_conversation`: the writers of what its agent got, whom it
  answered and the owners of its feelings), the strongest first (§7). ai.generic shows them with
  their rule: the agent takes them into account to answer, and never repeats them, to the person
  or to anyone.

---

## 5. Its own file

SY.cognition keeps its communications in a SQLite file of its own on its hive
(`CognitionStore` in `src/bin/sy_cognition/state_store.rs`; docs/cognition-hands-v1.md §11). It
is online state: what the next turn needs, never a history. SY.storage keeps the messages and
nothing of cognition's.

- **Where:** `cognition.db` in SY.cognition's folder, `/var/lib/fluxbee/nodes/SY/SY.cognition@<hive>/`
  (`STATUS` and `CONFIG_GET` give its path).
- **Who reads it:** only SY.cognition. The folder is 0700, and the file and its WAL sidecars are
  0600 from their creation; each start makes them so again (`CognitionStore::open`). Every Fluxbee
  process runs as root, so this keeps out every other user, not another root process.
- **One row per communication** (`communications`): the thread and the agent ILK as its key, its
  last activity (`thread_recency_epoch_ms`), the version of its state, and the state as one JSON
  (`ThreadCognitionState`: its open items, the active scope, the memories, the open feelings, the
  last turns and the counters). A row takes about 10 to 60 KB, the turns most of it.
- **One row per episode** (`episodes`): its id, when it fades, and the episode as one JSON
  (§4.1), each kept once (`save_episode`) and deleted when it fades (`sweep_episodes`).
- **Reading a row:** a field the row lacks takes its default, so a field added later keeps the
  rows. A row of an older state version, or one that cannot be read, starts its communication
  again, and is left out at start. Version 2 (0.1.104) names the feelings `feelings`, so the rows
  0.1.103 wrote start again. A row of a newer state version (after a rollback) is kept as
  it is, and its communication's turns are skipped (`store_load_failures_total`) until the newer
  release is back.
- **When it is written:** each communication changed, whole, in one transaction, whenever the held
  state changes (`sync_held_state`): at every intake batch and after every worker turn. So every
  decay is kept (FINDINGS A-156), and a restart takes up where the last turn left off. A write
  that fails (`store_write_failures_total`) leaves its communications held, and the next write
  takes them again.
- **Nothing deletes a communication:** not when it goes quiet, not when its agent is torn down.
  Its memories and feelings involve people (docs/cognition-hands-v1.md §11.2). The file grows with
  each new communication.
- **Durability:** WAL, with `synchronous=NORMAL`. A lost hive loses its file, as it loses its WF
  instances and its agents' own state; the VM snapshots cover it.
- **SY.storage's tables:** until 0.1.103 cognition wrote its items to 8 `cognition_*` tables of
  SY.storage through the stream (`storage.cognition.*`) and read them back over the mesh
  (`COGNITION_STATE_GET`). SY.storage drops those tables once at its start
  (`drop_cognition_tables`), with the inbox rows of their subjects.

---

## 6. Start and loads

- **At start, before the first batch** (`load_most_recent`): the most recently active
  communications with an agent are loaded from the file, up to twice the region's bytes of state,
  and the region is written from them: what does not fit it is let go again. A communication with
  no agent comes back at its next turn: its package reaches no one. So after a restart, or a host reboot
  that emptied the region, the agent's first message carries its memory
  (`loaded_at_start_total`).
- **At the next turn of a communication not held** (`held_or_loaded`): the intake loads it from
  the file before it adds the turn, and the worker before it analyzes one. A communication the file
  has not is new.
- **A file that cannot be read** at a load skips the turn (`store_load_failures_total`); at start
  nothing is loaded, and each communication loads at its next turn. A file that cannot be opened
  stops the node: its state lives there.

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
intake batch, after every worker turn and at start (`sync_held_state`):

- The communications SY.cognition holds are ranked and kept while they fit the 4 MiB. A
  communication with no agent is left out: its package would reach no one.
- The ranking puts first the communications just updated (the batch's, or the worker's turn).
  They stay held even when they do not fit: one that does not fit stays out of the region, not
  out of memory. So do the communications a failed write left unsaved. Then: most recent turn (the intake's last, or the worker's
  `last_seen_at`), with an active scope, most live entities, highest turn count.
- A communication that did not fit is let go from memory too. The file keeps it, and it comes back
  at its next turn (§6).
- The last turns make the entries bigger: a communication of 20 short turns (about 300 characters
  each) takes some 8 KB more, one of 20 turns of 2,000 characters about 45 KB, so a full region
  holds fewer communications than before (some 90 of the largest).

**The package** (`build_memory_package_for_thread`; `MemoryPackage` in
`crates/fluxbee_sdk/src/protocol.rs`):

- `package_version` 5 (`MEMORY_PACKAGE_VERSION`) and the `thread_id`.
- The dominant context and reason: the open ones with the highest weight.
- Up to 6 open contexts and 6 open reasons, by weight.
- Up to 6 memories by weight.
- Up to 12 open feelings (`feelings`), the heaviest first, then the most recently felt
  (`COGNITION_PACKAGE_MAX_FEELINGS`). Each has its owner (ILK, name, kind), affect, context,
  title, level, weight, when its owner last showed it, and the narrator's text cut at 300
  characters.
- Up to 5 `news` (`NewsSummary`): the episodes about the communication's people that still rank,
  the strongest first, then the newest (`EpisodeBook::news_for`, §4.1). Each has its lens, title,
  summary, state, people (ILK, name, kind), birth time, and its weight when the package was
  written. A package written before one faded shows it with its age.
- `turns`: the last turns, oldest first, as §3.4 keeps them (`MemoryTurn`); left out when there
  are none. The message the package rides on is not among them.
- `truncated` counts the labels, memories, feelings and news left out, when any were.
- There is no byte budget and no summary cut beyond those counts; the turns are bounded at intake
  (48 KiB).

**What the router does** (`deliver_locally`, `package_for`, `src/router/mod.rs`):

- It attaches a package only to messages delivered to an agent node (`AI.*`) that carry a
  `thread_id` and are not `system`, `admin` or `query_response`.
- The lookup is by `thread_id` and the destination node's name (`agent_node`), one per
  destination.
- A package the sender set is removed for every destination (`prepare_local_delivery`).
- It reads the package before it appends the turn's copy for cognition, so the message being
  delivered is never among its own recent turns.
- It reads the region at delivery, and reopens it when its heartbeat is older than 30 s
  (`HEARTBEAT_STALE_MS`) or a read fails. When it still cannot read it, the message goes without
  a package, and the router says so in its journal, at most every 30 s.
- **A read** (`read_memory_snapshot` in `src/shm/mod.rs`) copies the payload out between two reads
  of the region's sequence, again when a write moved it, and parses the copy: a read that
  overlaps a rewrite reads again instead of failing.
- **The parsed snapshot is kept** (`MemoryRegionReader::read_snapshot_cached`) until the region's
  sequence moves: SY.cognition rewrites it about twice a turn, and a full one takes milliseconds to
  parse, behind the router's one reader. The sequence moves on every write, heartbeats too (every
  5 s); `updated_at` is not used, since two writes in one millisecond would share it.
- **A package never costs the message** (`serialize_with_package`): when the message with its
  package is over a frame (`MAX_FRAME_SIZE`, 128 KiB), the package loses its oldest turns, as many
  as the excess takes, and then goes whole; the router says so in its journal, at most every 30 s.
  The link skips a frame over the size, so the agent would never get the message.
- If SY.cognition stops, the region keeps its last snapshot, and the router keeps serving it.

**ai.generic** renders the package into a "Conversation memory" block ahead of the user's text,
before a WF's block when the message carries one (`render_memory_package_prompt_block`,
`inject_prompt_blocks_into_text_input` in `nodes/ai/ai-generic/src/bin/ai_node_runner.rs`).

- The block ends with the recent conversation, oldest first: one line per turn, with its time
  (UTC, to the minute) and who spoke (`render_memory_turn`, docs/cognition-hands-v1.md §6):
  - another's turn names its writer: `<kind> <name> (<ILK>)`, the kind `person`, `agent`,
    `workflow` or `system` as the identity says, and `via <node>` after a person's, the node they
    wrote through. So a person, a supervisor, another agent and a WF's own text read apart;
  - the agent's own turns read `you, to <whom>`, or `you` when the turn names nobody;
  - a WF's relay between people adds whom it was said to: `<writer>, to <kind> <name> (<ILK>)
    via <node>`, or `<writer>, to <node>` when it names no one. With any relay among the turns,
    a line ahead of them gives the rule: a line between other people was not said to the agent,
    and its "you" is that person (cognition-hands-v1 §12.9);
  - a turn kept before turns named a writer reads its sender's ILK and the node it came through.
- Each turn's text is a JSON string, so no text can pass for a turn's line of its own; files are
  counted, and an answer that chose from a WF's menu reads `chose <option> <params>`.
- After the blocks, the message being answered is named the same way: `Current user message,
  from <writer>:` (`current_message_label`), a line of the text or a text part of its own. The
  agent reads its writer's kind and name in the identity memory of its own hive.
- The AI SDK cuts nothing of what the model reads ahead of the message: it cuts only the
  immediate memory's history (FINDINGS A-71), which ai.generic never gives it. With 20 turns of
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

- `AiAccount::charge` with the agent's tenant and `TenantSource::Agent`, recorded as `agent`; in a
  communication with no agent, with the writer's tenant and `TenantSource::Writer`, recorded as
  `writer` (`payer_of`);
- `node_ilk` is SY.cognition's own ILK; `counterpart_ilk` is the agent's ILK, or the writer's;
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

- A record from SY.cognition says `agent` or `writer` with a `counterpart_ilk`, or `fallback_root`
  with the root tenant. Anything else is refused (`wrong_source`, `not_root`).
- For `agent` and `writer`, the tenant charged is that ILK's tenant in the motherbee's identity
  SHM, whatever the record claims. If identity cannot answer by the inbox row's last attempt, the
  claimed tenant is kept with `verified = false`.
- For `fallback_root`, the root tenant is charged.

The records land in the `llm_usage` table (`docs/reference/storage.md`).

---

## 10. Counters

`STATUS` reports, under `turns_consumer`:

- the flow: `processed_turns_total`, `invalid_turns_total`, `dropped_turns_queue_full_total`,
  `stream_repeats_skipped_total`, `no_agent_ilk_total`, `intake_turns_total` (turns the
  intake added to their communications' last turns, §3.4), `turns_not_analyzed_total` (turns
  with no person at their other end, §3), `relay_turns_total` (the turns a WF relayed between
  people, §2) and `duplicate_turns_skipped_total` (one message copied twice, §2);
- the model calls: `semantic_tagger_*` and `narrative_summarizer_*` calls, failures and invalid
  outputs;
- the live state: the communications held, their contexts, reasons and co-occurrences, active
  scopes, memories and feelings (`active_feelings_total`).

Under `store`: the file's path, the communications it keeps and its bytes, the communications
loaded at start (`loaded_at_start_total`), and the loads and writes with their failures.

Under `episodes` (§4.1): those kept now (`live_total`), made here (`made_total`), proposed and
refused by the gate (`gated_out_total`), taken from other hives (`received_total`) and refused
from them (`refused_total`).

Under `shm`: the capacity, the communications kept and pruned, the payload bytes and the last
sync status.

---

## 11. Known gaps

- **FINDINGS A-72:** agents with a definition only work on the motherbee, since their assets do
  not reach the workers (`docs/reference/identity.md` §4). Cognition itself runs on the workers.
- **Acked but not processed:** SY.cognition acks a batch before the worker processes its turns.
  Turns queued when it stops lose their labels, memories and feelings; the stream does not resend
  them. Their text is in the last turns already.
- **The region is written whole:** SY.cognition rewrites it at every batch and every worker turn,
  and the router parses it again after each write. The last turns make it fuller, so each write and
  parse takes longer.
- **A lost hive loses its file:** the VM snapshots cover it for now
  (docs/cognition-hands-v1.md §11.2).
- **An episode is not backfilled:** a hive added later, or a cognition that was down when one was
  sent, does not get it. The broadcast is sent once, and episodes fade within weeks.
- **Handing over between agents:** two agents on one thread are two communications, so the agent
  that takes the conversation back sees nothing of the other agent's turns
  (docs/cognition-hands-v1.md §12.6).
