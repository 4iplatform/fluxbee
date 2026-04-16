# Fluxbee IO.api Multitenant - Task Checklist

**Fecha:** 2026-04-14
**Estado:** validado funcionalmente en Linux para el alcance IO.api / io-common
**Objetivo:** ejecutar y seguir, de forma incremental, los cambios necesarios para llevar `IO.api` a un modelo multitenant tenant-scoped con handoff correcto a `SY.frontdesk.gov`.

---

## 0. Criterio de avance

- Cada tarea debe cerrarse con cambio de codigo y validacion minima asociada.
- No avanzar al bloque siguiente si el bloque actual deja inconsistencia estructural.
- Si una tarea revela que el punto correcto esta fuera de `IO.api` / `io-common`, registrarlo antes de seguir.

---

## 1. Helpers canonicos de identity / SHM

- [x] Agregar helper canonico para validar existencia de tenant por `tenant_id` desde identity SHM.
- [x] Agregar helper canonico para validar existencia de ILK por `ilk_id` desde identity SHM.
- [x] Definir errores/retenos reutilizables para `exists / missing / unavailable / timeout` si aplica.
- [x] Cubrir ambos helpers con tests unitarios.
- [x] Documentar el alcance de estos helpers: validan existencia, no aislamiento tenant-aware.

Archivos tocados:

- `crates/fluxbee_sdk/src/identity.rs`

Estado actual:

- helpers agregados en `fluxbee_sdk::identity`
- semantica actual:
  - `Ok(true)` => existe
  - `Ok(false)` => no existe
  - `Err(...)` => unavailable / timeout / error de lectura
- estos helpers no vuelven tenant-aware al pipeline de identity; solo validan existencia sobre el dataset SHM

---

## 2. Pipeline comun `io-common` tenant-aware

- [x] Extender `ResolveOrCreateInput` para incluir `tenant_id` efectivo.
- [x] Agregar carrier para `src_ilk_override` y usarlo en el camino `by_ilk`.
- [x] Asegurar que el pathway de `by_ilk` valide existencia de ILK antes de aceptar.
- [ ] Eliminar dependencia conceptual de `tenant_hint` en el pipeline comun usado por `IO.api`.
- [x] Cambiar el lookup para que opere con tenant efectivo incorporado.
- [x] Cambiar la provision para que transporte tenant efectivo a `MSG_ILK_PROVISION`.
- [ ] Distinguir resultados estructurados para:
- [ ] sujeto existente
- [ ] sujeto provisionado temporalmente
- [ ] tenant inexistente
- [ ] ILK inexistente
- [ ] identity unavailable
- [ ] identity timeout
- [x] Cubrir el flujo tenant-aware con tests.
- [x] Confirmar por codigo que el aislamiento multitenant real ya no depende solo de `(channel, external_id)`.

Archivos tocados:

- `nodes/io/common/src/identity.rs`
- `nodes/io/common/src/inbound.rs`
- `nodes/io/common/src/relay.rs`
- callers en `io-api`, `io-slack`, `io-sim`

Estado actual:

- `io-common` ya lookupea por `(tenant_id, channel, external_id)` cuando el tenant esta presente.
- `io-common` ya envia `tenant_id` explicito en `ILK_PROVISION`.
- el `ich_id` temporal derivado para provision ahora incluye `tenant_id` en la semilla para evitar colisiones entre tenants.
- quedaron agregados tests unitarios minimos para fijar que el mismo `(channel, external_id)` se separa por tenant.

---

## 3. `IO.api` auth y contrato HTTP

