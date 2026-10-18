# AI Frontdesk - Core Inconsistencies Report

Date: 2026-03-13  
Scope: AI node implementation constraints without core changes.

## C1. `thread_id` top-level vs wire model

- Requested behavior: accept `thread_id` both at top-level and in `meta.context.thread_id` during transition.
- Current limitation: `fluxbee_sdk::protocol::Message` does not model a top-level `thread_id` field.
- Impact in AI nodes:
  - `ai_node_runner` can only read `thread_id` from `meta.context.thread_id` today.
  - Supporting top-level `thread_id` requires a core/protocol decision and implementation.

### Recommendation for core team

Choose one transitional strategy:

1. Add optional `thread_id` to protocol `Message` (backward compatible), and define precedence during transition.
2. Keep `thread_id` only in `meta.context` until protocol vNext is finalized.

Until that decision is implemented in core, AI nodes will continue with:
- `thread_id` source = `meta.context.thread_id` (temporary).

