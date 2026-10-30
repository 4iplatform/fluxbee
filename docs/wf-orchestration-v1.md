# WF orchestration v1: the WF as Fluxbee's deterministic interconnection

**Status:**
- **The direction** was agreed with the operator on 2026-10-27; the conversation is in
  [agent-tools-v1](agent-tools-v1.md) §6.2–§6.3.
- **The review of the WF in the code** was done on 2026-10-27, at `e49ffeaa`, as three readings:
  - the engine and its messages;
  - the definition language and CEL;
  - deploy, authoring and operation.

  Everything below was read in the code; a point marked *(read, not run)* was not exercised.
- **Case 0 (§5.1)** was decided by the operator on 2026-10-28. It was built, and validated on 8.x
  in 0.1.90 (2026-10-29; ledger 0.1.90).
- **Case 1 (§5.2)** is the operator's circuit of 2026-10-29: a sale handed to invoicing, then a
  due-date timer and a payment check. Its decisions were taken, and it was built and reviewed, on
  2026-10-29 (§5.2.1, §5.2.3). It was released in 0.1.91 and validated on 8.x on 2026-10-30
  (§5.2.2; ledger 0.1.91). The operator: close the WF topic completely, case by case; Archi
  through Cloud waits.
- **Case 3 (§5.3)** is the internal consultation: an agent asks a supervisor, and stays the
  person's voice. It was decided with the operator on 2026-10-30, with the menu of options, the
  catch, the cap on agent calls and the way back through io.slack. Case 4 (handing the
  conversation to a person of the team) and Case 5 (Archi writes and operates WFs) follow. The
  operator: *"no tomar otro tema hasta terminar este"*.

This is the single place for the track. The cases are worked here one at a time.

---

## 1. The direction (the operator, 2026-10-27)

- **AI agents are pure responders.** An agent has two kinds of event:
  - it answers a message;
  - a timer fires. That timer lives in a WF, never in the agent.
- **The WF is the deterministic interconnection:** *"el WF es la interconexión inevitable
  determinística hecha con workflows… no queda otra"*. It covers sequences, forwarding, timers and
  escalation, whenever Archi's orchestration needs them.
- **The WF is a forwarder.**
  - It rebuilds the message as it received it and never breaks its format.
  - It may change the destination, and, when its logic requires it, some content, through its own
    functions (CEL). Today it has *"dos o tres funciones nada más"*.
- **Routing is by node name, written in the definition.**
  - A person enters and leaves Fluxbee only through an IO node, and exists only once registered,
    through the frontdesk or cloud identity.
  - A WF never routes by ILK; its CEL chooses among names written in the definition.
  - The ILK travels intact inside the message, for the agent's memory and the IO node's delivery.
- **The pass-through pattern:**
  1. A timer, launched by the WF's logic, fires.
  2. The WF sends the agent a message.
  3. The agent evaluates it and answers the WF with the state of its evaluation. It *"puede
     colocar la variable con el valor correcto para que el CEL del WF ejecute y cambie
     dependiendo de la respuesta evaluada"*.
  4. The WF sends the result to its final destination.

  *"Es un pasamanos pero queda todo determinístico."*
- **OPA stays for rules of substance,** never for routing between nodes (FINDINGS A-113).

## 2. What works today

- **Deploy is on the standard path,** validated on 8.x (see [wf-standard-path-v1](wf-standard-path-v1.md)):
  - SY.admin → SY.wf-rules (compile, apply, rollback, delete) → a versioned package on the
    `wf.engine` base runtime → the orchestrator;
  - one validator (`go/pkg/wfcel`) runs at compile and at the engine's boot. It is strict and
    names the exact path of an error;
  - a broken definition leaves the node up in `FAILED_CONFIG`, with its reason.
- **What a WF can see and send:**
  - it sees who sent a message (`event.src`, the router-stamped name) and its whole `context`,
    including `context.io.reply_target`;
  - `{"$ref":"event.payload"}` and `{"$ref":"event.context"}` copy the payload and the context
    whole, attachments and `content_ref` included;
  - `thread_id_ref` answers an external caller on its own thread;
  - `target_ref` takes a destination from data (but see §4.9).
- **The agent's evaluation works:**
  - a WF asks for a structured answer with `meta.context.response_envelope`;
  - its CEL reads it with `json_parse(event.payload.content).field`;
  - an agent's error comes back as `<msg>.error`.
- **The engine:**
  - SQLite in WAL mode; state and internal events are committed atomically;
  - timers use a deterministic `client_ref`, are cancelled when an instance ends, and fires missed
    while the WF was down are recovered at its next start;
  - a fatal action routes the instance to a `failed` state that can notify;
  - the trace_id is kept along a chain.
- **Tests:** about 180 Go unit tests run in CI with `-race`, plus a deploy lifecycle e2e
  (`scripts/wf_rules_lifecycle_e2e.sh`).

## 3. Broken today

### 3.1 In the shipped examples, the docs and Archi's seeds (FINDINGS A-117)

- **No timeout ever fires.**
  - SY.timer puts the key at `payload.user_payload.timer_key`.
  - The example packages test `event.payload.timer_key`, and the docs test
    `event.user_payload.timer_key`. Both paths are wrong, so the guard is always false.
  - An instance waiting with a timeout waits forever. No test checks a real fire against a guard.
- **A send to an agent hangs without a trace.**
  - `send_message` defaults to `meta.type: system`.
  - An agent answers a system message from its control plane, and the WF drops that answer.
  - Archi's two seeds, `wf.invoice` and SY.admin's help example all send without a type.
- **`wf.router`'s error branches cannot fire:** they match `HOST_TURN` with
  `payload.type == 'error'`, and the error arrives as `HOST_TURN.error`.
- **`wf.invoice` relies on a JSON-Schema `default`** that validation never applies, so an invoice
  without a currency fails at creation.

### 3.2 In the engine (FINDINGS A-118)

- **A CEL map literal in `set_variable` panics the node** *(read, not run)*. The value cannot be
  serialized, and the receive loop has no recover. If the transition came from an internal event,
  it replays at every start.
- **An `emit_internal_event` that re-emits itself never stops.** The drain has no cap, and it
  blocks the node's only receive loop.
- **A crash loses the event in progress.** Nothing is replayed, and a crash during creation leaves
  a half-started instance. The spec promises at-least-once.
- **A redeploy moves running instances onto the new definition.** The docs say they keep their
  own. An instance whose state was removed errors on every event, and stays running forever.
- **A fired timer can fire again after a restart.** Its index row stays, so recovery fires it
  once more.
- **The WF does not start while SY.timer is unreachable** (it crash-loops). Recovery reads at most
  1000 timers.
- **Numbers change type across a restart:** live they are float64, restored they are int64. A
  guard can work after a restart and fail before it.
- **A `$ref` to a missing key fails the instance.** The spec says the field is left out.

### 3.3 Open doors (FINDINGS A-119)

