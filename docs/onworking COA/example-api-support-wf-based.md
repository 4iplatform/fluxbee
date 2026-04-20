# Solution Example: API Support with Slack Mirror (WF-based)

Three-layer example: messages arrive via API, a WF orchestrates the flow deterministically — mirrors input to Slack, sends to AI for analysis, mirrors response to Slack, responds to API.

---

## Layer 1 — Solution Manifest

```json
{
  "manifest_version": "0.1",
  "kind": "solution_manifest",

  "tenant": {
    "id": "tnt:acme-corp",
    "mode": "existing",
    "owner_ilk": "ilk:admin-acme"
  },

  "solution": {
    "name": "api-support",
    "description": "External API receives support messages. AI analyzes and responds. All traffic (inbound and outbound) is mirrored to a Slack channel for visibility. A workflow orchestrates the full flow deterministically.",
    "hive": "motherbee"
  },

  "components": {

    "io_nodes": [
      {
        "name": "IO.api.support",
        "description": "HTTP API endpoint that receives support messages from external systems. Each message contains a text body and metadata (source system, ticket ID, priority). The node translates incoming HTTP requests into Fluxbee messages and routes responses back to the caller.",
        "type": "api_gateway",
        "protocol": "HTTP REST",
        "direction": "inbound + response",
        "expected_payload": {
          "text": "string, the support message body",
          "source_system": "string, identifier of the calling system",
          "ticket_id": "string, external ticket reference",
          "priority": "string, one of: low, normal, high, urgent"
        },
        "response_format": {
          "analysis": "string, AI analysis of the issue",
          "suggested_action": "string, recommended next step",
          "confidence": "number, 0-1"
        }
      },
      {
        "name": "IO.slack",
        "description": "Sends formatted messages to a Slack channel. Used to mirror all support traffic (both the incoming request and the AI response) for team visibility.",
        "type": "slack_webhook",
        "protocol": "Slack Incoming Webhook",
        "direction": "outbound only",
        "channel": "#support-mirror",
        "message_format": "Formatted card with: direction (inbound/outbound), source, ticket_id, priority, message body"
      }
    ],

    "ai_nodes": [
      {
        "name": "AI.support",
        "description": "Analyzes incoming support messages. Classifies the issue, assesses urgency, and generates a structured response. Stateless — receives a question, returns an answer. Does not know about Slack, does not manage routing. The workflow handles all orchestration.",
        "behavior": "Receive support message → classify (billing, technical, account, other) → assess urgency vs stated priority → respond with structured JSON: {analysis, suggested_action, confidence}",
        "does_not": [
          "Route messages to other nodes",
          "Mirror to Slack (WF handles this)",
          "Escalate to humans",
          "Store conversation history"
        ]
      }
    ],

    "workflows": [
      {
        "name": "WF.support-mirror",
        "description": "Orchestrates the support message flow. Deterministic process: receive message from API → mirror input to Slack → send to AI for analysis → wait for AI response → mirror response to Slack → send response back to API.",
        "trigger": "SUPPORT_REQUEST from IO.api.support",
        "flow": [
          "1. Receive support request from IO.api.support",
          "2. Mirror the inbound message to IO.slack (team sees what came in)",
          "3. Forward the message to AI.support for analysis",
          "4. Wait for AI.support response (timeout: 2 minutes)",
          "5. Mirror the AI response to IO.slack (team sees what went out)",
          "6. Send the structured response back to IO.api.support",
          "7. Complete"
        ],
        "timeout": "2 minutes for AI response, then fail",
        "failure_mode": "If AI doesn't respond in time, send error response to API and mirror the failure to Slack"
      }
    ],

    "routing": {
      "description": "Four routing rules for the support flow",
      "rules": [
        {
          "description": "Inbound: API support messages go to WF.support-mirror",
          "from": "IO.api.support",
          "to": "WF.support-mirror",
          "message_type": "SUPPORT_REQUEST"
        },
        {
          "description": "WF sends analysis requests to AI.support",
          "from": "WF.support-mirror",
          "to": "AI.support",
          "message_type": "ANALYZE_SUPPORT"
        },
        {
          "description": "WF sends mirror messages to IO.slack",
          "from": "WF.support-mirror",
          "to": "IO.slack",
          "message_type": "SLACK_MIRROR"
        },
        {
          "description": "WF sends responses back to IO.api.support",
          "from": "WF.support-mirror",
          "to": "IO.api.support",
          "message_type": "SUPPORT_RESPONSE"
        }
      ]
    },

    "identity": {
      "description": "No new identity requirements. All nodes register as system ILKs under the existing tenant."
    },

    "channels": {
      "description": "No end-user channels. API endpoint is the entry point. Slack is output-only."
    }
  }
}
```

