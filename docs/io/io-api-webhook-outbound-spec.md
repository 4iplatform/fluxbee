# IO.api — Especificación técnica de webhook outbound

**Estado:** propuesta normativa para implementación  
**Fecha:** 2026-04-20  
**Audiencia:** arquitectura, backend, desarrollo, integradores, seguridad, QA  
**Aplica a:** `IO.api` como API HTTP multitenant de ingreso asíncrono  
**Objetivo:** definir el primer mecanismo canónico de salida final para `IO.api`: **webhook outbound HTTP**

---

## 0. Resumen ejecutivo

`IO.api` mantiene su naturaleza de **ingress HTTP asíncrono**:

- el sistema externo envía un mensaje a `IO.api` por HTTP;
- `IO.api` valida, autentica y acepta el ingreso;
- `IO.api` responde `202 Accepted` al caller;
- el procesamiento real continúa dentro de Fluxbee;
- si existe un resultado terminal notificable y la integración lo requiere, `IO.api` lo entrega al sistema externo mediante un **webhook outbound**.

Este documento define ese webhook outbound como el **primer método canónico** de respuesta final.

Queda explícitamente fuera de alcance en esta etapa:

- esperar la respuesta final dentro del mismo `POST /`;
- polling externo para consultar resultados;
- override del webhook por request;
- callbacks de progreso o eventos intermedios;
- múltiples reply transports de salida final;
- durabilidad cross-restart del outbox outbound.

---

## 1. Principios de diseño

### 1.1 Inbound y outbound separados

`IO.api` no es request/reply síncrono clásico. Su modelo es:

- **inbound** por HTTP;
- **procesamiento interno** por Fluxbee;
- **outbound** por webhook HTTP.

### 1.2 La respuesta final es opcional

No todo ingreso por `IO.api` requiere una salida final.

Regla:

- todo ingreso por `IO.api` **puede** resolver un canal de respuesta final;
- en esta etapa, el único canal de respuesta final soportado es `webhook`;
- la integración puede requerir o no callback final.

### 1.3 Configuración por integración autenticada, no por request

`IO.api` es una **única API multitenant**.

Por lo tanto, la configuración del webhook outbound no vive en el body de cada request. Vive en la **integración autenticada** resuelta por la credencial de ingreso.

Esto evita:

- exfiltración de respuestas a URLs arbitrarias;
- ambigüedad operativa;
- mezcla entre payload operativo y configuración de integración;
- pérdida de auditabilidad.

### 1.4 Contrato externo distinto del payload canónico interno

Fluxbee internamente sigue usando su contrato canónico de mensajes y payloads.

El webhook outbound de `IO.api` **no** debe exponer crudo ese contrato interno al sistema externo. Debe materializar un **contrato externo de entrega** orientado a integraciones HTTP.

Esto aplica especialmente a:

- `payload.type = "text"` interno;
- `content_ref`;
- `attachments[]` con referencias internas a blobs.

### 1.5 Carrier interno de reply target

La resolución del callback outbound debe integrarse con el carrier IO ya previsto en el repositorio:

- `meta.context.io.reply_target`

Para webhook outbound, el kind normativo de esta etapa es:

- `reply_target.kind = "webhook_post"`

### 1.6 Seguridad por firma HMAC

La autenticación canónica del webhook outbound es mediante:

- secreto compartido por integración;
- firma `HMAC-SHA256` del request saliente;
- headers estables y versionados.

### 1.7 Entrega best-effort con retries acotados

El webhook outbound es un mecanismo **best-effort con retries acotados**.

En v1:

- la cola outbound puede vivir en memoria por proceso;
- no se promete durabilidad cross-restart;
- no reemplaza una cola confiable externa del cliente.

---

## 2. Modelo conceptual

### 2.1 Flujo general

```text
Cliente externo
   │
   │ POST /  (Authorization: Bearer <api-key>)
   ▼
IO.api
   │
   │ 202 Accepted
   ▼
Fluxbee interno (AI / WF / frontdesk / otros)
   │
   │ se produce un resultado terminal notificable
   ▼
IO.api
   │
   │ POST webhook outbound
   ▼
Endpoint público del cliente
```

### 2.2 Qué se considera “resultado terminal notificable”

Un resultado terminal notificable es un resultado final del procesamiento iniciado por el ingreso HTTP.

Puede ser:

