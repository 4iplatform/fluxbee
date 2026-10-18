# Handoff a Core - API de estado de conexión para AI SDK

## Contexto

Al implementar reconexión explícita en runtimes AI con conexión compartida (`run_single_connection_runtime`), surgió una dependencia de API en `fluxbee_ai_sdk::router_client::RouterReader` para consultar estado de enlace y esperar reconexión.

## Problema observado

En ramas/entornos donde `RouterReader` no expone estas funciones, el build falla:

- `no method named is_connected found for struct RouterReader`

Esto rompe compilación de `nodes/ai/ai-generic` y `nodes/gov/ai-frontdesk-gov` cuando el runner intenta usar esa API.

## Propuesta (core/SDK)

Agregar en `crates/fluxbee_ai_sdk/src/router_client.rs`:

```rust
impl RouterReader {
    pub fn is_connected(&self) -> bool {
        self.receiver.is_connected()
    }

    pub async fn wait_connected(&self) {
        self.receiver.wait_connected().await;
    }
}
```

## Justificación

- El core (`fluxbee_sdk`) ya mantiene `ConnectionState` y notificación de reconexión.
- Exponerlo en `fluxbee_ai_sdk` evita reimplementar polling/circuitos alternativos en cada runner.
- Simplifica observabilidad y semántica homogénea entre runtime default y runtime single-connection.

## Estado actual / workaround aplicado

Para no bloquear integración local mientras core define esta API, se aplicó workaround en SDK/runtime y runners AI:

- No usa `is_connected()`.
- En error transitorio (`NodeError::Io` / `NodeError::Disconnected`), entra en loop con backoff+jitter.
- Sondea reconexión con `read_runtime_message` de timeout corto.
- Si recibe un mensaje durante el sondeo, lo reinyecta en backlog y retoma loop normal.

Este workaround compila con SDK actual, pero es menos limpio que exponer `is_connected/wait_connected` en `RouterReader`.
