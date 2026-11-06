# Cognition across hands v1: who wrote each turn, and which turns cognition analyzes

**Status:**

- FINDINGS A-137 asks how cognition follows a conversation that passes through several hands.
  The operator opened that chat on 2026-11-05. Its first part was agreed the same day (§3): who
  wrote each turn, which turns cognition analyzes, and how the immediate memory names its
  speakers.
- Built in code the same day, and released in 0.1.101 with A-153 and A-154 (§8), checked on 8.x.
- The rest of A-137 is open (§9), one topic at a time. The first, several people on one thread
  and the feelings, was agreed on 2026-11-05 (§10). It was released in 0.1.102 and checked on
  8.x (§10.5). Its numbers wait for the lab's code.
- Where cognition keeps its state was reviewed whole with the operator and agreed on 2026-11-06
  (§11): its own file on each hive, and SY.storage keeps the messages only. To build next.

This is the single place for the topic.

---

## 1. What it is, from outside

Cognition gives an agent a conversation's history as metadata: abstract cues, feelings, what was
lived. It comes beside the immediate memory, the last turns as they were said. The operator: the
first is for conversations with people; *"las conversaciones entre agentes solo necesitan la
memoria inmediata, nada de análisis de metadata cognitiva"*.

A WF puts more hands in one conversation. **The example** (WF Case 4, with a timer;
docs/wf-orchestration-v1.md §5.4):

- Ana writes on Slack, and the WF forwards her message to sales.
- Sales answers, and the WF forwards the answer to invoice.
- Two days later, a timer fires, and the WF writes to sales itself: "Ana has not answered in 2
  days; send a reminder".

What sales read in its immediate memory before this design (the ILKs are cut short):

```text
- [2026-11-05 10:05] ilk:7c1e… via IO.slack@motherbee: "Me cobraron dos veces la factura 123"
- [2026-11-05 10:05] you: "Ya lo paso a facturación"
- [2026-11-05 10:09] ilk:9a4b… via IO.slack@motherbee: "Nota de crédito NC-55 emitida"
- [2026-11-07 10:09] ilk:7c1e… via WF.soporte@worker1: "Ana no contesta hace 2 días; mandale un recordatorio"
```

The WF's own text carried Ana's ILK. Invoice's answer looked like a second person on Slack.
`you` did not say whom it answered. And cognition analyzed every line, the WF's text and invoice's
as if a person had said them.

What it reads now:

```text
- [2026-11-05 10:05] person Ana Pérez (ilk:7c1e…) via IO.slack@motherbee: "Me cobraron dos veces la factura 123"
- [2026-11-05 10:05] you, to person Ana Pérez (ilk:7c1e…): "Ya lo paso a facturación"
- [2026-11-05 10:09] agent AI.invoice@worker1 (ilk:9a4b…): "Nota de crédito NC-55 emitida"
- [2026-11-07 10:09] workflow WF.soporte@worker1 (ilk:5d0f…): "Ana no contesta hace 2 días; mandale un recordatorio"
```

Cognition analyzes the first two lines only: a person is at their other end.

---

## 2. What exists (verified in the code, 2026-11-05)

- **The key holds.**
  - A communication is `(thread_id, agent ILK)`.
  - The WF puts its instance's thread on every message it sends (`execForward` and
    `execSendMessage`, go/nodes/wf/wf-generic/node/actions.go). So what a person says through a WF
    lands in one communication, the person's thread with the agent.
- **A forward reads right.**
  - It keeps the message's meta, its `src_ilk` and `ich` among it.
  - Cognition names the node a turn came in through by the origin of its trace (`turn_via`,
    src/bin/sy_cognition.rs; A-138).
- **The WF's own text reads as the person's.** A `send_message` to an agent carries the person as
  its `src_ilk`: the agent keeps each interlocutor's state under the `src_ilk`
  (`ThreadStateToolsProvider`, nodes/ai/ai-generic), and that state must be the person's
  (docs/wf-orchestration-v1.md §5.4.3; FINDINGS A-139).
- **An agent's answer that the WF forwards to another agent keeps its ILKs:**
  - its `src_ilk` is the first agent's, and its `dst_ilk` the person's
    (`build_reply_meta`, crates/fluxbee_ai_sdk/src/message.rs);
  - its trace's origin is the IO node where the cycle started, not the agent.
