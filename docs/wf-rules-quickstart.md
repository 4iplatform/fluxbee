# SY.wf-rules — Quick Start Guide

A practical guide to define, deploy and operate workflows in Fluxbee.

**Prerequisite:** the `wf.engine` base runtime must be published on the hive.

```bash
# publish wf.engine (only once per install)
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

```json
{
  "wf_schema_version": "1",
  "workflow_type": "invoice",
  "description": "Issues an invoice, collects missing data if needed, delivers to customer.",
  "input_schema": {
    "type": "object",
    "required": ["customer_id", "items", "originator_channel"],
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
      "currency": { "type": "string" },
      "originator_channel": { "type": "string" },
      "originator_ilk": { "type": "string" }
    }
  },
  "initial_state": "validating_data",
  "terminal_states": ["completed", "failed", "cancelled"],
  "states": [
    {
      "name": "validating_data",
      "description": "Asking AI.billing-validator if the customer data is complete.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing-validator@motherbee",
          "meta": { "msg": "VALIDATE_INVOICE_DATA" },
          "payload": {
            "customer_id": { "$ref": "input.customer_id" },
            "items": { "$ref": "input.items" },
            "currency": { "$ref": "input.currency" }
          }
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
          "event_match": { "msg": "VALIDATE_INVOICE_DATA_RESPONSE" },
          "guard": "event.payload.complete == true",
          "target_state": "creating_invoice",
          "actions": [
            { "type": "set_variable", "name": "validated_at", "value": "now()" }
          ]
        },
        {
          "event_match": { "msg": "VALIDATE_INVOICE_DATA_RESPONSE" },
          "guard": "event.payload.complete == false",
          "target_state": "requesting_missing_data",
          "actions": [
            { "type": "set_variable", "name": "missing_fields", "value": "event.payload.missing" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.user_payload.timer_key == 'validation_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'validator timeout'" }
          ]
        }
      ]
    },
    {
      "name": "requesting_missing_data",
      "description": "Asking the customer for missing fields.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing-responder@motherbee",
          "meta": { "msg": "REQUEST_MISSING_INVOICE_DATA" },
          "payload": {
            "originator_channel": { "$ref": "input.originator_channel" },
            "originator_ilk": { "$ref": "input.originator_ilk" },
            "missing_fields": { "$ref": "state.missing_fields" }
          }
        },
        {
          "type": "schedule_timer",
          "timer_key": "customer_response_timeout",
          "fire_in": "24h",
          "missed_policy": "fire"
        }
      ],
      "exit_actions": [
        { "type": "cancel_timer", "timer_key": "customer_response_timeout" }
      ],
      "transitions": [
        {
          "event_match": { "msg": "CUSTOMER_RESPONSE" },
          "guard": "has(event.payload.missing_data)",
          "target_state": "validating_data",
          "actions": [
            { "type": "set_variable", "name": "items", "value": "event.payload.missing_data.items" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.user_payload.timer_key == 'customer_response_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'customer did not respond'" }
          ]
        }
      ]
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
            "currency": { "$ref": "input.currency" }
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
            { "type": "set_variable", "name": "invoice_link", "value": "event.payload.link" }
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
          "guard": "event.user_payload.timer_key == 'qb_creation_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'quickbooks timeout'" }
          ]
        }
      ]
    },
    {
      "name": "delivering_to_customer",
      "description": "Asking AI.billing-responder to send the invoice to the customer.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing-responder@motherbee",
          "meta": { "msg": "DELIVER_INVOICE_TO_CUSTOMER" },
          "payload": {
            "originator_channel": { "$ref": "input.originator_channel" },
            "originator_ilk": { "$ref": "input.originator_ilk" },
            "invoice_id": { "$ref": "state.invoice_id" },
            "invoice_link": { "$ref": "state.invoice_link" }
          }
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
          "event_match": { "msg": "DELIVER_INVOICE_RESPONSE" },
          "guard": "event.payload.delivered == true",
          "target_state": "completed",
          "actions": []
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.user_payload.timer_key == 'delivery_timeout'",
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
      "description": "Terminal failure. Check variable 'failure_reason'.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "cancelled",
      "description": "Cancelled by operator via WF_CANCEL_INSTANCE.",
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
  "ok": true,
  "state": "configured",
  "effective_config": {
    "staged": {
      "version": 1,
      "hash": "sha256:...",
      "guard_count": 8,
      "state_count": 7
    }
  }
}
```

If there are validation errors (an invalid CEL guard, a referenced state that does not exist, etc.), the system returns the error with the exact location:

```json
{
  "ok": false,
  "error": {
    "code": "COMPILE_ERROR",
    "detail": "states[0].transitions[0].guard: undeclared reference to 'event.payload.complette'"
  }
}
```

### 2.2 Apply (promote staged and deploy)

Promotes the validated definition from staged to current, generates the package, and spawns/restarts the WF node.

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules/apply" \
  -H "Content-Type: application/json" \
  -d "{\"workflow_name\":\"invoice\",\"tenant_id\":\"$TENANT_ID\",\"auto_spawn\":true}" | jq .
```