- **The engine's verbs check no caller:** `WF_LIST_INSTANCES`, `WF_GET_INSTANCE`, `WF_GET_CODE`
  and `WF_CANCEL_INSTANCE`. Any node of the VPN can read an instance's state, which holds people's
  reply targets, or cancel it.
- **Anyone can forge a `TIMER_FIRED`** for an instance, by its `client_ref`.
- **An error answer can bounce:** an `INVALID_INPUT` to an unknown message carries no thread, so
  two WFs could bounce it back and forth *(read, not run)*.

## 4. What the direction needs

### 4.1 Forwarding the message as received

- **There is no "forward as received".** `send_message` builds a new frame:
  - its `msg` must be a literal written in the definition;
  - it carries exactly `type`, `msg`, `thread_id` and `context`;
  - `src_ilk`, `dst_ilk`, `ich`, `priority` and `scope` are dropped.
- **Half the meta is invisible:** the event map has no `src_ilk`, `dst_ilk` or `ich`, and no
  definition field can set them.
- **"Copy, then change one field" is impossible.** A `$ref` replaces a whole value, and a CEL map
  cannot be sent (§3.2). Editing a payload means rebuilding it, which drops `content_ref`.
- **No optional `$ref` and no default.** Forwarding a message without `context` or `thread_id`
  fails the instance.
- **The message is not byte-faithful.**
  - The payload and context are re-encoded through Go maps: keys reordered, numbers through
    float64, `<`, `>` and `&` escaped (HTML grows).
  - A frame over 128 KiB fails, because the Go SDK never moves long text to a blob.

### 4.2 The conversation and the person (FINDINGS A-116)

- **An instance is per message.** A person's second message never finds the first instance:
  correlation is only `thread_id == instance_id`, and nothing keys an instance by the
  conversation's thread.
- **A message from an IO node cannot advance an instance.** It has no `meta.msg`, and a
  transition needs one. It can only create an instance.
- **Keep the thread, or keep the answer.**
  - Sending on the instance's thread brings the agent's answer back, but every message is a new
    conversation for the agent.
  - Sending on the person's thread keeps the conversation, but the answer finds no instance. A
    forwarder's permissive `input_schema` then turns it into a new instance, which can loop with
    the agent.

  A definition cannot even read its own instance id.
- **Nothing filters what creates an instance** beyond the payload's schema. A late answer to an
  ended instance starts a new one.
- **The trigger never meets the transitions.** To branch on the first message, the initial state
  must emit an internal event first.
- **No per-request correlation:** two agents asked at once answer with the same thread and the
  same `msg`.

### 4.3 The agent's evaluation

- It works (§2). The remaining issues:
  - with tools, the schema is only an instruction to the model;
  - a long answer arrives as `content_ref`, which `json_parse` cannot read;
  - every agent call needs a second transition for `.error`, because `event_match` has no prefix
    or wildcard.
- `json_parse` stops at 64 KiB.

### 4.4 Timers

- **Times are literals.**
  - `timer_key`, `fire_in` and `fire_at` are written in the definition. A time taken from state,
    or from the agent's evaluation, cannot be expressed.
  - The operator's pass-through, with a time chosen at run time, needs this.
- **Nothing under 60 s; no recurring timer; no timer that starts an instance.** So a run cannot
  be started by a schedule.
- **Every timer request is fire-and-forget.**
  - SY.timer's refusals go unseen, and the WF's index drifts.
  - Re-scheduling a pending key keeps SY.timer's old time.
  - A reschedule after a fire fails silently.
- **Recovery runs only at start.** A fire lost while the WF is up waits for the next restart.
- **A timer carries no data:** its payload is fixed (`{instance_id, timer_key}`).

### 4.5 Functions (CEL)

- **CEL runs only in guards and in `set_variable`.** Payloads, targets, `msg` and timers hold no
  expression, and the `${...}` interpolation named in the code's comments does not exist.
- **The functions:**
  - custom: `now()` (the node's clock, not SY.timer's) and `json_parse`;
  - extensions: `ext.Strings` only;
  - plus CEL's standard library.
- **Shipped in cel-go v0.26.1 but not enabled, one option each:**
  - `ext.Math`, `ext.Lists`, `ext.Sets`, `ext.Encoders`, `ext.Regex`;
  - optional values (`?.`, `orValue`);
  - cross-type numeric comparisons.
- **Missing altogether:** JSON stringify, merge, set and delete; Unicode-aware case
  (`lowerAscii` leaves Ñ and accents as they are).
- **Limits:**
  - JSON-Schema defaults are never applied;
  - a guard's 10 ms limit does not stop its evaluation;
  - `set_variable` has no time bound and no cost limit.

### 4.6 Lifecycle and robustness

- **No delivery feedback:** an `UNREACHABLE` for the WF's own send is dropped.
- **Nothing bounds an instance:** no TTL, no cap on instances, no default answer timeout. Every
  running instance is loaded at start.
- **One serial receive loop for every instance.** The inbound channel holds 64 messages and drops
  the rest. A guard that times out under load drops its event.
- **The person never hears of a failure.** An unhandled event, or an `INVALID_INPUT` (a system
  message), never reaches the IO node.
- **The `failed` state cannot learn the cause** of the failure.

### 4.7 Authoring by Archi

- **No guide.** Archi's handbook says when to choose a WF, never how to write one: correlation,
  `meta.type`, timers and their payload, `on_error`, structured answers, the functions.
- **The two seeds teach the hang (§3.1).** Handbook §6.2 lists `workflow_definition` as an
  artifact the programmer cannot produce.
- **No check before deploy.**
  - There is no read-only `wf_rules_check` (OPA has `opa_check`) and no simulator.
  - Archi's shape check is dead code.
- **A `COMPILE_ERROR` blocks the pipeline** without reaching the designer, and
  `restart_from_design` drops the error.
- **The pipeline:**
  - it can mark a workflow deployed without a node;
  - it can never remove a workflow that has a running instance, because it deletes without
    `force`;
  - an update restarts the node twice.

### 4.8 Operating a running WF

- **Nobody can see or act on instances from SY.admin or Archi.** The engine's verbs exist (§3.3),
  but nothing exposes them.
  - Their answers use PascalCase keys and JSON strings.
  - The full list goes in one 128 KiB frame.
  - Cancel works only for instances in memory.
- **No stuck-instance view:** nothing shows "waiting since" or "waiting for".
- **Health reflects only the config.** `last_error` is never set, the logs are only in journald,
  and an instance's log keeps its last 100 entries.
- **`WF_HELP` is minimal,** and `NODE_HELP` is sent only to ai.generic nodes.

### 4.9 Decisions for the operator

- **`target_ref`** takes a destination from data. That goes against "routing by node name,
  written in the definition" (§1). The options:
  - remove it;
  - limit it to a list of names written in the definition. The 2026-07-24 design had that
    allowlist as optional, and it was never built.

  **Decided (§5.1, item 7):** removed, in 0.1.90.
