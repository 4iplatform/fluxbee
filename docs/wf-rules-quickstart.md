# SY.wf-rules — Quick Start Guide

A practical guide to define, deploy and operate workflows in Fluxbee.

**Prerequisite:** the `wf.engine` base runtime on the hive. The packaged `.deb` seeds it, at the
package's version; check that it is there (its versions, and which is current):

```bash
curl -sS "http://127.0.0.1:8080/hives/motherbee/runtimes/wf.engine" | jq .payload
```

Do not publish `wf.engine` by hand on a packaged install: a base runtime published by hand keeps
`current` through every upgrade (FINDINGS A-122), so every WF node would stay on that build.
`scripts/publish-wf-runtime.sh` is for a dev checkout without the package only:

```bash
# dev checkout only: makes this build the engine of every WF node of the hive (each restarts)
bash scripts/publish-wf-runtime.sh --version 0.1.0 --set-current --sudo
```

---

## Environment variables

```bash
export BASE="http://127.0.0.1:8080"
export HIVE="motherbee"
export TENANT_ID="tnt:REPLACE_ME"
```

All endpoints use the prefix `$BASE/hives/$HIVE/wf-rules`.

`TENANT_ID` is used on the **first deploy** when `auto_spawn: true` and the node `WF.<workflow_name>@<hive>` does not exist yet. It is not propagated through an environment variable of the `sy.wf-rules` service; it must travel in the request.

If you operate through `SY.admin`/Archi, the surface is discovered with:

```bash
curl -sS "$BASE/admin/actions/wf_rules_compile_apply"
curl -sS "$BASE/admin/actions/wf_rules_get_status"
curl -sS "$BASE/admin/actions/wf_rules_list_workflows"
```

That exposes the `request_contract`, the canonical example and the `example_scmd` that Archi should use.

---

## 1. Write the workflow definition

A workflow is a JSON with states, transitions, CEL guards and actions. The system validates everything when it receives it — there is no need to compile locally.

### Full example: `WF.invoice`

The workflow of the spec's case study (`docs/spec-systemnode-wf-v1_1.md` §19). It asks an agent to
check the customer's data, for a structured answer; creates the invoice through `IO.quickbooks`;
and asks the billing agent to deliver it. When data is missing, it tells the billing agent and ends:
a person answers the agent of their conversation, never a WF instance, and the agent requests the
invoice again.