1. **resultado exitoso final**
   - existe contenido final para entregar al sistema externo;
2. **resultado final con error terminal**
   - el flujo terminó y debe notificarse un cierre fallido o no exitoso.

No se incluyen en esta etapa:

- eventos intermedios de progreso;
- streaming parcial;
- callbacks parciales no terminales;
- errores transitorios mientras el flujo siga vivo o sujeto a retry interno.

### 2.3 Relación con el comportamiento esperado de los nodos IO

El webhook outbound debe seguir la semántica habitual esperada de un nodo `IO.*`:

- emitir hacia afuera solo cuando recibe o resuelve un resultado que ya corresponde entregar;
- no emitir estados intermedios como si fueran cierre final.

### 2.4 Regla operativa para `IO.api`

En esta etapa, `IO.api` no debe inferir terminalidad a partir de heurísticas débiles del payload.

La regla operativa de v1 es:

- `IO.api` solo considera elegible para webhook outbound un mensaje de salida que:
  - preserve `meta.context.io.reply_target`;
  - tenga `reply_target.kind = "webhook_post"`;
  - represente un cierre ya entregable hacia el exterior.

En otras palabras:

- el request HTTP original sigue resolviéndose con `202 Accepted`;
- el webhook no responde al ingress;
- el webhook se dispara recién cuando `IO.api` recibe el mensaje terminal de vuelta para entrega outbound.

---

## 3. Configuración por integración autenticada

### 3.1 Unidad de configuración

La configuración del webhook outbound pertenece a una **integración autenticada** resuelta por la credencial de ingreso.

Una integración autenticada representa, como mínimo:

- quién llama a `IO.api`;
- a qué tenant pertenece;
- qué política outbound aplica;
- qué webhook outbound debe usar `IO.api` para notificar resultados terminales.

### 3.2 Modelo de configuración recomendado

El camino recomendado para implementación es:

- mantener `auth.api_keys[*]` para autenticación inbound;
- introducir `integrations[*]` como unidad de configuración de integración;
- hacer que cada API key resuelva una `integration_id`.

Ejemplo conceptual:

```json
{
  "auth": {
    "mode": "api_key",
    "api_keys": [
      {
        "key_id": "partner-main",
        "tenant_id": "tnt:550e8400-e29b-41d4-a716-446655440000",
        "integration_id": "int_acme_portal",
        "token_ref": "local_file:io-api/acme.token"
      }
    ]
  },
  "integrations": [
    {
      "integration_id": "int_acme_portal",
      "tenant_id": "tnt:550e8400-e29b-41d4-a716-446655440000",
      "final_reply_required": true,
      "webhook": {
        "enabled": true,
        "url": "https://cliente.example.com/fluxbee/webhook",
        "secret_ref": "local_file:io-api/acme.webhook.secret",
        "timeout_ms": 5000,
        "max_retries": 5,
        "initial_backoff_ms": 1000,
        "max_backoff_ms": 30000
      }
    }
  ]
}
```

### 3.3 Justificación de modelo

La configuración outbound no debe colgar solo de `tenant_id`.

Motivo:

- un mismo tenant puede tener más de una integración;
- una misma integración puede rotar o multiplicar credenciales inbound;
- auth inbound y delivery outbound son responsabilidades distintas.

### 3.4 Campos mínimos requeridos

Cada integración que habilite webhook outbound debe definir al menos:

- `integration_id`
- `tenant_id`
- `final_reply_required`
- `webhook.enabled`
- `webhook.url`
- `webhook.secret_ref`
- `webhook.timeout_ms`
- `webhook.max_retries`
- `webhook.initial_backoff_ms`
- `webhook.max_backoff_ms`

### 3.5 Reglas normativas de configuración

#### 3.5.1 Regla general

- `IO.api` **MUST** resolver la configuración de webhook outbound a partir de la integración autenticada.
- `IO.api` **MUST NOT** aceptar `webhook_url`, `webhook_secret` o equivalentes dentro del body del request de ingreso.

#### 3.5.2 Habilitación

- Si `webhook.enabled=false`, la integración no debe recibir callbacks finales.
- Si `webhook.enabled=true`, `webhook.url` y `webhook.secret_ref` son obligatorios.

#### 3.5.3 URL

- `webhook.url` **MUST** ser absoluta.
- `webhook.url` **MUST** usar `https` en producción.
- `http` simple solo **MAY** permitirse en entornos explícitamente no productivos.

