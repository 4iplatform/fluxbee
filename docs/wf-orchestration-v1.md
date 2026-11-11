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
  catch, the cap on agent calls and the way back through io.slack. It was released in 0.1.92 and
  validated on 8.x on 2026-10-30, with the agents' memory of 0.1.93. The operator: *"no tomar otro
  tema hasta terminar este"*.
- **Case 4 (§5.4)** hands the conversation to a person of the team. It was decided with the
  operator from 2026-10-31 to 2026-11-02, released in 0.1.94, and validated on 8.x the same day
  (§5.4.12; ledger 0.1.94).
- **Case 5 (§5.5)**, Archi writes and operates WFs. It was decided with the operator on 2026-11-02
  and 2026-11-03, and released in 0.1.95. Its gate surfaced four bugs, fixed in 0.1.96 to 0.1.98.
  It was validated on 8.x on 2026-11-03, in two runs, the second from zero (§5.5.3; ledger 0.1.95
  to 0.1.98). With it went the ILK fix (A-139) and the bugs of Case 4's test. **The track is
  complete.**
- **After the track (the operator, 2026-11-03):** the atoms (handbook §4.2.6) become a document of
  their own, a reservoir Archi reads to develop solutions. They move with the solutions chapter,
  where a solution lives; until then they stay in the handbook.
- **A solution runs on one hive** (the operator, 2026-11-06, docs/cognition-hands-v1.md §12.3):
  a WF and the agents and WFs it names run on the same hive, *"formal y forzado para archi, que
  sea sí o sí"*. SY.wf-rules refuses any other definition at the check and the deploy
  (`checkSolutionOnHive`, docs/sy-wf-rules-spec.md §6), and the handbook says it as a rule
  (§4.2.7). IO nodes are the doors and may run on any hive.

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
  be started by a schedule. Nothing past 366 days either, SY.timer's ceiling: a definition that
  asks more is refused since FINDINGS A-254 (it loaded, and its timer never fired).
- **Every timer request is fire-and-forget.**
  - SY.timer's refusals go unseen, and the WF's index drifts. Since FINDINGS A-254 the WF still
    sends without waiting, but keeps each schedule and reschedule until SY.timer answers: a refused
    one (the router's `UNREACHABLE`, SY.timer's own refusal, no answer in 30 s) marks its timer in
    the index, which goes again after 5, 10, 20 and 40 s, then each minute, until SY.timer takes
    it; the node is `DEGRADED` meanwhile.
  - Re-scheduling a pending key keeps SY.timer's old time.
  - A reschedule after a fire fails silently.
- **Recovery runs only at start.** Since 0.1.99 SY.timer sends a fire again while the WF is not
  connected, for up to an hour ([sy-timer](sy-timer.md) §7.8, A-115); a fire lost past that waits
  for the WF's next restart.
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
- **Case 4, handing the conversation to a person of the team (§5.4, decided 2026-11-02).** The WF
  bridges the person's IO node and the team's until the conversation comes back. With it come the
  subject marks, the menu for people, "not taken", and the catalog of known options.