```json
{
  "wf_schema_version": "1",
  "workflow_type": "invoice",
  "description": "Issues an invoice: has the customer's data checked, creates the invoice and has it delivered; tells the billing agent what is missing.",
  "input_schema": {
    "type": "object",
    "required": ["customer_id", "items"],
    "properties": {
      "customer_id": { "type": "string" },
      "items": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["sku", "qty", "unit_price"],
          "properties": {
            "sku": { "type": "string" },
            "qty": { "type": "integer" },
            "unit_price": { "type": "number" }
          }
        }
      },
      "currency": { "type": "string" }
    }
  },
  "initial_state": "validating_data",
  "terminal_states": ["completed", "missing_data_requested", "failed", "cancelled"],
  "states": [
    {
      "name": "validating_data",
      "description": "Asking AI.billing-validator, for a structured answer, whether the customer's data is complete.",
      "entry_actions": [
        {
          "type": "set_variable",
          "name": "validation_request",
          "value": "'Customer %s asks for an invoice of these items: %s.'.format([input.customer_id, input.items])"
        },
        {
          "type": "send_message",
          "target": "AI.billing-validator@motherbee",
          "meta": {
            "msg": "VALIDATE_INVOICE_DATA",
            "context": {
              "response_envelope": {
                "kind": "json_object_v1",
                "strict": true,
                "required": ["complete", "missing"],
                "properties": {
                  "complete": { "type": "boolean" },
                  "missing": { "type": "array", "items": { "type": "string" } }
                }
              }
            }
          },
          "payload": { "type": "text", "content": { "$ref": "state.validation_request" } }
        },
        {
          "type": "schedule_timer",
          "timer_key": "validation_timeout",
          "fire_in": "10m",
          "missed_policy": "fire"
        }
      ],
      "exit_actions": [
        { "type": "cancel_timer", "timer_key": "validation_timeout" }
      ],
      "transitions": [
        {
          "event_match": { "msg": "VALIDATE_INVOICE_DATA" },
          "guard": "json_parse(event.payload.content).complete == true",
          "target_state": "creating_invoice",
          "actions": [
            { "type": "set_variable", "name": "validated_at", "value": "now()" }
          ]
        },
        {
          "event_match": { "msg": "VALIDATE_INVOICE_DATA" },
          "guard": "json_parse(event.payload.content).complete == false",
          "target_state": "missing_data_requested",
          "actions": [
            {
              "type": "set_variable",
              "name": "missing_data_request",
              "value": "'The invoice for customer %s needs: %s. Ask the customer, then request the invoice again.'.format([input.customer_id, json_parse(event.payload.content).missing.join(', ')])"
            }
          ]
        },
        {
          "event_match": { "msg": "VALIDATE_INVOICE_DATA.error" },
          "guard": "true",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'validator error: ' + event.payload.code" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.timer_key == 'validation_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'validator timeout'" }
          ]
        }
      ]
    },
    {
      "name": "missing_data_requested",
      "description": "Terminal: tells the billing agent what is missing. The customer answers the agent, never this instance; the agent requests the invoice again.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing@motherbee",
          "meta": { "msg": "INVOICE_DATA_MISSING" },
          "payload": { "type": "text", "content": { "$ref": "state.missing_data_request" } }
        }
      ],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "creating_invoice",
      "description": "Invoking IO.quickbooks to create the invoice.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "IO.quickbooks@motherbee",
          "meta": { "msg": "INVOICE_CREATE_REQUEST" },
          "payload": {
            "customer_id": { "$ref": "input.customer_id" },
            "items": { "$ref": "input.items" },
            "currency": { "$ref": "input.currency", "default": "USD" }
          }
        },
        {
          "type": "schedule_timer",
          "timer_key": "qb_creation_timeout",
          "fire_in": "5m",
          "missed_policy": "drop"
        }
      ],
      "exit_actions": [
        { "type": "cancel_timer", "timer_key": "qb_creation_timeout" }
      ],
      "transitions": [
        {
          "event_match": { "msg": "INVOICE_CREATE_RESPONSE" },
          "guard": "event.payload.ok == true",
          "target_state": "delivering_to_customer",
          "actions": [
            { "type": "set_variable", "name": "invoice_id", "value": "event.payload.invoice_id" },
            {
              "type": "set_variable",
              "name": "delivery_request",
              "value": "'Send customer %s the invoice %s: %s'.format([input.customer_id, event.payload.invoice_id, event.payload.link])"
            }
          ]
        },
        {
          "event_match": { "msg": "INVOICE_CREATE_RESPONSE" },
          "guard": "event.payload.ok == false",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'quickbooks error: ' + event.payload.error" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.timer_key == 'qb_creation_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'quickbooks timeout'" }
          ]
        }
      ]
    },
    {
      "name": "delivering_to_customer",
      "description": "Asking the billing agent, for a structured answer, to send the invoice to the customer.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing@motherbee",
          "meta": {
            "msg": "DELIVER_INVOICE",
            "context": {
              "response_envelope": {
                "kind": "json_object_v1",
                "strict": true,
                "required": ["delivered"],
                "properties": { "delivered": { "type": "boolean" } }
              }
            }
          },
          "payload": { "type": "text", "content": { "$ref": "state.delivery_request" } }
        },
        {
          "type": "schedule_timer",
          "timer_key": "delivery_timeout",
          "fire_in": "5m",
          "missed_policy": "fire"
        }
      ],
      "exit_actions": [
        { "type": "cancel_timer", "timer_key": "delivery_timeout" }
      ],
      "transitions": [
        {
          "event_match": { "msg": "DELIVER_INVOICE" },
          "guard": "json_parse(event.payload.content).delivered == true",
          "target_state": "completed",
          "actions": []
        },
        {
          "event_match": { "msg": "DELIVER_INVOICE" },
          "guard": "true",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'the agent did not deliver the invoice'" }
          ]
        },
        {
          "event_match": { "msg": "DELIVER_INVOICE.error" },
          "guard": "true",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'delivery error: ' + event.payload.code" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.timer_key == 'delivery_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'delivery timeout'" }
          ]
        }
      ]
    },
    {
      "name": "completed",
      "description": "Invoice issued and delivered.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "failed",
      "description": "Terminal failure. The variable 'failure_reason' holds the cause.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "cancelled",
      "description": "Cancelled through WF_CANCEL_INSTANCE.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    }
  ]
}
```

