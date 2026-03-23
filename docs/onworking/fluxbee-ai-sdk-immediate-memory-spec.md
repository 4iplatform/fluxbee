# Fluxbee AI SDK - Immediate Conversation Memory Spec

Estado: onworking  
Alcance: `crates/fluxbee_ai_sdk`  
Consumidores iniciales:
- `SY.architect`
- futuros nodos `AI.*`

## 1. Purpose

This document defines the **immediate conversation memory** that should be native to `fluxbee_ai_sdk`.

It is intentionally scoped to the short-horizon context that a live chat/agent needs between turns:

- chat summary
- recent interactions
- active or ambiguous operations

It is **not** a long-term memory design. Long-range recall, semantic retrieval, and durable cognitive synthesis belong to `SY.cognitive` and are explicitly out of scope here.

## 2. Why This Exists

Today the SDK function-calling loop starts each new user turn with only:

- system prompt
- current user message
- tool loop context from the same run

That is not enough for real operational chats. The model loses:

- what the operator was trying to do
- what `archi` already answered
- which hive/node/ILK is currently in focus
- whether a mutating action is still pending, running, or timed out ambiguously

This gap must be solved in the SDK itself so it can be reused by both:

- `SY.architect`
- future `AI.*` runtimes

## 3. Non-Goals

This spec does **not** cover:

- semantic/vector memory
- long-horizon summaries from `SY.cognitive`
- cross-chat knowledge retrieval
- tool catalog persistence
- large transcript replay
- background summarization pipelines

## 4. Design Rule

The SDK should send **enough immediate context to be operationally coherent**, but not dump the full raw chat transcript on every turn.

Default principle:

- keep short-term context explicit
- compress old context aggressively
- keep active operations visible
- leave long-term recall to later systems

## 5. Immediate Memory Bundle

The SDK should expose a native input structure for a turn that includes:

1. `system_prompt`
2. `chat_summary`
3. `recent_interactions`
4. `active_operations`
5. `current_user_message`

This bundle should be assembled by the caller, but normalized and serialized by `fluxbee_ai_sdk`.

## 6. Components

### 6.1 Chat Summary

A compact, structured summary of the current chat state.

Purpose:

- preserve operator intent
- preserve current target and decisions
- reduce need to replay older turns

Recommended fields:

- `goal`
- `current_focus`
- `decisions`
- `confirmed_facts`
- `open_questions`

Constraints:

- short text only
- no raw JSON dumps
- rewritten over time by caller logic, not by the SDK itself

Example:

```json
{
  "goal": "Provision and validate a new hive worker-220",
  "current_focus": "Hive creation flow on motherbee",
  "decisions": [
    "Mutations require explicit CONFIRM",
    "Architect tracks long-running operations locally"
  ],
  "confirmed_facts": [
    "worker-220 did not exist before this chat"
  ],
  "open_questions": [
    "Whether add_hive completed after timeout"
  ]
}
```

### 6.2 Recent Interactions

A bounded window of the most recent conversational turns.

Recommended default:

- last `8-12` interactions

Where one interaction may include:

- user text
- assistant text
- compact tool/result summary when relevant

Do not send:

- full raw payloads for every old tool result
- repeated status dumps already captured in summary

Recommended shape:

```json
[
  {
    "role": "user",
    "kind": "text",
    "content": "create hive worker-220 at 192.168.8.220"
  },
  {
    "role": "assistant",
    "kind": "text",
    "content": "Hive creation prepared. Reply CONFIRM to execute."
  },
  {
    "role": "system",
    "kind": "operation_summary",
    "content": "add_hive worker-220 -> timeout_unknown"
  }
]
```

### 6.3 Active Operations

This is mandatory for operational agents.

The model must see ambiguous or in-flight actions without needing to rediscover them from logs.

Recommended fields per operation:

- `operation_id`
- `scope_id`
- `action`
- `target`
- `status`
- `summary`
- `created_at_ms`
- `updated_at_ms`

Statuses that matter immediately:

- `pending_confirm`
- `dispatched`
- `running`
- `timeout_unknown`
- `succeeded`
- `failed`
- `canceled`

For model input, only include:

- non-terminal operations
- recent terminal operations that still matter to the current chat

Example:

```json
[
  {
    "operation_id": "op-123",
    "scope_id": "chat:abc",
    "action": "add_hive",
    "target": "worker-220",
    "status": "timeout_unknown",
    "summary": "Hive creation may still be running; inspect before retrying"
  }
]
```

## 7. SDK Boundary

The SDK should own:

- canonical structs for immediate memory
- validation and normalization
- serialization into model input items
- token-aware truncation policy for recent interactions

The caller should own:

- reading session history from local storage
- maintaining chat summary
- deciding which operations are relevant
- passing the current user message