- **Case 5, Archi writes and operates WFs (§5.5, done 2026-11-03):**
  - a check before deploy, with `COMPILE_ERROR` back to the designer. Today's `check` is a
    compile, and it stages the definition (FINDINGS A-128);
  - instances visible through SY.admin;
  - the pipeline's problems of §4.7.

  Archi's guide to writing WFs moved into Case 3; in Case 5 it becomes atomized cases.
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
7. **The WF's block, `meta.context.wf`,** goes on every message a WF sends. **Since 0.1.101** only
   when the action has `vars`, a `menu` or a `subject`: without the trail, an empty block said
   nothing (found in 0.1.100's gate).
   - `trail` lists the nodes the message went through. A `forward` takes the message's own trail
     (or its sender's name, when it has none) and adds the WF's name. A `send_message` starts it
     with the WF's name. **Replaced in 0.1.100** by the message's trace, `meta.context.trace`, which
     every node writes ([message-trace-v1](message-trace-v1.md), FINDINGS A-138).
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
   - With tools, the contract is an instruction in the system prompt; until FINDINGS A-150 the
     provider enforced it only on the call without tools. The instruction asks for a value of its
     type in every field, never null.
   - **Since FINDINGS A-150** the provider enforces it inside the tool loop too: every turn of the
     loop asks for the contract's schema next to the tools, as the call without tools does
     (`FunctionCallingConfig::output_schema`, crates/fluxbee_ai_sdk/src/function_calling.rs).
     - Anthropic holds the model's text to the schema while it can still call tools
       (`output_config.format`; structured outputs constrain the text, never a tool call or the
       thinking). A model without structured outputs refuses it with a 400: that turn is asked
       again without it, as before.
     - OpenAI takes it as `text.format` next to `tools`, and holds the text to it only when the
       contract is `strict`, which OpenAI accepts only with every property required. A 400 that
       names `text.format` (a strict contract with an optional property, or a model without
       `json_schema`) has that turn asked again without it.
     - The instruction stays, for OpenAI without `strict` and for a model without structured
       outputs. With tools, the object is checked afterwards only when the contract names a
       `text_field`; without one, the reply is the model's text as it is, which a refusal may
       leave outside the schema (a cut answer is an error of its own).
     - It reaches a contract that a caller other than a WF writes, or that a WF's `forward` keeps
       from the message it forwards: since Case 3 a WF asks an agent with a menu (§5.3.1), which
       has no schema, and wfcel refuses a `response_envelope` in a WF's action.

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
- the contract enforced by the provider inside the tool loop too (FINDINGS A-150, done later:
  §5.2.1, the agents' item 3).

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
  enforced by the provider inside the tool loop (A-150, done later: §5.2.1).

### 5.3 Case 3: the internal consultation. Decided with the operator, 2026-10-30

**Validated on 8.x, 2026-10-30.** Built in 0.1.92. Its gate passed on its second run, with the
agents' recent turns of 0.1.93 (cognition, FINDINGS A-134): Case 1 reached the invoice, and sales
knew a supervisor's decision on the person's next question.

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
  - an agent's error: an answer off the menu, a timeout, a provider failure. A provider failure's
    error carries `reply`, the operator's text for the person (FINDINGS A-184, 2026-11-08): the
    catch sends it with `{"$ref": "event.payload.reply", "default": ...}`, its own text for any
    other error;
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
  - **The log stays local** (the operator, 2026-10-30): *"el tema de log por ahora déjalo
    local… este tema de log es transversal a la plataforma"*. A stream for any node that logs,
    with SY.storage as its sink, comes in a later stage ([router-stream-v1](router-stream-v1.md)
    §8).
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
- **Decided after the review (the operator, 2026-10-30): each message a WF sends to a channel opens
  a new post there.**
  - **What the review found:** a thread id is the conversation's, so it outlives one question. A
    second consultation of the same customer, days later, went as a reply into the first
    question's Slack thread, out of the channel's view. And a late reply there, meant for the
    first question, decided the second.
  - **The rule:** a place io.slack opened with its own post serves one message. The next message on
    that thread id opens a new post, and io.slack forgets the older post it opened, so a late
    reply to an old question is no longer taken. A place a person opened keeps today's behavior.
  - **For Case 4:** when a WF must keep writing in one thread (handing a conversation to a person
    of the team), the WF will ask for it.
  - **The operator's note:** cognition already has two axes for this, the thread (physical) and the
    context (contextual, [12-cognition-v2](history/12-cognition-v2.md)). For now, the rule above.

#### 5.3.5 Decided: Archi learns to write this, from one handbook

- *"A Archi vamos a tener que ponerle info para que sepa programar esto… de hecho Fluxbee necesita
  un handbook único, sería el de Archi, y que lo use el que quiera, sea humano o quien sea que
  necesite, como un super-Archi."* And, on its shape: *"yo iría por capítulos… un doc completo
  es imposible, sí algo conceptualmente conectado"*.
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
  - the handbook's WF section becomes one self-contained chapter, the one reference for writing a
    WF. It covers what a guard reads and calls, the actions, the menu, the catch, `none` and the
    cap;
  - Archi gets a seed with an agent and a menu;
  - the quickstart's §3 and §4 point to the handbook instead of repeating it.
- **The handbook for all of Fluxbee is its own topic, after this track:** chapters, conceptually
  connected. The handbook goes whole into Archi's prompt today, so Archi will read it a chapter at
  a time.

#### 5.3.6 The build design and the gate

Read in the code on 2026-10-30, at `32353f18`. Four pieces; the contracts between them are exact.

**1. The definition language (go/pkg/wfcel):**
- **`menu`** is a field of `send_message` and `forward` (`actionFields`), allowed only when the
  action's `target` is an `AI.*` node:

  ```json
  "menu": {
    "help": "optional, 1 to 600 characters",
    "options": [
      {"name": "invoice",
       "description": "1 to 200 characters",
       "params": {"quantity": {"type": "integer", "description": "optional, up to 200 characters"},
                  "plan": {"type": "string", "enum": ["basic", "pro"]}}}
    ]
  }
  ```

  - 1 to 8 options. A name matches `^[a-z][a-z0-9_]{0,31}$`, is unique in its menu, and is never
    `none`.
  - `params` is optional, with up to 8 per option. A param's name follows the same pattern, and
    its `type` is `string`, `boolean`, `integer` or `number`. `enum` is for strings only, with 1 to
    20 values.
  - Characters are counted as Unicode code points. The menu is literal: no `$ref` inside it.
- **`meta.context.response_envelope` is refused in a WF's action:** "a WF asks an agent with
  `menu`".
- **`catch`** is a top-level list of transitions (`WorkflowDefinition.Catch`).
  - It has a state transition's shape, except that its `event_match` may be empty, which matches
    every event. `guard` is required, and `target_state` is optional: without it, the transition
    is internal and stays in the current state.
  - Its actions are `send_message`, `forward`, `keep_message`, `set_variable` and
    `emit_internal_event`. Timer actions belong to states: the catch goes to a state whose entry
    actions set them.
  - A `keep_message`, or a `forward` of the event (no `message`), needs an `event_match` that names
    a `src`, so the event is a node's message and never a timer's.
  - The catch's actions and targets count as the definition's own: `eachAction` and `Involves`
    include them.
- **Checked against the menus:**
  - a guard that compares `event.context.evaluation.option` with a string literal (`==`, `!=`,
    `in`) must name an option of one of the definition's menus, or `none`;
  - a guard or a `$ref` that reads `event.context.evaluation.params.<p>` must name a param of one
    of them, or `reason`.

  An error names the path, as any other does.

**2. The engine (go/nodes/wf/wf-generic/node):**
- **The menu:** `withWFBlock` writes the action's menu into `meta.context.wf.menu`, next to
  `trail` and `vars`: as written, but with each option's params in name order and no empty field.
- **The catch:** when no transition of the current state takes an event, the engine tries the
  catch's transitions in order. A chosen one runs like a state's: exit and entry actions with a
  `target_state`, or internal without one.
- **The default:** when neither takes the event, the instance's log gets today's `unhandled
  event` entry, and the node's local log gets a line naming the instance, the state and the event.
  Both are local (§5.3.2).
- **The cap:** `maxAgentCallsPerTrigger = 8`.
  - **A trigger** is an inbound event whose sender is not an `AI.*` node (a message from an IO node
    or a WF), or a `TIMER_FIRED`. An agent's answer or an internal event starts no new count.
  - **What counts:** each `send_message` or `forward` whose target is `AI.*` counts one.
  - **Past the cap:** the send is not made. The instance's log says so, and the transition's other
    actions run.
  - **The error event:** after the transition, the instance gets one internal event per trigger,
    `msg` `AGENT_CALL_CAP`, with the payload `{"type": "error", "code": "agent_call_cap",
    "message": "...", "target": "<the agent>"}`. It goes through the state's transitions, then the
    catch, then the default. The instance does not fail.
  - **The count** lives in memory, per instance. A restart starts it again.
- **The example** (go/nodes/wf/examples/packages/wf.sales.invoice):
  - it moves to the menu, with sales' options `answer`, `invoice` and `consult`;
  - `consult` goes to `IO.slack.supervisores@motherbee` and waits with a timer;
  - the definition gets a catch.

  Every definition in the docs and the seeds stays valid: `shipped_definitions_test.go` reads
  them.

**3. ai.generic and its SDK (nodes/ai/ai-generic, crates/fluxbee_ai_sdk):**
- **The menu** is read from `meta.context.wf.menu`.
  - ai.generic checks it again, with wfcel's limits. An invalid menu gets the error answer
    `invalid_menu`, with no model call.
  - A message with both a menu and a `response_envelope` gets `conflicting_contracts`, also with
    no model call.
- **Rendered in the WF's block** (`render_wf_prompt_block`), after the path and the variables:

  ```text
  Help: The person is buying licenses. ...
  Options (choose exactly one):
  - answer: You answer the person yourself.
  - invoice: They accepted the quote and ask for the invoice.
    params: product (string), quantity (integer), unit_price (number)
  - consult: A supervisor must approve something before you answer.
    params: question (string): What you ask, with the data to decide.
  - none: None of the options fits.
    params: reason (string): Why none of the options fits.
  Answer with only one JSON object: {"reply": "<text for the person, may be empty>", "option": "<one name above>", "params": {<that option's params>}}
  ```

  - The system prompt stays the role: no fallback instruction is appended for a menu.
  - The block stays out of the history, as the WF's block does.
  - The agent may still use its tools; its final text is the object.
- **The answer** is parsed as `parse_structured_output` parses: a fenced object, or an object
  followed by text. Then it is checked:
  - `option` is one of the menu's, or `none`;
  - each param of that option is present with its type. An `integer` is a JSON integer, a
    `number` any number, and an `enum` one of its values. `none` needs `reason`, a string;
  - `reply` is a string, and may be empty;
  - a param the option does not declare is dropped, with a log line.

  Anything else is `invalid_structured_output`, with a detail: `not_json`, `unknown_option:<x>`,
  `missing_param:<p>` or `type_mismatch:<p>:<type>`. The model's first 200 characters are logged,
  as for any contract.
- **The answer message:**
  - its payload's text is `reply`;
  - `meta.context.evaluation` is the checked object, `{"reply", "option", "params"}`;
  - the history keeps the whole object, as with a `text_field`.
- **The SDK** gets a menu contract next to `ResponseContract`, with the same `answer` shape
  (`ContractAnswer`). The request log says which contract a message had.

**4. io.slack (nodes/io/io-slack, nodes/io/common): the thread records.**
- **Storage:** a SQLite file in the node's own directory, next to its `config.json`. One row per
  Slack place: `thread_id`, `channel_id`, `thread_ts` (empty for a channel's top level),
  `created_at` and `last_used_at`. A Slack place is one channel and one thread, and has one
  thread id.
- **In:**
  - a reply in a Slack thread whose root `ts` is recorded takes the recorded thread id, in
    `meta.thread_id` and in the IO context, instead of the native thread's id;
  - any other message takes today's id, and its place is recorded under it.
- **Out:**
  - a message on a recorded thread id goes to that thread's place, and `reply_target` is not read;
  - an unrecorded thread id with a `reply_target` of the node's own channel is posted as today,
    and recorded;
  - otherwise the message is posted at the top level of the channel, and the post's `ts` is
    recorded for that thread id.
- **Expiry:** a record unused for 30 days is deleted, at start and daily.
- **Failure:** if the records fail, the node logs it and works as today.

**5. Archi (Case 3's part of §5.3.5, after pieces 1 to 3):**
- the handbook's WF section is rewritten as one self-contained chapter, kept compact because it
  rides in Archi's prompt;
- the two WF seeds hold the menu example;
- the quickstart's §3 and §4 point to the chapter.

**Checks before the review round:** Rust fmt, clippy (`-D warnings`) and the workspace's tests; Go
gofmt, vet and race tests in each module; the doc links. Then one review round, as in Case 1.

**As built (2026-10-30),** where it differs from the above or adds to it:
- **The quickstart's §5** (the structure, the catch and the default) points to the chapter too, with
  §3 and §4: the catch had been written there while building, and one reference lives in one
  place. The quickstart covers deploying and operating.
- **The seeds' example** is `sales.consult`: an agent answers through a menu or consults the
  supervisors, and a catch takes `none` and errors. The seeds are at `seed_version` 4.
- **An agent's `reply` may be empty,** and io.slack posts a fallback for a message with no text
  (FINDINGS A-130). The chapter and the seeds guard that forward with
  `event.context.evaluation.reply != ''`.
- **The agent checks the menu with exactly wfcel's limits,** blank texts and empty or repeated
  `enum` values included.
- **A menu's answer is the first whole JSON object,** fenced (on the opening fence's line too) or
  among other text (FINDINGS A-131, resolved).
- **A blank `thread_ts` panicked io-common's thread id** (FINDINGS A-130): it is now the channel's
  top level.
- **Not checked at load:** a `meta.context` written whole by a `$ref` may carry a
  `response_envelope`. An agent that gets one with a menu answers `conflicting_contracts`.

**The gate:**
  1. Case 1 again, on the menu, with roles that keep only their trade.
  2. The consultation, with the supervisors' side on a real io.slack in a test channel. To check
     first: whether two io.slack nodes can share one Slack app over Socket Mode.

  The gate's deployment stays up until the track ends, then one last run from zero (the operator,
  2026-10-30).

#### 5.3.7 Open, for Case 4 (answered in §5.4)

- How the team member hands the conversation back: a button, or an inactivity timer (§5.4.4,
  §5.4.5).
- What the agent knows, when it takes the conversation back, of what was said meanwhile: the WF's
  facts and the team member's note (§5.4.4). The rest is FINDINGS A-137, a topic of its own.
- What the first message to the team's channel carries, so that a person can take over: the
  agent's summary and the facts (§5.4.3).
- **What the gate showed (2026-10-30):**
  - **A supervisor who answers in the channel,** not in the thread of the question, is not heard:
    the reply has the channel's thread id, and the WF drops it. Natural for a person to do: the
    operator's first answer went to the channel in both runs of the gate. Now it gets "not taken"
    (§5.4.6); placing it is io.slack's, in the IO track (A-136).
  - **A supervisor who asks the agent something** ("between 20% and 15%, which would you give?")
    gets no answer: the agent may only decide or wait. Now it has `answer_team` (§5.4.7).
- **Which posts io.slack records.** It records the place of each message that reaches it, and of
  each post it makes on a thread it does not know. Its answer at a channel's top level is not
  recorded, so a person who replies in that answer's Slack thread starts a new thread id, and a
  new instance. Recording every post's `ts` would keep that reply on its conversation. Moved to the
  IO track (A-136).

**Withdrawn:**
- the routing rules in the agents' roles (Case 1's gate);
- the options as terminal tools. The tool list would change with each message, though it is fixed
  as a stable prefix for the provider's cache, and the runner has no terminal tools;
- a catch that every definition must have, first proposed on 2026-10-30. The operator chose
  .NET's model: an optional catch, with the log as the default.

#### 5.3.8 The review round (2026-10-30)

Six reviewers, each blind to the others' lens: the engine, the language, the agents, io.slack,
security and the docs. Fixed before the release:
- **A second consultation of a customer went into the first one's Slack thread** (io.slack,
  security, docs), out of the channel's view, and a late reply there decided it. The operator
  decided that a post io.slack opens serves one message (§5.3.4).
- **A person's text reached the supervisors' channel as Slack markup** (security): `<!channel>`,
  mentions, and links with a label of the person's choosing. io.slack escapes the text it posts,
  and decodes what Slack escapes on the way in.
- **The first message in the question's thread was the decision** (security), so an internal
  remark could reach the customer, and the decision after it was dropped. A supervisor's message
  goes to the agent with a menu that lets it say the message decides nothing yet (`wait`).
- **The seeds' example dropped answers it asked for** (docs): a person who writes twice, or a
  decision that crosses an answer. Its catch acts on the agent's answers wherever they arrive, and
  a test walks the example on the engine (`seed_test.go`).
- **A file or a link never reached the person under the recommended guard** (agents, docs): the
  evaluation's `reply` is now the text the person gets.
- **The agent's memory in a consultation** (agents): ai.generic's immediate memory is the
  sender's, so a supervisor's turn was kept under the supervisor. The operator: it stays off, and
  cognition is the agents' memory (FINDINGS A-133).
- **The language:**
  - a state's transition on `AGENT_CALL_CAP`, or on an internal event, that forwards or keeps the
    event loaded, then failed the instance;
  - a misspelled evaluation field, a param looked up with `in`, and a field name written with other
    capitals loaded;
  - `option != ''` was refused.
- **The engine:** a cancelled or failed instance's send to an agent could be dropped by a spent cap;
  a WF's message to itself restarted the count; the node's log kept the text of messages it
  dropped.
- **Smaller:** a brace before the object, an object on the opening fence's line, a failed touch that
  lost a thread record; the docs: an IO node's `io.dst_node`, the supervisors' io.slack, params as
  the person's words, the param name rule, one reference per topic.
- **Recorded for later:** io.slack drops a reply with a file and does not record a post with only
  files (A-130, the IO family's track); the engine's unused `cancelling` status (A-132).

### 5.4 Case 4: handing the conversation to a person of the team. Decided with the operator, 2026-10-31 to 2026-11-02

**Released in 0.1.94 and validated on 8.x on 2026-11-02 (§5.4.12).** The decisions come from the
design chat; the build design is §5.4.10, the review round §5.4.11, and the gate §5.4.12.

**The circuit** (the chat's example):
1. **10:00** Ana asks sales for a credit note, which only a person of the team can issue. Sales
   answers with the option `handover`. Its `summary` says what the team needs to know, and its
   `reply` tells Ana that a person will take over.
2. The WF forwards the reply to Ana. It posts the summary in the team's channel as a new post, with
   the case's facts and a "Handback" button.
3. **10:02** Juan answers in that post's thread, and his message goes to Ana. From then on, Ana's
   messages go to that thread, and Juan's go to Ana.
4. **10:16** Juan clicks "Handback" and writes a note in the field Slack opens. The post loses its
   button and is marked closed. The note goes to sales, with the WF's facts.
5. **10:17** Ana asks "¿entonces ya está la nota de crédito?", and sales answers from the note.

#### 5.4.1 Decided: the WF regulates, and never owns the conversation

- **The operator's frame:** the WF decides only what its code fixes deterministically. It never
  writes an LLM's content, and it never owns a conversation or a thread.
- **So a handover starts** with the agent's menu (`handover`), or with one of the WF's fixed rules:
  the catch, the cap, a timer.
- **The facts travel in the message** (the operator, 2026-10-31): the trail, the nodes it went
  through, and the state. With them, each node decides within its own responsibility, by the
  principle that the datum's owner decides.
  - *"mismo caso en donde el agente sabe la decisión y el wf toma la acción con esos datos"*: the
    WF knows the subject, and io.slack knows the places.
  - The general trace in the message is a topic of its own (FINDINGS A-138). This case uses the
    WF's facts and marks.

#### 5.4.2 Decided: the WF marks the subject, and io.slack places the message

- **The WF marks each message to an IO node** with `subject`:
  - `new`: the message opens a subject;
  - `same`: it continues the subject of the WF's last message on this conversation to this node;
  - `end`: it ends that subject.
- **io.slack places it,** since it owns the places (§5.3.4):
  - `new`, or no mark: as today. A post the node opens serves one message;
  - `same`: in the thread of the node's last post on the conversation, so a case keeps one Slack
    thread;
  - `end`: where `same` would put it, when it has text. Then io.slack closes the subject's open
    menu (§5.4.4) and marks its post closed;
  - a place a person opened takes every message, whatever the mark.
- **An agent gets no mark,** since it has no places: the language takes `subject` only on a send or a
  forward to an `IO.*` node.

#### 5.4.3 Decided: the first post to the team

- **Sales' summary, as written:** the `summary` param of its `handover`. The WF writes no text.
- **The facts, in the action's `vars`:** who the person is and where they write, from the person's
  IO context, which the WF keeps (`state.io`). io.slack renders a message's `vars` under its text,
  as one line of facts.
- **The last turns stay out for now:**
  - Juan talks with Ana directly, and asks her when something is missing;
  - cognition's package reaches only `AI.*` nodes;
  - posting a customer's conversation in a team's channel is an exposure decision of its own.

  If the gate shows that the summary falls short, they come in.
- **The WF's own messages carry the person** (the rest of A-116; since the next release after
  0.1.100, they also name the WF as their writer, docs/cognition-hands-v1.md): a `send_message` carries the
  `src_ilk` and `ich` of the message that started the instance. Today it carries none, and the agent
  logs `missing src_ilk`.

#### 5.4.4 Decided: a menu for people, and the hand-back

- **A menu is the same for an agent and a person.** The WF sends it in its block. An agent answers
  it with an object; io.slack shows it as buttons.
- **The hand-back is the known option `handback`** (§5.4.8), with the param `note`. io.slack shows a
  "Handback" button on the post, and the click opens a Slack field for the note, which is required.
- **The click comes back to the WF as a fact:** a message on the conversation's thread, from
  whoever clicked (their ILK), with `meta.context.evaluation` =
  `{"option": "handback", "params": {"note": "..."}}`.
  - That is the shape of an agent's answer, so the same guards read both.
  - Its text is the option's name, then each param as `name: value`, one per line.
- **The note reaches sales as Juan's message:** the WF forwards the click, so sales' memory has it
  under Juan.
- **A hand-back by a timer brings no note:** the WF sends sales a fixed text written in its
  definition.
- **The operator:** the buttons are io.slack's presentation. They make the gate easier, but they are
  not its key.

#### 5.4.5 Decided: two timers, the WF's fixed rules

- **"Nobody took it"** counts from the post, and only a message from the team cancels it. When it
  fires, the WF closes the post (`end`) and tells sales, which tells the person.
- **"Quiet"** counts once the case is taken. Each message, the person's or the team's, starts it
  again. When it fires, the WF closes the post and hands the conversation back to sales.
- **Each N is the definition's.** The operator: *"las reglas son cosas que se tiene que programar en
  el wf"*.
- **The three ends** (the button, nobody took it, quiet) send `end`, and io.slack closes the post. A
  later click, or a message in the closed thread, gets "not taken" (§5.4.6).

#### 5.4.6 Decided: nothing from an IO ends in silence

- **A message from an IO node the definition involves:**
  - on a thread with a running instance, when no transition and no catch takes it: the instance's
    log gets its entry, as today, and the IO node gets "not taken";
  - on a thread with no instance: when the definition starts from that node, it starts one.
    Otherwise, "not taken";
  - with no thread: "not taken", since a WF never makes up a thread (A-135).
- **"Not taken" is the engine's answer `NOT_TAKEN`,** a system message to the sender, with a reason:
  `no_conversation`, `not_expected`, `invalid_input` or `no_thread`. It takes `INVALID_INPUT`'s
  place.
  - *Changed by A-217 (0.1.125):* a fifth reason, `unavailable`: the WF node cannot use its
    instance store, and takes no message until it can. Any other sender gets it for a named
    request, as `no_thread` and `invalid_input`.
- **The IO node shows it to whoever wrote, as an error.** io.slack posts a message that only that
  person sees, where they wrote.
- **Today's rules stay for everyone else:**
  - a sender the definition does not involve is dropped, unanswered (A-123);
  - an agent's late answer starts nothing (A-118);
  - a WF gets an answer only to a named request (A-119).
- **Why:** someone who writes is looking for a result. The cause on the Slack side is io.slack's
  thread model (A-136, the IO track), and the WF is not patched for it.

#### 5.4.7 Decided: a supervisor who asks the agent something

- **Case 3's gate:** a supervisor asked "¿entre 20% y 15%, cuál darías vos?", and sales could only
  answer `decided` or `wait`.
- **A third option in the consultation's menu, `answer_team`:** sales' reply is for the team. The WF
  forwards it to the team's node with `subject: same`, so it lands in the consultation's thread, and
  the consultation goes on.
- **No loop:** each round needs a person to write, and the cap bounds the agent calls. The engine
  does not change for it; the menu is each definition's.

#### 5.4.8 Decided: the catalog of known options

- **The operator's idea:** *"un DEF para todo lo conocido y algo que sea a gusto del usuario"*, a
  list that grows and makes writing Fluxbee's solutions easier for Archi.
- **Today:**
  - ai.generic knows no option, only the menu's contract;
  - `none` is the only option in code (`wfcel.NoneOption`);
  - every other option, and what it does, lives in each definition: the menu in the action, the
    effect in the guards. The Case 1 example already describes `answer` in two ways.
- **The catalog is declared once, in the definition language** (go/pkg/wfcel), as the agent tools
  are ([agent-tools-v1](agent-tools-v1.md)):
  - each known option has its name, its description and its params;
  - a definition names a known option by its name alone, and the compiler writes its description
    and params in;
  - a known name is never redefined, as `none` is not today. A definition's own options (`invoice`)
    are written whole, as today;
  - what an option does stays the definition's: the catalog fixes the words, not the flow;
  - Archi's handbook lists the catalog, and a test keeps the list equal to the declaration;
  - the WF sends the whole menu, so the agent and the IO node do not change for it.
- **The first known options:** `answer`, `consult`, `decided` and `wait` (Case 3), then
  `answer_team`, `handover` and `handback` (this case). The operator's "supervisor" is `handover`,
  named for what happens, because `consult` also goes to supervisors. The review added `done`, an
  agent's hand-back with no note (§5.4.11).
- **The operator, on how much to build in each case:** *"fluxbee hoy es versión alpha no expuesta a
  clientes... es mejor romper ahora que dejar para después y no hay backcompatibilidad"*.

#### 5.4.9 Opened by this case, each a topic of its own

- **A-136** (the IO track): io.slack splits one interlocutor's conversation by where they write in
  Slack.
- **A-137:** cognition follows only the conversations an agent is in. The question: which
  conversations it analyzes, and how much, when every interlocutor is an agent. The operator's idea
  is a mark in the message.
- **A-138:** the conversation's trace in the message. The operator: *"ya veo la herramienta completa
  en el mensaje para ver con el trail cómo funcionó o dónde falló"*.

#### 5.4.10 The build design and the gate

Read in the code on 2026-11-02, at `3d9325b4`. Five pieces; the contracts between them are exact.

**1. The definition language (go/pkg/wfcel):**
- **`subject`** is a field of `send_message` and `forward` (`actionFields`): `new`, `same` or `end`,
  literal, and only with an `IO.*` target. The engine writes it into the WF's block as
  `meta.context.wf.subject`.
- **`menu`** goes on a send or a forward to an `AI.*` or an `IO.*` node. A `WF.*` target takes none.
- **The catalog,** `KnownOptions`, in this order:

  | Name | Description | Params |
  |---|---|---|
  | `answer` | Your reply goes to the person. | |
  | `consult` | A supervisor must approve something before you answer. Your reply tells the person you will check. | `question` (string): What you ask the supervisor, with the data to decide. |
  | `decided` | The message decides the question: your reply tells the person the decision. | |
  | `wait` | The message decides nothing yet, such as a remark among supervisors: your reply is empty. | |
  | `answer_team` | The message asks you something: your reply goes to the team, not to the person. | |
  | `handover` | A person of the team must take the conversation. Your reply tells the person; the summary goes to the team. | `summary` (string): What the team needs to take over: who the person is, what they want, what was done. |
  | `handback` | Hand the conversation back to the agent. | `note` (string): What the agent must know: what was done and agreed. |
  | `done` | Your part is done: your reply goes to the person, and the conversation goes back to the agent that had it. | |

  - **At load,** an option written as `{"name": "<known>"}` alone takes the catalog's description
    and params, before every other check.
  - A known name with any other field is refused: "<name> is a known option: write it by its name
    alone". An unknown name keeps today's rules.
  - The checks of guards and `$ref`s against the menus see the options as expanded.
  - **The handbook's table** (docs/archi/handbook_fluxbee.md §4.2.4, between
    `<!-- known-options:start -->` and `<!-- known-options:end -->`) is rendered from
    `KnownOptions`, and a test compares the two.

**2. The engine (go/nodes/wf/wf-generic/node):**
- **`NOT_TAKEN`** takes `INVALID_INPUT`'s place. It is a `system` message to the sender's uuid, with
  the message's `trace_id` and `thread_id`, and the payload
  `{"ok": false, "error": "not_taken", "reason": "<reason>", "detail": "<text>"}`.
  - **To an `IO.*` sender the definition involves,** for any message that is no reply and no
    notice, named or not:
    - `no_thread`: a message with no thread (A-135);
    - `no_conversation`: no running instance holds its thread, and it starts none;
    - `invalid_input`: it would start one, and `input_schema` refuses it;
    - `not_expected`: the instance took nothing, neither a transition nor the catch. The
      instance's log keeps today's `unhandled event` entry.
  - **To any other sender,** only a named request gets it (`neverAnswered`), for `no_thread` and
    `invalid_input`, as `INVALID_INPUT` today.
  - *Changed by A-217 (0.1.125):* and `unavailable`, to both, while the WF node cannot use its
    instance store (§5.4.6).
  - `isReply` takes `NOT_TAKEN` instead of `INVALID_INPUT`, so it never starts an instance and is
    never answered.
- **A-135:** a message with no thread starts nothing, and `creationThread` goes. A sender other than
  an IO node gets `no_thread` only for a named request, as above; otherwise a log line.
- **The rest of A-116, the instance's person:** the instance keeps the `src_ilk` and `ich` of the
  message that started it (new store columns `person_ilk` and `person_ich`). A `send_message`
  carries them as `meta.src_ilk` and `meta.ich`, when they are set.
- **A-132:** the `cancelling` status goes, from the store's CHECK (migrated), and from instance.go,
  dispatch.go, correlate.go and recover.go.
- **The WF's block** gets `subject` when the action has one.
- **The example** (go/nodes/wf/examples/packages/wf.sales.invoice) writes its known options by name,
  and adds, on the same supervisors' node (`IO.slack.supervisores@motherbee`):
  - `answer_team` in the consultation;
  - `handover` in sales' menus. The handover forwards sales' reply to the person (when it is not
    empty) and posts the summary with `subject: new`, the person's facts in `vars`, and a menu with
    `handback`. Then the state `with_team`:
    - a team message is forwarded to the person. The first one cancels "nobody took it"
      (`team_untaken`, 15 minutes) and starts "quiet" (`team_quiet`, 30 minutes);
    - a person's message is kept and forwarded to the team with `subject: same`. Once the case is
      taken, it starts "quiet" again;
    - a `handback` click: `end` to the team, and the click forwarded to sales with a menu of
      `answer`;
    - either timer: `end` to the team with a fixed text, and a fixed text to sales with a menu of
      `answer`;
    - on the way back, the facts in `vars`: how the case came back (`handback`, `untaken` or
      `quiet`) and since when the team had it;
    - an agent's late answer is not delivered while the team has the conversation, and a
      `payment_due` is noted, as in the other states.
- `shipped_definitions_test.go` and `seed_test.go` keep every definition in the docs and the seeds
  valid.

**3. ai.generic (nodes/ai/ai-generic): the rest of A-133.** `runtime.immediate_memory` goes from
ai.generic: its config, its runner and its docs. SY.architect and the frontdesk keep the AI SDK's.
- **Built** (`72bc2e62`): a config that still names it is refused with the reason, on CONFIG_SET
  (`invalid_config`) and at boot (`FAILED_CONFIG`). No shim, as the operator asked.
- **For the deploy:** every `config.json` a CONFIG_SET wrote before 0.1.94 carries the key, because
  the node wrote its default there. After the upgrade, each such agent is set again with a
  CONFIG_SET without it, one by one, and the release's notes list them.

**4. io.slack (nodes/io/io-slack, nodes/io/common):**
- **The marks** (`meta.context.wf.subject`), in the thread records' outbound resolution:
  - `same` on a thread id with node posts: the thread of the node's last post, with no new post and
    nothing forgotten. On an unrecorded thread id: as `new`;
  - `end`: as `same`, then the subject's open menu is closed;
  - a place a person opened: unchanged, whatever the mark.
- **The menu for people** (`meta.context.wf.menu` on an outbound message):
  - **The post** gets an actions block, one button per option, labeled with its name (`_` as a
    space, the first letter capitalized). The menu's `help`, when it has one, is a context line
    above the buttons.
  - **The records keep each open menu:** its channel, its message's `ts`, its thread id, the menu,
    and the post's blocks. A thread id has one open menu per channel, and a new one closes the
    older.
  - **Closing a menu** is a `chat.update` of its post: the same blocks without the actions, and a
    context line "Closed.".
  - **A click** comes as a Socket Mode `interactive` envelope (`block_actions`), acked at once.
    - An option without params sends the click message at once.
    - An option with params opens a modal (`views.open`, with the click's `trigger_id`), with one
      required input per param: a string is multiline text, an integer or a number is a number
      input, a boolean is a yes/no select, and an enum is a select of its values. Slack checks the
      inputs. The submission (`view_submission`) is acked at once, checked again against the
      types, and dropped with a log line when it does not match.
    - **The click message** goes on the menu's thread id, as the person who clicked (resolved or
      provisioned as for a message), with `meta.context.evaluation` = `{"option", "params"}` and
      the text of §5.4.4.
    - A click on a post with no open menu gets "This is no longer open.", which only the clicker
      sees, and nothing goes.
- **"Not taken":** io.slack remembers, for 15 minutes, each message and click it relays: its
  `trace_id`, its channel, its thread and its Slack user.
  - A `NOT_TAKEN` with that `trace_id` posts "Not taken: <reason>", which only that user sees,
    where they wrote.
  - The reasons' texts: `no_conversation`, "no conversation here takes this message"; `not_expected`,
    "the conversation does not expect a message from you now"; `invalid_input`, "the message is not
    valid here"; `no_thread`, "the message has no conversation".
    - *Added by A-226 (the operator's call, 2026-11-10):* `unavailable` (A-217), "the conversation
      cannot take messages right now; try again later".
  - Any other system message stays ignored, as today.
- **The facts:** a message's `meta.context.wf.vars`, when it is a non-empty object, is a context line
  under its text: `key: value` pairs, escaped, each value cut at 200 characters, 10 keys at most.
- **A message marked `end` with no text, no blocks and no files** closes the subject's open menu and
  posts nothing.
- **Only what the WF's test needs** (the operator, 2026-11-02): *"solo para terminar la prueba sobre
  los casos de WF no quería cambiar el IO.slack ahora"*.
  - This piece is built on today's io.slack, and io.slack's revamp in the IO track will redo it.
  - The WF's side does not depend on it. The WF gives facts (the thread, `subject`, the menu, `vars`,
    `NOT_TAKEN`), and each IO node decides what to do with them.
- **Out of this case, to io.slack's revamp:** A-130, A-108 and A-129, first planned here.
  - A-129's plan was a relay of events between io.slack nodes. The operator: it looks like a
    workaround, and it is not how the system must end up.
  - The test does not need it: each io.slack node of the gate has its own Slack app.
  - The operator's definition for the revamp is in FINDINGS A-129.

**5. Archi (after pieces 1 and 2):**
- the handbook's §4.2: the catalog, `subject`, the menu for people, "not taken", and the handover
  pattern;
- the seeds' `sales.consult`, on the catalog, with `answer_team`;
- the quickstart's pointers.

**Checks before the review round:** as in Case 3. Rust: fmt, clippy (`-D warnings`) and the
workspace's tests. Go: gofmt, vet and the race tests of each module. The doc links. Then one review
round.

**The deploy's own steps** (the review, §5.4.11):
- **The gate's WF goes down at the upgrade, until it is redeployed.** Its definition, Case 3's,
  writes known options whole, which 0.1.94 refuses. So WF.gate0192@worker1 comes up with no
  definition in use after the core update. It is redeployed from the new example right after.
  Instances born on the old definition fail at recovery; none should be running.
- **AI.pruebas@worker1 gets its config again,** without `runtime.immediate_memory` (piece 3). It is
  the only agent whose `config.json` names it, checked on 2026-11-02.
- **No rollback across 0.1.94 for these two** without redeploying them on the old version.

**The gate** (0.1.94 on 8.x), on Case 3's deployment, which stays up:
- **Before it, the operator** turns on Interactivity in the Slack app `fluxbeetest` (A0B291N47KL),
  under "Interactivity & Shortcuts". Socket Mode needs no Request URL and no new scope. Without
  it, the buttons show, but a click shows Slack's own error and never reaches the node.
- **The person** is the gate's client on the router socket. **The team** is the operator, in the
  test channel, through IO.slack.gate0192.
1. Case 1 and the consultation again, short, with a supervisor's question answered by
   `answer_team`, and two supervisor messages in a row.
2. The handover end to end: the post with its facts and its button, the operator's answer in the
   thread, the person's message in it, the hand-back with a note, and sales' next answer from the
   note.
3. "Nobody took it" and "quiet", each with a short N.
4. "Not taken": a message in a closed case's thread, and a message at the channel's top level. A
   click on a closed post gets io.slack's own "This is no longer open.". On the person's side, a
   message with no thread (A-135).

#### 5.4.11 The review round (2026-11-02)

Five reviewers, each blind to the others' lens: the engine, the language, io.slack, security, and
the example with the docs. No high finding in the code; one high in the example, from Case 3's
logic. Fixed before the release:
- **The example lost supervisors' messages and dropped the consultation** (the example):
  - a supervisor's second message, while sales evaluated the first, got "not taken";
  - the person's message during an evaluation took the instance out of the consultation, its timer
    cancelled, and the decision then found no conversation.

  The consultation now takes every supervisor's message, and ends only on a decision or on the
  answer to its own timeout.
- **A late answer promised what never happened** (the example): a late `consult`, `invoice` or
  `handover` reached the person with nothing behind it. Each option now acts wherever it arrives;
  while the team has the conversation, nothing is delivered.
- **Case 1's invoicing took the team's `handback`,** which needs a note, so a hand-back without one
  failed (the example). The catalog gained `done`: an agent's part is over, its reply goes to the
  person, and the conversation goes back.
- **A timer the WF cancelled or moved could still fire** (the engine, the example). The engine
  drops a fire the instance no longer has, or has moved, and schedules a moved one again. The
  example also guards "nobody took it" with `team_taken`.
- **A WF's own messages carried the first writer's identity on a thread several people share**
  (security). The instance's person is the one its `start_from` nodes' messages share; with two,
  it has none. The cause is io.slack's thread model (A-136).
- **Stale definitions failed only at a node's restart** (the language): SY.wf-rules' apply and
  rollback now load what they publish. The deploy's steps above cover the gate's WF.
- **io.slack:**
  - its read loop handled events inline, so a click could miss its modal's 3 seconds behind a
    file. The loop now only reads, acks and hands off, and a modal that does not open tells the
    clicker;
  - the WF's block and `NOT_TAKEN` were taken from any sender: now only from the WF the message
    went to;
  - a new post left the older post's buttons open; the WF's blocks could pass Slack's 50.
- **Smaller:**
  - `NOT_TAKEN` after the commit, so a crash sends it once;
  - a replayed creation that starts nothing is answered;
  - a number out of range is refused by its path;
  - `end` takes no menu;
  - the catalog only grows (a test pins it);
  - a `none` to a supervisor's message tells nobody;
  - the docs: only io.slack shows `NOT_TAKEN`, a click is matched by its IO node, the handover
    pattern in full in Archi's handbook.
- **Left as they are:**
  - in the seeds' `sales.consult`, a `none` or an error while sales evaluates a supervisor's
    message reaches the person: rare, and the seed teaches the consultation, not every crossing;
  - io.slack's Spanish fallback texts, and a config change that waits on a provisioning, for
    io.slack's revamp.

#### 5.4.12 The gate (0.1.94 on 8.x, 2026-11-02)

Run as §5.4.10 says, on fresh threads (`thread:gate0194-*`), with the operator as the team in the
test channel; Interactivity was turned on first. All of it passed. The times are from the person's
side.
- **The person's side, with no one in Slack:**
  - **A message with no thread** got `NOT_TAKEN` `no_thread` at once, and started nothing (A-135).
  - **A stranger on a conversation's thread** got nothing, not even `NOT_TAKEN`: the definition
    does not involve its sender.
  - **Case 1, short:** sales' quote in 2.4 s; invoice F-0192-0417 (1,000 USD, due in 2 minutes)
    4.1 s after "Dale, facturamelo"; invoice answered the thanks; payments' notice came 122.8 s
    after the invoice. The instance closed after 3 quiet minutes.
- **The consultation,** with a supervisor's question:
  1. Sales chose `consult` (20 Pro licenses at 20% off, 1,600 USD a month, for approval), and the
     person was told that sales would check.
  2. The operator asked sales in the post's thread "¿Y cuánto le quedaría con 15%?". Sales chose
     `answer_team`, and its answer was in the same thread 3.2 s later.
  3. "Aprobado 15%, no 20%." Sales chose `decided`, and the person got the approval 3.2 s later.
  4. The person asked for the total, and sales answered from its memory: 1,700 USD a month.
  5. The operator's second message, "Vale solo por esta semana.", came 6.4 s after the first, once
     the decision was made. Sales took it as a late decision and told the person.
- **The handover, end to end:**
  1. Sales chose `handover`. Its summary was posted to the channel as a new post, with the facts
     line, the menu's help and the Handback button.
  2. The operator answered as Juan in the post's thread 66 s later; it reached the person and took
     the case.
  3. The person's "Gracias! Y cuando me llega?" was posted in that thread.
  4. The operator clicked Handback and wrote a note (credit note NC-0001, by email in 48 hours).
     The post lost its button and showed "Closed."; its thread got "Handed back to the agent.".
  5. Sales told the person from the note. The person asked again, and sales answered from the
     note in 2.4 s.
- **"Nobody took it":** 123 s after the post, the post closed with its text, and sales told the
  person. It happened three times: in its own run, and in the first two runs of "quiet", where no
  answer reached the case within its 2 minutes.
- **"Quiet":** the operator answered once. 180 s later the post closed with "The case went quiet;
  it went back to the agent.", and sales told the person.
- **"Not taken":**
  - a message in the closed consultation's thread, and one at the channel's top level, each got
    "Not taken: no conversation here takes this message", which only the writer sees, at once;
  - in the second run of "quiet", the operator's answer went to the consultation's thread, which
    Slack's side panel still had open. It got the same notice at once: §5.4.6's point, in real use.
- **Not exercised live:**
  - a click on a closed post ("This is no longer open."): a closed post keeps no button to click.
    The path is in io.slack's code (slack_menu.rs), with no test of its own;
  - two supervisors' messages within one evaluation of sales: the example's test covers it, and
    live the second message came after the decision.
- **Health:**
  - `failed=0` and `denials=0` on all 4 hives;
  - the WF's journal holds only the expected `NOT_TAKEN`s and the stranger;
  - io.slack and SY.cognition@worker1 logged no warning after the upgrade's restart.
- **Seen on the way:**
  - **A-139:** a forwarded answer of sales to a supervisor or to a click reached the person with
    that team member's `dst_ilk`;
  - **A-140, for Case 5:**
    - the example's texts repeat the timers' durations, so with the gate's short timers sales told
      the person "15 minutos" and "30 minutos";
    - the handover summary named the person's ILK and the IO node;
  - **for io.slack's revamp (A-130):**
    - an empty fact shows as "person:" (the gate's client sends no sender);
    - Slack's own formatting reaches the person as typed (`_Hola…_`).

### 5.5 Case 5: Archi writes and operates WFs. Decided with the operator on 2026-11-02 and 2026-11-03, validated on 8.x on 2026-11-03

- **The operator's frame (2026-11-02):**
  - *"enfocate en terminar el WF y usar Archi lo más simple posible, el handbook con los casos
    atomizados"*.
  - Archi needs general cases: not a whole case to copy, but the mechanisms it puts together
    itself (*"no es pasarle todo el caso... sino los mecanismos para que él lo arme"*).
  - **Out of Case 5, a chapter of its own:** where the solutions Archi deploys are kept, ids for
    their nodes, debugging them (*"un montón de temas"*).
- **The topics, outside in.** The operator agreed to all four; each is decided in turn.
  1. **Archi writes a WF** from atomized cases in its handbook (§4.2): one mechanism each, which
     Archi combines. A-140's rules go with it: a text never repeats a value the definition holds
     elsewhere, and what goes to the team names the person, with no internal ids.
     - **Decided (2026-11-02):** one small case per mechanism, each a definition that compiles
       alone (the docs test compiles every definition in the handbook). The operator: a whole
       case is never one of the normal patterns. Case 4's handover is not an atom; what is
       extracted from it is: redirecting the conversation to another agent, escalating, and the
       like.
     - **Verified:** the handbook (57 KB) goes whole into the four roles of Archi's pipeline,
       while of the cookbook only the first 8 entries go, in the file's order
       (`build_designer_prompt`). The WF example is two seeds of 13 KB, the whole consultation.
       So the atoms go in the handbook, and the cookbook keeps one small seed.
     - **Verified:** each of those roles asks for at most 4,096 tokens an answer, thinking
       included (the AI SDK's default; Archi sets no other), and the SDK gives a call 300 s. The
       example's definition alone is 13 KB. Case 5 sizes both for a definition Archi writes.
  2. **A check before deploy** that stages nothing (A-128), with `COMPILE_ERROR` back to the
     designer (§4.7).
     - **Verified (2026-11-03):**
       - Archi checks only a definition's shape before deploy (`workflow_definition_shape_error`).
         The real check, CEL, menus and node names, runs only at the deploy, after the operator
         said yes.
       - A deploy's error is `ExecutionActionFailed`: the run stops and asks the operator
         (`retry`, `restart_from_design`, `discard`), and the role that wrote the definition never
         sees it.
       - `restart_from_design` runs the task again with no context, so the error is lost
         (`execute_pipeline_start_with_context(…, "")`).
     - **Decided (2026-11-03),** the operator: as OPA's.
       - **`wf_rules_check`** (`POST /hives/{hive}/wf-rules/check`), as `opa_check` is. The hive's
         SY.wf-rules compiles the definition and only answers. It takes no version, writes nothing,
         and leaves the staged and the current definitions alone. A check id ties the answer to
         its request, and each error names its exact path.
       - **Archi checks every definition before showing it to the operator.** In the pipeline, a
         failed check goes back to the role that wrote the definition, the designer, as design
         feedback (3 iterations, `MAX_DESIGN_ITERATIONS`). In chat, the handbook's order is check,
         show, deploy.
       - *Corrected 2026-11-03, while building:* a workflow's definition is the designer's, in the
         manifest's `wf_deployments`. The programmer builds only runtime packages
         (`artifact_bundle_from_real_programmer_submission`). So the check belongs to the design
         stage, not to the artifact loop.
       - **A deploy that fails anyway** goes back to that role too, and `restart_from_design`
         carries the error.
  3. **Deploying and updating without breaking what runs.** Each instance keeps its definition
     (§5.1, item 3). §4.7's pipeline problems are read again in the code first.
     - **Verified (2026-11-03),** §4.7's three problems hold:
       - **A first deploy without `auto_spawn`** publishes the workflow and makes it current, but
         creates no node, and answers ok (go/sy-wf-rules/node/deploy.go, "auto_spawn disabled").
         The pipeline leaves `auto_spawn` and the tenant to the plan compiler, a model
         (`pin_wf_plan_steps`).
       - **An update restarts the node twice:** `WfDeployRestart` plans `wf_rules_compile_apply`
         and `restart_node`, and the compile_apply already restarts it.
       - **A removal never removes a workflow with running instances:** the pipeline deletes
         without `force`, and SY.wf-rules refuses (`INSTANCES_ACTIVE`, or `INSTANCES_UNKNOWN`
         when it cannot count them). The run stops.
     - **Decided (2026-11-03),** the operator, on the three:
       - **A first deploy always creates the node:** the pipeline pins `auto_spawn` and the
         solution's tenant, not the model;
       - **an update is one compile_apply,** so one restart;
       - **a removal's plan says how many conversations are running** (the snapshot counts
         them). The operator's confirmation removes it with `force`, and those conversations end;
         otherwise the operator waits, or cancels them (topic 4).
  4. **Operating instances through SY.admin** (§4.8): the engine's verbs exist, and nothing exposes
     them.
     - **Verified (2026-11-03):**
       - The engine answers `WF_LIST_INSTANCES` (a status, a limit, a time), `WF_GET_INSTANCE`
         (the instance, its input, its state variables and its last log entries) and
         `WF_CANCEL_INSTANCE` (an id and a reason; the cancel transition runs).
       - Only SY.admin@motherbee and the hive's SY.wf-rules may ask them (A-119).
       - Neither SY.admin nor Archi names them.
     - **Decided (2026-11-03), mine,** under the operator's word: *"primero miralo vos.. si explota
       lo charlamos"*. SY.admin gets three actions on a WF node, and Archi reaches them as it
       reaches every admin action:
       - `wf_list_instances`;
       - `wf_get_instance`;
       - `wf_cancel_instance`, which changes something, so Archi asks the operator first.

       An instance that waits shows its state and how long it has been there. Nothing new is built
       in the engine.

  The gate's deployment is mine to choose (the operator): Archi deploys a WF, its agents too, or
  io.slack as well.
- **Solution development, a chapter of its own** (the operator, 2026-11-03). What Archi needs as
  the creator of solutions was not planned well, and must not be born half-built:
  - a document on developing solutions;
  - a handbook of good development practices;
  - for each solution, a place on disk, a minimal manifest, and how to deploy it.

  For now, *"si es necesario borrar todo y sobre cada caso iniciar de cero el entorno, tal vez es
  preferible ahora"*. So Case 5's gate starts from zero, and so does each case after it.
- **The gate is mine to run** (the operator, 2026-11-03): *"no me esperes a mí para esta prueba,
  confío en que si el bug no sale ahora, explota mañana"*.
- **Archi's model (the operator, 2026-11-02):** Claude Opus 5.5, *"así programa la solución ese
  modelo"*. Done the same night as a config change of the motherbee (ledger 0.1.94).
  - **Later, Opus for Archi alone** (the operator, 2026-11-03). The build gives Archi a model of
    its own: a `model` in its own config, set through SY.admin as ai.generic's `behavior.model` is,
    and the hive's when it has none. The motherbee then goes back to Sonnet 5.5. Until then it
    stays on Opus, and the operator watches its costs.
  - **A token of its own needs no code.** A node's key is the one it owns, else one assigned to
    it, else its tenant's general key (`resolve_resource`, crates/fluxbee_sdk/src/vault.rs).
    SY.admin's vault put takes `metadata.assigned_nodes`. The operator makes a key for Archi at
    Anthropic and hands it over by a file's path, and it is loaded assigned to SY.architect.
  - **The limits go up in the build** (the operator: *"subí límites en costo de tokens"*):
    - an answer of each of Archi's roles: 4,096 tokens today, thinking included;
    - a pipeline task's budget: 250,000 tokens today (`TASK_AGENT_TOKEN_BUDGET`);
    - a call's time: 300 s today (`MODEL_CALL_TIMEOUT`).
- **The ILKs (A-139), decided 2026-11-02, built with Case 5.** The WF routes, so it owns the fact:
  - everything the WF sends to the person's IO node names the person as its `dst_ilk`;
  - a message the WF writes to an IO node does not name the person as its sender (`src_ilk`,
    `ich`). Who said what travels in the facts (`vars`), and later in the trace (A-138);
  - to an agent nothing changes: it gets the person, as A-116 decided.
- **The test's details go with Case 5 as bugs; their mechanisms wait.** The operator: io.slack's
  mechanism is partial, and its socket and token rotation are reviewed in its revamp, *"pero si
  podés sacar bugs, mejor"*; the same for cognition and the rest.
  - **io.slack** (proposed 2026-11-02):
    - a reply also sent to the channel, and a reply with a file, reach the mesh (A-130);
    - a reply with an HTML file goes out (A-108);
    - a mention that arrives as two events is relayed once (A-130);
    - a message with no text posts nothing, with a log line, instead of the Spanish fallback
      (A-130);
    - a post with only files is recorded (A-130);
    - an empty fact is left out of the facts line (A-130, the gate);
    - an event of another channel that a node drops is logged above `debug`, and the doc says that
      each node needs its own Slack app (A-129).
  - **For io.slack's revamp:** Socket Mode, and one node per channel or per workspace (A-129);
    token rotation; the text's format across channels (Slack's `_Hola…_`, A-130); where a message
    in a channel belongs (A-136).
  - **Cognition: no bug.** Every node's journal on worker1 and the motherbee during the gate
    (2026-11-02, 18:45 to 20:40 UTC) holds only the upgrade's restarts, and SY.cognition logged no
    warning. Its open point is a mechanism, which conversations it follows (A-137): a topic of its
    own, like the trace (A-138).
  - **The rest:** SY.frontdesk.gov, and SY.admin's executor, wait for the vault at boot as their
    peers do (A-141). The upgrade's restart left frontdesk without its AI.
  - **A-140,** the example's texts and the handover summary, goes with topic 1.
  - **Case 5's own:** A-126 and A-127 (Archi and SY.admin around the agents), and A-128.
- **Next:** topics 1 to 4, one at a time; then the build's design, its review and the gate. A run
  from zero closes the track, with the flow fresh in mind (the operator).

#### 5.5.1 The build design and the gate (2026-11-03)

Six pieces. Each one's tests come with it, and every behavior below is a contract the review checks.

**Piece 1, the WF engine and the language** (go/nodes/wf/wf-generic, go/pkg/wfcel, the example):
- **A-139, the ILKs** (actions.go):
  - a `send_message` or a `forward` to the person's IO node names the instance's person as
    `dst_ilk`, when the instance has one. *Changed in the review (2026-11-03, §5.5.2):* the
    person's IO node is the node whose message started the instance (`person_io`, store v6), not any
    `IO.*` node of `start_from`; only its messages can leave the instance with no person. Without a person, a forward keeps its
    `dst_ilk`, and a send has none. *Added while building (2026-11-03):* the person's channel too,
    as `ich`, on the same condition. A forward of an agent's answer to a supervisor otherwise
    carries the supervisors' channel;
  - a `send_message` to an `IO.*` node carries no `src_ilk` and no `ich`. The router's stamp names
    the WF as its sender, and the facts (`vars`) say who said what;
  - to an `AI.*` or a `WF.*` node nothing changes: a send carries the person (A-116).
  - *Changed by A-215 (the operator's call, 2026-11-10):* a `forward` to an `IO.*` node other than
    the person's names no one either: no `dst_ilk` and no `ich`, its `src_ilk` kept. It kept the
    message's, so an agent's answer to the person forwarded to the team's channel reached
    cognition as said to the person there.
- **A-140:**
  - the example's texts (go/nodes/wf/examples/packages/wf.sales.invoice) name no duration;
  - the catalog's `handover` `summary` reads "What the team needs to take over: the person by name,
    what they want, what was done. No internal ids." The handbook's table follows
    (`TestTheHandbookListsTheKnownOptions`).
- **Tests:**
  - an agent's answer to a supervisor reaches the person's IO node with the person's `dst_ilk`;
  - a `HANDOVER` post carries no person;
  - an agent still gets the person.

**Piece 2, SY.wf-rules' check** (go/sy-wf-rules):
- **`check` is an operation of its own,** no longer `compile` under another name (dispatch.go). As
  go/sy-opa-rules' check:
  - it validates the definition as a compile does, with the same limits;
  - it writes nothing: no staged definition, no package, no store row. It takes no version;
  - it answers `ok`, the `check_id` of its request (echoed from `config.check_id`), and on a
    refusal the error with its exact path.
- **Tests:** a check between a compile and its apply leaves the staged definition and the current
  one as they were; a refused definition answers its path; the check id comes back.

**Piece 3, SY.admin and SY.frontdesk.gov** (src/bin/sy_admin.rs, nodes/gov/ai-frontdesk-gov):
- **`wf_rules_check`:** `POST /hives/{hive}/wf-rules/check`, `{workflow_name, definition}`, "Validate
  a workflow definition without staging or applying it."
  - It goes to `SY.wf-rules@<hive>` with a fresh `check_id`, and waits for that answer only, as
    `send_opa_check` does.
  - It is in every list `opa_check` is: the catalog, the help, the args, the examples, and the
    parity script.
- **The instances** (topic 4), each sent to the WF node, answering the engine's payload as it is:
  - `wf_list_instances`: `GET /hives/{hive}/nodes/{node}/wf/instances?status=&limit=`
    (`WF_LIST_INSTANCES`). The limit is 50 by default, and at most 200;
  - `wf_get_instance`: `GET /hives/{hive}/nodes/{node}/wf/instances/{instance_id}?log_limit=`
    (`WF_GET_INSTANCE`);
  - `wf_cancel_instance`: `POST /hives/{hive}/nodes/{node}/wf/instances/{instance_id}/cancel`, with
    `{reason}` (`WF_CANCEL_INSTANCE`).

  A bare node name takes the path's hive, and a name that is not `WF.*` is refused.
- **A-127:** `control/config-get`, `control/config-set` and `messages` qualify a bare name with the
  path's hive. `messages` refuses a qualified name of another hive, as the control routes do.
- **A-141:** SY.admin's executor key and SY.frontdesk.gov's key are read at boot with
  `resolve_resource_awaiting_vault` and `VAULT_BOOT_WAIT`, as their peers do (A-26).

**Piece 4, SY.architect** (src/bin/sy_architect.rs and src/bin/sy_architect/):
- **Topic 2, the check:**
  - **At the design stage,** once the designer's manifest passes `validate_manifest_v2`, each
    `wf_deployments[i]` goes to `wf_rules_check` on its hive. Each refusal is a design finding that
    names the workflow and the error's path, so the designer gets it as feedback (`RetryDesign`,
    up to `MAX_DESIGN_ITERATIONS`). A check that cannot run is a blocking finding, never a pass.
  - **A deploy's `COMPILE_ERROR`** goes back to the designer too, as a design failure that carries
    the error.
  - **`restart_from_design`** gives the new run's design the blocked run's last error.
  - **In chat,** Archi's prompt and the handbook say: check, show, deploy.
- **Topic 3, the deploy:**
  - **`DesiredWfDeployment.tenant_id`:** the designer declares it, and it is required for a
    workflow the snapshot does not have. The pipeline pins `auto_spawn: true` and that tenant on the
    create's `wf_rules_compile_apply` (`pin_wf_plan_steps`). The plan compiler is no longer told to
    add them.
  - **`WfDeployRestart`** plans `wf_rules_compile_apply` alone.
  - **A removal (`WfRemove`):**
    - its delta operation notes the workflow's running instances, from the snapshot's
      `active_instances`: a number, or "unknown";
    - the confirmation shows them;
    - the pinned `wf_rules_delete` carries `force: true`.
- **Topic 4:** Archi reaches `wf_list_instances` and `wf_get_instance` as reads, and
  `wf_cancel_instance` as a change that it asks the operator first, in the same lists and with the
  same timeouts as the other admin actions.
- **Archi's own model.** `CONFIG_SET` on SY.architect takes `ai.model`:
  - a model of the hive's provider, by ai.generic's check (A-104), kept with Archi's config;
  - without it, the hive's model;
  - `CONFIG_GET` shows the model and whose it is;
  - the AI runtime is rebuilt with no restart, and the contract's notes say so.
- **The limits:**
  - every call of Archi's asks for up to 32,000 tokens;
  - `TASK_AGENT_TOKEN_BUDGET` is 1,000,000;
  - a call of Archi's may take 600 s, with no shorter limit under it (the HTTP client's included).
    The SDK's `MODEL_CALL_TIMEOUT` stays 300 s for every other node.