---

## Layer 2 — Execution Plan

```json
{
  "plan_version": "0.1",
  "kind": "executor_plan",

  "metadata": {
    "name": "api-support-deploy",
    "target_hive": "motherbee",
    "tenant_id": "tnt:acme-corp",
    "source_manifest": "api-support",
    "generated_by": "implementor",
    "generated_at": "2026-04-16T10:00:00Z"
  },

  "execution": {
    "strict": true,
    "stop_on_error": true,
    "allow_help_lookup": true,

    "steps": [

      {
        "id": "s1",
        "action": "run_node",
        "args": {
          "node_name": "IO.api.support",
          "runtime": "io.api-gateway",
          "runtime_version": "current"
        },
        "executor_fill": ["hive", "tenant_id"],
        "manifest_ref": "components.io_nodes[0]",
        "rationale": "Spawn API gateway IO node for receiving external support messages"
      },

      {
        "id": "s2",
        "action": "run_node",
        "args": {
          "node_name": "AI.support",
          "runtime": "ai.chat",
          "runtime_version": "current"
        },
        "executor_fill": ["hive", "tenant_id"],
        "manifest_ref": "components.ai_nodes[0]",
        "rationale": "Spawn AI analysis node — stateless, receives questions, returns structured answers"
      },

      {
        "id": "s3",
        "action": "run_node",
        "args": {
          "node_name": "IO.slack",
          "runtime": "io.slack-webhook",
          "runtime_version": "current"
        },
        "executor_fill": ["hive", "tenant_id"],
        "manifest_ref": "components.io_nodes[1]",
        "rationale": "Spawn Slack webhook IO node for traffic mirroring"
      },

      {
        "id": "s4",
        "action": "set_node_config",
        "args": {
          "node_name": "AI.support",
          "config": {
            "system_prompt": "You are a support message analyzer. For each message you receive, produce a JSON response with exactly these fields:\n\n- analysis: clear description of the issue and classification (billing, technical, account, or other)\n- suggested_action: specific next step recommendation\n- confidence: number 0-1, how confident you are in the classification\n\nRespond ONLY with the JSON object. No preamble, no explanation outside the JSON."
          }
        },
        "executor_fill": ["hive"],
        "manifest_ref": "components.ai_nodes[0]",
        "rationale": "Configure AI.support with its analysis prompt. The AI is simple — it receives a message, returns structured JSON. It does not route, mirror, or orchestrate."
      },

      {
        "id": "s5",
        "action": "set_node_config",
        "args": {
          "node_name": "IO.slack",
          "config": {
            "webhook_url": "PLACEHOLDER_SLACK_WEBHOOK_URL",
            "channel": "#support-mirror",
            "message_template": {
              "blocks": [
                {"type": "header", "text": "Support Traffic — {{direction}}"},
                {"type": "section", "fields": [
                  {"label": "Source", "value": "{{source_system}}"},
                  {"label": "Ticket", "value": "{{ticket_id}}"},
                  {"label": "Priority", "value": "{{priority}}"}
                ]},
                {"type": "section", "text": "{{body}}"}
              ]
            }
          }
        },
        "executor_fill": ["hive"],
        "manifest_ref": "components.io_nodes[1]",
        "rationale": "Configure IO.slack with webhook and formatting. PLACEHOLDER: operator must provide real Slack webhook URL."
      },

      {
        "id": "s6",
        "action": "wf_rules_compile_apply",
        "args": {
          "workflow_name": "support-mirror",
          "auto_spawn": true,
          "definition": {
            "wf_schema_version": "1",
            "workflow_type": "support-mirror",
            "description": "Orchestrates support message flow: mirror input to Slack, analyze via AI, mirror response to Slack, respond to API.",
            "input_schema": {
              "type": "object",
              "required": ["text", "source_system", "ticket_id"],
              "properties": {
                "text": { "type": "string" },
                "source_system": { "type": "string" },
                "ticket_id": { "type": "string" },
                "priority": { "type": "string" },
                "originator_l2_name": { "type": "string" }
              }
            },
            "initial_state": "mirroring_input",
            "terminal_states": ["completed", "failed"],
            "states": [
              {
                "name": "mirroring_input",
                "description": "Mirror the inbound message to Slack, then forward to AI for analysis.",
                "entry_actions": [
                  {
                    "type": "send_message",
                    "target": "IO.slack@motherbee",
                    "meta": { "msg": "SLACK_MIRROR" },
                    "payload": {
                      "direction": "inbound",
                      "source_system": { "$ref": "input.source_system" },
                      "ticket_id": { "$ref": "input.ticket_id" },
                      "priority": { "$ref": "input.priority" },
                      "body": { "$ref": "input.text" }
                    }
                  },
                  {
                    "type": "send_message",
                    "target": "AI.support@motherbee",
                    "meta": { "msg": "ANALYZE_SUPPORT" },
                    "payload": {
                      "text": { "$ref": "input.text" },
                      "source_system": { "$ref": "input.source_system" },
                      "ticket_id": { "$ref": "input.ticket_id" },
                      "priority": { "$ref": "input.priority" }
                    }
                  },
                  {
                    "type": "schedule_timer",
                    "timer_key": "ai_response_timeout",
                    "fire_in": "2m",
                    "missed_policy": "fire"
                  }
                ],
                "exit_actions": [
                  { "type": "cancel_timer", "timer_key": "ai_response_timeout" }
                ],
                "transitions": [
                  {
                    "event_match": { "msg": "ANALYZE_SUPPORT_RESPONSE" },
                    "guard": "has(event.payload.analysis)",
                    "target_state": "mirroring_response",
                    "actions": [
                      { "type": "set_variable", "name": "analysis", "value": "event.payload.analysis" },
                      { "type": "set_variable", "name": "suggested_action", "value": "event.payload.suggested_action" },
                      { "type": "set_variable", "name": "confidence", "value": "event.payload.confidence" }
                    ]
                  },
                  {
                    "event_match": { "msg": "TIMER_FIRED" },
                    "guard": "event.user_payload.timer_key == 'ai_response_timeout'",
                    "target_state": "failed",
                    "actions": [
                      { "type": "set_variable", "name": "failure_reason", "value": "'AI response timeout after 2 minutes'" }
                    ]
                  }
                ]
              },
              {
                "name": "mirroring_response",
                "description": "Mirror the AI response to Slack and send it back to the API caller.",
                "entry_actions": [
                  {
                    "type": "send_message",
                    "target": "IO.slack@motherbee",
                    "meta": { "msg": "SLACK_MIRROR" },
                    "payload": {
                      "direction": "outbound",
                      "source_system": { "$ref": "input.source_system" },
                      "ticket_id": { "$ref": "input.ticket_id" },
                      "priority": { "$ref": "input.priority" },
                      "body": { "$ref": "state.analysis" }
                    }
                  },
                  {
                    "type": "send_message",
                    "target": "IO.api.support@motherbee",
                    "meta": { "msg": "SUPPORT_RESPONSE" },
                    "payload": {
                      "ticket_id": { "$ref": "input.ticket_id" },
                      "analysis": { "$ref": "state.analysis" },
                      "suggested_action": { "$ref": "state.suggested_action" },
                      "confidence": { "$ref": "state.confidence" }
                    }
                  },
                  {
                    "type": "emit_internal_event",
                    "meta": { "msg": "INTERNAL_COMPLETE" },
                    "payload": {
                      "ticket_id": { "$ref": "input.ticket_id" }
                    }
                  }
                ],
                "exit_actions": [],
                "transitions": [
                  {
                    "event_match": { "msg": "INTERNAL_COMPLETE" },
                    "guard": "true",
                    "target_state": "completed",
                    "actions": []
                  }
                ]
              },
              {
                "name": "completed",
                "description": "Support request processed and mirrored successfully.",
                "entry_actions": [],
                "exit_actions": [],
                "transitions": []
              },
              {
                "name": "failed",
                "description": "Support request failed. Variable 'failure_reason' has the cause.",
                "entry_actions": [
                  {
                    "type": "send_message",
                    "target": "IO.api.support@motherbee",
                    "meta": { "msg": "SUPPORT_RESPONSE" },
                    "payload": {
                      "ticket_id": { "$ref": "input.ticket_id" },
                      "error": { "$ref": "state.failure_reason" }
                    }
                  },
                  {
                    "type": "send_message",
                    "target": "IO.slack@motherbee",
                    "meta": { "msg": "SLACK_MIRROR" },
                    "payload": {
                      "direction": "error",
                      "source_system": { "$ref": "input.source_system" },
                      "ticket_id": { "$ref": "input.ticket_id" },
                      "body": { "$ref": "state.failure_reason" }
                    }
                  }
                ],
                "exit_actions": [],
                "transitions": []
              }
            ]
          }
        },
        "executor_fill": ["hive", "tenant_id"],
        "manifest_ref": "components.workflows[0]",
        "rationale": "Deploy the support-mirror workflow via sy.wf-rules. This is the orchestrator of the entire flow: mirror input → analyze → mirror response → respond. Uses auto_spawn to create WF.support-mirror node."
      },

      {
        "id": "s7",
        "action": "opa_compile_apply",
        "args": {
          "rego": "package fluxbee.routing\n\n# API support messages enter via WF.support-mirror\nallow {\n    input.routing.dst == \"WF.support-mirror@motherbee\"\n    input.meta.msg == \"SUPPORT_REQUEST\"\n    input.routing.src_l2_name == \"IO.api.support@motherbee\"\n}\n\n# WF sends analysis requests to AI.support\nallow {\n    input.routing.dst == \"AI.support@motherbee\"\n    input.meta.msg == \"ANALYZE_SUPPORT\"\n    input.routing.src_l2_name == \"WF.support-mirror@motherbee\"\n}\n\n# WF mirrors to IO.slack\nallow {\n    input.routing.dst == \"IO.slack@motherbee\"\n    input.meta.msg == \"SLACK_MIRROR\"\n    input.routing.src_l2_name == \"WF.support-mirror@motherbee\"\n}\n\n# WF responds back to IO.api.support\nallow {\n    input.routing.dst == \"IO.api.support@motherbee\"\n    input.meta.msg == \"SUPPORT_RESPONSE\"\n    input.routing.src_l2_name == \"WF.support-mirror@motherbee\"\n}",
          "entrypoint": "fluxbee/routing/allow"
        },
        "executor_fill": ["hive"],
        "manifest_ref": "components.routing",
        "rationale": "Deploy OPA rules for the 4 routing paths: IO.api→WF (trigger), WF→AI (analyze), WF→IO.slack (mirror), WF→IO.api (response). Note: AI.support never sends messages directly — it only responds to WF via thread_id correlation."
      }
    ]
  }
}
```