- **A redeploy and running instances:**
  - keep each instance on the definition it was born with, as the docs promise; or
  - move it explicitly, and report the states that no longer exist.

  **Decided (§5.1, item 3):** each instance keeps its definition, in 0.1.90.
- **The conversation model (§4.2):**
  - one instance per conversation, keyed by the IO node's thread; or
  - one instance per message, with the conversation carried through to the agent.

  **Decided (§5.2):** one instance per conversation, on 2026-10-29.
- **What "forward as received" may change:** the destination only, or named fields too.

  **Decided (§5.2):** the metadata only, on 2026-10-29. The message's content is never touched.

## 5. Case by case (proposed; the operator picks)

- **Case 0, before anything: what is broken today** (§3, FINDINGS A-117 to A-119). The timeouts,
  `user` by default, the examples, the seeds, the panic, the unbounded loop, the duplicate fire
  and the open verbs. **Done in 0.1.90 (§5.1).**
- **Case 1, the operator's circuit (§5.2, 2026-10-29).** A conversation enters through sales and
  is handed to invoicing, and the invoice's due date fires a payment check. It takes in two cases
  this list first proposed:
  - the old Case 1, the pass-through: a WF timer asks an agent for an evaluation, and the result
    goes to a person who wrote before;
  - the old Case 2, a WF in front of a live conversation (IO → WF → AI → WF → IO).

  It needs:
  - the conversation model (§4.9);
  - a message from an IO node that advances an instance;
  - "forward as received", with `src_ilk`, `ich` and the thread;
  - the agents' answers correlated by the conversation;
  - the structured answer;
  - a timer time taken from an agent's answer;
  - the IO node's `reply_target` kept in state.

  **Done in 0.1.91 (§5.2).**
- **Case 2:** taken into Case 1 on 2026-10-29.
- **Case 3, the internal consultation (§5.3, decided 2026-10-30).** The agent asks a supervisor
  through the WF, and stays the person's voice. With it come the menu of options, the catch, the
  cap on agent calls (A-125), the way back through io.slack, and Archi's guide.
- **Case 4, handing the conversation to a person of the team.** The WF bridges the person's IO
  node and the team's until the conversation comes back. Its open questions are in §5.3.
- **Case 5, Archi writes and operates WFs:**
  - a check before deploy, with `COMPILE_ERROR` back to the designer. Today's `check` is a
    compile, and it stages the definition (FINDINGS A-128);
  - instances visible through SY.admin;
  - the pipeline's problems of §4.7.

  Archi's guide to writing WFs moved into Case 3.
- **Later, in the IO family:** writing first to a person who never wrote (FINDINGS A-114).

### 5.1 Case 0 (0.1.90): what is broken today. Decided by the operator, 2026-10-28

1. **A WF always sends `user`.**
   - `meta.type` leaves the definition, and validation refuses it.
   - SY.\* and RT.\* are already refused as destinations, so a `system` send had no legitimate
     destination left. It only hung the instance.
   - **Decided in principle, for the forwarding case (§5, Cases 1–2):** the operator's idea of a
     trail. A field in the message's meta lists the nodes it passed through, and each forwarding
     WF adds its own name. It serves the logs, debugging, and the agent, which then knows where a
     message comes from. *"Si es complicado, como lo propusiste vos por ahora sirve."*
2. **`event.timer_key`.** The engine exposes a fired timer's key directly, and `event.payload`
   stays as it is. The examples, the docs and Archi's seeds use it.
   - The operator: *"esto es causa de evolución de ideas, estandarizar la herramienta es lo
     correcto"*.
   - A test fires SY.timer's real `TIMER_FIRED` shape against a guard.
3. **Each instance finishes on the definition it was born with.**
   - Each instance records its definition's hash, and the engine keeps every definition a running
     instance still uses.
   - Recovery binds by hash. A new instance takes the current definition.
4. **At least once.**
   - The engine saves an inbound event before it processes it, and drops it once the transition
     has committed.
   - Recovery processes again whatever is left. Creation is covered too.
   - A repeated send keeps its `trace_id`.
5. **A `$ref` to a missing field still fails the instance.** A new optional form gives a default:
   `{"$ref": "event.context", "default": {}}`. The spec is corrected: it said the field was left
   out.
6. **Who may talk to the WF's controls** (FINDINGS A-119; the operator: a security topic that
   stays open):
   - the instance verbs answer only SY.admin@motherbee and SY.wf-rules of the WF's hive;
   - a `TIMER_FIRED` is taken only from SY.timer of the WF's hive;
   - an error answer, a `*_RESPONSE` or a router notice is never answered, so two WFs cannot
     bounce an `INVALID_INPUT`. This is the rule SY.timer and the AI nodes follow.
7. **`target_ref` goes.**
   - A WF that chooses among destinations does it with guards and names written in the definition,
     one transition per destination. `wf.router` is rewritten that way.
   - `thread_id_ref` stays: it is not routing.
   - The operator: *"mejor ir de lo simple a lo complejo… si quieren routing nuevo cambien el
     CEL"*.

**Fixed with them, no decision needed:**
- **The CEL map panic:** CEL values are converted to plain JSON before they are stored, and a
  panic in a transition fails that instance, never the node.
- **A cap on internal events per transition.** Past it, the instance fails with that cause.
- **A fired timer's index row** goes when the fire arrives, so a restart cannot fire it again.
- **Numbers keep one type,** live and after a restart.
- **Recovery is no longer fatal when SY.timer does not answer.** The WF starts degraded and
  reconciles when SY.timer answers.
- **The examples, the seeds and the docs:**
  - the examples use the new key and `user`;
  - `wf.router`'s error branches match `<msg>.error`;
  - `wf.invoice` no longer relies on a schema default, which the engine does not apply;
  - SY.admin's help example is a valid definition;
  - the spec and the quickstart match the code (§6).

**Built and validated on 8.x in 0.1.90 (2026-10-29; ledger 0.1.90).**
- **The review round added, beyond this list:**
  - a late answer on an ended instance's thread starts no instance (an agent answers with the
    request's name, so it used to start one: a WF↔AI loop);
  - a `TIMER_FIRED` is taken only for a timer the WF scheduled, because SY.timer fires a timer
    another node scheduled under its own name;
  - a system message other than `TIMER_FIRED` is no workflow event, as in the AI and IO nodes;
  - `event_match.type` left with `meta.type`;
  - an event that does not finish in 3 starts fails its instance, so one bad event cannot
    crash-loop the node;
  - integer literals in a definition stay integers on the wire;
  - a malformed `$ref` is refused;
  - a test validates every definition in the docs.
