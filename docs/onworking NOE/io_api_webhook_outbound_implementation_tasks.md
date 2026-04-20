# IO.api Webhook Outbound - Tareas de implementacion

Estado: propuesta de ejecucion ordenada  
Fecha: 2026-04-20  
Base de trabajo:
- `docs/io/io-api-webhook-outbound-spec.md`
- `docs/io/io-api-node-spec.md`
- `docs/io/io-common.md`
- `docs/02-protocolo.md`

Alcance inicial:
- `IO.api`
- `io-common`
- config de integraciones autenticadas en `IO.api`
- primer delivery outbound por `webhook_post`

Fuera de alcance inicial:
- otros reply transports
- callbacks intermedios o de progreso
- polling externo
- override del webhook por request
- durabilidad cross-restart del outbox outbound
- `download_url` para attachments en v1

---

## Criterio de orden

La secuencia propuesta busca:

1. cerrar primero el modelo de configuración y correlación;
2. introducir después el carrier interno `reply_target`;
3. recién entonces implementar materialización del contrato externo outbound;
4. cerrar luego envío HTTP firmado y retries;
5. terminar con tests, documentación y validación operativa.

---

## Fase 0 - Congelar decisiones v1

- [x] WOB-R0.1 Confirmar como decisión cerrada que `IO.api` mantiene `POST /` asincrónico con `202 Accepted`.
- [x] WOB-R0.2 Confirmar como decisión cerrada que el único reply transport de v1 es `webhook`.
- [x] WOB-R0.3 Confirmar como decisión cerrada que el webhook se resuelve por `integration_id`, no por body del request.
- [x] WOB-R0.4 Confirmar como decisión cerrada que el carrier interno es `meta.context.io.reply_target`.
- [x] WOB-R0.5 Confirmar como decisión cerrada que el `reply_target.kind` de v1 es `webhook_post`.
- [x] WOB-R0.6 Confirmar como decisión cerrada que el webhook outbound usa contrato externo propio y no expone crudo el payload canónico interno de Fluxbee.
- [x] WOB-R0.7 Confirmar como decisión cerrada que los attachments de v1 se entregan inline como `content_base64`.
- [x] WOB-R0.8 Confirmar como decisión cerrada que solo se notifican resultados terminales.
- [x] WOB-R0.9 Confirmar como decisión cerrada que v1 es best-effort en memoria por proceso.

---

## Fase 1 - Modelo de configuración

- [x] WOB-R1.1 Definir el shape final de `auth.api_keys[*]` con `integration_id`.
- [x] WOB-R1.2 Definir el shape final de `integrations[*]` dentro de la config de `IO.api`.
- [x] WOB-R1.3 Definir validaciones mínimas de `integrations[*]`:
  - `integration_id`
  - `tenant_id`
  - `final_reply_required`
  - `webhook.enabled`
  - `webhook.url`
  - `webhook.secret_ref`
  - `timeout_ms`
  - `max_retries`
  - `initial_backoff_ms`
  - `max_backoff_ms`
- [x] WOB-R1.4 Definir cómo se resuelve una integración a partir de una API key autenticada.
- [x] WOB-R1.5 Definir comportamiento cuando:
  - la API key no tiene `integration_id`
  - `integration_id` no existe
  - `tenant_id` de la integración no coincide con el de la key
  - `final_reply_required=true` pero `webhook.enabled=false`
- [x] WOB-R1.6 Definir secretos de webhook via `secret_ref` y su política de redacción.
- [x] WOB-R1.7 Actualizar la spec/config formal de `IO.api` para reflejar el nuevo modelo.

---

## Fase 2 - Resolución y preservación de `reply_target`

- [x] WOB-R2.1 Relevar el contrato actual de `meta.context.io.reply_target` en `io-common`.
- [x] WOB-R2.2 Definir el shape exacto de `reply_target.kind = "webhook_post"`.
- [x] WOB-R2.3 Definir los campos mínimos que deben viajar en `reply_target.params`:
  - `integration_id`
  - `tenant_id`
  - `request_id`
  - `trace_id`