- [x] Cambiar el modelo de API keys para que cada key tenga `tenant_id` obligatorio.
- [x] Hacer que autenticacion devuelva tenant efectivo junto con la key autenticada.
- [x] Validar fail-closed que el tenant derivado de la key exista.
- [ ] Rechazar request si la key es invalida, revocada o no resuelve tenant.
- [x] Eliminar `tenant_hint` del parsing de request.
- [x] No aceptar `tenant_id` en el body del request.
- [x] Redefinir `explicit_subject by_data` para requerir:
- [x] `subject.external_user_id`
- [x] `subject.display_name`
- [x] `subject.email`
- [x] Aceptar `subject.company_name` como metadata opcional.
- [x] Aceptar `subject.attributes` como metadata opcional extensible.
- [x] Hacer que `by_ilk` use validacion canonica de existencia de ILK.
- [x] Agregar override opcional de destino por request:
- [x] `options.routing.dst_node`
- [x] Conectar `IO.api` al pipeline tenant-aware de `io-common`.
- [ ] Revisar errores HTTP esperados:
- [x] `unauthorized`
- [x] `tenant_not_found`
- [x] `ilk_does_not_exist`
- [x] `subject_data_incomplete`
- [x] `identity_unavailable`
- [ ] `identity_timeout`
- [ ] Eliminar aliases legacy del adapter si se confirma la limpieza completa:
- [ ] `GET /schema`
- [ ] `POST /messages`
- [x] Ajustar `GET /` para reflejar el contrato nuevo.
- [ ] Cubrir con tests unitarios e integracion adicionales.

Archivos tocados:

- `nodes/io/common/src/io_api_adapter_config.rs`
- `nodes/io/io-api/src/auth.rs`
- `nodes/io/io-api/src/subject.rs`
- `nodes/io/io-api/src/schema.rs`
- `nodes/io/io-api/src/main.rs`

Estado actual:

- `IO.api` ya es tenant-scoped por API key.
- `tenant_hint` ya no forma parte del contrato HTTP.
- `by_ilk` ya no esta bloqueado como `not_implemented`.
- `IO.api` ya puede resolver metadata canonica del sujeto desde SHM por `(tenant_id, channel, external_id)`.
- `IO.api` ya usa `registration_status` para decidir si intermedia `SY.frontdesk.gov`.
- `IO.api` ya puede continuar luego al `dst_final` cuando frontdesk devuelve `ok`.
- `IO.api` ya queda cableado al pipeline tenant-aware de `io-common` tanto para lookup como para provision.
- el pendiente principal de `IO.api` paso a ser revalidacion funcional tenant-aware end-to-end en Linux.

---

## 4. Contrato compartido `frontdesk_*`

- [x] Definir contrato tipado compartido del lado IO para `frontdesk_handoff`.
- [x] Definir contrato tipado compartido del lado IO para `frontdesk_result`.
- [x] Definir contrato tipado compartido del lado GOV para `frontdesk_handoff`.
- [x] Definir contrato tipado compartido del lado GOV para `frontdesk_result`.
- [x] Exponer helpers canonicos de parseo y acceso.

Archivos tocados:

- `nodes/io/common/src/frontdesk_contract.rs`
- `nodes/io/common/src/lib.rs`
- `nodes/gov/common/src/frontdesk_contract.rs`
- `nodes/gov/common/src/lib.rs`

---

## 5. Handoff a `SY.frontdesk.gov`

- [x] Implementar contrato oficial de input estructurado `payload.type = "frontdesk_handoff"` en `SY.frontdesk.gov`.
- [x] Mantener tambien el input conversacional oficial `payload.type = "text"` para el flujo normal.
- [x] Incluir en `frontdesk_handoff`:
- [x] `payload.schema_version`
- [x] `payload.operation`
- [x] `meta.src_ilk`
- [x] `meta.thread_id`
- [x] `payload.tenant_id` cuando exista
- [x] `payload.subject.display_name`
- [x] `payload.subject.email`
- [x] `payload.subject.company_name` si vino
- [x] `payload.subject.attributes` si vinieron
- [x] `payload.context` si vino
- [x] Separar explicitamente en frontdesk:
- [x] modo `register_automatic` via handoff estructurado deterministico
- [x] modo conversacional
- [x] Asegurar que el camino API `by_data` use `register_automatic` cuando ya tiene datos minimos validos.
- [x] Evitar que frontdesk vuelva a resolver tenancy desde hints textuales en el handoff estructurado.
- [x] Hacer que frontdesk complete `ILK_REGISTER` con `tenant_id` ya resuelto cuando viene en handoff.
- [x] Implementar el output canonico unico `payload.type = "frontdesk_result"`.
- [x] Hacer `human_message` obligatorio siempre en `frontdesk_result`.
- [x] Incluir en `frontdesk_result`:
- [x] `payload.schema_version`
- [x] `payload.status`
- [x] `payload.result_code`
- [x] `payload.human_message`
- [x] `payload.missing_fields`
- [x] `payload.error_code`
- [x] `payload.error_detail`
- [x] `payload.ilk_id` cuando se conozca
- [x] `payload.tenant_id` cuando se conozca
- [x] `payload.registration_status` cuando se conozca
- [x] Eliminar `text/v1` como salida canonica de frontdesk.
- [ ] Ajustar comportamiento conversacional fino para distinguir con precision:
- [ ] `REGISTERED`
- [ ] `ALREADY_COMPLETE`
- [ ] `REGISTER_FAILED`
- [ ] `IDENTITY_UNAVAILABLE`
- [ ] Cubrir el flujo con tests o validacion equivalente.

