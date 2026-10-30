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

The workflow of the spec's case study (`docs/spec-systemnode-wf-v1_1.md` §19). The billing
system asks for an invoice through its IO node, `IO.api.billing@motherbee`, the only node in its
`start_from`. It asks an agent, with a menu, whether the customer's data is complete; creates the
invoice through `IO.quickbooks`; and asks the billing agent, with a menu, to deliver it. When data
is missing, it tells the billing agent and ends: the agent asks the customer, who answers it, never
a WF instance, and the billing system asks for the invoice again once it has the data. Its catch
fails the request when an agent finds that no option fits (`none`).

A WF that sits at an IO node's exit and hands a conversation among agents is the example package
`go/nodes/wf/examples/packages/wf.sales.invoice` (`docs/wf-orchestration-v1.md` §5.2 and §5.3): it
uses `forward`, `keep_message`, the WF's block, menus and a timer time from an agent's answer,
internal transitions and a catch (handbook §4.2), and asks the supervisors through their IO node,
`IO.slack.supervisores`, when sales chooses `consult`.

```json
{
  "wf_schema_version": "1",
  "workflow_type": "invoice",
  "description": "Issues an invoice the billing system asks for: has the customer's data checked, creates the invoice and has the billing agent deliver it; tells the billing agent what is missing.",
  "start_from": ["IO.api.billing@motherbee"],
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
  "catch": [
    {
      "event_match": {},
      "guard": "event.context.evaluation.option == 'none'",
      "target_state": "failed",
      "actions": [
        { "type": "set_variable", "name": "failure_reason", "value": "'no option fits: ' + event.context.evaluation.params.reason" }
      ]
    }
  ],
  "states": [
    {
      "name": "validating_data",
      "description": "Asking AI.billing-validator, with a menu, whether the customer's data is complete.",
      "entry_actions": [
        {
          "type": "set_variable",
          "name": "validation_request",
          "value": "'Customer %s asks for an invoice of these items: %s.'.format([input.customer_id, input.items])"
        },
        {
          "type": "send_message",
          "target": "AI.billing-validator@motherbee",
          "meta": { "msg": "VALIDATE_INVOICE_DATA" },
          "menu": {
            "help": "The billing system asks for an invoice, which is issued only when the customer's data is complete.",
            "options": [
              { "name": "complete", "description": "The customer's data is complete." },
              {
                "name": "missing",
                "description": "The invoice needs data the customer has not given.",
                "params": { "missing": { "type": "string", "description": "What is missing." } }
              }
            ]
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
          "guard": "event.context.evaluation.option == 'complete'",
          "target_state": "creating_invoice",
          "actions": [
            { "type": "set_variable", "name": "validated_at", "value": "now()" }
          ]
        },
        {
          "event_match": { "msg": "VALIDATE_INVOICE_DATA" },
          "guard": "event.context.evaluation.option == 'missing'",
          "target_state": "missing_data_requested",
          "actions": [
            {
              "type": "set_variable",
              "name": "missing_data_request",
              "value": "'The invoice for customer %s cannot be issued yet: it needs %s. Ask the customer for it.'.format([input.customer_id, event.context.evaluation.params.missing])"
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
      "description": "Terminal: tells the billing agent what is missing, and ends. The agent asks the customer, who answers it, never this instance; the billing system asks for the invoice again once it has the data.",
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
      "description": "Asking the billing agent, with a menu, to send the invoice to the customer.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing@motherbee",
          "meta": { "msg": "DELIVER_INVOICE" },
          "menu": {
            "options": [
              { "name": "delivered", "description": "You sent the customer the invoice." },
              {
                "name": "undelivered",
                "description": "You could not send it.",
                "params": { "why": { "type": "string" } }
              }
            ]
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
          "guard": "event.context.evaluation.option == 'delivered'",
          "target_state": "completed",
          "actions": []
        },
        {
          "event_match": { "msg": "DELIVER_INVOICE" },
          "guard": "event.context.evaluation.option == 'undelivered'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'not delivered: ' + event.context.evaluation.params.why" }
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
        "guard_count": 12,
        "state_count": 7,
        "action_count": 23
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
false. What it reads of an agent's evaluation is the exception (handbook §4.2.4): an option or a param
that no menu of the definition offers (`event.context.evaluation.option == 'complet'`) is refused,
by its path.

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

## 3. Writing the definition

The reference for writing a definition is one chapter of Fluxbee's handbook, which people and Archi
share: [§4.2, WF nodes and workflows](archi/handbook_fluxbee.md#42-wf-nodes-and-workflows). It
covers the definition and its states (§4.2.1), what a guard reads and calls (§4.2.2), every action
(§4.2.3), the menu that asks an agent to choose (§4.2.4), the catch, the default and the cap on
agent calls (§4.2.5), and patterns (§4.2.6). This guide covers deploying and operating a WF.

---

## 4. Typical development cycle

```
1. Write/edit the workflow JSON

2. Validate:
   curl -X POST .../wf-rules/compile -d '{workflow_name, definition}'
   → fix CEL or structure errors

3. Deploy:
   curl -X POST .../wf-rules/apply -d '{workflow_name, tenant_id, auto_spawn: true}'
   → the WF node starts

4. Test by sending messages to the WF node, from a node its start_from names

5. Iterate: edit JSON → compile_apply (steps 2+3 together)
   curl -X POST .../wf-rules -d '{workflow_name, definition, tenant_id, auto_spawn: true}'

6. If something goes wrong: rollback
   curl -X POST .../wf-rules/rollback -d '{workflow_name}'

