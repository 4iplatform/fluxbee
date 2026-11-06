# Cognition across hands v1: who wrote each turn, and which turns cognition analyzes

**Status:**

- FINDINGS A-137 asks how cognition follows a conversation that passes through several hands.
  The operator opened that chat on 2026-11-05. Its first part was agreed the same day (§3): who
  wrote each turn, which turns cognition analyzes, and how the immediate memory names its
  speakers.
- Built in code the same day, and released in 0.1.101 with A-153 and A-154 (§8), checked on 8.x.
- The rest of A-137 is open (§9), one topic at a time. The first, several people on one thread
  and the feelings, is in the chat (§10). Its numbers wait for the lab's code.

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
- **Where cognition keeps its state** (§10.6): the next topic.
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
   Its own topic (§10.6).

### 10.3 The feelings' math (proposed; its numbers to check against the lab's code)

- **A feeling** is one per person, affect and context: "Ana, anger, billing". The beta keyed its
  habit counter by affect and main context.
- **A person's turn** whose signals pass the gate for an affect (`build_episode_candidate`, as
  today) makes that feeling's weight `weight × d + s`. Each other analyzed turn of the same person
  makes each of that person's open feelings `weight × d`. A feeling closes under 0.25, and opens
  again when it comes back.
  - The clock is the owner's own turns: Pedro writing calm does not fade Ana's anger.
  - Only a person's own turn raises a feeling. An agent's answer adds to the contexts and reasons,
    never to a feeling.
- **The numbers:** a context's decay (0.85) with its half-life stretched by each affect's inertia
  (`d = 0.85^(1/k)`), and a step of the affect's base intensity over 4:

| Affect | Inertia | d | s | Half-life (turns) | Said once, gone on its turn without it |
| --- | --- | --- | --- | --- | --- |
| urgency | ×2 | 0.922 | 1.75 | 8.5 | 24th |
| anger | ×5 | 0.968 | 2 | 21.3 | 64th |
| escalation | ×10 | 0.984 | 2 | 42.7 | 128th |
| (a context, today) | ×1 | 0.85 | 1 | 4.3 | 9th |

  Said every 20 turns, urgency stays at 0.5 to 2.2, anger at 2.3 to 4.2, and escalation at 5.3 to
  7.2. Said every 40 turns, urgency goes between them, and anger and escalation stay.
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

### 10.5 What the change touches

- SY.cognition: the feelings' key, math and narrator input.
- The package's `EpisodeSummary`: owner, context, level, date and text.
- ai.generic: the "Feelings" block.

No new node, message or model call. SY.storage needs no new column: a communication reloads from
each row's `payload`.

### 10.6 Next topic: where cognition keeps its state

- **Today it has none of its own:**
  - Locally it keeps only its config. It reports a `memory.lance` path that nothing opens: LanceDB
    was 12-cognition-v2's local layer, never built (FINDINGS A-38).
  - Since 0.1.77 its rows go to SY.storage@motherbee (`storage.cognition.batch`, the `cognition_*`
    tables), and it reloads from there over the mesh (`COGNITION_STATE_GET`), as
    docs/router-stream-v1.md §6.2 designed it (2026-10-17).
- **The operator's direction** (§10.2, 6): cognition keeps its own state, and SY.storage keeps the
  messages.
- **A-156's first defect belongs there:** an open item's decay is not stored, because each write
  is a stream record to the motherbee.