This keeps the SDK reusable while still giving every node the same immediate-memory model.

## 8. Proposed Native SDK Types

These are design targets, not final Rust signatures.

```rust
pub struct ImmediateConversationMemory {
    pub summary: Option<ConversationSummary>,
    pub recent_interactions: Vec<ImmediateInteraction>,
    pub active_operations: Vec<ImmediateOperation>,
}

pub struct ConversationSummary {
    pub goal: Option<String>,
    pub current_focus: Option<String>,
    pub decisions: Vec<String>,
    pub confirmed_facts: Vec<String>,
    pub open_questions: Vec<String>,
}

pub struct ImmediateInteraction {
    pub role: ImmediateRole,
    pub kind: ImmediateInteractionKind,
    pub content: String,
}

pub struct ImmediateOperation {
    pub operation_id: String,
    pub scope_id: String,
    pub action: String,
    pub target: Option<String>,
    pub status: String,
    pub summary: String,
    pub created_at_ms: Option<u64>,
    pub updated_at_ms: Option<u64>,
}
```

## 9. Proposed SDK Assembly Flow

`fluxbee_ai_sdk` should support a turn input richer than a single `user_input: String`.

Target flow:

1. caller loads session summary
2. caller loads recent interactions
3. caller loads active operations
4. caller builds an `ImmediateConversationMemory`
5. SDK converts that into model input items
6. SDK appends the new user turn last

The model should see the immediate memory in this order:

1. system prompt
2. immediate summary
3. active operations
4. recent interactions
5. current user message

## 10. Serialization Policy

The SDK should serialize immediate memory as explicit textual blocks, not opaque blobs.

Recommended pattern:

- one synthetic system/developer-style block for summary
- one compact block for active operations
- then recent turns as user/assistant/system items

This is preferable to dumping a large JSON object because:

- it is easier for the model to follow
- it preserves role ordering
- it makes truncation simpler

## 11. Default Limits

Recommended v1 defaults:

- `recent_interactions_max = 10`
- `active_operations_max = 8`
- `summary_max_chars = 1600`
- `interaction_max_chars = 1200` per item after compaction

If limits are exceeded:

- drop oldest interactions first
- keep `timeout_unknown` and `pending_confirm` operations before terminal ones
- never drop current user message

## 12. Immediate Memory Rules

### 12.1 What Must Be Kept

- latest operator intent
- latest assistant guidance
- active or ambiguous operations
- explicit confirmations/cancellations

### 12.2 What Should Be Compressed

- large tool outputs
- repeated inventory dumps
- repeated status refreshes
- old command payloads that have already become confirmed facts

### 12.3 What Must Not Be Assumed

- full transcript replay forever
- durable memory across chats
- cognitive/semantic retrieval

## 13. `SY.architect` Mapping

For `SY.architect`, the first implementation should map:

- `chat_summary`: derived from session-local state
- `recent_interactions`: from persisted chat messages
- `active_operations`: from `architect.lance/operations`

Important:

- this memory remains **chat-scoped**
- later, if each chat maps to a distinct ILK, the same design can evolve to `chat/ILK` scope without changing the SDK contract

## 14. Relationship With `SY.cognitive`

`SY.cognitive` is an additive later layer.

It may eventually provide:

- durable summaries
- semantic retrieval
- cross-session context
- long-range memory compression

But it should plug into this model as **extra memory**, not replace immediate turn memory.

Immediate memory remains necessary even after `SY.cognitive` exists.

## 15. Implementation Plan

### Phase 1

- add native immediate-memory structs to `fluxbee_ai_sdk`
- add a richer turn-input API beside the current `run(..., user_input)`
- support summary + recent interactions + active operations

### Phase 2

- migrate `SY.architect` to build and pass this bundle
- keep old single-string path temporarily for compatibility

### Phase 3

- let future `AI.*` nodes reuse the same immediate-memory contract

## 16. Acceptance Criteria

This work is correct when:

- the model no longer loses basic chat continuity after one turn
- pending/ambiguous operations remain visible between turns
- old command payloads do not have to be repeated manually by the operator
- the SDK can support `SY.architect` and `AI.*` with the same immediate-memory contract
- no dependency on `SY.cognitive` is required for this short-horizon memory

## 17. Backlog

- [ ] AI-SDK-IM1. Add native immediate-memory structs to `fluxbee_ai_sdk`
- [ ] AI-SDK-IM2. Add turn-input builder/API for summary + recent interactions + active operations
- [ ] AI-SDK-IM3. Add default truncation/compaction policy for recent interactions
- [ ] AI-SDK-IM4. Update `SY.architect` to rehydrate immediate memory from session history + operation tracking
- [ ] AI-SDK-IM5. Document immediate-memory usage for future `AI.*` nodes