Save it as `invoice-workflow.json` for the examples below.

---

## 2. Operations

SY.admin answers every call below in its envelope, `{status, action, payload, error_code,
error_detail}`; `payload` is what SY.wf-rules answered.

### 2.1 Compile (validate without deploying)

Validates the definition, compiles the CEL guards, saves it to staged. It deploys nothing.

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules/compile" \
  -H "Content-Type: application/json" \
  -d "$(jq -n --arg wf invoice --slurpfile def invoice-workflow.json \
    '{workflow_name: $wf, definition: $def[0]}')" | jq .
```

Expected response:
```json
{
  "status": "ok",
  "action": "compile",
  "payload": {
    "ok": true,
    "node_name": "SY.wf-rules@motherbee",
    "state": "configured",
    "schema_version": 1,
    "config_version": 1,
    "effective_config": {
      "staged": {
        "version": 1,
        "hash": "sha256:...",
        "workflow_name": "invoice",
        "compiled_at": "2026-10-29T12:00:00Z",
        "guard_count": 11,
        "state_count": 7,
        "action_count": 22
      }
    }
  },
  "error_code": null,
  "error_detail": null
}
```

If there are validation errors (an invalid CEL guard, a referenced state that does not exist, etc.), the system returns the error with the exact location. With `evnt.payload.ok == true` as the first guard of `creating_invoice`:

```json
{
  "status": "error",
  "action": "compile",
  "payload": {
    "ok": false,
    "node_name": "SY.wf-rules@motherbee",
    "state": "error",
    "schema_version": 1,
    "config_version": 0,
    "error": {
      "code": "COMPILE_ERROR",
      "detail": "states[2].transitions[0].guard: ERROR: <input>:1:1: undeclared reference to 'evnt' (in container '')\n | evnt.payload.ok == true\n | ^"
    }
  },
  "error_code": "COMPILE_ERROR",
  "error_detail": "states[2].transitions[0].guard: ERROR: <input>:1:1: undeclared reference to 'evnt' (in container '')\n | evnt.payload.ok == true\n | ^"
}
```

A guard is checked for its syntax and its roots (`input`, `state`, `event`), not for its fields: a
misspelled field (`event.payload.complette`) compiles, and at run time it is a guard that errs, so
false.

### 2.2 Apply (promote staged and deploy)

Promotes the validated definition from staged to current, generates the package, and spawns/restarts the WF node.

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules/apply" \
  -H "Content-Type: application/json" \
  -d "{\"workflow_name\":\"invoice\",\"tenant_id\":\"$TENANT_ID\",\"auto_spawn\":true}" | jq .
```

`auto_spawn: true` makes the node `WF.invoice@motherbee` be created automatically if it does not exist. If it already exists, it is restarted with the new version. In the first case, `tenant_id` is required.