- **Cognition analyzed every turn an agent sent or received** (`handle_turn`): a semantic tagger
  call and a narrator call each, paid by the agent's tenant.
- **The analysis has no speakers.**
  - The tagger takes one turn at a time, with its ILKs as opaque hints. Both ILKs weigh the same
    (`current_turn_ilk_weights`).
  - The narrator writes the memory and the episodes from labels and signals
    (src/bin/sy_cognition/narrative_summarizer_ai.rs).
  - With several people on one thread, their feelings blend into one, with no name (§9).
- **The immediate memory names a speaker** (`render_memory_turn` and `current_message_label`,
  nodes/ai/ai-generic): `you`, or `<ILK> via <node>`.
- **The identity memory has each ILK's type and name, on every hive.**
  - The workers' replicas copy the primary (docs/reference/identity.md §5).
  - Every AI, IO and WF node is registered as an `agent`, with its node name as its display name
    (`derive_ilk_type_for_node` and `ILK_REGISTER`, src/bin/sy_orchestrator.rs).
  - A person has a name once registered. An IO node sends none when it provisions one
    (nodes/io/common/src/provision.rs).
  - The Rust SDK reads every ILK's type and name. The Go SDK could find an ILK only by its channel
    or its handler node (go/fluxbee-go-sdk/identity_shm.go).
- **`turns`** keeps `from_ilk` (the `src_ilk`), `to_ilk` and the whole message
  (src/bin/sy_storage.rs). A message with no `src_ilk`, like a WF's to an IO node (A-139), put its
  sender's node UUID in `from_ilk`, which is no ILK (`parse_turn`).

---

## 3. Decided (the operator, 2026-11-05)

1. **Cognition decides which turns it analyzes, turn by turn, by their origin and destination.**
   *"el truco es informarle al cognitive que mensajes tiene que analizar cognitivamente y cuales no
   basado en origen/destino.. [...] deberia ser cognitive que sepa o no si tiene que evaluarlo"*.
2. **A turn is analyzed when a person is at its other end:** a turn to the agent that a person
   wrote, or an answer of the agent to a person. The other turns still go into `turns` and the
   immediate memory, unanalyzed, so they stay detectable for a later analysis of a whole process.
3. **The ILK's type comes from the identity memory.** Nothing new in the router: it names the WF
   as the sender both of a forward and of the WF's own text, and only the WF knows which it sent.
4. **The WF's own text keeps the person as its `src_ilk`** (§5.4.3): the `src_ilk` says whose
   conversation it is. Who wrote the text is said apart.
5. **The immediate memory stays one list, and each line names its writer:** *"mezclado esta bien,
   el tema es como identificar a los interlocutores"*.
   - a person by their ILK and IO node, with their name when the identity has one;
   - an agent or a WF by its node, with its ILK;
   - the agent's own turns name whom they answered.
6. **The writer's ILK and name both travel in the message and are stored.** A node that sends a
   message fills a missing name from the identity memory: *"yo diria de poner los dos, y que luego
   con la SHM de ilk se pueda llenar si no esta el nombre [...] que los nodos tengan la capacida de
   resolver eso, el que mande el mensaje"*.
7. **The writer travels in the message, from whoever writes the text** (*"si, cerrado"*). It no
   longer comes from the trace, which stays debug and log (docs/message-trace-v1.md).

---

## 4. The format

`meta.context.writer`, in every solution message that a node wrote the text of:

```json
"writer": {"ilk": "ilk:7c1e…", "name": "Ana Pérez"}
```

- `ilk`: the writer's ILK. A node with no ILK of its own names only its name.
- `name`: the identity's display name for that ILK: a registered person's name, or a node's name.
  Left out when the identity has none (a person not registered yet). The identity memory writes
  an ILK's own id as its display name then; the readers take that as no name (found in 0.1.101's
  gate).
- Plain JSON, beside the trace. System messages carry none.

---

## 5. Who writes what

| Node | Its writer |
|---|---|
| An IO node (`build_user_message`, nodes/io/common) | the person who wrote: the message's `src_ilk` |
| An agent (the AI runtime, as it sends its answer) | itself: its ILK, and its node name |
| A WF's `send_message` | itself: its ILK (`FLUXBEE_NODE_ILK_ID`), and its node name |
| A WF's `forward` | the forwarded message's writer, as it came |