- **Left open, recorded:**
  - a message on a running instance's thread drives it, whoever sends it (FINDINGS A-123, the
    security topic decision 6 left open);
  - a send to an agent with `thread_id_ref` loops (A-116, Case 2's correlation);
  - SY.timer takes a timer for another node from any node (A-115).

### 5.2 Case 1 (0.1.91): the operator's circuit. Decided and built 2026-10-29, validated 2026-10-30

**The circuit:**
1. A registered person writes. The IO node hands every message to the WF, which sends it to
   `AI.sales`: every conversation enters through sales.
2. The message is about an invoice. Sales answers the WF with a field (`area`), and the WF's CEL
   forwards the person's message to `AI.invoice`. From then on, invoice holds the conversation.
3. `AI.invoice` issues the invoice and answers with its data, the due date included.
4. The WF keeps the invoice in the instance's state and sets a timer at its due date. The time
   comes from invoice's answer, not from the definition.
5. At the due date, the WF writes to `AI.payments`: the payment period has ended, and the customer
   must be told the invoice was not paid. The agent decides what to say.
6. The WF delivers that answer to the person through the IO node.

The timer stands in for checking the payment's real state through an API. It is a simplification,
though a realistic case for an agent.

Every hop is the WF's, because the agents only answer (§1). The names (`AI.sales`, `AI.invoice`,
`AI.payments` and the IO node) are written in the definition.

**The operator's rule (2026-10-29):** whenever a solution involves several agents, a WF sits at the
IO node's exit as the router, however many agents take part.

**Decided: one instance per conversation,** keyed by the IO node's thread.
- The WF must remember that the conversation now belongs to invoice, and that lives in the instance.
- The person's messages and the agents' answers travel on the conversation's thread. So they all
  reach the same instance, and each agent keeps its memory. This removes A-116's choice between
  keeping the conversation and getting the answer (§4.2).
- With one instance per message, every message would go through sales again.

**Checked in the code for this case:**
- An agent answers whoever sent it the message (`build_reply_routing`,
  crates/fluxbee_ai_sdk/src/message.rs). With the WF in front, every answer comes back to the WF.
- An agent's memory is per thread and agent node (`package_for`, src/router/mod.rs). Invoice starts
  its own memory of the conversation at the forwarded message, and never sees what was said to
  sales.
- An agent's model sees the payload's text and attachments and its memory package. It never sees
  `meta.context` (the message handler in nodes/ai/ai-generic/src/bin/ai_node_runner.rs). Since
  Case 1 it also sees the WF's block, `meta.context.wf` (§5.2.1).
- IO.api sends every message to one configured `io.dst_node`, here the WF. It only takes messages
  in: it ignores any answer (`run_router_loop`), and this version has no outbound delivery
  (docs/io/io-api-node-spec.md).

**The test.** The operator: no extra pieces in the infrastructure for now; the integrated tests
come later.
- The messages are sent from inside, with Case 0's gate tool, and the answers are read in the
  trace.
- The due date is a few minutes away; SY.timer's floor is 60 s.

**Decided: a forward changes only the metadata.**
- The message's content, its text and its attachments, is never touched.
- The example that raised it: in step 2 the person writes *"dale, facturámelo"*. Invoice receives
  that sentence alone, and the order (10 licenses at their price) was said to sales.