- **A-126:** `DELETE /api/agent-assets?hash=` and `delete_agent_asset` refuse while an ILK definition
  names the hash, and list those ILKs. With `force`, the asset is deleted anyway.
- **The cookbook:** the two WF seeds give way to one small seed, the basic loop, at `seed_version` 6.

**Piece 5, io.slack's bugs** (nodes/io/io-slack; A-130, A-108, A-129):
- **Replies that reach the mesh now:**
  - a `thread_broadcast` is relayed as the plain message it is;
  - a `file_share` is relayed with its files, after the checks a plain message gets.
- **Deduplication** is by the message (channel and `ts`) as well as by the event.
- **A message with no text, no blocks and no files** posts nothing, and logs it.
- **A post with only files, on a new place:** a first text post names the files and is recorded,
  and the files go under it.
- **An empty fact** is left out of the facts line.
- **Files:**
  - an HTML file goes out;
  - a file Slack refuses no longer fails the rest: the text and the other files go, and the text
    names what was left out.
- **A dropped event of another channel** is logged at `info`.
- **docs/io/io-slack.md** says that each node needs its own Slack app.

**Piece 6, mine, the handbook and the docs:**
- **Handbook §4.2, the atoms.** One small case per mechanism, each a definition that compiles alone,
  and the docs test compiles them:
  1. a person talks with an agent;
  2. the conversation goes to another agent, and comes back by `done` or by inactivity;
  3. escalating a question: the agent consults the supervisors, and stays the person's voice;
  4. escalating the conversation: people take it, and give it back by an option or a timer;
  5. a timer whose time an agent's answer gives;
  6. the catch: answers in any state, late events, `none` and errors.

  Case 4's handover is a combination of 4 and 5, never a pattern of its own.
