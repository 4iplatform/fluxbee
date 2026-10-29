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
  due-date timer and a payment check. Decided so far: one instance per conversation. Next: what the
  forward may change. The operator: close the WF topic completely, case by case; Archi through
  Cloud waits.

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
- **What "forward as received" may change:** the destination only, or named fields too. Being
  decided in §5.2.

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
- **Case 2:** taken into Case 1 on 2026-10-29.
- **Case 3, escalation.** A field of the agent's evaluation routes the case to a supervisor's IO
  node (a name written in the definition), with its data.
- **Case 4, Archi writes and operates WFs:**
  - the guide and seeds;
  - a check before deploy, with `COMPILE_ERROR` back to the designer;
  - instances visible through SY.admin.
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

### 5.2 Case 1: the operator's circuit (being decided, 2026-10-29)

**The circuit:**
1. A registered person writes. The IO node hands every message to the WF, which sends it to
   `AI.sales`: every conversation enters through sales.
2. The message is about an invoice. Sales answers the WF with a field (`area`), and the WF's CEL
   forwards the person's message to `AI.invoice`. From then on, invoice holds the conversation.
3. `AI.invoice` issues the invoice and answers with its data, the due date included.
4. The WF keeps the invoice in the instance's state and sets a timer at its due date. The time
   comes from invoice's answer, not from the definition.
5. At the due date, the WF sends the invoice to `AI.payments`, which checks the payment and
   answers.
6. The WF delivers that answer to the person through the IO node.

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
  `meta.context` (the message handler in nodes/ai/ai-generic/src/bin/ai_node_runner.rs).
- IO.api sends every message to one configured `io.dst_node`, here the WF. It only takes messages
  in: it ignores any answer (`run_router_loop`), and this version has no outbound delivery
  (docs/io/io-api-node-spec.md).

**The test.** The operator: no extra pieces in the infrastructure for now; the integrated tests
come later.
- The messages are sent from inside, with Case 0's gate tool, and the answers are read in the
  trace.
- The due date is a few minutes away; SY.timer's floor is 60 s.

**Open, one at a time:**
1. **What the forward may change** (§4.9). In step 2 the person writes *"dale, facturámelo"*. If
   only the destination changes, invoice receives that sentence alone, and the order (10 licenses
   at their price) was said to sales.
2. **When control goes back to sales,** and when the instance ends. It must outlive the due date.
3. **Who may drive a conversation's instance** (A-123). The proposal: only the names written in the
   definition, and SY.timer.

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