Archivos tocados:

- `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs`
- `nodes/gov/common/src/frontdesk_contract.rs`

Estado actual:

- `SY.frontdesk.gov` ya acepta `payload.type = "frontdesk_handoff"` y `payload.type = "text"`.
- el handoff estructurado ya sigue una via deterministica y devuelve `frontdesk_result`.
- el camino conversacional ya devuelve `frontdesk_result` en lugar de `text/v1`.
- validado en Linux con `cargo test -p sy-frontdesk-gov`.
- quedan pendientes el refinamiento fino de algunos `result_code` conversacionales y la validacion funcional real del handoff.

---

## 6. Consumidores de `frontdesk_result`

- [x] Ajustar nodos conversacionales para leer `human_message` desde `frontdesk_result`.
- [x] Ajustar nodos no conversacionales para consumir el bloque estructurado de `frontdesk_result`.

Archivos tocados:

- `nodes/io/io-slack/src/main.rs`

Pendiente principal:

- ninguno en este bloque para el alcance `IO.api / io-slack`; el ajuste de consumidores ya quedo implementado.

---

## 7. Documentacion

- [x] Actualizar `docs/io/io-api-node-spec.md`.
- [x] Actualizar `docs/io/io-api-http-contract-examples.md`.
- [x] Actualizar el runbook operativo/validacion de `IO.api`.
- [x] Actualizar checklist y documentos onworking de multitenant / handoff.
- [x] Actualizar documentacion formal de `SY.frontdesk.gov` para:
- [x] `register_automatic`
- [x] `frontdesk_handoff`
- [x] `frontdesk_result`
- [x] reemplazo de `text/v1` como salida canonica
- [x] Dejar explicito en docs:
- [x] el tenant sale de la API key
- [x] `tenant_hint` ya no existe en `IO.api`
- [x] `company_name` y `attributes` son metadata, no tenancy
- [x] validar existencia por SHM no reemplaza aislamiento tenant-aware

Pendiente documental:

- revisar docs residuales si reaparece alguna referencia legacy a salida canonica `text/v1` fuera de la spec ya actualizada

---

## 8. Validacion ejecutada hasta ahora

- [x] `cargo test -p fluxbee-sdk read_tenant_exists_returns_true_for_active_tenant --lib` en Linux
- [x] `cargo test -p fluxbee-sdk read_ilk_exists_returns_true_for_active_ilk --lib` en Linux
- [x] `cargo test -p io-common --lib` en Linux
- [x] `cargo test -p io-slack` en Linux
- [x] `cargo test -p io-api` en Linux
- [x] `cargo test -p io-sim` en Linux
- [x] `cargo test -p sy-frontdesk-gov` en Linux
- [x] `cargo test` en `nodes/gov/common` en Linux

Nota:

- la compilacion local en Windows sigue limitada por dependencias Unix-only de `fluxbee-sdk`; por eso la validacion efectiva de `sy-frontdesk-gov` debe correrse en Linux.

---

## 9. Validacion funcional pendiente

- [x] Caso feliz `by_ilk` con ILK existente.
- [x] Caso rechazo `by_ilk` con ILK inexistente.
- [x] Caso feliz `by_data` con sujeto existente en tenant correcto.
- [x] Caso feliz `by_data` con sujeto nuevo y provision temporal.
- [x] Caso de key con tenant inexistente -> rechazo fail-closed.
- [x] Caso de handoff a frontdesk no conversacional.
- [x] Caso de salida `frontdesk_result` consumida por un nodo conversacional via `human_message`.
- [x] Caso de salida `frontdesk_result` consumida por un nodo no conversacional via campos estructurados.
- [x] Verificacion de docs actualizadas contra comportamiento real.
- [x] Caso tenant-aware con mismo `external_user_id` en tenants distintos -> ILKs distintos.
- [x] Caso tenant-aware con sujeto nuevo en tenant B -> `lookup miss` + provision.
- [x] Caso tenant-aware con repeticion del mismo sujeto en tenant B -> `lookup hit` + mismo ILK.