- **Every SDK fills a missing name** as it sends a solution message (`NodeSender::send_traced`,
  crates/fluxbee_sdk; `NodeSender.SendTraced`, go/fluxbee-go-sdk). It reads the writer's ILK in
  the identity memory of its hive, and keeps what it read for a minute. So a person who registers
  in the middle of a conversation gets their name in their next messages.
- **An action's `meta.context` never writes `writer`:** the WF's validator refuses it, as it
  refuses `wf`.
- **A message with no writer** (a node that does not write one) stands for its `src_ilk`, as
  before.

---

## 6. What cognition does

- **The analysis** (`handle_turn`, after the communication is loaded):
  - a turn the agent got is analyzed unless its writer is known not to be a person: an ILK the
    identity names `agent` or `system`, or a writer with no ILK, a node;
  - an answer the agent sent is analyzed unless its `dst_ilk` is known not to be a person.

  An ILK the identity does not know yet is taken as a person's. A person's first message can come
  before the workers' replica has their ILK, and a turn analyzed for nothing costs less than a
  person's turn lost. A turn not analyzed is counted (`turns_not_analyzed_total`, in the status).
- **The immediate memory** (`MemoryTurn`, crates/fluxbee_sdk/src/protocol.rs): each turn keeps
  `writer`, the writer of a turn the agent got, and `to`, the `dst_ilk` of an answer it sent. Each
  is `{ilk, name, kind}`:
  - `kind` is `person` for a human ILK, `workflow` for an agent ILK whose name is a `WF.*` node,
    `agent` for any other agent ILK, and `system` for a system ILK. It is left out when the
    identity does not know the ILK.
  - `name` is the message's, else the identity's.
- **The agent reads it** (`render_memory_turn`, `current_message_label`, nodes/ai/ai-generic):
  `<kind> <name> (<ilk>)`, with `via <node>` for a person, and `you, to <whom>` for its own. A turn
  with no writer reads as before.

---

## 7. What `turns` keeps

- `writer_ilk` and `writer_name`, from `meta.context.writer`.
- `from_ilk` is the `src_ilk`, else the writer's ILK, and never a node's UUID.
- Archi's Messages screen shows the writer.

---

## 8. What changed (the build, 2026-11-05)

- **The writer in both SDKs:**
  - `fluxbee_sdk::message_writer` (crates/fluxbee_sdk/src/message_writer.rs) and
    go/fluxbee-go-sdk/message_writer.go read and write `meta.context.writer`.
  - `NodeSender::send_traced` and `NodeSender.SendTraced` fill a missing name from the identity
    memory of the node's hive, as they write the trace entry.
  - Each SDK keeps what it read of an ILK for a minute (`ilk_facts`, `IlkFactsOf`).
- **The identity memory by ILK:** `find_ilk_by_id_from_hive_id` (Rust) and `LookupIlk` (Go, over a
  reader shared with `LookupIlkByHandlerNode`) give an active ILK's type and display name. The Go
  SDK reads the node's own ILK too (`ReadSelfIlkFromEnv`).
- **Who names it:**
  - an IO node names the person (`build_user_message`, nodes/io/common);
  - the AI runtime names the agent on its answer (`stamp_reply`, crates/fluxbee_ai_sdk);
  - the WF names itself on a `send_message`, and puts back the forwarded message's own writer on
    a `forward` (`ActionContext.writer`, `withSourceWriter`). The validator refuses a
    `meta.context.writer`.
- **Cognition** (src/bin/sy_cognition.rs):
  - `person_at_other_end` decides in `handle_turn`, after the load;
  - `turn_writer` and `turn_party` fill `MemoryTurn.writer` and `MemoryTurn.to`;
  - the counter `turns_not_analyzed_total` is new.
- **ai.generic:** `party_label`, used by `render_memory_turn` and `current_message_label`.
- **SY.storage:** `turns.writer_ilk` and `writer_name`, added once to an old table
  (`add_turns_writer`). `from_ilk` is never a node's UUID.
- **Archi:** its Messages screen shows the writer.
- **Tests:**
  - in each piece above;
  - cognition loads a turn with no person at its other end and does not analyze it;
  - the Go SDK reads an identity region written as SY.identity lays it out.