- [x] WOB-R2.4 Confirmar que `reply_target` no transporta `webhook.url` ni secretos.
- [x] WOB-R2.5 Implementar helper de `io-common` para construir el `reply_target` webhook.
- [x] WOB-R2.6 Implementar helper de `io-common` para extraer/parsing del `reply_target` webhook desde mensajes de salida.
- [x] WOB-R2.7 Definir regla de preservación:
  - `IO.api` adjunta `reply_target` al ingreso interno
  - los hops intermedios no deben destruirlo
  - el mensaje terminal que vuelve a `IO.api` debe conservarlo

---

## Fase 3 - Criterio de terminalidad

- [x] WOB-R3.1 Identificar en `IO.api` el path exacto por el que recibe mensajes de salida candidatos a outbound.
- [x] WOB-R3.2 Definir qué mensajes se consideran terminales y entregables por webhook.
- [x] WOB-R3.3 Definir qué mensajes no disparan webhook:
  - progreso
  - estados parciales
  - errores transitorios
  - retries internos en curso
- [x] WOB-R3.4 Definir el mapping v1 de resultado terminal:
  - `status = "ok"`
  - `status = "error"`
- [x] WOB-R3.5 Definir qué datos mínimos debe conservar `IO.api` para correlacionar el resultado terminal con el request original.

---

## Fase 4 - Contrato externo del webhook outbound

- [x] WOB-R4.1 Implementar el shape externo top-level del webhook:
  - `schema_version`
  - `delivery_id`
  - `event_type`
  - `occurred_at`
  - `integration_id`
  - `tenant_id`
  - `request`
  - `result`
  - `source`
- [x] WOB-R4.2 Implementar el shape de `request`:
  - `request_id`
  - `trace_id`
  - `ilk`
- [x] WOB-R4.3 Implementar el shape de `result`:
  - `final`
  - `status`
  - `message`
  - `error`
- [x] WOB-R4.4 Implementar el shape de `message` con:
  - `text`
  - `attachments[]`
- [x] WOB-R4.5 Implementar el shape de attachment externo:
  - `name`
  - `mime_type`
  - `size_bytes`
  - `content_base64`
- [x] WOB-R4.6 Definir y aplicar la política cuando el mensaje terminal no sea textual simple o tenga payload no compatible con el contrato externo.
- [x] WOB-R4.7 Definir y aplicar límites de tamaño para attachments inline en v1.

---

## Fase 5 - Materialización del contenido outbound

- [x] WOB-R5.1 Identificar el punto exacto en `IO.api` donde se transforma el mensaje terminal Fluxbee al contrato externo del webhook.
- [x] WOB-R5.2 Implementar extracción del texto final desde el payload canónico interno.
- [x] WOB-R5.3 Implementar materialización de attachments externos desde blobs/referencias internas.
- [x] WOB-R5.4 Implementar encoding `content_base64` de attachments v1.
- [x] WOB-R5.5 Definir el comportamiento si un attachment no puede materializarse.
- [x] WOB-R5.6 Definir el comportamiento si el resultado terminal no tiene texto pero sí error terminal.
- [ ] WOB-R5.7 Definir el comportamiento si el resultado terminal supera límites razonables del webhook.

---

## Fase 6 - Envío HTTP y firma HMAC

- [x] WOB-R6.1 Implementar cliente HTTP outbound para webhook en `IO.api`.
- [x] WOB-R6.2 Implementar generación de `delivery_id` estable por callback lógico.
- [x] WOB-R6.3 Implementar headers obligatorios:
  - `Content-Type`
  - `X-Fluxbee-Delivery-Id`
  - `X-Fluxbee-Timestamp`
  - `X-Fluxbee-Signature`
- [x] WOB-R6.4 Implementar signing input canónico:
  - `<timestamp>.<raw_body>`
- [x] WOB-R6.5 Implementar `HMAC-SHA256` con encoding hex lowercase.
- [x] WOB-R6.6 Implementar resolución segura de `webhook.secret_ref`.
- [x] WOB-R6.7 Implementar validación de éxito solo ante `2xx`.
- [x] WOB-R6.8 Implementar redacción segura de logs para no exponer secretos ni payloads sensibles completos.

---

## Fase 7 - Retries y outbox v1