Estado actual:

- validado en Linux:
  - `by_ilk` con ILK existente:
    - HTTP `202 Accepted`
    - respuesta con el mismo `ilk` pedido
  - `by_ilk` con ILK inexistente:
    - HTTP `404 Not Found`
    - `error_code = "ilk_does_not_exist"`
  - `lookup hit` con reutilizacion del mismo `ilk`
  - `lookup miss -> ILK_PROVISION -> frontdesk -> 202 Accepted`
  - flush posterior del relay y entrega al router hacia `AI.chat@motherbee`
  - rechazo temprano `422 subject_data_incomplete` para payload incompleto
- implementado por codigo:
  - `io-slack` consume `frontdesk_result` y renderiza `human_message` para outbound conversacional
  - `IO.api` consume `frontdesk_result` como bloque estructurado y lo mapea a respuesta HTTP/no conversacional
  - mismo `external_user_id` en tenants distintos produce ILKs distintos:
    - tenant A -> `ilk:ee207fb9-e3cb-4703-b648-711a8b656692`
    - tenant B -> `ilk:24c38a6c-2265-4298-8c44-6748af85c5cf`
  - sujeto nuevo en tenant B:
    - primer request -> `lookup miss` + `identity provisioned` con `ilk:d66c6b25-b0a8-4ff5-9f5b-2cfb1baa7f2f`
    - segundo request -> `lookup hit` con el mismo `ilk`
    - `registration_status=complete`
- el caso `frontdesk_result.status = needs_input` no es hoy un E2E alcanzable desde `IO.api` porque el adapter exige `subject.display_name` y `subject.email` antes de construir el handoff
- las incidencias de `identity_timeout` observadas durante la validacion quedaron correlacionadas con estado operativo de `sy-identity`, no con la logica funcional de `IO.api`
- las incidencias de `invalid_frontdesk_response` con payload `NODE_NOT_FOUND` quedaron correlacionadas con `SY.frontdesk.gov` activo en systemd pero no registrado en el router; el reinicio del servicio recompuso el registro y la validacion siguio normalmente
- `tenant_not_found` fail-closed ya quedo validado:
  - HTTP `403 Forbidden`
  - `error_code = "tenant_not_found"`
- `by_ilk` con ILK inexistente quedo corregido y validado:
  - resultado final observado: HTTP `404`
  - `error_code = "ilk_does_not_exist"`

---

## 10. Proximo corte de trabajo

- [x] Ejecutar validacion E2E real de `IO.api -> SY.frontdesk.gov`.
- [x] Verificar mapping HTTP de `frontdesk_result` para:
- [x] `ok`
- [x] `needs_input`
- [x] `error`
- [ ] Ajustar warnings menores restantes (`unused variable` / `dead_code`) si se decide limpiar.
- [x] Revaluar el gap de core si `ILK_PROVISION` agrega `tenant_id`.

Lectura actual del bloque:

- `frontdesk_result.ok` ya quedo validado por E2E desde `IO.api`
- `frontdesk_result.needs_input` no debe seguirse como prueba E2E de `IO.api` mientras el contrato actual del adapter bloquee antes los mismos faltantes
- `frontdesk_result.error` ya queda cubierto a nivel de mapping HTTP por tests unitarios en `nodes/io/io-api/src/http.rs`
- el gap tenant-aware del lado IO quedo validado funcionalmente en Linux

---

## 11. Orden recomendado de ejecucion

- [x] Bloque 1: helpers SHM en `fluxbee_sdk`
- [x] Bloque 2: pipeline tenant-aware en `io-common`
- [x] Bloque 3: auth + contrato en `IO.api`
- [x] Bloque 4: handoff y registro base en `SY.frontdesk.gov`
- [x] Bloque 5: documentacion formal de `SY.frontdesk.gov`
- [x] Bloque 6: validacion final E2E