---

## 9. Open

The rest of A-137, one topic at a time:

- **Several people on one thread:** the analysis blends their feelings into one, with no name.
  In the chat since 2026-11-05 (§10).
- **Where cognition keeps its state:** agreed 2026-11-06 (§11), to build next.
- **A conversation with no agent:** who owns it, and which hive's cognition keeps it. In Case 4,
  Juan talks with Ana from 10:00 to 10:16 and no agent is in that part.
- **Handing over:** how the next agent sees what was said meanwhile.
- **Who pays** for an analysis with no agent.

---

## 10. Several people on one thread, and the feelings (chat of 2026-11-05)

### 10.1 Facts (verified in the code, 2026-11-05)

- **Where several people share a communication:**
  - in a Slack channel: the thread is the channel, or the Slack thread, whoever writes
    (`slack_inbound_io_context`, nodes/io/common/src/io_context.rs);
  - in WF Case 4: Juan's hand-back note reaches sales on Ana's thread.

  WhatsApp is one to one.
- **The analysis has no owners** (src/bin/sy_cognition.rs):
  - the tagger reads each turn alone, with its ILKs as opaque hints;
  - the narrator writes the memory and the episodes from labels and signals, with no speaker and
    no text;
  - an episode is one per scope instance and affect, with no ILK, and an agent's own answer can
    raise one;
  - contexts and reasons count who sent and who got them (`ilk_profile`), and nothing reads it.
- **The feelings never reach the agent, and never fade** (FINDINGS A-155):
  - ai.generic shows no episode;
  - an episode's intensity is its affect's fixed base (8, 8, 7), never lowered, never closed;
  - the package takes 4 by intensity, which among more than 4 is arbitrary.
- **The other items fade as designed** (docs/reference/cognition.md §5): a turn that names an item
  makes its weight `weight × decay + 1`, a turn that does not makes it `weight × decay`, and an
  open one closes under 0.25.
  - Contexts decay by 0.85, reasons by 0.75, co-occurrences by 0.80, and memories by 0.97
    (memories never close).
  - So an item said every turn builds up, one said every few turns stays faint, and one said once
    goes: a context on its 9th turn without it.
- **Three defects** (FINDINGS A-156):
  - an open item's decay is not stored, so a reload brings back its weight of the last turn that
    named it;
  - `last_seen_at` moves on a turn that does not name the item;
  - the averages (`weight_avg_cumulative`, `weight_avg_ema`) average each naming turn's score,
    always 1.0.
- **The docs:** no doc has the feelings' formula. 12-cognition-v2 §8.6 points to the Cognitive
  Lab audit, which is not in the repo. The beta spec (docs/history/legacy/12-cognition-v2-BETA.md)
  names `habit_count`, `habit_penalty` and `novelty_bonus`, keyed by affect and main context, with
  no formula. The lab's code is not in the repo, its history, or the noe branch.

### 10.2 Decided (the operator, 2026-11-05)

1. **One communication, and each feeling and each memory names its owner** (*"Concuerdo con esa
   opción que propones"*). Same rule as the immediate memory: mixed, with each interlocutor named.
2. **Each ILK owns its feelings, and each person is evaluated apart:** *"Cada ilk es dueña de sus
   sentires [...] si hay más de una persona jugando en la charla, cada uno tendrá su historia y se
   evaluará por ilk"*.
3. **The feelings follow the math of every other item,** a weighted average that fades with time:
   *"si ocurre siempre se refuerzan, si ocurren de vez en cuando perduran pero leves y si pasa una
   vez desaparece con el tiempo"*.
   - Only their numbers are larger, for more inertia: *"al menos x2-x10 dependiendo del sentir"*.
   - That math was measured in the lab, with its own code: *"esa dinámica fue medida y funcionó"*.
4. **The numbers:** today's for now. The operator brings the lab's code to check them: *"supongamos
   que los números son los del soft actuales y mañana consigo el código y verificamos los
   números"*.
5. **`last_seen_at` and the averages are bugs to fix:** *"son bugs desastrosos de programación
   deficiente y que no se terminó"*.
6. **Cognition keeps its own state; SY.storage keeps the messages:** *"cognitive tiene
   persistencia propia, storage guarda mensajes, la idea no es que sea repositorio de cognitive"*.
   Its own topic, agreed on 2026-11-06 (§11).
7. **The proposal of §10.3 and §10.4 is agreed, and the topic is to be finished whole:**
   *"coincido con todo, avanza como planteas pero este tema es para terminarlo, con las
   prioridades que creas, pero completo. Sentires está bien."* The order: the feelings with
   A-156's two bugs, then where cognition keeps its state, then the rest of §9.

### 10.3 The feelings' math (agreed 2026-11-05; its numbers to check against the lab's code)

- **A feeling** is one per person, affect and context: "Ana, anger, billing". The beta keyed its
  habit counter by affect and main context.
- **A person's turn** whose signals pass the gate for an affect (`build_episode_candidate`, as
  today) makes that feeling's weight `weight × d + s`. Each other analyzed turn of the same person
  makes each of that person's open feelings `weight × d`. A feeling closes under 0.25, and opens
  again when it comes back. Since §11 a closed one leaves cognition's state, so one that comes
  back starts from zero; until then it started from the weight it closed with.
  - The clock is the owner's own turns: Pedro writing calm does not fade Ana's anger.
  - Only a person's own turn raises a feeling. An agent's answer adds to the contexts and reasons,
    never to a feeling.
  - Each affect has its own gate, so one turn may show several: each is raised, none of them
    fades on that turn, and the narrator writes one text for the moment.