- [x] WOB-R7.1 Definir el modelo de reintento v1 best-effort en memoria.
- [x] WOB-R7.2 Implementar `timeout_ms` por integración.
- [x] WOB-R7.3 Implementar `max_retries` por integración.
- [x] WOB-R7.4 Implementar backoff exponencial con jitter.
- [x] WOB-R7.5 Mantener `delivery_id` estable entre retries del mismo callback.
- [x] WOB-R7.6 Garantizar que el payload no mute entre retries.
- [x] WOB-R7.7 Definir y registrar exhaustión de retries como fallo terminal de delivery outbound.
- [x] WOB-R7.8 Documentar explícitamente que v1 no garantiza durabilidad tras restart.

---

## Fase 8 - Integración en `IO.api`

- [x] WOB-R8.1 Extender el path de autenticación para resolver `integration_id`.
- [x] WOB-R8.2 Extender el path de aceptación inbound para adjuntar `reply_target` webhook cuando corresponda.
- [x] WOB-R8.3 Mantener el comportamiento actual cuando la integración no requiere callback final.
- [x] WOB-R8.4 Hacer que `IO.api` consuma mensajes terminales con `reply_target.kind = "webhook_post"`.
- [x] WOB-R8.5 Entregar el callback final usando la config de la integración resuelta.
- [x] WOB-R8.6 Mantener separación clara entre:
  - aceptación del request inbound
  - procesamiento interno
  - entrega final webhook
- [x] WOB-R8.7 Agregar trazabilidad/logs para distinguir:
  - ingreso autenticado
  - integración resuelta
  - callback enviado
  - retry
  - callback agotado

---

## Fase 9 - Tests

- [x] WOB-R9.1 Agregar tests unitarios del parser/validator de config `integrations[*]`.
- [x] WOB-R9.2 Agregar tests unitarios del helper `reply_target.kind = "webhook_post"`.
- [x] WOB-R9.3 Agregar tests del mapping `Message Fluxbee -> contrato externo webhook`.
- [x] WOB-R9.4 Agregar tests de materialización de attachments inline `content_base64`.
- [x] WOB-R9.5 Agregar tests de firma HMAC y headers outbound.
- [x] WOB-R9.6 Agregar tests de retries con `delivery_id` estable.
- [x] WOB-R9.7 Agregar tests de `IO.api` para:
  - integración con callback requerido
  - integración sin callback requerido
  - integración inválida
  - mensaje no terminal
  - error terminal
- [x] WOB-R9.8 Agregar test de integración con servidor webhook local fake que valide:
  - body esperado
  - headers esperados
  - firma HMAC
  - retry ante no-`2xx`

---

## Fase 10 - Documentación y rollout

- [x] WOB-R10.1 Actualizar la spec principal de `IO.api` con el modelo de webhook outbound implementado.
- [x] WOB-R10.2 Actualizar `io-common` con el nuevo `reply_target.kind = "webhook_post"`.
- [x] WOB-R10.3 Documentar el shape final de config `auth.api_keys[*]` + `integrations[*]`.
- [x] WOB-R10.4 Documentar explícitamente que el contrato saliente del webhook es externo y no expone el payload canónico interno.
- [x] WOB-R10.5 Documentar límites y trade-offs de attachments inline en v1.
- [x] WOB-R10.6 Documentar la política de retries y no-durabilidad cross-restart.
- [x] WOB-R10.7 Dejar checklist operativo de validación local/Linux con un endpoint webhook de prueba.

---

## Orden sugerido de ejecución

1. Fase 0 - Congelar decisiones v1
2. Fase 1 - Modelo de configuración
3. Fase 2 - Resolución y preservación de `reply_target`
4. Fase 3 - Criterio de terminalidad
5. Fase 4 - Contrato externo del webhook outbound
6. Fase 5 - Materialización del contenido outbound
7. Fase 6 - Envío HTTP y firma HMAC
8. Fase 7 - Retries y outbox v1
9. Fase 8 - Integración en `IO.api`
10. Fase 9 - Tests
11. Fase 10 - Documentación y rollout

---

## Estado actual

- `WOB-R5.7` sigue pendiente por decisión explícita: falta definir el límite global del body HTTP webhook ya materializado y su política exacta de degradación.
- El resto del corte v1 quedó implementado, testeado y documentado.

---