`auto_spawn: true` makes the node `WF.invoice@motherbee` be created automatically if it does not exist. If it already exists, it is restarted with the new version. In the first case, `tenant_id` is required.

Expected response:
```json
{
  "ok": true,
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
}
```

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

Response:
```json
{
  "status": "ok",
  "workflow_name": "invoice",
  "current_version": 2,
  "current_hash": "sha256:...",
  "wf_node": {
    "node_name": "WF.invoice@motherbee",
    "running": true,
    "active_instances": 3
  }
}
```

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

Built-in function: `now()` returns a UTC timestamp in milliseconds (the process's local time).

### Common patterns

**Check a boolean field in the response:**
```
event.payload.ok == true
```

**Check that a field exists:**
```
has(event.payload.missing_data)
```

**Match a specific timer:**
```
event.user_payload.timer_key == 'validation_timeout'
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

The `{"$ref": "..."}` values are resolved at execution time. The three valid roots are `input`, `state`, `event`. Everything else is literal.

### `schedule_timer` — schedule a timer through SY.timer

```json
{
  "type": "schedule_timer",
  "timer_key": "customer_timeout",
  "fire_in": "24h",
  "missed_policy": "fire"
}
```

Minimum duration: 60 seconds. `missed_policy` options: `fire` (fires on recovery), `drop` (ignores it), `fire_if_within` (fires if the delay is within a margin).

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

The `value` is a CEL expression. Examples: `"now()"`, `"'pending'"`, `"state.retry_count + 1"`, `"event.payload.result"`.

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
        ├── event_match   { "msg": "MESSAGE_NAME" }
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

**In-flight instances on update:** when a new version is applied, the instances that were already running keep using the definition they were born with. Only new instances use the updated definition. This is transparent — nothing special needs to be done.

**Timers:** all the workflow's timers go through `SY.timer`. Minimum duration 60 seconds. If the WF node restarts, it recovers its pending timers automatically.

**Downstream idempotency:** the nodes that receive messages from a WF (AI, IO) must be idempotent with respect to `meta.trace_id`. In case of a WF crash and recovery, the actions can run again.

**Response correlation:** when the WF sends a `send_message`, it sets `meta.thread_id = instance_id`. The nodes that answer must copy that `thread_id` into their response so the WF correlates it with the right instance.

**Cancel an in-flight instance:** it is done directly on the WF node, not on sy.wf-rules:
```bash
# send WF_CANCEL_INSTANCE to the WF node directly via admin
curl -sS -X POST "$BASE/hives/$HIVE/nodes/WF.invoice@$HIVE/command" \
  -H "Content-Type: application/json" \
  -d '{"action": "cancel_instance", "instance_id": "wfi:..."}'
```

**See active instances:**
```bash
curl -sS "$BASE/hives/$HIVE/wf-rules/status?workflow_name=invoice" | jq .wf_node
```