Expected response (abridged):
```json
{
  "status": "ok",
  "action": "apply",
  "payload": {
    "ok": true,
    "state": "configured",
    "config_version": 1,
    "effective_config": {
      "current": {
        "version": 1,
        "hash": "sha256:..."
      }
    },
    "wf_node": {
      "node_name": "WF.invoice@motherbee",
      "action": "restarted",
      "status": "ok"
    }
  },
  "error_code": null,
  "error_detail": null
}
```

`wf_node.action` is `restarted` for a first spawn too; `restart_failed` (with `error`) when the
node could not be rebound, restarted or spawned, and `none` (with `reason`) when nothing ran
(`auto_spawn` off, or a first deploy without `tenant_id`).

### 2.3 Compile + Apply in a single step

For fast iteration: it validates and deploys in one go.

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules" \
  -H "Content-Type: application/json" \
  -d "$(jq -n --arg wf invoice --slurpfile def invoice-workflow.json \
    --arg tenant_id "$TENANT_ID" \
    '{workflow_name: $wf, definition: $def[0], auto_spawn: true, tenant_id: $tenant_id}')" | jq .
```

This is the most used command during development. It is equivalent to `compile` + `apply` in sequence.

### 2.4 Query the state

**See the current definition of a workflow:**

```bash
curl -sS "$BASE/hives/$HIVE/wf-rules?workflow_name=invoice" | jq .
```

**See the state of the workflow and its node:**

```bash
curl -sS "$BASE/hives/$HIVE/wf-rules/status?workflow_name=invoice" | jq .
```

Response (abridged):
```json
{
  "status": "ok",
  "action": "get_status",
  "payload": {
    "status": "ok",
    "hive": "motherbee",
    "workflow_name": "invoice",
    "current_version": 2,
    "current_hash": "sha256:...",
    "wf_node": {
      "node_name": "WF.invoice@motherbee",
      "running": true,
      "health_state": "HEALTHY",
      "active_instances": 3
    }
  },
  "error_code": null,
  "error_detail": null
}
```

`active_instances` is the node's own count (SY.wf-rules asks it `WF_LIST_INSTANCES`); it is `null`,
with `instances_error`, when the node does not answer.

**List all the workflows of the hive:**

```bash
curl -sS "$BASE/hives/$HIVE/wf-rules" | jq .
```

### 2.5 Rollback

Restores the previous version (backup → current) and restarts the node.

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules/rollback" \
  -H "Content-Type: application/json" \
  -d '{"workflow_name": "invoice"}' | jq .
```

The rollback is only one level deep (current ↔ backup). If you need to go back to an older version, you have to resend the definition with `compile_apply`.

### 2.6 Delete a workflow

**Safe deletion (fails if there are active instances):**

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules/delete" \
  -H "Content-Type: application/json" \
  -d '{"workflow_name": "invoice", "force": false}' | jq .
```

**Forced deletion (kills the node and deletes everything, including in-flight instances):**

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules/delete" \
  -H "Content-Type: application/json" \
  -d '{"workflow_name": "invoice", "force": true}' | jq .
```

---

## 3. CEL guards quick reference