#### 3.5.4 Secret

- `webhook.secret_ref` **MUST** resolver un secreto accesible para `IO.api`.
- `IO.api` **MUST NOT** exponer el valor plano del secreto en `CONFIG_GET`, `STATUS`, logs ni métricas.

#### 3.5.5 Multitenancy

- La resolución del webhook **MUST** depender de la integración autenticada y no de una configuración global única de la instancia.
- Dos callers autenticados por credenciales distintas **MAY** resolver a webhooks distintos, aun si comparten tenant o instancia.

---

## 4. Carrier interno y correlación

### 4.1 Reply target interno

Al aceptar un ingreso que pueda requerir callback final, `IO.api` debe materializar un reply target interno en:

- `meta.context.io.reply_target`

Este carrier debe ser compatible con el shape ya existente de `io-common`:

```json
{
  "kind": "<string>",
  "address": "<string>",
  "params": {}
}
```

Para webhook outbound v1, el shape normativo exacto es:

```json
{
  "io": {
    "reply_target": {
      "kind": "webhook_post",
      "address": "int_acme_portal",
      "params": {
        "integration_id": "int_acme_portal",
        "tenant_id": "tnt:550e8400-e29b-41d4-a716-446655440000",
        "request_id": "req_01JXYZ...",
        "trace_id": "4c66b54c-9d53-4b44-8eb0-c9f88f8f4c1e",
        "reply_mode": "final_only"
      }
    }
  }
}
```

### 4.1.1 Semántica de campos

- `kind` **MUST** ser `webhook_post`
- `address` **MUST** ser el `integration_id`
- `params.integration_id` **MUST** repetir el mismo `integration_id` de `address`
- `params.tenant_id` **MUST** ser el tenant efectivo de la integración
- `params.request_id` **MUST** ser el identificador local del ingreso HTTP original
- `params.trace_id` **MUST** ser la correlación interna del request original
- `params.reply_mode` **MUST** ser `final_only` en v1

### 4.1.2 Campos opcionales

En v1 no se requieren más campos mínimos en `params`.

Se **MAY** agregar campos pasivos adicionales si no alteran la semántica canónica del carrier, pero `IO.api` no debe depender de ellos para resolver el callback básico.

### 4.2 Regla de secretos

El mensaje interno no debe transportar:

- `webhook.url`
- `webhook.secret`
- headers prefirmados

Esos valores deben resolverse en `IO.api` al momento de la entrega outbound, a partir de `integration_id`.

Regla adicional:

- `reply_target` **MUST NOT** transportar secretos, headers firmados ni configuración sensible derivable del secreto
- `reply_target` **MUST NOT** convertirse en una copia de la configuración completa de la integración

### 4.3 Correlación mínima

Para cada entrega final, `IO.api` debe poder reconstruir al menos:

- `integration_id`
- `tenant_id`
- `request_id`
- `trace_id`
- `ilk` efectivo si aplica

### 4.4 Regla de preservación

Cuando `IO.api` adjunta `meta.context.io.reply_target` al ingreso interno:

- los hops intermedios no deben destruirlo ni reemplazarlo arbitrariamente;
- el mensaje terminal que vuelve a `IO.api` debe conservarlo si el callback final todavía depende de ese carrier;
- los nodos intermedios no deben reinterpretar `reply_target.kind = "webhook_post"` como si ellos fueran el emisor outbound;
- la resolución real del webhook sigue siendo responsabilidad de `IO.api`.

---

## 5. Contrato HTTP del webhook outbound

### 5.1 Método y content type

El callback outbound se entrega como:

```http
POST <webhook.url>
Content-Type: application/json
```

### 5.2 Semántica de respuesta del receptor

El endpoint del cliente debe responder:

- `2xx` -> entrega aceptada por el receptor;
- cualquier otro status -> entrega no aceptada;
- timeout o error de red -> entrega no aceptada.

Regla:

- `IO.api` **MUST** considerar una entrega exitosa solo cuando recibe una respuesta `2xx`.

---

## 6. Payload del webhook outbound

### 6.1 Regla general

El payload del webhook outbound es un **contrato externo de integración**.

No debe exponer:

- `payload.type = "text"` del contrato interno Fluxbee como si fuera contrato externo del webhook;
- `content_ref`;
- `BlobRef` internos;
- referencias que requieran acceso al storage interno de Fluxbee.