7. When you finish testing: delete
   curl -X POST .../wf-rules/delete -d '{workflow_name, force: true}'
```

---

## 5. Endpoints (summary)

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

## 6. Operational notes

**In-flight instances on update:** when a new version is applied, the instances that were already running keep using the definition they were born with: each records its definition's hash, and the WF node keeps every definition a running instance uses (and drops it when the last one ends). Only new instances use the updated definition. This is transparent — nothing special needs to be done. An instance whose definition no longer loads (one written before 0.1.90 with `meta.type`, say) fails at the node's start.

**Timers:** all the workflow's timers go through `SY.timer` of the WF's hive, and the WF takes a `TIMER_FIRED` only from it, and only for a timer it scheduled itself (`owner_l2_name`): a fire of a timer another node scheduled is dropped. Minimum duration 60 seconds. If the WF node restarts, it recovers its pending timers automatically: it fires the ones it missed in the order they fire, reading each again first (a fire that cancels a later timer keeps that one from firing), and a timer that fired before the restart does not fire again. If `SY.timer` does not answer at start, the node starts anyway, reports itself `DEGRADED` (`timers_not_reconciled`), and reconciles its timers when `SY.timer` answers; a timer scheduled in the meantime that SY.timer never got is scheduled again then.

**Messages a WF takes:** a `user` message, which starts an instance or reaches the running instance on its thread, and a `TIMER_FIRED`. Any other `system` message is control, never a workflow event: it is dropped and never answered. `event_match` matches on `msg`, `src` or both (a definition with `event_match.type` is refused).

**One instance per conversation, and only the nodes involved:** an instance lives on the thread of the message that started it, and a message on that thread reaches it, but only from a node the definition names: one in `start_from`, in an `event_match.src`, or a target of a `send_message` or a `forward`, the catch's included (and SY.timer for its own timers). Anyone else is dropped, never answered. A message from a `start_from` node on a thread no instance holds starts a new instance there, as `input_schema` allows: a person who writes again after their instance ended starts a new one on the same conversation. `start_from` names IO nodes and WFs, never an agent (which only answers) nor a `SY.*` or `RT.*` node.

**A message the state has no transition for** goes to the catch (handbook §4.2.5), and when the catch takes nothing either it is logged (`unhandled event ... in state ...`, in the instance's log and the node's) and dropped: it is never queued for a later state. A definition that may get a message early handles it where it can arrive, with an internal transition (one without `target_state`) when it must not leave the state.

**Downstream idempotency:** the WF delivers an event at least once: it saves the event before running its transition, and a crash before the transition commits runs it again at the next start, its creation included. The nodes that receive messages from a WF (AI, IO) must be idempotent with respect to `meta.trace_id`: a repeated send keeps its `trace_id`. Each start counts the attempts of an event it runs again: one already run at 3 starts (an event that keeps taking the node down) is not run again, its instance fails (`replay_exhausted`), and the node goes on.

**Replies, errors and late answers:** a reply (a `*_RESPONSE`, an `INVALID_INPUT`, an agent's `<msg>.error`) reaches only the instance whose thread it carries; it never starts an instance and is never answered. A late answer from a node outside `start_from` (an agent's, after its instance ended: its timer fired first, say) is dropped, never answered, only while no instance runs on its thread, instead of a new instance that would ask the agent again; once the conversation's next instance runs there, the late answer reaches it, as any message on its thread from a node the definition names. A message that starts no instance (its payload fails `input_schema`) gets an `INVALID_INPUT` only when it is a named request.

**The cap on agent calls** is in the handbook (§4.2.5). Operating it: a send past the cap is in the instance's log as not sent, with the agent it was for; the count lives in memory, so a restart starts it again.

**Limits enforced:** at most 100 internal events may follow one event; past that the instance fails (`internal_event_cap`). A panic in a transition fails that instance (`transition_panic`), never the node. A guard stops at 10 ms (false), a `set_variable` at its cost limit (the action fails), and an event at 3 starts (`replay_exhausted`). The store commits with `synchronous=FULL`, so a transition whose messages went out is on disk.

**Response correlation:** every message an instance sends carries the instance's thread in `meta.thread_id`: the thread of the message that started it, or the `instance_id` when that message had none. The nodes that answer must copy that `thread_id` into their response so the WF correlates it with the right instance.

**Cancel an in-flight instance:** the WF node's instance verbs (`WF_CANCEL_INSTANCE`,
`WF_LIST_INSTANCES`, `WF_GET_INSTANCE`, `WF_GET_CODE`, `WF_HELP`) answer only `SY.admin@motherbee`
and `SY.wf-rules` of the WF's hive; anyone else gets `UNAUTHORIZED`. SY.admin has no instance
actions yet, so a cancel goes as a message from SY.admin, whose answer is not shown:
```bash
curl -sS -X POST "$BASE/hives/$HIVE/nodes/WF.invoice@$HIVE/messages" \
  -H "Content-Type: application/json" \
  -d '{"msg_type": "system", "msg": "WF_CANCEL_INSTANCE", "payload": {"instance_id": "wfi:...", "reason": "operator"}}'
```
The node's journal logs the instance id (`wfi:...`) and its thread when the instance starts:
```bash
journalctl -u fluxbee-node-WF.invoice-motherbee | grep 'create accepted'
# wf: create accepted instance_id=wfi:550e8400-... thread_id=conv-7c1e workflow_type=invoice initial_state=validating_data trace_id=... src_l2="AI.billing@motherbee"
```

**See active instances:**
```bash
curl -sS "$BASE/hives/$HIVE/wf-rules/status?workflow_name=invoice" | jq .payload.wf_node
```