---

## Layer 3 — Audit Traceability

### Coverage matrix

| Manifest section | Steps | Status |
|---|---|---|
| `tenant` | — | Pre-existing, no steps needed |
| `solution` | all | Covered by deployment target |
| `io_nodes[0]` IO.api.support | s1 | Spawn |
| `io_nodes[1]` IO.slack | s3, s5 | Spawn + configure |
| `ai_nodes[0]` AI.support | s2, s4 | Spawn + configure |
| `workflows[0]` WF.support-mirror | s6 | Deploy via sy.wf-rules (compile_apply + auto_spawn) |
| `routing` | s7 | OPA rules cover all 4 declared routes |
| `identity` | — | No new identity, no steps needed |
| `channels` | — | Handled by IO node configs |

Every non-empty manifest section has corresponding steps. No steps exist without a manifest reference.

### Coherence check

- Spawn steps (s1-s3) come before config steps (s4-s5). Correct.
- WF deploy (s6) comes after AI and IO nodes are spawned. Correct — WF.support-mirror sends messages to AI.support and IO.slack, which must exist for messages to be deliverable (though not strictly required for WF compile, the order makes operational sense).
- OPA rules (s7) come last. Correct — all nodes referenced in the rules exist from prior steps.
- AI.support response correlation: the WF sends to AI.support with `meta.thread_id = instance_id`. AI.support's response preserves that `thread_id`, allowing the WF to correlate the response to the correct instance. This is the standard WF response correlation pattern (wf-v1.md §14.5). No special routing needed for the return path — AI just responds to whoever asked.