### 6.2 Forma general

Payload JSON normativo para v1:

```json
{
  "schema_version": 1,
  "delivery_id": "dly_01JXYZABCDEFG1234567890",
  "event_type": "io.api.final",
  "occurred_at": "2026-04-20T12:00:00Z",
  "integration_id": "int_acme_portal",
  "tenant_id": "tnt:550e8400-e29b-41d4-a716-446655440000",
  "request": {
    "request_id": "req_01JXYZ...",
    "trace_id": "4c66b54c-9d53-4b44-8eb0-c9f88f8f4c1e",
    "ilk": "ilk:3e47407a-cd56-4790-8044-aa118e3e3dfc"
  },
  "result": {
    "final": true,
    "status": "ok",
    "message": {
      "text": "Su solicitud fue aprobada.",
      "attachments": []
    },
    "error": null
  },
  "source": {
    "node_name": "IO.api.portal@worker-220"
  }
}
```

### 6.3 Campos mínimos

#### Top-level

- `schema_version` — versión del contrato de webhook
- `delivery_id` — id único y estable de esta entrega outbound
- `event_type` — tipo de evento
- `occurred_at` — timestamp UTC ISO-8601 del evento
- `integration_id` — integración autenticada que originó el callback
- `tenant_id` — tenant lógico asociado al flujo
- `request` — identificadores correlativos del ingreso original
- `result` — resultado terminal notificable
- `source` — origen técnico de la entrega

#### `request`

- `request_id` — identificador HTTP local del ingreso original
- `trace_id` — correlación interna de Fluxbee
- `ilk` — ILK efectivo resuelto para el flujo, si aplica

#### `result`

- `final` — siempre `true` en esta etapa
- `status` — `ok` o `error`
- `message` — contenido final cuando existe
- `error` — estructura de error cuando el resultado final es fallido

### 6.4 Shape de `message`

Si `result.status="ok"`, `message` debe usar el siguiente contrato externo:

```json
{
  "text": "Su pedido ya fue despachado.",
  "attachments": [
    {
      "name": "factura.pdf",
      "mime_type": "application/pdf",
      "size_bytes": 123456,
      "content_base64": "<base64>"
    }
  ]
}
```

### 6.4.1 Regla de compatibilidad de contenido en v1

En v1, `IO.api` solo debe materializar `result.message` exitoso cuando pueda resolver un resultado terminal compatible con este contrato externo:

- texto final resoluble como string;
- cero o más attachments materializables inline;
- metadata suficiente para el receptor externo.

No es válido en v1 exponer hacia el webhook:

- payloads internos no textuales sin transformación explícita;
- `content_ref`;
- `BlobRef` internos;
- referencias opacas que el receptor no pueda resolver fuera de Fluxbee.

### 6.4.2 Política para resultados no textuales o incompatibles

Si el resultado terminal interno no puede materializarse al contrato externo de `message`, `IO.api` no debe enviar un `status="ok"` ambiguo.

Debe degradar a error terminal del webhook con un código explícito, por ejemplo:

```json
{
  "code": "unsupported_outbound_payload",
  "message": "The terminal result could not be materialized to the webhook payload contract",
  "retryable": false
}
```

Esto aplica, por ejemplo, cuando:

- el payload terminal no puede resolverse a texto;
- el payload terminal usa un contrato no soportado por el webhook v1;
- un attachment no puede materializarse de forma segura al shape externo requerido.

### 6.4.3 Límite global del body webhook

En esta etapa queda definido el comportamiento para:

- texto resoluble;
- attachments inline materializables;
- límites por attachment y total de attachments.

Queda pendiente de definición fina una política explícita para el caso en que el body total del webhook ya materializado resulte demasiado grande para el transporte HTTP externo, aun cumpliendo los límites actuales de attachments.

Por lo tanto, en v1:

- este límite global de payload outbound todavía no se considera cerrado como contrato;
- la implementación actual puede entregar mientras el payload sea materializable con la política vigente;
- la definición precisa del corte por tamaño total del body queda diferida a una iteración posterior.

### 6.5 Reglas normativas del payload

#### 6.5.1 Finalidad

- Este webhook **MUST** representar solo un resultado terminal notificable.
- `result.final` **MUST** ser `true` en v1.