## Estado de cierre documental inicial

Quedó cerrado en documentación el 2026-04-20:

- Fase 0 completa
- Fase 1 desde `WOB-R1.1` hasta `WOB-R1.6`

Queda pendiente antes de arrancar código de config:

- `WOB-R1.7` actualizar la spec/config principal de `IO.api` para que el modelo nuevo deje de vivir solo en esta spec específica de webhook outbound

Quedó cerrado en documentación el 2026-04-20:

- Fase 2 desde `WOB-R2.1` hasta `WOB-R2.4`
- `WOB-R2.7`

Queda pendiente en Fase 2:

- `WOB-R2.5` helper de construcción en `io-common`
- `WOB-R2.6` helper de extracción/parsing en `io-common`

Quedó cerrado en documentación el 2026-04-20:

- Fase 3 completa

Regla operativa cerrada para v1:

- `IO.api` solo dispara webhook outbound cuando recibe un mensaje terminal que conserva `meta.context.io.reply_target` con `kind = "webhook_post"`;
- el ingress original sigue resolviéndose con `202 Accepted`;
- el mapping top-level del webhook queda reducido a:
  - `result.status = "ok"`
  - `result.status = "error"`

Quedó cerrado en documentación el 2026-04-20:

- Fase 4 completa

Reglas cerradas para el contrato externo v1:

- el webhook usa un contrato externo propio con `request`, `result` y `source`;
- `result.message` solo aplica cuando el resultado puede materializarse a texto + attachments inline;
- los attachments externos usan:
  - `name`
  - `mime_type`
  - `size_bytes`
  - `content_base64`
- si el resultado terminal no puede materializarse al contrato externo, `IO.api` degrada a `result.status = "error"`;
- los límites v1 de attachments inline se apoyan en la política actual de `IoTextBlobConfig`.

Quedó implementado en código el 2026-04-20:

- validación de `auth.api_keys[].integration_id` en `io-common`
- validación de `integrations[*]` y consistencia `api_key -> integration`
- carga runtime de integraciones autenticadas en `IO.api`
- resolución de `integration_id` durante autenticación bearer
- helper `build_webhook_post_reply_target(...)` en `io-common`
- helper `extract_webhook_post_target(...)` en `io-common`
- adjunto automático de `reply_target.kind = "webhook_post"` en el ingress de `IO.api` cuando la integración tiene webhook habilitado
- mantenimiento de `io_api_noop` cuando no hay webhook habilitado
- materialización inicial de mensaje terminal Fluxbee -> contrato externo de webhook
- resolución outbound de `text/v1` y attachments inline `content_base64`
- envío HTTP firmado con `HMAC-SHA256`
- validación de éxito solo ante `2xx`
- consumo de mensajes entrantes con `reply_target.kind = "webhook_post"` en el loop del router de `IO.api`
- retries v1 en memoria con:
  - `timeout_ms`
  - `max_retries`
  - backoff exponencial con jitter
  - `delivery_id` estable
  - payload congelado por callback

Pendiente explícito:

- `WOB-R5.7` todavía no cierra una política fina de límite global del body HTTP webhook ya materializado
- hoy la implementación se apoya en la materialización vigente de texto + attachments inline, pero ese corte total de payload sigue siendo una decisión pendiente de producto/técnica

---

## Criterio de aceptación del primer corte

Se puede considerar implementado el primer corte si:

1. `IO.api` resuelve una `integration_id` válida desde la API key autenticada.
2. `IO.api` adjunta `meta.context.io.reply_target` con `kind = "webhook_post"` al flujo interno cuando corresponde.
3. `IO.api` solo intenta webhook outbound ante resultados terminales.
4. el contrato saliente del webhook usa shape externo propio y no expone blobs/ref internas de Fluxbee.
5. los attachments v1 se entregan inline como `content_base64` con metadata mínima.
6. el callback outbound se firma con `HMAC-SHA256` usando `<timestamp>.<raw_body>`.
7. `IO.api` considera éxito solo ante `2xx`.
8. existen retries acotados con `delivery_id` estable.
9. la implementación queda cubierta por tests unitarios e integración mínima con webhook fake.
10. la documentación operativa deja explícito que v1 es best-effort en memoria por proceso.