### Open items for operator

- **s5**: `webhook_url` is `PLACEHOLDER_SLACK_WEBHOOK_URL`. Operator must provide the real Slack webhook URL before execution.
- **s7**: OPA rules replace the entire current policy. If other solutions exist in this hive, their rules must be merged into this rego before applying.
- **Runtimes**: `io.api-gateway`, `ai.chat`, `io.slack-webhook` must be pre-installed in dist.

---

## Design notes

### Why WF instead of AI-managed routing

In the previous version of this example, AI.support was responsible for sending messages to both IO.api.support (response) and IO.slack (mirror). This worked but violated a clean principle: **AI nodes should analyze, not orchestrate.**

With WF.support-mirror:

- **AI.support is stateless and single-purpose.** It receives a question, returns a structured answer. It doesn't know about Slack, doesn't know about mirroring, doesn't manage routing. If you swap the AI for a different model or a different prompt, the flow doesn't break.

- **The mirror is deterministic.** The WF guarantees that every inbound message is mirrored before analysis, and every AI response is mirrored after analysis. If the AI crashes before responding, the inbound mirror already happened (Slack shows what came in). If the AI times out, the WF sends an error to both API and Slack. No ambiguity.

- **The flow is auditable.** Each WF instance tracks its state: `mirroring_input` → `mirroring_response` → `completed` (or `failed`). An operator can query `WF_LIST_INSTANCES` and see exactly where each support request is in the flow.