- **The numbers:** a context's decay (0.85) with its half-life stretched by each affect's inertia
  (`d = 0.85^(1/k)`), and a step of the affect's base intensity over 4:

| Affect | Inertia | d | s | Half-life (turns) | Said once, gone on its turn without it |
| --- | --- | --- | --- | --- | --- |
| urgency | ×2 | 0.922 | 1.75 | 8.5 | 24th |
| anger | ×5 | 0.968 | 2 | 21.3 | 64th |
| escalation | ×10 | 0.984 | 2 | 42.7 | 128th |
| (a context, today) | ×1 | 0.85 | 1 | 4.3 | 9th |

- **How they hold:** said every 20 turns, urgency stays at 0.5 to 2.2, anger at 2.3 to 4.2, and
  escalation at 5.3 to 7.2. Said every 40 turns, urgency goes between them, and anger and
  escalation stay.
- **Its text:** the narrator's, as today, now knowing whose feeling it is.
- **The memory:** one per scope, as today, now naming who said what. The narrator gets each turn's
  writer.
- **The package:** the open feelings by weight, up to 12. Each has its owner, affect and context,
  a level from its weight, when it was last felt, and its text.
- **The agent:** a "Feelings" block, the person being answered first.

### 10.4 The bugs fixed with it (A-156)

- **`last_seen_at`** moves only on a turn that names the item.
- **The averages** average the item's weight over the turns it is open. The beta's example does:
  a context of weight 4.2 has averages of 2.85 over 23 samples. Today they average the score,
  always 1.0, and only on a naming turn.
- **The docs:** 12-cognition-v2 §9 loses the beta's values, and docs/reference/cognition.md §5
  says how a feeling builds up and fades.

### 10.5 Built (2026-11-05), released in 0.1.102 (2026-11-06)

- **SY.cognition** (src/bin/sy_cognition.rs):
  - `feeling_owner` and `narrator_speaker` read the turn's owner and speaker in `handle_turn`;
  - `feeling_candidates`, `update_feelings_for_turn` and `feeling_envelope` move the owner's
    feelings, with `feeling_decay`, `feeling_step`, `feeling_level` and `COGNITION_FEELING_INERTIA`;
  - the narrator gets the speaker (`NarrativeSummarizerAiInput.speaker`), and its prompt names
    people;
  - the tagger names the writer's affects in a closed set, `affect_signals`, which the gate reads:
    its free extra hints never named them, so no feeling was ever made on 8.x (FINDINGS A-157);
  - a rebuild keys feelings by `feeling_key`, and leaves out a row with no owner;
  - `sample_weight` keeps every item's averages over its open turns, and a turn that only decays
    an item leaves its `last_seen_at` (A-156).
- **The package** (crates/fluxbee_sdk/src/protocol.rs): `MEMORY_PACKAGE_VERSION` 4; an
  `EpisodeSummary` has its owner, affect, context, level, weight, `last_felt_at` and text. The Go
  SDK mirrors it, with the turns' `writer` and `to`, which it lacked since 0.1.101.