#### 6.5.2 Payload exitoso

Si `result.status="ok"`:

- `result.message` **MUST** estar presente;
- `result.error` **MUST** ser `null`.

#### 6.5.3 Payload de error terminal

Si `result.status="error"`:

- `result.error` **MUST** estar presente;
- `result.message` **MAY** ser `null`.

Estructura sugerida:

```json
{
  "code": "final_delivery_failed",
  "message": "The request finished with an error",
  "retryable": false
}
```

#### 6.5.3.b Mapping de terminalidad a `result.status`

En v1, el mapping mínimo normativo es:

- resultado terminal exitoso -> `result.status = "ok"`
- resultado terminal fallido -> `result.status = "error"`

`IO.api` no debe introducir en esta etapa una taxonomía más fina de estados top-level del webhook.

La distinción más detallada, si hace falta, debe vivir dentro de `result.error.code` y `result.error.retryable`.

#### 6.5.4 Attachments en v1

En v1, los attachments del webhook outbound:

- **MUST** materializarse inline como `content_base64`;
- **MUST NOT** requerir acceso al storage interno de Fluxbee;
- **MUST** incluir metadata suficiente para interpretación del receptor.

Campos mínimos por attachment:

- `name`
- `mime_type`
- `size_bytes`
- `content_base64`

#### 6.5.4.b Límites de attachments inline

En v1, la materialización outbound de attachments debe respetar límites explícitos.

La implementación **SHOULD** reutilizar como base la política ya existente de `IoTextBlobConfig` mientras no exista una configuración outbound separada.

Límites por defecto coherentes con el stack actual:

- `max_attachments = 8`
- `max_attachment_bytes = 20 MiB`
- `max_total_attachment_bytes = 40 MiB`

Reglas:

- si un attachment individual supera `max_attachment_bytes`, el resultado outbound **MUST** degradar a error terminal;
- si la suma supera `max_total_attachment_bytes`, el resultado outbound **MUST** degradar a error terminal;
- si el mime no está permitido, el resultado outbound **MUST** degradar a error terminal.

Código sugerido de error:

```json
{
  "code": "unsupported_outbound_attachment",
  "message": "One or more attachments could not be materialized for webhook delivery",
  "retryable": false
}
```

#### 6.5.5 `delivery_id`

- `delivery_id` **MUST** ser único por entrega lógica final.
- `delivery_id` **MUST** mantenerse estable entre retries del mismo callback.

Esto permite idempotencia del lado del receptor.

---

## 7. Headers del webhook outbound

### 7.1 Headers obligatorios

`IO.api` **MUST** enviar los siguientes headers:

```http
Content-Type: application/json
X-Fluxbee-Delivery-Id: dly_01JXYZABCDEFG1234567890
X-Fluxbee-Timestamp: 1776331200
X-Fluxbee-Signature: v1=<hex_hmac_sha256>
```

### 7.2 Significado

- `X-Fluxbee-Delivery-Id` — identificador estable del callback
- `X-Fluxbee-Timestamp` — epoch seconds UTC usados para la firma
- `X-Fluxbee-Signature` — firma HMAC versionada

---

## 8. Firma HMAC

### 8.1 Objetivo

La firma HMAC permite al receptor verificar:

- autenticidad: el emisor conoce el secreto compartido;
- integridad: el body no fue alterado en tránsito;
- frescura temporal: el request no es un replay arbitrario fuera de ventana.

### 8.2 Algoritmo canónico

Algoritmo:

- `HMAC-SHA256`

Encoding del resultado:

- hexadecimal lowercase

Formato del header:

```text
X-Fluxbee-Signature: v1=<hex>
```

### 8.3 String a firmar

El mensaje canónico a firmar es:

```text
<timestamp>.<raw_body>
```

Donde:

- `<timestamp>` es exactamente el valor de `X-Fluxbee-Timestamp`;
- `.` es un punto ASCII literal;
- `<raw_body>` es el cuerpo HTTP exacto, en bytes, tal como se envía.

### 8.4 Reglas críticas de implementación

#### 8.4.1 Raw body

- El emisor y el receptor **MUST** usar el **raw body exacto**.
- El receptor **MUST NOT** parsear y reserializar JSON antes de validar la firma.

No es válido:

- cambiar espacios;
- reordenar claves;
- normalizar el JSON.

#### 8.4.2 Comparación segura