- **OPA rules are cleaner.** All routing flows through WF.support-mirror. AI.support never sends to IO nodes directly. The authorization surface is smaller: 4 rules instead of needing to trust AI.support to route correctly.

### The `mirroring_response` → `completed` transition

With `emit_internal_event` available in the WF runtime, this transition can now be modeled directly without timers or terminal-state tricks.

The intended pattern is:

1. enter `mirroring_response`
2. run the delivery `entry_actions` (mirror to Slack + respond to API)
3. emit a synthetic internal event such as `INTERNAL_COMPLETE`
4. let the WF consume that internal event from its durable per-instance queue
5. take the normal transition to `completed`

That keeps the state machine explicit: `mirroring_response` remains a real state, `completed` remains the terminal state, and no artificial waiting is introduced.

The `mirroring_response` state should therefore include an additional entry action:

```json
{
  "type": "emit_internal_event",
  "meta": { "msg": "INTERNAL_COMPLETE" },
  "payload": {
    "ticket_id": { "$ref": "input.ticket_id" }
  }
}
```

The WF persists that internal event atomically with the state update, then drains it immediately. If the node crashes after the commit and before consuming it, recovery resumes from the durable internal queue and still reaches `completed`.

### What the four properties of WF mean here

| Property | Present? | How |
|---|---|---|
| Multi-step | Yes | Mirror → analyze → mirror → respond (4 actions across 2 states) |
| Memory | Yes | AI response stored in `state.analysis`, `state.suggested_action`, `state.confidence` — used in the response step |
| Deterministic | Yes | Every step is fire-and-forget or guard-based. No probabilistic decisions in the WF itself |
| Lives in time | Yes | Waits up to 2 minutes for AI response. Survives node restart via SQLite persistence |

All four properties are present. This is a legitimate WF.