- When an agent needs more to answer, the WF gives it deterministic context in the metadata, and
  the agent reads it. Named so far:
  - the path the message took (Case 0's trail);
  - variables of the WF's state, named in the definition;
  - whatever other metadata a case needs.
- A message the WF writes itself, as in step 5, has its text written in the definition and its
  data in the metadata.
- The operator: *"este tema es el más profundo"*. Which metadata, and how an agent reads it, will
  be worked out with real cases. Case 1 builds the least it needs.

**Decided: control goes back to sales in two ways,** and Archi's guide recommends both:
- the agent hands the case back, for example `AI.payments` closing it;
- an inactivity timer.

Which of them a WF uses is its author's choice (*"flavor del programador"*).

**Decided: only the nodes involved drive an instance** (A-123): those written in the definition,
and SY.timer for the WF's own timers. *"Determinístico… no veo por qué algo adaptable."*

**Built** as §5.2.1 describes, then one review round (§5.2.3). Released in 0.1.91 and validated on
8.x on 2026-10-30 (§5.2.2). The operator, 2026-10-29: *"avanzá como describís… terminá cuando esté
todo probado"*.

#### 5.2.1 The build design

**Where the code stands** (read on 2026-10-29, at `4a32cf64`):
- **Correlation** (go/nodes/wf/wf-generic/node/correlate.go, `dispatchWorkflowEvent`): an event
  reaches the running instance whose id is its thread. Otherwise it starts one, filtered only by
  `input_schema`.
- **A transition matches by `meta.msg` only** (instance.go, `matchesEvent`). A person's message has
  no `msg`, and neither has an agent's answer to one.
- **`send_message` builds a new frame** (actions.go, `execSendMessage`): its thread is the instance
  id, its payload is rebuilt from the definition, and `src_ilk` and `ich` are not carried.
- **Timer times are literals** (actions.go, `execScheduleTimer`).
- **A transition always leaves its state:** the exit actions, its own actions, then the target's
  entry actions (instance.go, `RunTransition`).
- **The Go SDK encodes frames with `json.Marshal`** (go/fluxbee-go-sdk/client.go, `Send`). It
  escapes `<`, `>` and `&` inside a payload passed as raw JSON.
- **An agent's model sees the payload's text and attachments, and the memory block**
  (nodes/ai/ai-generic/src/bin/ai_node_runner.rs: the user-message handler, and
  `render_memory_package_prompt_block`).
  - A structured answer comes back as JSON in the text (crates/fluxbee_ai_sdk/src/message.rs,
    `response_contract_to_output_schema_spec`).
  - The answer's context is the request's, minus `thread_id`, `src_ilk` and `response_envelope`
    (`sanitize_reply_context`).

**The engine (Go):**
1. **One instance per conversation.**
   - An instance's thread is the `thread_id` of the message that started it, or the instance's
     own id when that message had none.
   - Every message the instance sends carries that thread. The agents keep one memory per
     conversation, and their answers come back to the instance.
   - `thread_id_ref` leaves the language, because the instance's thread already answers a caller on
     its own thread. A definition that still has it is refused, as `target_ref` was in 0.1.90.
2. **`start_from`, required:** the node names whose messages start an instance. Each is a valid
   name with a hive, never SY.\*, RT.\* or AI.\*. An agent only answers (§1), so its message is
   never a request; an agent in `start_from` that the WF also asks would have its late answer start
   a new instance, the WF↔AI loop of A-118 (the review round, §5.2.3).
3. **Only the nodes involved drive an instance** (A-123). The involved nodes are those in
   `start_from`, those in any `event_match.src`, and every `send_message` or `forward` target. An
   event follows this order:
   1. a `TIMER_FIRED` is taken as in 0.1.90: from SY.timer of the hive, for a timer this node
      scheduled;
   2. from a sender that is not involved: dropped and logged, never answered;
   3. on the thread of a running instance: that instance;
   4. a message from a `start_from` node that is not a reply: a new instance on its thread, as the
      `input_schema` allows (an `INVALID_INPUT` otherwise, unless it is never answered);
   5. anything else, such as an agent's answer: dropped, never answered.

   So a person who writes again after the instance ended starts a new one on the same
   conversation, at the initial state.
   - The sender is judged by the running instance's own definition on its thread, and by the
     current definition otherwise. Recovery drops a journaled creation whose sender the current
     definition no longer starts from, and gives a running instance a creation replayed on its
     thread.
   - An agent's answer that arrives after its instance ended is dropped only while no instance
     runs on the thread. When the conversation's next instance is running, the answer reaches it.
   - A message the current state has no transition for is logged and dropped, never queued.
4. **`event_match.src`:** a transition can match the sender's name, the router-stamped
   `event.src`, alone or together with `msg`. At least one of the two is required, and with both,
   both must match. The name is written with its hive, never SY.\* or RT.\*.
5. **`forward`** sends a message on as it was received.
   - The message is the event's, or one kept earlier (`"message": "<name>"`, item 6).
   - The payload goes out byte for byte, never decoded.
   - The meta is the message's, every field but `memory_package`.
   - What the WF changes, all of it metadata:
     - the destination, a `target` written in the definition;
     - the thread, which becomes the instance's;
     - the WF's block (item 7);
     - the `context` keys the action writes, resolved by the `$ref` rules, for instance
       `response_envelope`.
   - It fails when there is no message to forward (a `TIMER_FIRED`, an internal event, a name never
     kept), or when the message's context is not a JSON object. Its `on_error` defaults to `fail`,
     as for `send_message`.
   - Validation refuses `meta.msg` and `payload` on a forward, and a `message` that no
     `keep_message` of the definition keeps.
6. **`keep_message`** keeps the event's message, whole and as received, under a name:
   `{"type": "keep_message", "name": "customer"}`.
   - A later keep under the same name replaces it.
   - Kept messages are committed with the transition, and go when the instance ends.
   - It refuses a `TIMER_FIRED` and an internal event, and its `on_error` defaults to `fail`.
7. **The WF's block, `meta.context.wf`,** goes on every message a WF sends.
   - `trail` lists the nodes the message went through. A `forward` takes the message's own trail
     (or its sender's name, when it has none) and adds the WF's name. A `send_message` starts it
     with the WF's name.
   - `vars` holds what the action's `vars` writes, `$ref`-resolved: the WF's deterministic context
     for the receiver. It is left out when the action has none.
   - Neither action's `context` may write `wf`, and validation refuses it. A `wf` that a `$ref`
     context brings is overwritten.
8. **Timer times from state:** `fire_in` and `fire_at` take a literal or a `$ref`, in
   `schedule_timer` and `reschedule_timer`. The value is resolved when the action runs, must be a
   string, and the 60 s floor is checked then, for `fire_at` too. A `$ref`'s default is checked at
   load.
9. **Internal transitions:** a transition without `target_state` runs its actions and stays in its
   state, with no exit or entry actions. An event that arrives early can then be noted without
   leaving the state, for instance a due date that fires while an agent is answering.
10. **The Go SDK encodes frames without escaping HTML,** so a forwarded payload goes out as it came.
    `wfcel.EncodeJSON` stops escaping too, and the journal and the kept messages use the frame's
    encoding.
11. **The store, schema v4:** an instance keeps its thread (indexed), and a new table holds its kept
    messages. A v3 instance takes its own id as its thread, which is what its sends carried.
    `WF_LIST_INSTANCES` and `WF_GET_INSTANCE` return the thread.
    - **The upgrade:** a definition from before 0.1.91 has no `start_from`, so it no longer loads.
      Its node is FAILED_CONFIG until the workflow is redeployed, and its running instances fail
      at the next start (`definition_unavailable`). 8.x had no WF deployed.
12. **Validation (go/pkg/wfcel):** every rule above, naming the exact path of an error, as today.
    The review round added (§5.2.3):
    - an action refuses the fields of other action types;
    - `keep_message`, and a `forward` without `message`, are refused in a transition matched on
      `TIMER_FIRED`;
    - a `target` with surrounding spaces is refused;
    - a duration whose day count overflows is refused;
    - a literal `response_envelope` is checked against the AI SDK's contract rules, `text_field`
      included.

**The agents (Rust):**
1. **ai.generic shows the WF's block to the model,** before the person's text, the way it shows the
   memory block.
   - It shows the trail as a path, and `vars` as JSON, cut at 8 KiB with a mark.
   - It does so for text and multimodal input alike. The block is not kept in the conversation's
     history.
2. **An answer never carries the request's `wf` back** (`sanitize_reply_context`), nor the
   request's `evaluation`: an answer carries only its own.
3. **A response contract may name a `text_field`,** a required string property.
   - The answer's text is then that field, and the whole object goes in the answer's
     `meta.context.evaluation`. A WF delivers the text and branches on `event.context.evaluation`,
     without touching the content.
   - Without `text_field`, an answer stays what it is today.
   - An output that is not the contract's object is answered as an error, with tools and without
     them. The whole object is checked: required fields, types, enums and no extra properties. The
     object is found inside a fence or among other text. The error's code is
     `invalid_structured_output`, not retryable; a `text_field` that is not a required string makes
     the contract invalid (`ai_runtime_error`).
   - The history keeps the whole object as the agent's turn, so a blank text never leaves a blank
     turn (Anthropic refuses one).
   - With tools, the contract is an instruction in the system prompt, checked afterwards; the
     provider enforces it only on the call without tools. The instruction asks for a value of its
     type in every field, never null.

**Every definition in the repo changes** (pre-beta, no legacy):
- `start_from` is added and `thread_id_ref` goes, in the examples, Archi's seeds, the docs,
  SY.admin's help example, the scaffold and the tests;
- `wf.router` is rewritten with `forward` (0.0.7): a person's further message while an agent
  answers goes to that agent;
- the spec's case study (§19.2), with the quickstart, SY.admin's help example and the seeds, starts
  from an IO node, `IO.api.billing@motherbee`, instead of the agent `AI.billing` (item 2);
- `examples/wf_client.rs` connects as `WF.client.example@motherbee`, which `wf.invoice` names in
  `start_from`; Archi's shape check refuses a definition without `start_from`;
- a test still validates every definition in the docs.

**Case 1's definition,** shipped as the example `wf.sales.invoice` (a package name takes no `_`):
- **`start_from`:** the IO node, `IO.api.sales@worker1`. **The agents:** `AI.sales`, `AI.invoice`
  and `AI.payments`, on worker1.
- **`start`** keeps the person's message (`customer`) and their `context.io`, and moves on to sales.
  A message without `context.io` fails the instance: every IO node sends one.
- **`sales_thinking`** forwards `customer` to sales with its contract: `reply`, `area` (sales or
  invoice), `product`, `quantity` and `unit_price`, all required.
  - `area: invoice` → `invoice_thinking`, with the order kept in state.
  - Any other answer is delivered to the person → `sales_listening`.
- **`invoice_thinking`** forwards `customer` to invoice, with `vars.order` and its contract:
  `reply`, `area` (invoice or sales), `invoice_number`, `amount` and `due_in`.
  - An issued invoice (a number, a `due_in` such as `2m`, `1h` or `30d`, and none pending) is
    delivered to the person, and the timer `payment_due` is set at `due_in` → `invoice_listening`.
  - A number with a malformed `due_in` is delivered, without a payment check, and `last_error`
    says so → `invoice_listening`.
  - `area: sales`, the agent handing back: its answer is delivered, and sales holds the
    conversation from the person's next message → `sales_listening`.
  - Any other answer is delivered → `invoice_listening`.
- **Every thinking state:** an agent's error, or no answer in 120 s, sets `last_error` (for example
  `AI.sales@worker1: error`) and goes to `sales_listening`. The conversation and a pending payment
  check survive, and the person gets no answer to that message. A payments error or timeout also
  clears the pending invoice, whose due date already fired. Each state has its own timer
  (`sales_timeout`, `invoice_timeout`, `payments_timeout`), so a late fire of one never ends the
  next.
- **`sales_listening` and `invoice_listening`:** the person's next message is kept, and goes to the
  agent in control. A later answer is delivered: in `sales_listening`, of sales, invoice or
  payments; in `invoice_listening`, of invoice.
  - After 3 minutes without messages, invoice gives control back to sales.
  - After 3 minutes without messages, sales closes the instance, when no invoice is pending.
- **`payments_thinking`** starts when `payment_due` fires. The WF writes to `AI.payments`
  (`PAYMENT_DUE`): *the payment period has ended; tell the customer it was not paid*. It adds
  `vars.invoice` and the person's `context.io`.
  - Payments answers free text, without a contract. Its answer is delivered → `sales_listening`,
    with nothing pending: the agent hands the case back.
- **`closed`** is terminal, and so is `failed`, kept for the engine's own failures (a
  `payment_due` that cannot be scheduled).