- **Rules:**
  - A-140's two rules;
  - check, show, deploy;
  - operating instances;
  - a removal with conversations running.
- **The docs:** docs/nodes/sy-wf-rules.md (the check), the admin actions, docs/io/io-slack.md, and
  §4.7 and §4.8 marked as resolved here.

**The release (0.1.95), beyond the deploy:**
- **Archi's model:** `ai.model` set to `claude-opus-5-5` on SY.architect, through SY.admin.
- **The motherbee's `hive.yaml`** goes back to `claude-sonnet-5-5`, with its four AI nodes
  restarted. Only Archi stays on Opus (the operator).

**The gate, mine, from zero:**
1. **The old gate goes:** WF `gate0192`, its node, its three agents and their roles. These stay:
   - the test tools (the tenant `pruebas`, its keys, `AI.pruebas@worker1`);
   - `IO.slack.gate0192`, for io.slack's own checks.
2. **I ask Archi for a solution** in the operator's words: sales serves a person; invoicing takes
   the conversation and gives it back; sales escalates a question and the conversation to a team; a
   timer. The person and the team are gate clients on worker1. Archi works from the atoms: it
   checks, shows, and deploys.
3. **The person and the team talk to it,** and each atom's behavior is checked, with the ILKs of
   A-139 in the messages that arrive.
