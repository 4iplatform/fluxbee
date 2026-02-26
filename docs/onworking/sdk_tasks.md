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