- **Early events:**
  - a `payment_due` that fires while an agent is answering is noted by an internal transition, and
    acted on in the next listening state;
  - a person's message during an answer goes to the same agent.
- **One person message costs at most two agent calls:** sales, then invoice. A handback never asks
  sales again with the same message, so the two agents cannot bounce it (§5.2.3).

**Tests before the release:**
- the Go unit tests of each item, under `-race`;
- an engine test that runs the whole circuit with fake agents and a fake SY.timer: the handoff, a
  handback, a due date during an answer, the idle close, a stranger refused, a late answer
  dropped, and a new instance after the close;
- the Rust tests of the three agent items;
- the shipped definitions and those in the docs, validated.

**Left out of Case 1, recorded:**
- a message the WF writes itself carries no `src_ilk` (the rest of A-116);
- IO.api delivers no answer, because it only takes messages in (the IO family's own track);
- Archi's guide to writing WFs, with handback and inactivity timers as the recommended pair
  (Case 3, §5.3);
- a cap in the engine on the agent calls between two triggers, as a backstop for a definition
  whose agents keep answering each other (A-125, with Case 3, §5.3);
- the contract enforced by the provider inside the tool loop too.

#### 5.2.2 The validation on 8.x

**The setup, from inside.** The operator asked for no extra pieces in the infrastructure.
- Three agents in the `pruebas` tenant on worker1: `AI.gate0191sales`, `AI.gate0191invoice` and
  `AI.gate0191payments`, with immediate memory on. Their Spanish prompts follow their contracts,
  and a test invoice falls due in 2 minutes.
- `WF.gate0191@worker1`, deployed through SY.admin, with Case 1's definition on those names.
- A client on worker1's router socket, named `IO.gate0191`, stands for the IO node. It sends a
  registered person's messages, with a `meta.context.io` as an IO node does, and receives what the
  WF delivers.

**The script:**
1. *"Hola, quiero cotizar 10 licencias del plan Pro"*: sales answers.
2. *"Dale, facturámelo"*: sales hands the conversation to invoice, which issues the invoice. Its
   answer arrives, with a trail through invoice.
3. *"Gracias"*: it goes straight to invoice.
4. At the due date, payments' notice arrives.
5. After 3 minutes without messages, the instance closes.

**The checks:**
- the trail of every delivery, and the instance's states and log, through SY.admin;
- a stranger on the conversation's thread is dropped (A-123);
- a late answer starts nothing;
- a message after the close starts a new instance at sales;
- a restart of the WF in the middle keeps the conversation.

Afterwards the gate's objects are removed. `pruebas`, its keys and `AI.pruebas` stay.

**Run on 2026-10-30, on 0.1.91 (ledger 0.1.91). Every check held.**
- **The setup:** the agents ran on the hive's model, Claude Sonnet 5.5, with their roles composed.
  The WF compiled in 5 s: 8 states, 55 actions and 34 guards.
- **The script:**
  1. Sales answered the quote in 3 s, with the trail `[AI.gate0191sales, WF.gate0191]`.
  2. *"Dale, facturámelo"*: sales answered `area: invoice`, and the WF forwarded the same message to
     invoice with the order in its block. Invoice issued `F-0191-0001` for 1000, due in `2m`. The
     person got invoice's answer 8 s after the message, and nothing from sales.
  3. After the WF's restart, *"gracias"* went straight to invoice. Invoice remembered the invoice and
     issued none.
  4. SY.timer fired `payment_due` exactly 2 minutes after the invoice, across the restart.
     Payments' notice reached the person 3 s later, as `PAYMENT_DUE` on the conversation's thread.
  5. The instance closed 3 minutes after the notice, with nothing kept and no timer left.
- **The checks:**
  - **A stranger:** `IO.gate0191x` wrote on the conversation's thread. It was dropped ("the
    definition does not involve its sender"), and nothing was delivered.
  - **A late answer:** written under `AI.gate0191payments`'s name, once on the closed conversation's
    thread and once on a fresh one. Both were dropped ("no running instance on its thread, and it
    starts none").
  - **After the close:** the person's next message on the same thread started a new instance at
    sales.
  - **The restart:** recovery loaded the instance, with its state and timers unchanged.
  - **The person:** the agents saw the person's `src_ilk` on every forwarded message. Payments
    logged `missing src_ilk` on the WF's own `PAYMENT_DUE`, as recorded (A-116).
  - **The language:** six definitions were refused through SY.admin with `COMPILE_ERROR`, naming
    the path:
    - `AI.*` in `start_from`;
    - `SY.*` in `start_from`;
    - no `start_from`;
    - a forward with `name`;
    - a `keep_message` on a timer;
    - a target with a space.
  - **The store:** the inbound journal and the internal events were empty at every check.
  - **Health** was clean on the four hives.
- **Read from the WF's store, read only:** the instances' states and logs. SY.admin has no instance
  actions yet (Case 5).

#### 5.2.3 The review round (2026-10-29)

Five reviewers, each blind to the others' lens: the engine, security, the definition language, the
docs, and the agents. Everything below was fixed before the release.
- **One person's message could bounce between sales and invoice forever** (security). A handback
  asked sales again with the same message, and each hop re-armed the answer timer; a probe made 101
  agent requests from one message. Now a handback delivers invoice's answer, and sales takes the
  person's next message.
- **An agent in `start_from` that the WF also asks** would have its late answer start a new
  instance, the loop of A-118 (docs, engine, security). `start_from` refuses `AI.*` (§5.2.1 item 2).
  The spec's case study had exactly that shape.
- **The person could end a pending payment check** (security): an agent error or timeout failed
  the instance with its timers, and a malformed `due_in` failed it after the invoice was
  delivered. Errors and timeouts now go back to sales, and only a well-formed `due_in` sets the
  timer.
- **A late timer fire at a handoff could end the next state** (engine, language): one timer per
  state.
- **A replayed creation could put a second instance on a conversation's thread** (engine): it
  becomes the running instance's event, and the registry refuses a second instance on a thread.
- **A blank text under a `text_field` left a blank turn in the agent's history**, and Anthropic
  refused every later turn of that person (agents): the history keeps the whole object.
- **A `forward` with `name` instead of `message` loaded and forwarded the wrong message**
  (language): an action refuses other types' fields.
- **`wf.router` dropped a person's second message while an agent answered** (language, docs).
- **Smaller:** a target with spaces, a day count that overflowed, a `keep_message` on a timer's
  transition, a contract the agents refuse (now checked at load), a fenced object followed by text,
  the model's output missing from a failure's log, and the docs: the late-answer rule, the upgrade,
  what a forward copies, who can start an instance, and Archi's seeds and SY.admin's help on errors
  and sender-only matches.
- **Kept open:** a cap on agent calls between two triggers (A-125, with Case 3), and the contract
  enforced by the provider inside the tool loop.

### 5.3 Case 3: the internal consultation. Decided with the operator, 2026-10-30

**The operator's split:** escalation is two cases, built and validated one after the other.
- **The internal consultation (this case):** the agent asks a supervisor, the supervisor decides,
  and the agent stays the person's only voice.
- **Handing the conversation over (Case 4):** a person of the team takes the conversation, and the
  WF bridges two IO nodes until it comes back.

**The circuit:**
1. A person asks sales for a 20% discount on 10 Pro licenses.
2. Sales answers with the option `consult`. Its `question` carries the data to decide, and its
   `reply` tells the person that it will check.
3. The WF forwards the reply to the person. It writes the question to the supervisors' IO node,
   `IO.slack.supervisores`, a name written in the definition, and waits, with a timer.
4. A supervisor answers in Slack, in the thread of that post, and the answer reaches the same
   instance.
5. The WF forwards the answer to sales. Sales answers the person, and the WF delivers it.

#### 5.3.1 Decided: the WF hands the agent a menu

- **What an agent may do is fixed and clear, and never in its role:** *"me molesta que esté en el
  prompt"*.
  - Case 1's gate had to write the routing rules into the agents' roles.
  - The contract reached the model only as `area:string(sales|invoice)`, appended to the system
    prompt (`build_output_schema_fallback_instruction`, crates/fluxbee_ai_sdk/src/llm.rs).
- **The WF sends the possible actions in the message,** *"como opciones y no otra cosa"*. Each WF
  builds its own menu, and a role keeps only the agent's trade, so it stays reusable.
- **The menu** is an action's `menu`. The engine writes it into the WF's block
  (`meta.context.wf`), next to the trail and the variables. It has:
  - `help`: a fixed text that gives the choice context, in case the person's message is unclear:
    *"algo acotado pero que le dé contexto a la elección"*. The instance's data travels in `vars`,
    as in Case 1;
  - `options`: each with a `name`, a `description` and `params`. Each param has a type (string,
    boolean, integer or number, with an enum for strings) and a description.

  ```json
  "menu": {
    "help": "The person is buying licenses. Once they accept the quote, invoicing issues the invoice; a discount above 10% needs a supervisor's approval.",
    "options": [
      {"name": "answer",  "description": "You answer the person yourself."},
      {"name": "invoice", "description": "They accepted the quote and ask for the invoice.",
       "params": {"product": {"type": "string"}, "quantity": {"type": "integer"}, "unit_price": {"type": "number"}}},
      {"name": "consult", "description": "A supervisor must approve something before you answer.",
       "params": {"question": {"type": "string", "description": "What you ask, with the data to decide."}}}
    ]
  }
  ```

- **The agent answers `{reply, option, params}`:**
  - `reply` is the text for the person, which the WF forwards or not;
  - `option` and `params` are what the CEL reads: `event.context.evaluation.option`,
    `event.context.evaluation.params.question`.
- **"None of the options" is always an answer:** ai.generic adds `none` to every menu, with one
  param, `reason`. A definition cannot use the name.
- **An answer off the menu is an error, with no retries:** *"coincido en no hacer cosas raras con
  esto, por ahora queda en error"*. These come back to the WF as an error payload:
  - an option the menu does not have;
  - a missing or mistyped param;
  - no JSON at all.
- **Limits,** checked at compile: `help` up to 600 characters; each description up to 200; up to
  8 options, plus `none`.
- **One way to ask an agent within a WF:** the menu takes the place of `response_envelope` and
  `text_field` in a WF's actions. IO nodes keep `response_envelope` for their own callers (IO.api,
  IO.cloud, the frontdesk).

#### 5.3.2 Decided: the catch, as in .NET

- *"El catch hacerlo igual que el .NET, que podés programar qué querés que haga, o el default,
  que ahora sería log."*
- **A definition may have a `catch`:** a list of transitions, like a state's.
  - The engine tries it when the current state's transitions take nothing, in any state.
  - A state can take `none` or an error with its own transitions first, as a local try/catch
    does.
- **What reaches it:**
  - `none`;
  - an agent's error: an answer off the menu, a timeout, a provider failure;
  - any event no transition took.

  Its guards tell them apart as any guard does: `event.context.evaluation.option == 'none'`,
  `event.payload.type == 'error'`.
- **What it does is the designer's choice:** for example, hand the conversation to a person
  (Case 4), or ask another agent.
- **Without a catch, or when none of its transitions takes the event, the default is the log,**
  as today.
  - The instance's log gets an `unhandled event` entry, and the instance stays where it was
    (go/nodes/wf/wf-generic/node/instance.go, where no transition is chosen).
  - The engine also writes a line to the node's log. Today it does not.
- **Where the failures come from.** The operator asked whether only a badly built CEL leads here.
  It does not:
  - the model does not follow the menu. This is the likeliest;
  - the provider fails;
  - the definition offers an option that no transition takes.

  wfcel also refuses, at compile, a guard that compares the option with a name that no menu has
  (`option == "invoce"`).

#### 5.3.3 Decided: the cap on agent calls (A-125) comes with the catch

- **Why now:** the catch makes a loop easy to write. The agent fails, the catch asks the same agent
  again, and so on.
- **The cap:** the engine sends at most 8 messages to agents (`AI.*`) between two triggers. A
  trigger is a message from an IO node or another WF, or a timer fire.
- **Past the cap:**
  - the send is not made;
  - the instance takes it as an error, with the cause `agent_call_cap`: the catch if the
    definition has one, else the log;
  - the instance does not fail, so its timers stay. The next trigger starts a new count.

#### 5.3.4 Decided: the way back belongs to the IO node

- **The principle (the operator):** *"el que responde tiene que ser dueño o saber más que nadie
  del dato para decidir.. en este caso parece ser el IO porque creó el evento de comunicación
  inicial."*
- **How it fails today,** verified:
  - every message an instance sends carries the instance's thread (`meta.ThreadID`,
    go/nodes/wf/wf-generic/node/actions.go);
  - io.slack posts a message with no `reply_target` at the top level of its channel
    (nodes/io/io-slack/src/main.rs, the outbound target);
  - a reply in the thread of that post gets a new thread id, made from the channel and the post
    (nodes/io/common/src/io_context.rs). So it misses the instance;
  - an agent's answer carries the `context.io` of the message it answers
    (crates/fluxbee_ai_sdk/src/message.rs). Sales' answer to the supervisor therefore carries the
    supervisors' channel. Forwarded to a person's io.slack, it would be posted in that node's
    channel, which the node forces, but with the other channel's `thread_ts`, which it keeps: a
    thread that does not exist there.
