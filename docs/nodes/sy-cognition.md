# SY.cognition

SY.cognition keeps the conversational memory of a hive's AI agents. Its router copies the turns
its `AI.*` agents send and receive, and what its WFs relay between people; SY.cognition keeps each
communication's last 20 turns, taken as they arrive, and tags the turns with the hive's LLM to
keep the rest of its state (contexts, reasons, co-occurrences, scopes, memories, feelings). It
makes the episodes, social events between people that every hive's cognition keeps as public
news. It publishes memory packages in a shared-memory region that the router attaches to
deliveries to agents, and keeps its state in a file of its own on its hive. The agents' recent conversation comes from here: ai.generic's
immediate memory stays off for them.

Source: `src/bin/sy_cognition.rs` and `src/bin/sy_cognition/` (the two LLM calls). The model
(communications, packages, retention): [docs/reference/cognition.md](../reference/cognition.md).

| | |
| --- | --- |
| Binary | `/usr/bin/sy-cognition` (`packaging/build-deb.sh`, `RUST_BINS` entry `sy-cognition:sy_cognition`) |
| Node name | `SY.cognition@<hive_id>` (`FLUXBEE_NODE_NAME` or `SY_COGNITION_NODE_NAME` override the base name) |
| Role | motherbee and worker |
| systemd unit | `sy-cognition.service` (`Restart=always`, `RestartSec=5`) |
| Listens on | the local router socket only; no TCP port |
| Writes | POSIX shared memory `/jsr-memory-<hive_id>` (`memory_shm_name_for_hive`) |
| State | `/var/lib/fluxbee/nodes/SY/SY.cognition@<hive>/`: `config.json` and `cognition.db`, its own file (the folder 0700, the file 0600) |

## Where it runs and how it starts

- `SY.cognition` is in the release's lists for the motherbee and the worker (and their `wait_for`
  lists, `src/release_settings.rs`). The binary has no role check; the router feeds
  it only on motherbee and worker hives (`role_runs_cognition` in `src/router/stream.rs`).