4. **Archi operates it:** it lists the instances, shows one, and cancels one after asking.
5. **Archi changes a text.** The node restarts once, and a running instance keeps its definition.
6. **Archi removes the workflow with a conversation running.** Its plan says so, and the removal
   goes ahead on the confirmation.
7. **Everything Archi deployed goes,** so the next case starts from zero.

On io.slack, whatever needs no person writing in Slack is checked live: an HTML file, an empty
fact, a files-only post. The rest is checked by tests.

#### 5.5.2 The review round (2026-11-03)

Five reviewers, each blind to the others' lens:
- the engine with SY.wf-rules;
- SY.admin with SY.frontdesk.gov;
- SY.architect;
- io.slack;
- the seams between the components, with the handbook Archi follows. This one ran the atoms on a
  copy of the engine.

No high finding. Fixed before the release:
- **A deploy whose WF node did not run was reported as done** (medium, found by two reviewers).
  SY.wf-rules answered `ok` with the node's failure only in `wf_node`. SY.admin's executor and
  Archi took that for a deploy that ran, in the pipeline and in chat. Examples: a spawn refused
  over a tenant the designer copied from a seed, or a package that reached a worker late. A later
  run found the definition current and did nothing. The fixes:
  - SY.wf-rules refuses such a deploy, `WF_NODE_NOT_DEPLOYED` (SY.admin 502). The answer still says
    what is current and what happened to the node. Only `auto_spawn` off, with no node, is no
    failure;
  - Archi's verification reads each deploy's node step too. That covers a spoke still on 0.1.94
    during the upgrade;
  - the reconciler deploys again a workflow whose definition is current and whose node is gone;
  - every deploy pins `auto_spawn` and the declared tenant;
  - Archi's classifier hands `WF_NODE_NOT_DEPLOYED` to the operator.