- **The design:** each IO node knows where a thread is on its side.
  - **What io.slack records, on disk:** the thread id of each Slack thread it handles. That covers
    the threads it makes from people's messages, and each post it makes on a thread it did not
    know, such as the WF's question.
  - **Why on disk:** a supervisor may answer hours later, after a restart.
  - **In:** a reply in the thread of a recorded post takes that post's thread id.
  - **Out:** a message on a recorded thread goes to that thread's place on the node's side. A
    `reply_target` of another channel is ignored.
  - **What the WF carries:** only the thread id.
- **Scope:** io.slack, the normalized IO node. The other IO nodes follow in the IO family's track.
- The operator expects thread ids to be *"otro debug grande cuando empecemos a hacer pruebas
  reales"*.

#### 5.3.5 Decided: Archi learns to write this, from one handbook

- *"A Archi vamos a tener que ponerle info para que sepa programar esto… de hecho Fluxbee necesita
  un handbook único, sería el de Archi, y que lo use el que quiera, sea humano o quien sea que
  necesite, como un super-Archi."*
- **Today,** verified:
  - **What Archi reads:** its handbook, appended whole to its prompt (`build_archi_prompt`,
    src/bin/sy_architect.rs; 39 KB), and its cookbook seeds.
  - **The handbook's WF section (§4.2):** it has the rules up to `start_from`. It does not say
    what a guard can read and call, and it does not explain `forward`, `keep_message`, the WF's
    block or `text_field`.
  - **The two WF seeds:** they hold the same example, a request to `IO.quickbooks` with no agent.
  - **For people:** the reference is [wf-rules-quickstart](wf-rules-quickstart.md): CEL guards in
    §3 and every action in §4.
