# fluxbee-ai-sdk

SDK base para nodos `AI.*` y para `SY.architect`.

Expone cuatro bloques principales:

- cliente/modelos LLM
- loop de function calling con tools
- thread state tools/store
- memoria inmediata de conversación para continuidad entre turns

Regla canónica actual:

- `thread_id` llega al nodo AI como metadata conversacional
- `src_ilk` sigue siendo la key canónica de state/immediate memory en runtimes AI
- los thread-state tools quedan por compatibilidad de nombre, pero en runtimes scoped operan sobre `src_ilk`
- `thread_id` sigue disponible como metadata y como alias legacy de migración para hard state previo

La spec de memoria inmediata vive en [docs/immediate-conversation-memory-spec.md](/Users/cagostino/Documents/GitHub/fluxbee/docs/immediate-conversation-memory-spec.md).

## Function Calling

El loop clásico sigue disponible:

```rust
use fluxbee_ai_sdk::{
    FunctionCallingConfig, FunctionCallingRunner, FunctionToolRegistry,
    OpenAiResponsesClient,
};

let model = OpenAiResponsesClient::new(api_key).function_model(
    "gpt-4.1-mini",
    Some("You are a Fluxbee node.".to_string()),
    Default::default(),
);
let tools = FunctionToolRegistry::new();
let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());

let result = runner.run(&model, &tools, "hello").await?;
```

## Immediate Memory

Para nodos que necesitan continuidad real entre turns, usar `run_with_input(...)` con `FunctionRunInput`.

El bundle admite:

- `summary`
- `recent_interactions`
- `active_operations`
- `thread_id`
- `scope_id`

Semántica recomendada en v2:

- `thread_id` describe continuidad conversacional/cognitiva
- `scope_id` en runtimes AI suele mapear a `src_ilk`
- la persistencia de memoria inmediata se resuelve por `src_ilk`, no por `thread_id`

Ejemplo:

```rust
use fluxbee_ai_sdk::{
    ConversationSummary, FunctionCallingConfig, FunctionCallingRunner, FunctionRunInput,
    FunctionToolRegistry, ImmediateConversationMemory, ImmediateInteraction,
    ImmediateInteractionKind, ImmediateOperation, ImmediateRole, OpenAiResponsesClient,
};

let model = OpenAiResponsesClient::new(api_key).function_model(
    "gpt-4.1-mini",
    Some("You are a Fluxbee node.".to_string()),
    Default::default(),
);
let tools = FunctionToolRegistry::new();
let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());

let input = FunctionRunInput {
    current_user_message: "confirm the previous hive operation".to_string(),
    immediate_memory: Some(ImmediateConversationMemory {
        thread_id: Some("thread:abc".to_string()),
        scope_id: None,
        summary: Some(ConversationSummary {
            goal: Some("Provision worker-220".to_string()),
            current_focus: Some("Review add_hive result".to_string()),
            decisions: vec!["Mutations require explicit CONFIRM".to_string()],
            confirmed_facts: vec![],
            open_questions: vec!["Whether add_hive already completed".to_string()],
        }),
        recent_interactions: vec![
            ImmediateInteraction {
                role: ImmediateRole::User,
                kind: ImmediateInteractionKind::Text,
                content: "create hive worker-220".to_string(),
            },
            ImmediateInteraction {
                role: ImmediateRole::Assistant,
                kind: ImmediateInteractionKind::Text,
                content: "Hive creation prepared. Reply CONFIRM to execute.".to_string(),
            },
        ],
        active_operations: vec![ImmediateOperation {
            operation_id: "op-1".to_string(),
            resource_scope: Some("hive:worker-220".to_string()),
            origin_thread_id: Some("thread:abc".to_string()),
            origin_session_id: Some("session-1".to_string()),
            scope_id: None,
            action: "add_hive".to_string(),
            target: Some("worker-220".to_string()),
            status: "timeout_unknown".to_string(),
            summary: "Inspect before retrying".to_string(),
            created_at_ms: None,
            updated_at_ms: None,
        }],
    }),
};

let result = runner.run_with_input(&model, &tools, input).await?;
```

El SDK serializa esa memoria en este orden:

1. system prompt
2. conversation summary
3. recent interactions
4. active operations
5. current user message

## Summary Refresh Helper

El summary sigue siendo estado del caller. El SDK no lo refresca automáticamente en cada turn.

Para nodos que quieran actualizarlo explícitamente, usar:

```rust
use fluxbee_ai_sdk::{
    refresh_conversation_summary, SummaryRefreshConfig, SummaryRefreshInput,
};

let refreshed = refresh_conversation_summary(
    llm.as_ref(),
    SummaryRefreshConfig {
        model: "gpt-4.1-mini".to_string(),
        system_prompt: None,
        model_settings: None,
    },
    SummaryRefreshInput {
        thread_id: Some("thread:abc".to_string()),
        scope_id: None,
        existing_summary: current_summary,
        recent_interactions,
        active_operations,
    },
).await?;
```

Ese helper:

- usa el LLM normal del SDK
- pide salida JSON estricta
- toma `existing_summary + recent_interactions + active_operations`
- devuelve un `ConversationSummary` normalizado

## Current V1 Guidance

Para `AI.*` y `SY.architect`, la recomendación v1 es:

- mandar los últimos `8-12` mensajes recientes
- preservar errores/tool failures recientes
- incluir operaciones activas o ambiguas
- usar el helper de refresh solo cuando el caller decida actualizar el summary

Compat/migración:

- los replies nuevos del AI SDK ya no deben reemitir `ctx`, `ctx_seq`, `ctx_window` ni `memory_package`
- los runtimes AI siguen leyendo `thread_id` y `src_ilk` desde `meta` top-level
- si hace falta compat, pueden caer a `meta.context.thread_id` y `meta.context.src_ilk` solo como lectura legacy

No hace compresión semántica avanzada. Esa capa queda para sistemas cognitivos posteriores.