- **Atom 3 sent sales' decision to the person with the team's `io` context** (medium, confirmed on
  the engine's copy). An agent answers where it was asked, so an IO node that places a reply by its
  `reply_target` (io.wapp) would drop the decision. The atom's two forwards to the person now write
  `meta.context.io` from `state.io`, as the validated example does, and a line under the atom says
  why.
- **The person's IO node was any `IO.*` node of `start_from`** (low; the engine). A message from the
  team's IO node, when `start_from` names it too, took the person away from the instance. Sends to
  it also named the person. The person's IO node is now the one whose message started the instance
  (`person_io`, store v6). Only its messages can leave the instance with no person.
- **The bot token went to whatever host a shared file named** (low; io.slack). This became
  reachable once every file share in the channel reaches the mesh: an external file, or one an app
  added. The token now goes only to Slack's file store.
- **A deploy step that timed out said "will be retried automatically", and nothing retried it**
  (low; SY.architect). At execution, every failure but the design's now goes to the operator, with
  the options `retry`, `restart_from_design` and `discard`.
- **A torn `config.json` jammed Archi's CONFIG_GET and CONFIG_SET** (low). Archi now writes the
  file through a temporary one, and a CONFIG_SET replaces a config it cannot read.
- **The handbook said `updated_at_ms` is since when an instance is in its state** (low; the seams).
  It is the instance's last change. The time it entered its state is its last `transition` entry in
  the log.
- **Mine, while merging:**
  - an automatic redesign recorded its closed run as discarded by the operator: now
    `redesigned_after_failure`;
  - one of Archi's tests listed instances by a status the engine does not have.

Not fixed:
- **A check on a spoke's 0.1.94 SY.wf-rules writes its staged slot** (low; SY.admin). This only
  happens during the upgrade, where the old SY.wf-rules takes `check` for `compile`. The release
  uses Archi only once the spokes run 0.1.95. Renaming the operation would part it from OPA's
  `check`, which the operator chose.
- **Archi's chat answers only when its turn ends, and the edge waits 30 s** (A-142, new). It is not
  live: io.web proxies no Archi before its stage 2, and is recorded for that stage.

#### 5.5.3 The gate (0.1.95 to 0.1.98 on 8.x, 2026-11-03)

**The setup, from zero:**
- the old gate removed (WF gate0192, its three agents and their roles);
- a team member registered through Cloud;
- the person and the team as clients on worker1's router (`IO.gate0195@worker1`,
  `IO.gate0195team@worker1`), each sending its ILK and its ICH;