- **The durable row** (`CognitionEpisodeData`): owner, context, status, weight and averages,
  occurrences, `last_felt_at`, `closed_at`. SY.storage's `intensity` column takes the weight.
- **ai.generic:** the "Feelings" lines (`render_feeling`), those of the person being answered
  first.
- **The review round** (one, 2026-11-05) found that the gate gave one affect a turn, so an
  escalating person who was also angry never got the escalation, and a feeling the turn showed
  could fade on it. Fixed: each affect its own gate (`build_episode_candidates`). It also found
  the reference's word on a narrator failure (now: the feelings move, without new text), and an
  AI node on the old runtime cannot read a package with feelings: the deploy moves the AI nodes
  before a feeling can exist, since the first needs the new tagger.
- **Tests:**
  - a feeling is its owner's, and fades only with their own turns, closing on the 64th for anger;
  - one that comes back builds up and reads `strong`, and each affect's half-life is its inertia
    times a context's;
  - the agent's own answer shows and moves no feeling;
  - a turn that shows two affects raises both, and the owner's other feeling fades;
  - the package carries the open feelings by weight, 12 at most;
  - a rebuild keeps each owner's feelings and leaves out a row with no owner;
  - the averages follow the weight, and `last_seen_at` stays;
  - a turn names its speaker and its feelings' owner;
  - the tagger's affect signals are a closed set;
  - ai.generic's block names each feeling and its owner.

No new node, message or model call. SY.storage needs no new column: a communication reloads from
each row's `payload`.

**Checked on 8.x (0.1.102, 2026-11-06),** with a client standing for an IO node and
`AI.pruebas@worker1` on one thread, for two people of the tenant `pruebas` (FINDINGS A-155):

- One angry turn of A's opened three feelings of A's, anger, escalation and urgency, around the
  same context, with one text naming A.
- B's turn and the agent's answers left them. A's calm turn faded each once, by its own inertia.
- The agent got A's feelings, each named by its owner, and declined to copy them to B: notes about
  another person.
- The closed reasons kept `last_seen_at` at the turn that named them, and the averages followed
  the weight.
- The feelings added no model call.

### 10.6 Where cognition keeps its state

Reviewed whole and agreed on 2026-11-06: §11. The proposal written here on 2026-11-05 said an
agent could come up on another hive as the same ILK. That is wrong since 0.1.83 (§11.1).

---

## 11. Where cognition keeps its state (chats of 2026-11-06)

### 11.1 Facts (verified in the code, 2026-11-06)

- **Cognition keeps nothing on its own disk but its config** (`persist_cognition_config_state`).
  12-cognition-v2 §10.2 planned a local LanceDB, `memory.lance` in its folder. It was never built:
  the path is reported and nothing opens it.
- **Its state lives in SY.storage, on the motherbee:**
  - each turn sends up to ~100 rows (about 80 KB on a rich turn) through the stream
    (`storage.cognition.batch`) into 8 `cognition_*` tables;
  - each row keeps an item's last value, overwritten turn by turn;
  - cognition reads them back over the mesh (`COGNITION_STATE_GET`), and nothing else reads them.