Guards are [CEL](https://github.com/google/cel-go) expressions that decide whether a transition is taken. They are evaluated against three variables:

| Variable | Content |
|---|---|
| `input` | The payload of the event that created the instance (immutable) |
| `state` | Workflow variables, modified by `set_variable` |
| `event` | The message that just arrived and triggered the evaluation |

Built-in functions: `now()` returns the WF node's clock as Unix milliseconds (UTC), and
`json_parse(text)` decodes a JSON string (an agent's structured answer, say); CEL's string
extensions are there too (`format`, `join`, `split`, ...).

**Numbers.** A JSON integer is an `int` and any other number a `double`, in `input`, `state` and
`event` alike, live and after a restart, and in the definition's own literals. A comparison or an
equality between an `int` and a `double` works when a side comes from `input`, `state`, `event` or
`json_parse` (`state.score > 7.5` with an `int` score); arithmetic that mixes them, and `%` on a
`double`, fails. Convert first: `double(state.n) * 1.1`, `int(state.n) % 2`.

`event` holds `msg`, `type`, `thread_id`, `trace_id`, `src` (the node that sent it, as the router
stamps it), `context` and `payload`; for a `TIMER_FIRED`, also `timer_key`, the key the timer was
scheduled with.

**Bounds.** A guard runs at most 10 ms: it is stopped there, and a guard that is stopped or errs is
false. A `set_variable` value has a cost limit (1,000,000 CEL cost units): past it the action fails.

### Common patterns

**Check a boolean field in the response:**
```
event.payload.ok == true
```

**Check that a field exists:**
```
has(event.payload.missing_data)
```

**Match a specific timer** (on a `TIMER_FIRED` transition):
```
event.timer_key == 'validation_timeout'
```

**Compare strings:**
```
event.payload.status == 'approved'
```

**Check a numeric value:**
```
state.retry_count < 3
```

**Combine conditions:**
```
event.payload.ok == true && state.retry_count < 5
```

**Check membership:**
```
event.payload.status in ['approved', 'auto_approved']
```

---

## 4. Actions quick reference

Actions are the effects the workflow executes. Fixed vocabulary in v1:

### `send_message` — send a message to another node

```json
{
  "type": "send_message",
  "target": "IO.quickbooks@motherbee",
  "meta": { "msg": "INVOICE_CREATE_REQUEST" },
  "payload": {
    "customer_id": { "$ref": "input.customer_id" },
    "items": { "$ref": "state.validated_items" },
    "literal_field": "this is a literal string, not a reference"
  }
}
```

Every message a WF sends is a `user` message: `meta` takes `msg` (and `context`, see below), never
`type`, and `target` is a node name written in the definition (never a `SY.*` or `RT.*` node). A WF
that chooses among destinations writes one transition per destination, each with its guard and its
`send_message`; there is no destination taken from data. A definition with `meta.type` or
`target_ref` is refused, by its path.

The `{"$ref": "..."}` values are resolved at execution time, in `send_message` and
`emit_internal_event`, the actions that take one. A `$ref` is a dot path rooted at `input.`, `state.`
or `event.` (another root, such as a typo `inptu.x`, is refused at load); an object whose `$ref`
holds `#`, `/` or `:` (a JSON Schema's `"$ref": "#/definitions/item"`) is a literal, like
everything else.

A `$ref` to a field that is not there fails the action, and with it, by its default `on_error`, the
instance; a field that holds `null` gives `null`. For a field that may be missing, give a default:
`{"$ref": "event.context", "default": {}}` takes the literal `{}` when there is no context, or when
it is `null`. A `$ref` holds only `$ref` and `default`, and its default is a literal: anything else
is refused at load (a typo such as `defualt` included), by its path.

`meta.context` is resolved the same way, and goes on the message's `meta.context`: to ask an agent for
a structured answer, `"context": {"response_envelope": {...}}`. An agent answers with the request's
`msg`, and its error as `<msg>.error`, which needs a transition of its own.

### `schedule_timer` — schedule a timer through SY.timer

```json
{
  "type": "schedule_timer",
  "timer_key": "customer_timeout",
  "fire_in": "24h",
  "missed_policy": "fire"
}
```

Minimum duration: 60 seconds. `missed_policy` is SY.timer's, for a fire SY.timer itself is late
with: `fire` (it delivers the late fire), `drop` (it does not), `fire_if_within` (it delivers it
within `missed_within_ms`). The WF's own recovery does not apply it: a WF that restarts gives its
instance a fire SY.timer lost, or a past-due timer's fire, whatever the policy, `drop` included.

### `cancel_timer` — cancel a pending timer

```json
{ "type": "cancel_timer", "timer_key": "customer_timeout" }
```

### `reschedule_timer` — reschedule an existing timer

```json
{ "type": "reschedule_timer", "timer_key": "sla_check", "fire_in": "30m" }
```

### `set_variable` — save a value in the instance state

```json
{ "type": "set_variable", "name": "invoice_id", "value": "event.payload.invoice_id" }
```

The `value` is a CEL expression, written as a JSON string. Examples: `"now()"`, `"'pending'"`,
`"42"`, `"state.retry_count + 1"`, `"event.payload.result"`,
`"'Customer %s'.format([input.customer_id])"`. A JSON number or boolean (`42`, `true`) is refused
at load: it is no CEL expression.
The state keeps plain JSON: a list, or a map with string keys, is kept as such; a value JSON cannot
hold (bytes, a map with other keys) fails the action.

### `emit_internal_event` — queue an event for the same instance

```json
{ "type": "emit_internal_event", "meta": { "msg": "INTERNAL_DONE" }, "payload": { "result": { "$ref": "state.result" } } }
```

The event is committed with the transition and runs right after it, on the same instance, as a
`user` event named by `meta.msg`; at most 100 of them may follow one event.

---

## 5. Structure of a workflow

```
workflow
├── wf_schema_version    "1"
├── workflow_type         workflow name
├── description           free text
├── input_schema          JSON Schema of the event that creates instances
├── initial_state         name of the state where instances start
├── terminal_states       list of states that end the instance
└── states[]
    ├── name              unique state name
    ├── description       free text
    ├── entry_actions[]   actions on ENTERING the state
    ├── exit_actions[]    actions on LEAVING the state
    └── transitions[]     transition rules
        ├── event_match   { "msg": "MESSAGE_NAME" }   (msg only: no type)
        ├── guard         CEL expression → true/false
        ├── target_state  name of the target state
        └── actions[]     actions when taking this transition
```

Execution order in a transition:
```
exit_actions of the current state
  → actions of the transition
    → entry_actions of the target state
```

Transitions are evaluated in declaration order. The first one whose `event_match` matches and whose `guard` returns `true` is the one taken.

---

## 6. Typical development cycle

```
1. Write/edit the workflow JSON

2. Validate:
   curl -X POST .../wf-rules/compile -d '{workflow_name, definition}'
   → fix CEL or structure errors

3. Deploy:
   curl -X POST .../wf-rules/apply -d '{workflow_name, tenant_id, auto_spawn: true}'
   → the WF node starts

4. Test by sending messages to the WF node

5. Iterate: edit JSON → compile_apply (steps 2+3 together)
   curl -X POST .../wf-rules -d '{workflow_name, definition, tenant_id, auto_spawn: true}'

6. If something goes wrong: rollback
   curl -X POST .../wf-rules/rollback -d '{workflow_name}'

7. When you finish testing: delete
   curl -X POST .../wf-rules/delete -d '{workflow_name, force: true}'
```

---

## 7. Endpoints (summary)

| Method | Path | Action |
|---|---|---|
| `POST` | `/hives/{hive}/wf-rules/compile` | Validate and stage |
| `POST` | `/hives/{hive}/wf-rules/apply` | Promote staged → current, restart/spawn |
| `POST` | `/hives/{hive}/wf-rules` | Compile + apply in one step |
| `POST` | `/hives/{hive}/wf-rules/rollback` | Restore the previous version |
| `POST` | `/hives/{hive}/wf-rules/delete` | Delete a workflow |
| `GET` | `/hives/{hive}/wf-rules?workflow_name=X` | See the current definition |
| `GET` | `/hives/{hive}/wf-rules/status?workflow_name=X` | See the state + node info |
| `GET` | `/hives/{hive}/wf-rules` | List all workflows |

---

## 8. Operational notes

**In-flight instances on update:** when a new version is applied, the instances that were already running keep using the definition they were born with: each records its definition's hash, and the WF node keeps every definition a running instance uses (and drops it when the last one ends). Only new instances use the updated definition. This is transparent — nothing special needs to be done. An instance whose definition no longer loads (one written before 0.1.90 with `meta.type`, say) fails at the node's start.

**Timers:** all the workflow's timers go through `SY.timer` of the WF's hive, and the WF takes a `TIMER_FIRED` only from it, and only for a timer it scheduled itself (`owner_l2_name`): a fire of a timer another node scheduled is dropped. Minimum duration 60 seconds. If the WF node restarts, it recovers its pending timers automatically: it fires the ones it missed in the order they fire, reading each again first (a fire that cancels a later timer keeps that one from firing), and a timer that fired before the restart does not fire again. If `SY.timer` does not answer at start, the node starts anyway, reports itself `DEGRADED` (`timers_not_reconciled`), and reconciles its timers when `SY.timer` answers; a timer scheduled in the meantime that SY.timer never got is scheduled again then.

**Messages a WF takes:** a `user` message, which starts an instance or reaches the running instance on its thread, and a `TIMER_FIRED`. Any other `system` message is control, never a workflow event: it is dropped and never answered. `event_match` matches on `msg` only (a definition with `event_match.type` is refused).

**Downstream idempotency:** the WF delivers an event at least once: it saves the event before running its transition, and a crash before the transition commits runs it again at the next start, its creation included. The nodes that receive messages from a WF (AI, IO) must be idempotent with respect to `meta.trace_id`: a repeated send keeps its `trace_id`. Each start counts the attempts of an event it runs again: one already run at 3 starts (an event that keeps taking the node down) is not run again, its instance fails (`replay_exhausted`), and the node goes on.

**Replies, errors and late answers:** a reply (a `*_RESPONSE`, an `INVALID_INPUT`, an agent's `<msg>.error`) reaches only the instance whose thread it carries; it never starts an instance and is never answered. An answer on the thread of an instance of this node that ended (its timer fired first, say) is a late answer: dropped and never answered, instead of a new instance that would ask the agent again. A message that starts no instance (its payload fails `input_schema`) gets an `INVALID_INPUT` only when it is a named request.

**Limits enforced:** at most 100 internal events may follow one event; past that the instance fails (`internal_event_cap`). A panic in a transition fails that instance (`transition_panic`), never the node. A guard stops at 10 ms (false), a `set_variable` at its cost limit (the action fails), and an event at 3 starts (`replay_exhausted`). The store commits with `synchronous=FULL`, so a transition whose messages went out is on disk.

**Response correlation:** when the WF sends a `send_message`, it sets `meta.thread_id = instance_id`. The nodes that answer must copy that `thread_id` into their response so the WF correlates it with the right instance.

**Cancel an in-flight instance:** the WF node's instance verbs (`WF_CANCEL_INSTANCE`,
`WF_LIST_INSTANCES`, `WF_GET_INSTANCE`, `WF_GET_CODE`, `WF_HELP`) answer only `SY.admin@motherbee`
and `SY.wf-rules` of the WF's hive; anyone else gets `UNAUTHORIZED`. SY.admin has no instance
actions yet, so a cancel goes as a message from SY.admin, whose answer is not shown:
```bash
curl -sS -X POST "$BASE/hives/$HIVE/nodes/WF.invoice@$HIVE/messages" \
  -H "Content-Type: application/json" \
  -d '{"msg_type": "system", "msg": "WF_CANCEL_INSTANCE", "payload": {"instance_id": "wfi:...", "reason": "operator"}}'
```
The instance id (`wfi:...`) is the `meta.thread_id` of every message the instance sends, and the
node's journal logs it when the instance starts:
```bash
journalctl -u fluxbee-node-WF.invoice-motherbee | grep 'create accepted'
# wf: create accepted instance_id=wfi:550e8400-... workflow_type=invoice initial_state=validating_data trace_id=... src_l2="AI.billing@motherbee"
```

**See active instances:**
```bash
curl -sS "$BASE/hives/$HIVE/wf-rules/status?workflow_name=invoice" | jq .payload.wf_node
```