- Archi on its own Opus, the rest of the motherbee on Sonnet.

**The first run** asked for the solution in the operator's words: sales, invoicing and its return,
a discount the team decides in 3 minutes, a credit note to the team with 5 minutes of quiet, and a
due invoice for collections.

**What Archi did:**
- created three roles and a skill, and wrote the workflow from the atoms;
- checked it with `wf_rules_check` before every plan;
- stopped to ask when it could not read the two IO nodes' config to point them at the workflow.

The workflow it wrote:
- `start` keeps the person's `io` and message;
- sales talks, with an idle timer;
- invoicing gets the kept message, and `issued` sets the due timer from its answer;
- `consulting` asks the team, with a 3-minute timer, and forwards the decision with the person's
  `io`;
- `with_team` posts the handover with `handback`, with a 5-minute quiet timer;
- the catch takes the due timer and late answers.

**What the run surfaced, each fixed at the source and released before the run went on:**
- **A-143:** Archi set its agents' roles with `hive: worker1`, and the replica refused SY.admin.
  - 0.1.96 sends identity writes to the primary.
- **A-144:** the workflow's deploy outgrew SY.admin's executor model, which had to repeat the whole
  definition, and Archi's chat said "finished".
  - 0.1.96 dispatches a step with nothing to fill as planned, and the chat verifies the run. The
    chat then said `error`, as it should, for the next failure.
  - 0.1.97: the direct dispatch had reused a fallback that routes no wf-rules action, and now
    dispatches the way the model's tool does.