- El receptor **MUST** usar comparación en tiempo constante para verificar la firma.

#### 8.4.3 Ventana temporal

- El receptor **SHOULD** rechazar requests cuyo timestamp esté fuera de una ventana aceptable.
- Ventana recomendada: `±300s`.

#### 8.4.4 Versionado

- El prefijo `v1=` **MUST** formar parte del formato del header.
- Futuras versiones de firma **MAY** introducir nuevos esquemas sin romper compatibilidad.

---

## 9. Verificación del lado del cliente

### 9.1 Pasos de validación recomendados

Al recibir un webhook, el cliente debería:

1. leer `X-Fluxbee-Timestamp`;
2. leer `X-Fluxbee-Signature`;
3. leer el raw body exacto;
4. verificar que el timestamp esté dentro de la ventana permitida;
5. recalcular `HMAC-SHA256(secret, timestamp + "." + raw_body)`;
6. comparar con la firma recibida;
7. si coincide, procesar el payload;
8. usar `delivery_id` para idempotencia.

### 9.2 Errores típicos a evitar

- parsear JSON y volverlo a serializar antes de validar;
- olvidar incluir el timestamp en la firma;
- usar el secret directamente como bearer en vez de firma;
- no tratar `delivery_id` como clave idempotente.

---

## 10. Retries e idempotencia

### 10.1 Regla general

Si el webhook outbound no es aceptado por el receptor, `IO.api` **MUST** reintentar hasta el máximo configurado.

Un callback se considera no aceptado cuando ocurre cualquiera de estos casos:

- respuesta HTTP no `2xx`;
- timeout;
- error DNS, TCP o TLS;
- conexión abortada;
- cualquier error local de envío.

### 10.2 Política sugerida

Campos configurables por integración:

- `timeout_ms`
- `max_retries`
- `initial_backoff_ms`
- `max_backoff_ms`

Comportamiento sugerido:

- backoff exponencial con jitter;
- `delivery_id` estable entre todos los retries del mismo callback;
- sin mutar el payload entre retries.

### 10.3 Idempotencia del receptor

El receptor del webhook **SHOULD** tratar `delivery_id` como clave idempotente.

Regla:

- si recibe el mismo `delivery_id` más de una vez, debe poder responder `2xx` sin reprocesar lógicamente el evento.

### 10.4 Durabilidad de v1

En v1, la entrega outbound:

- **MAY** implementarse con outbox en memoria por proceso;
- **MUST NOT** prometer durabilidad cross-restart si esa capacidad no existe;
- **SHOULD** documentarse como best-effort con retries acotados.

---

## 11. Estado cuando no hay webhook configurado

### 11.1 Caso sin salida final

Si una integración no declara webhook outbound:

- el ingreso HTTP puede seguir existiendo;
- `IO.api` puede aceptar el request y procesarlo internamente;
- no existe obligación de notificar resultado final al exterior.

### 11.2 Regla normativa

- La ausencia de webhook outbound **MUST NOT** romper el ingreso HTTP por sí sola.
- Pero `IO.api` **MUST** comportarse de forma consistente con la política de la integración.

Ejemplos válidos:

- integración que explícitamente no requiere callback final;
- integración que sí requiere callback y por lo tanto es inválida si `webhook.enabled=false`.

---

## 12. Seguridad y observabilidad

### 12.1 TLS

- `IO.api` **MUST** validar TLS del endpoint remoto en producción.
- `IO.api` **MUST NOT** deshabilitar validación TLS salvo en entornos de desarrollo explícitos.

### 12.2 Logs

`IO.api` **SHOULD** loguear:

- `integration_id`
- `delivery_id`
- `request_id`
- `trace_id`
- URL destino redaccionada cuando corresponda
- status HTTP
- cantidad de intento
- latencia

`IO.api` **MUST NOT** loguear:

- secretos resueltos de webhook
- firma completa si la política de seguridad lo desaconseja
- payloads sensibles completos sin redacción apropiada

### 12.3 Métricas recomendadas

- callbacks enviados
- callbacks exitosos
- callbacks fallidos
- retries por integración
- latencia outbound
- porcentaje de aceptación `2xx`
- expiración por timeout

---

## 13. Contrato de producto propuesto para v1

### 13.1 Decisiones cerradas

