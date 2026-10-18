# AI Nodes SDK Specification (Rust)

**Version:** v1.0\
**Generated:** 2026-03-03 12:43:07 UTC\
**Audience:** Backend / Infrastructure Developers\
**Purpose:** Base specification to implement Fluxbee AI Nodes without
relying on external Agents SDK.

------------------------------------------------------------------------

# 1. Objetivo

Este documento define la especificación técnica mínima para implementar
nodos `AI.*` en Rust dentro del ecosistema Fluxbee.

Se organiza en etapas:

-   **Stage 1 -- Core mínimo indispensable**
-   **Stage 2 -- Capacidades extendidas**
-   **Stage 3 -- Orquestación avanzada (tool calling / handoffs)**

No incluye definiciones específicas de cognition.

------------------------------------------------------------------------

# 2. Supuestos Fundamentales

Un nodo AI:

-   Es un proceso independiente
-   Se conecta al router vía Unix Socket
-   Recibe y envía mensajes según el protocolo oficial
-   No persiste estado local obligatorio
-   No modifica el router ni el modelo de routing

Referencia de estructura de mensaje:

``` json
{
  "routing": { ... },
  "meta": { ... },
  "payload": { ... }
}
```

------------------------------------------------------------------------

# 3. Stage 1 -- CORE (MVP obligatorio)

## 3.1 Capacidades mínimas

El SDK debe proveer:

### 1. Cliente de conexión al Router

-   Unix socket
-   Framing (4 bytes length prefix, big-endian)
-   Reconexión automática
-   Envío y recepción async

### 2. Parser/Serializer de Mensajes

``` rust
pub struct Message {
    pub routing: Routing,
    pub meta: Meta,
    pub payload: serde_json::Value,
}
```

Debe soportar: - routing.src - routing.dst - routing.ttl -
routing.trace_id

### 3. Trait principal del Nodo AI

``` rust
#[async_trait]
pub trait AiNode {
    async fn on_message(&self, msg: Message) -> Result<Option<Message>>;
}
```

Reglas: - Si retorna Some(Message) → se envía al router - Si retorna
None → no hay respuesta - Nunca debe bloquear el runtime

### 4. Loop principal del SDK

``` rust
loop {
    let msg = router_client.read().await?;
    if let Some(response) = node.on_message(msg).await? {
        router_client.write(response).await?;
    }
}
```

### 5. Cliente LLM desacoplado

``` rust
#[async_trait]
pub trait LlmClient {
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse>;
}
```

Implementaciones posibles: - OpenAI REST - Anthropic REST - Mock (tests)

------------------------------------------------------------------------

# 4. Concurrencia

El SDK debe: - Ser 100% async (Tokio) - Permitir múltiples requests
simultáneos - No bloquear esperando respuestas externas

Modelo recomendado: - Channel interno (mpsc) - Worker pool
configurable - Timeout por request

------------------------------------------------------------------------

# 5. Payload text/v1

Contrato esperado:

``` json
{
  "payload": {
    "type": "text",
    "content": "string",
    "attachments": []
  }
}
```

Helpers requeridos:

``` rust
fn extract_text(payload: &Value) -> Option<String>;
fn build_text_response(content: String) -> Value;
```

------------------------------------------------------------------------

# 6. thread_id (Pendiente Formalización)

Provisional:

-   `meta.thread_id: Option<String>`
-   El SDK no debe depender de este campo internamente
-   Toda la estructura debe estar encapsulada para permitir migración
    futura

------------------------------------------------------------------------

# 7. Stage 2 -- Extensiones

## Tool Calling sin storage

-   AI responde con intención
-   Router envía a WF.\*
-   No espera respuesta síncrona

## Tool Calling con storage (futuro)

1.  AI envía tool_request
2.  Guarda estado pendiente
3.  WF responde tool_result
4.  AI continúa flujo

Requiere correlación por trace_id

------------------------------------------------------------------------

# 8. Stage 3 -- Handoffs

-   AI decide delegar
-   Emite nuevo target
-   No mantiene estado

------------------------------------------------------------------------

# 9. Estructura del Crate

    fluxbee_ai_sdk/
    ├── router_client.rs
    ├── message.rs
    ├── llm.rs
    ├── node_trait.rs
    ├── runtime.rs
    ├── text_payload.rs
    └── errors.rs

------------------------------------------------------------------------

# 10. Reglas Obligatorias

1.  Stateless por defecto
2.  No bloquear threads
3.  No depender de cognition
4.  No asumir storage
5.  No modificar routing
6.  Encapsular meta/routing parsing

------------------------------------------------------------------------

# 11. Roadmap

## Fase 1

-   Router client
-   Message structs
-   AiNode trait
-   Runtime básico
-   Implementación OpenAI REST

## Fase 2

-   Worker pool configurable
-   Timeouts
-   Logs estructurados

## Fase 3

-   Tool calling remoto
-   Pending calls manager

------------------------------------------------------------------------

# 12. Checklist

-   [ ] Cliente socket funcional
-   [ ] Parser validado
-   [ ] Trait AiNode implementado
-   [ ] Cliente LLM funcional
-   [ ] Runtime async estable
-   [ ] AI.echo funcionando

------------------------------------------------------------------------

# 13. Ejemplo mínimo

``` rust
pub struct EchoNode;

#[async_trait]
impl AiNode for EchoNode {
    async fn on_message(&self, msg: Message) -> Result<Option<Message>> {
        let text = extract_text(&msg.payload).unwrap_or_default();
        let response_payload = build_text_response(format!("Echo: {}", text));

        Ok(Some(Message {
            routing: build_reply_routing(&msg),
            meta: msg.meta.clone(),
            payload: response_payload,
        }))
    }
}
```