- **A-145:** the name `ventas-gate0195` passed the check, and the package's publish refused it.
  - 0.1.98 gives SY.wf-rules, Archi and SY.admin the runtime naming policy. Archi removed the broken
    workflow (`wf_rules_delete`) and deployed it as `ventas.gate0195`.
- **Seen, not fixed:**
  - Archi does not keep a definition across chat turns. It asked for the JSON once, and later
    rebuilt it from the conversation. The deployed one can be read (`wf_rules_get_workflow`, the
    handbook now says so). Where a solution lives is the chapter decided apart.
  - A-146, the router's warning at every connect.

**What passed, live:**
- **The ILKs (A-139):**
  - everything the person received named the person as `dst_ilk` and the person's channel as
    `ich`, whoever wrote it (sales, invoicing, collections, a team member);
  - the WF's own posts to the team named no one;
  - a forward kept its own sender.
- **The atoms:**
  - talk (every thread);
  - redirect: invoicing issued the invoice and gave the conversation back with `done`;
  - consult: a decision reached the person through sales; no decision in 3 minutes made sales tell
    the person; sales also consulted the team for a price it did not have;
  - escalate: the team talked with the person both ways and handed back with a note; 5 minutes of
    quiet gave the conversation back;
  - the due timer: collections told the person 3 minutes after the invoice;
  - the catch.
- **Operating:**
  - Archi listed the instances with the time each entered its state (from the log, not
    `updated_at_ms`) and showed one in detail;
  - it cancelled one after the operator's confirmation.
- **An update:**
  - Archi changed a text with one `compile_apply`, and the node restarted once;
  - the recovery loaded 5 instances on 2 definitions;
  - a conversation born before the update ended with the old text, and one born after with the
    new.
- **The removal:** Archi listed the 6 running conversations with their definitions, offered to
  remove now, wait, or cancel them, and removed with `force` on the operator's OK.
- **The cleanup:** Archi killed its three agents and deleted their roles and skill. A-126's check
  found no ILK naming them, so no `force`.
- **Journals:** the WF node logged no warning; the agents logged only the upgrades' restarts.

**The run from zero** (0.1.98): a new Archi session, the same request, and one line the operator
would know, that the IO nodes are clients with no config.
- **The design:** Archi created three roles, wrote the workflow `gate0195`, a name the policy
  admits, and checked it. It then staged one plan of 13 steps:
  - the three agents `AI.gate0195.*`, each with its config and its role;
  - the roles set with `hive: worker1`, which now went to the primary (A-143);
  - the workflow, with `auto_spawn`.

  The plan ran in 46 s, and its verification passed: the WF node runs.
- **The same five conversations,** on new threads, passed as before:
  - the ILKs and the ICH;
  - the consulted discount decided and undecided;
  - the handover handed back and gone quiet;
  - the invoice and its return with `done`;
  - collections at the due time.
- **Operating:** Archi listed the instances and cancelled one on confirmation. It answered the two
  in one plan, and the plan's message hid its listing (A-147, new).
- **The update:** Archi read the deployed definition (`wf_rules_get_workflow`) and changed one text.
  The node restarted once and recovered 5 instances on 2 definitions. A conversation born before
  ended with the old text, one born after with the new.
- **The removal:** Archi listed 6 running conversations and staged `wf_rules_delete` with `force`.
  The plan's task said they end; the operator confirmed.
- **The cleanup:** the three agents killed and their roles deleted. Nothing of the solution is
  left, and the test tools stay.
- **Archi on Opus:** its first run cost about 15 dollars, over 110 calls, with the retries the
  bugs caused.

**Not checked live:** io.slack's fixes (A-130, A-108, A-129), which need a person in Slack or a
file in the blob store. Its 79 tests cover them; io.slack's revamp will exercise them.

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
- docs/audits/2026-07-23-sy-timer-wf-audit.md: G1, G4, G5 and G9 are still open. G3 and G7 were
  fixed on 2026-07-23 (`0d9200e`), and G2 in 0.1.90 (FINDINGS A-117: a WF always sends `user`).
- docs/audits/2026-07-24-wf-dynamic-target-opa-design.md: the allowlist was never built.