1. `IO.api` mantiene `POST /` asincrónico.
2. La respuesta final no vuelve en la misma request HTTP.
3. El primer y único mecanismo de salida final es `webhook`.
4. El webhook se configura por **integración autenticada**, no por request.
5. El modelo recomendado es `auth.api_keys[*]` + `integrations[*]`.
6. El carrier interno es `meta.context.io.reply_target` con `kind = "webhook_post"`.
7. El payload outbound es un contrato externo de integración y no reutiliza crudo el payload interno canónico de Fluxbee.
8. Los attachments v1 se entregan inline como `content_base64` con metadata suficiente.
9. La seguridad canónica del webhook es `HMAC-SHA256` con headers versionados.
10. La entrega usa retries acotados e idempotencia por `delivery_id`.
11. Solo se notifican resultados terminales.
12. La entrega v1 es best-effort en memoria por proceso.

### 13.1.b Regla de terminalidad para implementación

Para considerar un callback webhook como válido en v1, deben cumplirse al menos estas condiciones:

1. `IO.api` ya respondió `202 Accepted` al ingress original.
2. El mensaje de salida que vuelve a `IO.api` conserva `meta.context.io.reply_target`.
3. `reply_target.kind = "webhook_post"`.
4. El mensaje representa un cierre entregable hacia el exterior, no un estado parcial.
5. `IO.api` puede mapear el resultado a:
   - `status = "ok"`, o
   - `status = "error"`.

### 13.2 Fuera de alcance de esta etapa

- múltiples métodos de reply transport;
- callbacks intermedios de progreso;
- streaming;
- override del endpoint por request;
- soporte general para entregar la salida final vía otros `IO.*`;
- `download_url` como mecanismo obligatorio de attachments;
- durabilidad cross-restart del outbox outbound.

---

## 14. Ejemplo completo

### 14.1 Ingreso HTTP

```http
POST / HTTP/1.1
Authorization: Bearer fk_live_abc123
Content-Type: application/json

{
  "message": "Necesito saber el estado de mi pedido"
}
```

Respuesta inmediata:

```http
202 Accepted
Content-Type: application/json

{
  "status": "accepted",
  "request_id": "req_01JXYZ...",
  "trace_id": "4c66b54c-9d53-4b44-8eb0-c9f88f8f4c1e"
}
```

### 14.2 Callback final saliente

```http
POST https://cliente.example.com/fluxbee/webhook HTTP/1.1
Content-Type: application/json
X-Fluxbee-Delivery-Id: dly_01JXYZABCDEFG1234567890
X-Fluxbee-Timestamp: 1776686400
X-Fluxbee-Signature: v1=8f2c0f...

{
  "schema_version": 1,
  "delivery_id": "dly_01JXYZABCDEFG1234567890",
  "event_type": "io.api.final",
  "occurred_at": "2026-04-20T12:00:00Z",
  "integration_id": "int_acme_portal",
  "tenant_id": "tnt:550e8400-e29b-41d4-a716-446655440000",
  "request": {
    "request_id": "req_01JXYZ...",
    "trace_id": "4c66b54c-9d53-4b44-8eb0-c9f88f8f4c1e",
    "ilk": "ilk:3e47407a-cd56-4790-8044-aa118e3e3dfc"
  },
  "result": {
    "final": true,
    "status": "ok",
    "message": {
      "text": "Su pedido ya fue despachado.",
      "attachments": [
        {
          "name": "factura.pdf",
          "mime_type": "application/pdf",
          "size_bytes": 123456,
          "content_base64": "<base64>"
        }
      ]
    },
    "error": null
  },
  "source": {
    "node_name": "IO.api.portal@worker-220"
  }
}
```

Respuesta esperada del receptor:

```http
200 OK
```

---

## 15. Texto corto para alinear al equipo

`IO.api` debe tratar el webhook outbound como parte del contrato de integración multitenant resuelto por la credencial autenticada. El caller entra por una sola API, recibe `202`, y si existe un resultado terminal notificable, éste se entrega por callback HTTP firmado con HMAC al endpoint configurado para esa integración. El endpoint no se define por request; se define por integración. El payload saliente es un contrato externo de webhook, no una exposición cruda del payload interno de Fluxbee. Los attachments de v1 se entregan inline en base64 con metadata suficiente. El éxito del callback se considera solo ante `2xx`, con retries acotados e idempotencia por `delivery_id`.
