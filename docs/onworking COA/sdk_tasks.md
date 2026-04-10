# Fluxbee SDK - Backlog de migración (`jsr_client` -> `fluxbee_sdk`)

Estado objetivo:
- `fluxbee_sdk` como librería única para nodos (comunicación + blob + futuras herramientas).
- `jsr_client` retirado del workspace principal (migración cerrada).

Regla operativa acordada:
- Todo cambio nuevo se valida compilando y ejecutando por `fluxbee_sdk`.

## Fase M1 - Base de compatibilidad

- [x] M1. Crear crate `fluxbee_sdk`.
- [x] M2. Integrar módulos de comunicación dentro de `fluxbee_sdk` (duplicado de `jsr_client`).
- [x] M3. Exponer API de comunicación desde `fluxbee_sdk` (`connect`, `NodeConfig`, `nats`, `protocol`, etc.).

## Fase M2 - Adopción en repo principal

- [x] M4. Migrar imports del repo principal de `jsr_client` a `fluxbee_sdk`.
- [x] M5. Compilar workspace usando `fluxbee_sdk` como dependencia principal.
- [x] M6. Ajustar scripts/diag para nomenclatura neutral (`jsr_client` -> `fluxbee_sdk`) sin romper compatibilidad temporal.

## Fase M3 - Gate de desarrollo

- [x] M7. Definir gate CI/local obligatorio: `cargo check` + pruebas diag con ruta `fluxbee_sdk` (`scripts/sdk_gate.sh`).
- [x] M8. Prohibir nuevos usos directos de `jsr_client` en código productivo (`scripts/check_no_jsr_client_usage.sh` integrado en `scripts/sdk_gate.sh`).
- [x] M9. Agregar aviso de deprecación en `jsr_client` (README/comentarios/docstring).

## Fase M4 - Integración de blob en SDK

- [x] M10. Implementar blob module canónico de `fluxbee_sdk` según `docs/blob-annex-spec.md`.
- [x] M11. Integrar contrato `text/v1` y utilidades de attachments en `fluxbee_sdk`.
- [x] M12. Validar E2E de comunicación + blob usando bins/scripts que ya consumen `fluxbee_sdk`.

## Fase M5 - Retiro de `jsr_client`

- [x] M13. Eliminar dependencias remanentes de `jsr_client`.
- [x] M14. Retirar crate `crates/jsr_client` del workspace.
- [x] M15. Actualizar documentación final de migración (spec + onworking + changelog).

## Pendientes reales para cerrar migración

- Migración SDK cerrada.
- `fluxbee_sdk` queda como único SDK soportado para nuevos desarrollos.

## Backlog explícito - `fluxbee-go-sdk`

- [x] GO-SDK-1. Formalizar `fluxbee-go-sdk` como módulo top-level reutilizable por nodos first-party y terceros.
- [x] GO-SDK-2. Portar envelope, handshake, framing y lifecycle base de conexión al router.
- [x] GO-SDK-3. Portar helpers canónicos de `CONFIG_GET` / `CONFIG_SET` / `CONFIG_RESPONSE` y `NODE_STATUS_GET`.
- [x] GO-SDK-4. Portar helpers RPC `system` y tipos reutilizables de `HELP`.
- [x] GO-SDK-5. Agregar resolución de identidad `uuid -> L2` vía router SHM local.
- [x] GO-SDK-6. Corregir la resolución de router para no depender de `gateway_name`:
  - persistir `ANNOUNCE.router_name` en el estado de conexión
  - exponerlo en `NodeSender` / `NodeReceiver`
  - usar ese router efectivo para abrir `state/<router_l2_name>/identity.yaml` y su SHM
- [ ] GO-SDK-7. Revisar si conviene extender `ANNOUNCE` con `shm_name` explícito para evitar lectura adicional de `identity.yaml`.
- [ ] GO-SDK-8. Revisar el modelo de múltiples routers por hive y congelar semántica para consumidores SDK:
  - un nodo puede conectarse a cualquier router local
  - la router SHM sigue siendo per-router, no una vista fusionada por hive
  - documentar esto como contrato explícito
- [ ] GO-SDK-9. Portar readers SHM adicionales que hoy siguen faltando respecto del runtime Rust, empezando por los casos con valor real para nodos Go.
- [ ] GO-SDK-10. Revisar el surface público del SDK Go y estabilizar política de versionado/compatibilidad para terceros.