- From `hive.yaml` it reads `hive_id` and `dist.path`. Its provider and model, read at the start
  (`cognition_ai_settings`), are the installation file's once its hive holds one, and its
  hive.yaml `ai` section's while it holds none ([hive-config-update-v1](../hive-config-update-v1.md)
  §4); the start logs which, with the file's version and hash. hive.yaml's `ai` is read untyped,
  and typed only while the hive holds no file. Settings it cannot use (an installation file no
  reader can use, never a fallback to hive.yaml; or an invalid hive.yaml `ai` while the hive holds
  no file) never stop it: it logs the reason as an error and runs on, every turn's semantic work
  skipped with that reason, as without a key (`degraded_ai_settings_invalid`). A file that
  arrives after it started is read at its next start. A new version of the file restarts it
  through its hive's orchestrator (the update with category `config`) only when its engine, the
  default provider with that provider's model, changed, or at its hive's first config update,
  when no record says what it runs; another provider's model restarts nothing
  ([hive-config-update-v1](../hive-config-update-v1.md) §4, "As built (stage 3c)";
  [SY.orchestrator](sy-orchestrator.md#the-installation-file)).
- Its AI key comes from SY.vault on the motherbee (`VaultClient::for_primary`): the resource type
  is the hive's provider (`openai` or `anthropic`) in the root tenant. It waits up to 60 s for
  the vault at start. Without a key it runs degraded and skips turns.
- It opens its file (without it, it does not start), opens the memory region and fills it with
  the most recent communications of the file, then starts taking turns.

## The interlocutor model

A communication is `(thread_id, agent ILK)` (`CommunicationKey`): memory is kept per
communication, never per hive or per node, and is owned by the agent that serves it. Two agents
in the same thread are two communications. The agent's ILK comes from the identity SHM by its
node name; a turn without a thread id, or from an agent without an ILK, is skipped.

The feed: the agent's own router copies both directions as `cognition.turns` records of its
stream (what the agent sends, at entry; what it receives, at delivery) and the stream delivers
them to `SY.cognition@<same hive>` (`sinks_for` in `src/router/stream.rs`). At the next turn of
a communication it does not hold, SY.cognition loads its state from its file.

Two paths take each turn:

- **The intake,** in the `STREAM_BATCH` handler, adds the turn to its communication's last turns:
  who spoke (`in` or `out`), the sender's ILK, the node it came through (where its cycle started,
  the first entry of its trace, `meta.context.trace`), its text cut at 2,000 characters, its files, the option an answer chose from a
  WF's menu, and its time. It writes the communication to its file and rewrites the memory region
  at once, so the agent's next message carries the turn before it, whatever the worker's backlog.
  20 turns at most, and 48 KiB.
- **The worker** takes the turns one at a time, with up to two model calls each (about 5 s a
  turn), and derives the labels, memories and feelings, and the episodes that pass the gate. A
  person's message the agent got first waits for the agent's answer, 2 s at most (below). It
  loads a communication it does not hold
  from its file first; then it works on a copy, so the intake never waits for it, and writes the
  copy back merging the turns the intake took meanwhile, then to its file. Its communication stays
  held while it works, even when it does not fit the region.

**A turn the agent refused** (FINDINGS A-292, since 0.1.137). The router copies a message for
cognition once the agent's link takes it, before the agent reads it. Since 0.1.136 a busy AI
runtime refuses a turn at once, without running it, and answers `ai_runtime_busy`. That answer is
no turn:

- **The intake** takes it by its exact shape (`is_busy_answer`): sent by the agent; its payload
  `{"type": "error", "code": "ai_runtime_busy", "message", "retryable": true}` and nothing else;
  its trace ending with the agent's entry, whose one act is `busy`.
- **It drops the turn the agent refused** from its communication's last turns, in the file and
  the region, with the turn's key: so the package never shows the message as said to the agent
  and unanswered, and the message sent again is one turn.
  - The refused message is told apart by its `trace_id` and its trace as it reached the agent
    (`request_of`), which the answer carries before the agent's entry: a WF can write to the agent
    twice in one cycle.
  - A turn stands for every copy of its message the agent got: a WF can send it one text twice in
    one cycle, under one key. While the agent took one of them, the turn stays.
  - One a WF relayed to a person too stays, as that relay ([cognition-hands-v1](../cognition-hands-v1.md)
    §12.9).
- **The worker** never gets the refusal, and does not analyze the refused turn. One a WF relayed
  to a person too it analyzes once, as that relay: the relay itself, or the agent's copy when the
  relay came after it and was its duplicate. When the refusal comes while it analyzes the turn, it
  drops what the turn derived and calls no model it has not called yet; a turn that is a relay
  again keeps it.
- **The worker waits for the agent first** (FINDINGS A-332, since 0.1.139). It analyzes a
  person's message the agent got once the agent answered or refused it, or 2 s after the intake
  took it (`COGNITION_REFUSAL_GRACE`), whichever comes first (`wait_for_the_agent`).
  - The intake wakes it at each answer and refusal it takes, each told apart by the message it
    answers, as the refusal is.
  - So a refusal costs no model call. Up to 0.1.138 the worker, with nothing else to do, started
    the turn's tagger call at once. The refusal came in the stream's next batch, some 0.04 s later,
    and only dropped the call's result.
  - A turn that waited longer than that in the queue does not wait again. The agent's own answers,
    the relays and the turns with no person at their other end never wait.
  - A cognition that cannot call a model (its `ai` settings unusable, or no key) waits too, and
    then skips the turn. The wait costs no call, and a refused turn is counted as refused, not as
    a tagger failure.
  - A refusal that comes after the grace finds the call made, and drops its result as before.
    When that call fails (no answer from the provider in time), the turn counts as a provider
    error (`semantic_tagger_provider_errors_total`), not as a refused turn not analyzed.
- **It remembers** the last 4,096 refusals (`COGNITION_REFUSALS_KEPT`), in memory only: a full
  turn queue's drain at about three refusals a second. Past that, a refused turn that still waits
  for the worker is analyzed, as before 0.1.137, and stays out of the last turns.
- **The journal** names the refusals at info: the first at once, with its `trace_id` and what it
  left (`turn`: `dropped`, `relay_again`, `kept_for_another_copy` or `no_turn_held`), then at most
  one line every 10 s with how many came since (`refusals`), the end of a burst included (the
  heartbeat names it). The counters are in `STATUS` and, since 0.1.139, in `CONFIG_GET`'s
  `runtime` (below).
- **What it does not change:** a refusal the runtime could not send (its direct lane full) leaves
  the turn as before, and so does one of a turn a cognition older than 0.1.137 took. A message the
  router delivers to the agent before cognition takes the refusal still carries the refused turn
  in its package: the router reads the package as it delivers.
- **What it does not see:** whether the sender was told. The router copies the agent's answer for
  cognition as the agent sends it, before routing it, so the turn drops even when the answer never
  reaches the sender (its link full, `LINK_BUSY`), or reaches a person who does not read it as
  "send it again": io.slack shows any error answer as `invalid_text_payload` until its revamp
  (FINDINGS A-291). The agent's next package then lacks the message, where up to 0.1.136 it showed
  it unanswered.

Details: [docs/reference/cognition.md](../reference/cognition.md) §3.4.

## Messages

| Message | Who may send / where it goes | What it does |
| --- | --- | --- |
| `STREAM_BATCH` → `STREAM_ACK` | only a router stream of its own hive (`take_stream_batch`) | takes the `cognition.turns` records and skips repeats by stream and epoch; adds them to their communications' last turns, in their order, and takes an agent's refusal (`ai_runtime_busy`) as no turn, dropping the turn it refused; writes those to its file and rewrites the memory region; then queues the turns for the worker and acks, without waiting for it; a full turn queue (256, `COGNITION_TURN_QUEUE`) drops turns and counts them |
| `STREAM_APPEND storage.usage` (sent) | to its router's stream | one usage record per LLM call, charged to the agent's tenant, or the writer's in a communication with no agent |
| `COGNITION_EPISODE` (sent and taken) | a system broadcast to `SY.cognition@*`, taken only from a cognition (`take_episode`) | an episode it made, which every hive's cognition keeps once ([docs/reference/cognition.md](../reference/cognition.md) §4.1) |
| `COGNITION_CLEAR` → `COGNITION_CLEAR_RESPONSE` | only `SY.admin@motherbee`, as its router names it, and never a tap's copy (`take_clear`); SY.admin's `cognition_clear` | drops every communication and every episode it keeps, in memory and in its file, and writes the memory region empty; answers what it removed and keeps running ([docs/reference/cognition.md](../reference/cognition.md) §5.1) |
| `CONFIG_GET`, `CONFIG_SET`, `PING`, `STATUS`, `NODE_STATUS_GET` | `CONFIG_*` only from `SY.admin@motherbee` (router system policy) | control and status |
| `VAULT_SECRET_CHANGED` | broadcast | re-probes the AI key and updates its state; it does not exit |

`CONFIG_SET` refuses secrets and any AI provider or model setting (`reject_cognition_secret_fields`;
those are the installation's, in its installation file). It accepts `thresholds.context_close`,
`thresholds.reason_close` and the semantic tagger's `timeout_ms`, `max_tags`,
`max_reason_signals`.

## State

- `cognition.db`, its own SQLite file: one row per communication with its whole state (its open
  items, scope, memories, feelings, last turns and counters), written at every intake batch and
  after every worker turn, and one per live episode. Only SY.cognition reads it; only a clear
  deletes a communication, all of them at once
  ([docs/reference/cognition.md](../reference/cognition.md) §5 and §5.1,
  [docs/cognition-hands-v1.md](../cognition-hands-v1.md) §11).
- In memory: the communications of the memory region; any other comes back from the file at its
  next turn.
- `/jsr-memory-<hive>`: the memory packages, at most 4 MiB of data (`MEMORY_MAX_DATA_SIZE` in
  `src/shm/mod.rs`); the router reads it when it delivers a message to an agent. Each package
  carries its communication's last turns, so the region holds some 90 communications when every
  turn is 2,000 characters long. It does not survive a host reboot: the start fills it again from
  the file.
- `fluxbee-factory-reset` has every hive's SY.cognition drop its own through SY.admin's
  `cognition_clear`, the motherbee's included, the node running (`clear_cognitions`, FINDINGS
  A-160): a stopped system unit is started again within seconds by SY.orchestrator's watchdog.

## Failure behavior

- Fail-open: without an AI key, or when the tagger fails, the worker skips a turn (no labels,
  memories or feelings). Its text is in the last turns all the same: the intake needs no model.
- A failed model call is counted by what it was: the provider gave no answer (an error status
  after the AI SDK's retries, no connection, none in time, an answer with no text, a refusal),
  which cognition does not judge (`*_provider_errors_total`); an answer that is not the object asked for
  (`*_invalid_outputs_total`); anything else (`*_failures_total`). See
  [cognition.md](../reference/cognition.md) §3.1 (FINDINGS A-184).
- A communication whose file row cannot be read is skipped and counted
  (`store_load_failures_total`); a file that cannot be opened stops the node. A thread's
  communication with no agent that cannot be read is not taken by the thread's first agent, which
  starts without it (cognition-hands-v1 §12.6).
- The orchestrator's boot waits up to 60 s for it to appear on the router and only warns if it
  does not.

## Health

- `systemctl status sy-cognition`; `journalctl -u sy-cognition` (`sy.cognition looked up its AI
  key`, `loaded its most recent communications from its file`, warnings when the key is not
  resolvable or a communication cannot be read or written).
- `STATUS`/`PING` report `state` (`ready_semantic_ai`, `degraded_no_ai_provider` or
  `degraded_ai_settings_invalid`, `cognition_state_label`), the degraded reasons
  (`ai_settings_invalid`, `ai_provider_missing`), where the provider and the model came from
  (`ai_provider.settings`: `{from: installation_file, version, sha256}` or `{from: hive_yaml}`,
  with `error` when they cannot be used; `config.ai.settings` in `CONFIG_GET`), the turn counters,
  and the clears it took (`clears`).
- Among the turn counters: the agents' refusals (`refusals_total`, `refused_turns_dropped_total`,
  `refused_turns_not_analyzed_total`) and the worker's waits for the agent (`agent_waits_total`,
  `agent_waits_grace_over_total`). Since 0.1.139 all five are in `CONFIG_GET`'s `runtime` too.
- Through the admin, which reaches the node's `CONFIG_GET` and not its `STATUS`:
  `POST /hives/{hive}/nodes/SY.cognition@{hive}/control/config-get`.

## Links

- [docs/reference/cognition.md](../reference/cognition.md): the model in detail.
- [rt-gateway](rt-gateway.md): the stream that feeds it and the memory packages.
- [SY.storage](sy-storage.md): its durable state.