- **A worker's cognition depends on the motherbee:**
  - in memory it holds only the communications that fit the memory region, 4 MB
    (`retain_memory_hot_set`);
  - any other loads from SY.storage at its next turn;
  - when SY.storage does not answer, that turn is not analyzed ("could not load a communication
    from SY.storage; turn skipped").
- **A host reboot loses the agents' memory until each conversation speaks:**
  - at start cognition reloads only what the memory region lists
    (`reload_communications_from_shared_memory`);
  - the region lives in `/dev/shm`, so it survives a restart of the process, not of the host;
  - after a host reboot, each conversation's first message reaches its agent with no memory.
- **Cognition's results are not in the messages:** `turns.tags` is the sender's
  (`payload.tags` or `meta.tags`), not cognition's.
- **What a communication's state is** (`ThreadCognitionState`):
  - its items with their weights: contexts, reasons, co-occurrences and feelings, each with its
    averages, open or closed, its dates and who took part;
  - the active scope: the bound context and reason, and its energy;
  - the memories: the narrator's text, one per scope;
  - the feelings' texts;
  - the last 20 turns, at most 48 KiB;
  - counters: the turn count, first and last seen, and the last turn's ILKs and channel.
- **Only open items are read:**
  - the package carries open contexts, reasons and feelings, and the memories
    (`build_memory_package_for_thread`);
  - the scope picks its pair among open items (`select_dominant_context`,
    `select_dominant_reason`);
  - a closed item is read only when it comes back: it reopens from the weight it closed with,
    under 0.25.
- **Memories never close:** one per scope. Each time the narrator writes one, the communication's
  others fade by 0.97.
- **A node cannot move to another hive:**
  - its hive is part of its name: `AI.x@worker1` started on worker2 is another node,
    `AI.x@worker2`;
  - since 0.1.83 SY.identity lets an orchestrator register only its own hive's node ILKs
    (`authorize_orchestrator_ilk_scope`), so `SY.orchestrator@worker2` registering the ILK of
    `AI.x@worker1` gets `UNAUTHORIZED`;
  - only a teardown frees the id, and then it is a new node with a recycled id;
  - cognition's 5-minute idle reload ("its agent may have moved away and back") exists only for
    that case.
- **A node is a node and its state:**
  - an agent keeps its thread state in its folder (LanceDB, `thread-state`);
  - a WF keeps its instances there (`wf_instances.db`), and its timers in its hive's SY.timer, by
    the owner's name;
  - io.slack keeps its thread records there (`slack_threads.db`);
  - that folder is removed with the node.
- **A node is named with its hive** in identity (`node_name`, an ICH's owner), in the vault's check
  (`handler_node`), in IO configs (`io.dst_node`), in WF actions' targets and CEL guards, in the
  operator's OPA, in routes, taps and timers.
- **Every Fluxbee process runs as root:** the units have no `User=`, and managed nodes run as
  `systemd-run` units. SY.vault keeps its SQLite file 0600 (`VaultStore::open`).

### 11.2 Decided (the operator, 2026-11-06)

1. **A node is born and dies on its hive** (*"nunca pensé en lo contrario"*), as D6 says of the
   tenant.
   - Moving one, if ever needed, is a process of its own.
   - The operator's view, far ahead: solutions made of groups of agents of a "digital agent" (AD).
     What would move is its soul, not a node: *"eso no se puede mudar así nomás.. se mudaría el
     alma de ese AD si fuera necesario"*.
2. **Cognition keeps its state in an embedded store of its own,** its "config file" for complex
   data and current state.
   - *"por mí es lo mismo mientras que esté embebido en cognitive y resuelva él sus problemas con
     esa herramienta"*.
   - Each node uses the tool that suits its data, since the data is bounded to it.
3. **The message carries what serves the communication,** and nothing of a node's own matters.
   - *"no carguemos el sistema fluxbee completo por temas propios del nodo o de la infra.. la idea
     es que el mensaje sirva para comunicar y guarde sus datos"*.
   - For cognition, that is the memory package the router attaches for the agent, as today.
   - No per-turn result goes to SY.storage.
4. **What cognition keeps is online state, not history:** *"no son históricos, son estados de uso
   'online' nada más"*.
   - A closed item is deleted when it closes: *"cerró, no necesita memoria de cálculo para seguir
     con eso.. se borra"*.
5. **A communication is never deleted,** not when it goes quiet, not when its agent is torn down.
   Its memories and feelings involve people: *"alguien muere y nadie lo olvida"*.
6. **Deleting an ILK is a chapter of its own** (FINDINGS A-159): an ILK is disabled, and deleted
   only when nothing anywhere refers to it. *"algo hay, no todo"*.
7. **A lost hive:** the snapshots cover it for now.
8. **8.x:** the cognition tables go (*"borra todo lo que quieras en 8.xx"*).
9. **The file is cognition's alone:** *"que el archivo de cognition tenga la seguridad para que
   solo él acceda"*.

### 11.3 The design

- **The file:** `cognition.db`, in SY.cognition's folder on each hive
  (`/var/lib/fluxbee/nodes/SY/SY.cognition@<hive>/`).
  - The folder is 0700, and the file and its WAL sidecars are 0600 from their creation.
  - Every Fluxbee process runs as root, so this keeps out every other user, not another root
    process. A user per service waits with A-22 and A-60, parked.
- **One row per communication `(thread_id, agent_ilk)`:** the agent node, the turn count, the last
  activity, and the state as one JSON with its own version.
  - The state holds the open items with their weights and averages, the active scope, the
    memories, the open feelings, the last 20 turns and the counters.
  - A row takes about 10 to 60 KB, the turns most of it.
- **Writes:** each communication's row whole, in one transaction:
  - when a turn's analysis ends;
  - when its new turns come in (the intake).

  Every decay is kept (A-156's first).
- **Start:** the most recent communications, up to the region's capacity, are loaded, and the
  region is written before the first turn. Any other loads from the file at its next turn.
- **A closed item leaves the state when it closes.** One that comes back starts from zero (§10.3).
- **Nothing deletes a communication.** The file grows with each new one: 10,000 take about 100 to
  600 MB.
- **What goes:**
  - the 8 `cognition_*` tables, dropped once with a log line;
  - the `storage.cognition.*` subjects, and their sink and producer in the router's stream;
  - `COGNITION_STATE_GET` and the reload over the mesh;
  - the 5-minute idle check;
  - the `memory.lance` path.
- **What stays:** the turns' copy to cognition (`cognition.turns`), its usage records
  (`storage.usage`), the memory region and the package.
- **What it gives:**
  - a worker's cognition works on its own, whether the motherbee answers or not;
  - after a host reboot, an agent has its memory from the first message;
  - a turn no longer sends up to ~100 rows to the motherbee.

### 11.4 Every leg

- **SY.cognition:** the store, its loads and writes, the closed items, and its status (the file,
  the communications, the bytes).
- **SY.storage:** the cognition handlers, their tables and `COGNITION_STATE_GET` go. The tables are
  dropped once, while the catalog shows them.
- **The router:** its stream drops `storage.cognition.*`.
- **The Rust SDK:** the subjects, the envelopes and `COGNITION_STATE_GET` go. The Go SDK has none.
- **Nothing changes** in the policy, identity, the vault, SY.admin, the orchestrator, packaging
  (`rusqlite` is already in the workspace), Archi or Cloud.
- **Posture:** the folder and the file.
- **Docs:** docs/reference/cognition.md and storage.md, docs/nodes/sy-cognition.md and
  sy-storage.md, the SDK READMEs, and a note in docs/router-stream-v1.md §6.2.
- **Tests:**
  - the store's round trip;
  - a closed item gone from it;
  - the folder's and the file's modes;
  - a start that fills the region from the file;
  - no `storage.cognition.*` subject left.
- **The gate on 8.x:**
  - a restart of SY.cognition keeps the memory;
  - after a reboot of worker1, the first message carries memory;
  - with SY.storage stopped for a while, worker1 keeps analyzing;
  - a feeling that closes leaves the file;
  - the file is 0600;
  - the tables are gone from the motherbee.

### 11.5 Built (2026-11-06)

- **SY.cognition** (src/bin/sy_cognition.rs, src/bin/sy_cognition/state_store.rs):
  - `CognitionStore`: the file, its folder and modes, `load`, `save` (one transaction),
    `most_recent` and `stats`;
  - `sync_held_state` writes each communication a change touched before it rewrites the region;
  - the intake and the worker load a communication they do not hold from the file
    (`held_or_loaded`); the worker no longer loads a turn it does not analyze;
  - at start, `load_most_recent` fills the region before the first batch;
  - an item under its threshold leaves the state (`retain` in each update); the memory and the
    feelings read the active scope's own pair, so they go on when the pair's items have closed;
  - the envelopes, the reload over the mesh, the 5-minute idle check, the scope instances and the
    `cache`, `shm` and `memory.lance` paths are gone; `STATUS` reports the file (`store`).
- **SY.storage:** the cognition handlers, tables and `COGNITION_STATE_GET` are gone;
  `drop_cognition_tables` drops the tables and the inbox rows of their subjects once.
- **The router's stream** and **the Rust SDK:** `storage.cognition.*` and the SDK's cognition module
  are gone.
- **`serde_json` reads floats back exactly** (`float_roundtrip`): without it a weight written to the
  file came back a last digit off (1.8075 for 1.8074999999999999), found by the file's round-trip
  test.
- **`fluxbee-factory-reset`** removes the motherbee's file. A worker's stays (FINDINGS A-160).
