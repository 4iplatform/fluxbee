# SY.cognition

SY.cognition keeps the conversational memory of a hive's AI agents. Its router copies the turns
its `AI.*` agents send and receive; SY.cognition tags them with the hive's LLM and keeps state
per communication (contexts, reasons, co-occurrences, scopes, memories, episodes). It publishes
memory packages in a shared-memory region that the router attaches to deliveries to agents, and
sends its durable state to SY.storage.

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
| State | `/var/lib/fluxbee/nodes/SY/SY.cognition@<hive>/config.json`; durable copy in SY.storage |

## Where it runs and how it starts

- `SY.cognition` is in the `system_nodes.motherbee` and `system_nodes.worker` lists (and their
  `wait_for` lists) of `/etc/fluxbee/hive.yaml`. The binary has no role check; the router feeds
  it only on motherbee and worker hives (`role_runs_cognition` in `src/router/stream.rs`).
- From `hive.yaml` it reads `hive_id` and the `ai` section (provider and model).
- Its AI key comes from SY.vault on the motherbee (`VaultClient::for_primary`): the resource type
  is the hive's provider (`openai` or `anthropic`) in the root tenant. It waits up to 60 s for
  the vault at start. Without a key it runs degraded and skips turns.
- It opens the memory region and reloads from SY.storage the communications that region still
  lists, then starts taking turns.

## The interlocutor model

A communication is `(thread_id, agent ILK)` (`CommunicationKey`): memory is kept per
communication, never per hive or per node, and is owned by the agent that serves it. Two agents
in the same thread are two communications. The agent's ILK comes from the identity SHM by its
node name; a turn without a thread id, or from an agent without an ILK, is skipped.

The feed: the agent's own router copies both directions as `cognition.turns` records of its
stream (what the agent sends, at entry; what it receives, at delivery) and the stream delivers
them to `SY.cognition@<same hive>` (`sinks_for` in `src/router/stream.rs`). On the first turn of
a communication it does not hold, SY.cognition loads its state from SY.storage.

## Messages

| Message | Who may send / where it goes | What it does |
| --- | --- | --- |
| `STREAM_BATCH` → `STREAM_ACK` | only a router stream of its own hive (`take_stream_batch`) | takes the `cognition.turns` records, skips repeats by stream and epoch, acks at once; a full turn queue (256, `COGNITION_TURN_QUEUE`) drops turns and counts them |
| `COGNITION_STATE_GET` (sent) | to `SY.storage@motherbee` | loads a communication's durable state, page by page |
| `STREAM_APPEND storage.cognition.batch` (sent) | to its router's stream | the durable envelopes of each turn, in packs of at most 64 KiB (`COGNITION_APPEND_MAX_BYTES`) |
| `STREAM_APPEND storage.usage` (sent) | to its router's stream | one usage record per LLM call, charged to the agent's tenant |
| `CONFIG_GET`, `CONFIG_SET`, `PING`, `STATUS`, `NODE_STATUS_GET` | `CONFIG_*` only from `SY.admin@motherbee` (router system policy) | control and status |
| `VAULT_SECRET_CHANGED` | broadcast | re-probes the AI key and updates its state; it does not exit |

`CONFIG_SET` refuses secrets and any AI provider or model setting (`reject_cognition_secret_fields`;
those are hive-wide, in `hive.yaml`). It accepts `thresholds.context_close`,
`thresholds.reason_close` and the semantic tagger's `timeout_ms`, `max_tags`,
`max_reason_signals`.

## State

- In memory: the communications it holds.
- `/jsr-memory-<hive>`: the memory packages, at most 4 MiB of data (`MEMORY_MAX_DATA_SIZE` in
  `src/shm/mod.rs`); the router reads it when it delivers a message to an agent.
- The durable state lives in SY.storage's `cognition_*` tables; SY.cognition has no database of
  its own. `fluxbee-factory-reset` removes its node directory entries and the region
  (`cognition_paths`).

## Failure behavior

- Fail-open: without an AI key, or when the tagger fails, a turn is skipped (no envelopes, the
  region unchanged).
- A communication that cannot be loaded from SY.storage is skipped and counted.
- The orchestrator's boot waits up to 60 s for it to appear on the router and only warns if it
  does not.

## Health

- `systemctl status sy-cognition`; `journalctl -u sy-cognition` (`sy.cognition looked up its AI
  key`, warnings when the key is not resolvable or a communication cannot be loaded).
- `STATUS`/`PING` report `state` (`ready_semantic_ai` or `degraded_no_ai_provider`,
  `cognition_state_label`) and the turn counters. Through the admin:
  `POST /hives/{hive}/nodes/SY.cognition@{hive}/control/config-get`.

## Links

- [docs/reference/cognition.md](../reference/cognition.md): the model in detail.
- [rt-gateway](rt-gateway.md): the stream that feeds it and the memory packages.
- [SY.storage](sy-storage.md): its durable state.
