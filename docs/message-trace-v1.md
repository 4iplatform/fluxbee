# Message trace v1: each message carries the trace of its cycle

**Status:**

- The direction was agreed with the operator on 2026-11-04, in a design chat (FINDINGS A-138, the
  sprint's block 3).
- The design closed the same day: nothing is open (§9).
- Built in code the same day, for the next release (§8).
- Next: the release, with A-151, and its WF gate on 8.x. Then the A-137 chat (cognition across
  hands), which builds on this trace.

This is the single place for the topic.

---

## 1. What it is, from outside

A message enters Fluxbee: a person writes, a person clicks, a timer fires. It goes through a few
nodes, as the solution configured in Archi decides, and the cycle ends. The operator calls it a
pulse (*"es como una pulsación"*). The trace is the record of that pulse, carried in the message
itself: who sent it to whom, when, and what each node did.

**The example** (WF Case 4, docs/wf-orchestration-v1.md §5.4): at 10:17 Ana asks "¿entonces ya
está la nota de crédito?". When the WF's last message of the cycle reaches io.slack, it carries:

```json
"trace": [
  {"t": "2026-11-04T10:17:03.120Z", "from": "IO.slack@motherbee", "to": "WF.ventas@worker1"},
  {"t": "2026-11-04T10:17:03.180Z", "from": "WF.ventas@worker1", "to": "AI.ventas@worker1", "act": ["forward"]},
  {"t": "2026-11-04T10:17:09.410Z", "from": "AI.ventas@worker1", "to": "WF.ventas@worker1"},
  {"t": "2026-11-04T10:17:09.470Z", "from": "WF.ventas@worker1", "to": "IO.slack@motherbee", "act": ["forward"]}
]
```

**What it is for:**

- Debug and log: *"va a quedar claro si se ve el mensaje como fue, para debug y log"*.
- Each node, and the AI model, sees the path while it decides.
- It is not for security (*"no es para seguridad"*).

---

## 2. What exists (verified in the code, 2026-11-04)

- **`routing.trace_id` is already the cycle's id:**
  - an IO mints a new one for each message that enters (`new_trace_id`,
    nodes/io/common/src/inbound.rs);
  - an agent's reply reuses the request's (`build_reply_routing`,
    crates/fluxbee_ai_sdk/src/message.rs);
  - a WF takes the trace_id of the event it handles, and every send of that transition carries it
    (`CurrentTraceID`, `transitionTraceID`, go/nodes/wf/wf-generic/node);
  - SY.timer mints one for each fire (go/sy-timer/scheduler.go), so a cycle can also start at a
    timer;
  - the SDK's RPC and SY.timer's retry correlate answers by it, so its meaning stays as it is.
- **`meta.thread_id` is the conversation,** across its cycles. The IO computes it from the native
  thread (`compute_thread_id`, nodes/io/common/src/io_context.rs). Cognition keeps memory per
  `(thread_id, agent ILK)`.
- **The WF's trail** (`meta.context.wf.trail`) holds node names only:
  - Only a WF writes it. A `send_message` starts it with the WF, and a `forward` adds the WF to the
    incoming one (go/nodes/wf/wf-generic/node/actions.go).
  - An agent's reply drops the whole `wf` block (`sanitize_reply_context`), so the trail ends at
    every agent's answer.
  - ai.generic reads it for the model's `Path:` line and for the "via" of the speaker's label.
    Cognition reads its first name too (src/bin/sy_cognition.rs).
- **What the model reads before a message** (nodes/ai/ai-generic/src/bin/ai_node_runner.rs):
  - the memory block;
  - the WF's block: `Workflow context:`, with `Path:` from the trail, `Variables:` and the menu;
  - the label `Current user message, from <ILK> via <node>:`.
- **Storage and Archi:**
  - The router publishes as a turn every message that is not system, admin or `query_response`
    (`should_publish_turn`, src/router/mod.rs).
  - `turns` keeps the whole message, with its `thread_id` and `trace_id`.
  - Archi's Messages screen reads them (`/api/messages`).
- **Size:**
  - A message is at most one frame, 128 KiB.
  - At delivery to an agent, the router adds the memory package (up to 20 turns, 48 KiB) and cuts it
    to fit.
- **When the router changes a destination:** only for a message with no destination (`dst` null).
  - `resolve_target_with_identity` picks the target: the frontdesk for a sender with no ILK or not
    registered, else the user OPA policy.
  - Static routes change the path (the next hive) or drop the message, and taps send a side copy.
    Neither changes the destination.
- **Where nodes build what they send:**
  - every IO node builds its messages with `build_user_message`
    (nodes/io/common/src/router_message.rs);
  - the AI runtime stamps the agent's ILK on each reply as it sends it
    (crates/fluxbee_ai_sdk/src/runtime.rs);
  - the WF sends through its actions (go/nodes/wf/wf-generic/node/actions.go) and the Go SDK.

---

## 3. Decided (the operator, 2026-11-04)

1. **One cycle.** The trace lives from where the cycle starts to where it ends: *"si ese mensaje
   termina el ciclo completo ya está"*.
   - The conversation's history across cycles stays where it is: in memory and in `turns`, by
     `thread_id`.
2. **`trace_id` is the cycle's id:** *"trace_id es lo correcto, para esto estaba"*. `thread_id` is
   the conversation.
3. **Nothing descriptive:**
   - the source and the destination, and the node's action: *"nada descriptivo.. fuente/destino y
     alguna acción que haga wf"*;
   - *"lo más importante es el origen/destino"*;
   - and the time, *"para ver cuándo cambió el mensaje y pasó por el nodo"*.
4. **Plain JSON, not compressed** (*"no comprimir"*).
   - A cycle is a few hundred bytes to about 1 KB.
   - Compressed bytes would travel as base64 and make the log unreadable.
5. **Each node writes its own entry:** *"cada nodo sabe y si lo sabe es el dueño de la info y
   responsable para escribir"*.
   - The WF is one more node: *"WF es un nodo más y agrega sus cambios en la traza, si lo hizo"*.
   - The router writes an entry only when it changes the destination: *"también tiene que poner la
     entrada si llega a cambiar el route"*.
6. **Debug and log, not security.**
   - No check by the router, no signatures, no cap on entries.
   - *"el ciclo de comunicación comienza y termina con pocos mensajes y depende de la solución
     configurada en archi"*.
7. **Solution data, not system data:** *"es de operación de fluxbee sobre la solución, no mezclar
   con sistema de fluxbee"*.
   - It lives in `meta.context.trace`, next to the `io` and `wf` blocks.
   - System messages carry none.
8. **The AI model reads it:** *"que lea esa metadata es interesante"*.
9. **Archi shows it later:** a screen that shows a cycle's whole trace as pulses (*"es para
   después"*).
   - The data stays in storage, in `turns`.
   - Archi's log screen (Messages) is the one to expand.
10. **The trace replaces the WF's trail:** *"esto es la actualización del trail, y sí integraremos
    esto"*.
    - The WF stops writing `meta.context.wf.trail`.
    - ai.generic and cognition read the trace instead.
11. **The actions are free, not a fixed list:** *"no debería ser fijo… sería mejor tipo tags
    libres"*, and *"dije tags por el concepto, que no sean fijas"*.
    - Each node notes what it did, in its own words, by the rule of decision 5.
    - The word is `act`, never "tag" (*"coincido en decir act, no tag nunca más"*).
12. **The WF notes what it does:** *"el wf tiene acciones, no hay invento en esto. Escribe lo que
    el wf hace"*.
    - Its entry lists the actions its transition ran, so an entry can hold more than one: *"tal
      vez un nodo hace más de una acción a la vez… raro… pero posible"*.
13. **Short words:** each one names an action, in up to 32 characters, and an entry holds up to 8
    (the operator's "ok" to the proposal).
14. **The frontdesk notes whether it assigned an identity:** *"No estaría mal que el frontdesk
    tenga la acción de asignar identidad o no cuando lo apruebe"*.
    - Once it decides, its entry says `identity_assigned` or `identity_not_assigned`, with its
      result's code (`registered`, `merged`, `register_failed`…).
    - While it collects the data, the code alone (`in_conversation`).

---

## 4. The format

`meta.context.trace` is a list of entries, oldest first:

- `t`: when the node sent the message, UTC, ISO 8601 with milliseconds.
- `from`: the node that sent it, by its L2 name.
- `to`: the destination, by its L2 name.
  - A reply goes to the requester's UUID, so its `to` is the request's `src_l2_name`, which the
    router stamps.
  - A message sent with no destination has no `to`: the router's entry follows it (§5).
- `act`: optional, a list of free words, each one an action the node took (§5). Each word has up to
  32 characters, and an entry has up to 8 of them.
  - `tags` already names something else: a message's own `payload.tags` or `meta.tags` fill the
    `tags` column of `turns` (`parse_turn`, src/bin/sy_storage.rs).

An entry takes about 90 bytes. A cycle of 4 to 12 entries is 0.4 to 1.1 KB, under 1% of a frame.

---

## 5. Who writes what

| Node | When | `from` → `to` | What it notes in `act`, at least |
|---|---|---|---|
| An IO (`build_user_message`) | a message enters: a person writes, a person clicks | the IO → its destination | nothing, or what it saw (a click) |
| A WF | each send of a transition | the WF → its destination | the actions its transition ran, the one that sent this message first: `["send_message", "schedule_timer"]` on Case 4's post to the team |
| An agent (the AI runtime, as it sends) | each reply | the agent → the requester | the option it chose, when it answers a menu |
| The frontdesk (an agent) | each reply | the frontdesk → the requester | `identity_assigned` or `identity_not_assigned` once it decides, and its result's code (decision 14) |
| The router | a message with no destination | the router → the destination it picked | `resolve` |

The last column is what each node writes by default. The words are free: a node adds whatever
else it knows it did.

**How it travels within a cycle:**

- **Whoever sends the next message starts from the trace it received.**
  - An agent's reply already copies `meta.context`, removing only some keys, so the trace travels
    with it.
  - A WF's `forward` keeps the message's context.
  - A WF's `send_message` builds a new context, so it copies the trace of the event it handles.
- **A new cycle starts a new trace:**
  - an IO's inbound message starts one;
  - a timer's fire does too. The WF's first send on it is the first entry: SY.timer's fire is a
    system message, and SY.timer writes nothing.
- **A node that does not carry the trace cuts it.** The message then shows only what follows that
  node, while `turns` still has every message of the cycle by `trace_id`.

---

## 6. What the model reads

- ai.generic renders the trace in place of the WF block's `Path:`, one line for each entry:

  ```
  Trace:
  10:17:03.120 IO.slack@motherbee -> WF.ventas@worker1
  10:17:03.180 WF.ventas@worker1 -> AI.ventas@worker1 [forward]
  ```

- The speaker label's "via" takes the trace's first entry, the cycle's origin. Cognition reads it
  the same way.

---

## 7. Withdrawn (2026-11-04)

- **A trace of the whole conversation in the message:** someone would have to hold the history
  between messages, and it grows without end. The cycle bounds it.
- **Compressing it** (zip, zstd or deflate, then base64):
  - little to gain on a few hundred bytes;
  - an unreadable log;
  - the same codec in both SDKs;
  - a cap against decompression bombs.
- **The router writing every entry, checking the nodes' entries, or signing them:** the purpose is
  debug and log, and each node owns its entry.
- **A cap on entries:** a cycle is a few messages, set by the solution.
- **A fixed list of actions for each kind of node:** each node owns what it notes (decision 11).
- **Words from the WF's definition,** a field on an action or a CEL function that adds to the
  trace: not now. The WF notes the actions it runs, and a function waits for a case that needs it
  (*"no sé si vale la pena alguna función en el wf"*).
- **Other places for it:**
  - among the system fields of `meta`: it is solution data;
  - next to `trace_id` in `routing`: both SDKs rebuild `routing` on a reply, and the router routes
    every message by it, system ones included.

---

## 8. What changed (the build, 2026-11-04)

- **Both SDKs write the sender's entry on every solution message they send.**
  - Rust: `NodeSender::send`, or `send_traced` with a note (crates/fluxbee_sdk/src/message_trace.rs).
  - Go: `NodeSender.Send`, or `SendTraced` with a note (go/fluxbee-go-sdk/message_trace.go).
  - The note (`TraceNote`) is what the node adds: the destination by name, and its actions.
  - So every IO node writes its entry with no change of its own: its messages go through the
    SDK's send.
- **The AI runtime** sends a reply with a note: to the request's `src_l2_name`, the words the node
  noted on its reply (`set_reply_act`), and the option the agent chose from a menu.
  - The frontdesk notes its identity decision (decision 14).
- **The WF:**
  - **Which trace a send carries:** the trace of the cycle the instance is handling
    (`takeCycleTrace`):
    - a node's message brings its own;
    - an internal event keeps it;
    - a timer's fire starts one with none.
  - It lives in memory: after a restart, a send starts its trace anew.
  - **Its note** names the kinds of action of its step, the one that sent the message first.
  - **It no longer writes the block's `trail`.** Since 0.1.101 it writes the block only when the
    action has `vars`, a `menu` or a `subject`: 0.1.100's gate showed an empty `wf: {}` on every
    other message.
- **The router** writes its entry when it resolves a message with no destination, only on the
  sender's router (`handle_message`).
- **ai.generic:**
  - the `Message trace (UTC)` block, with the newest 32 entries;
  - the speaker's "via" comes from the trace's origin.
- **Cognition** takes the "via" from the trace's origin too.
- **Tests:** in each place.
- **The docs** that described the trail now describe the trace.

---

## 9. Open

**Who wrote a message** is not the trace's: it travels apart, in `meta.context.writer`, from
whoever writes the text (A-137, [cognition-hands-v1](cognition-hands-v1.md) §4). The trace stays
debug and log.

Nothing: the design closed on 2026-11-04. The last two points went this way:
- the length of `act`'s words: decision 13;
- words from the WF's definition: withdrawn for now (§7).