- **In this case:**
  - the handbook's WF section becomes the one reference for writing a WF. It covers what a guard
    reads and calls, the actions, the menu, the catch, `none` and the cap;
  - Archi gets a seed with an agent and a menu;
  - the quickstart's §3 and §4 point to the handbook instead of repeating it.
- **Proposed: the handbook for all of Fluxbee as its own topic, after this track.** The handbook
  goes whole into Archi's prompt, so gathering all of Fluxbee there means Archi reads it by
  sections.

#### 5.3.6 The build and the gate

- **wfcel:**
  - the menu: its shape, names and limits, with `none` reserved;
  - the catch;
  - guards checked against the menus' option names;
  - `response_envelope` refused in a WF's action.
- **The engine:**
  - the menu goes into the WF's block;
  - the catch, tried after the state's transitions;
  - the default also logged to the node's log;
  - the cap.
- **ai.generic and its SDK:**
  - the menu and the answer's contract are rendered in the WF's block of the message, so the
    system prompt stays the role;
  - `none` is added;
  - the answer is checked against the menu;
  - `reply` becomes the payload's text, and the object goes in `meta.context.evaluation`, as
    `text_field` does.
- **io.slack:** the thread records, in and out.
- **Archi:** the handbook's WF section and a seed.
- **The gate:**
  1. Case 1 again, on the menu, with roles that keep only their trade.
  2. The consultation, with the supervisors' side on a real io.slack in a test channel. To check
     first: whether two io.slack nodes can share one Slack app over Socket Mode.

  The gate's deployment stays up until the track ends, then one last run from zero (the operator,
  2026-10-30).

#### 5.3.7 Open, for Case 4

- How the team member hands the conversation back: a word, a reaction, or an inactivity timer.
- What the agent knows, when it takes the conversation back, of what was said meanwhile.
- What the first message to the team's channel carries, so that a person can take over.

**Withdrawn:**
- the routing rules in the agents' roles (Case 1's gate);
- the options as terminal tools. The tool list would change with each message, though it is fixed
  as a stable prefix for the provider's cache, and the runner has no terminal tools;
- a catch that every definition must have, first proposed on 2026-10-30. The operator chose
  .NET's model: an optional catch, with the log as the default.

## 6. Docs that drift from the code

**Fixed in 0.1.90,** every item below and more that the review found. A test now validates every
definition in the docs (go/pkg/wfcel/shipped_definitions_test.go). The list as it was:
- **docs/spec-systemnode-wf-v1_1.md:**
  - `event` is a flat map, not a Message;
  - `now()` is the node's clock;
  - a failing action halts by default;
  - a missing `$ref` fails;
  - a `TIMER_FIRED` without an instance is dropped;
  - `missed_policy` is not applied at recovery;
  - instances are not frozen per definition, and there is no replay;
  - the §21 limits are not enforced;
  - the admin actions in §15–§16 do not exist;
  - every timer guard path is wrong.
- **docs/wf-rules-quickstart.md:**
  - the guard paths;
  - frozen instances;
  - a cancel route that does not exist;
  - publishing `wf.engine --set-current` now moves every WF node.
- **docs/sy-wf-rules-spec.md:**
  - "frozen by hash";
  - `status_filter`;
  - the OPA section;
  - the `/admin/wf-rules` paths;
  - errors never returned.
- **go/nodes/wf/README.md and wf-generic/README.md,** and Archi's handbook §6.2.
- **SY.admin's help example** for `wf_rules` uses `"states": []`, which validation refuses.

## 7. Related

- [agent-tools-v1](agent-tools-v1.md) §6.2–§6.3: how this direction was decided.
- FINDINGS A-113 (OPA, decided), A-114 (writing first to a person), A-115 (SY.timer), A-116 (the
  person and the conversation), A-117 to A-119 (§3, resolved in 0.1.90), A-123 (an instance's
  thread and its senders).
- [wf-standard-path-v1](wf-standard-path-v1.md) (A-68, the deploy path).
- docs/audits/2026-07-23-sy-timer-wf-audit.md: G1, G2, G4, G5 and G9 are still open.
- docs/audits/2026-07-24-wf-dynamic-target-opa-design.md: the allowlist was never built.
