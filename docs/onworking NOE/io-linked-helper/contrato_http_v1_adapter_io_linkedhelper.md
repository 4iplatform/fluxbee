# Contrato HTTP v1 — `adapter ↔ IO.linkedhelper`

> Estado: draft implementable para MVP
>
> Objetivo: congelar un contrato HTTP/HTTPS concreto para que pueda arrancar la implementación del nodo `IO.linkedhelper` y del adapter compatible.

---

## 1. Alcance

Este documento fija para el MVP:

- endpoint HTTP único de polling,
- autenticación mínima,
- envelope request/response,
- tipos de request soportados,
- tipos de respuesta soportados,
- reglas mínimas de correlación,
- ejemplos JSON implementables.

Queda fuera de alcance de esta v1:

- retry/deduplicación detallados,
- rotación de credenciales,
- adjuntos,
- edición/cancelación de mensajes,
- payloads no conversacionales avanzados,
- versionado fino de acciones salientes hacia Linked Helper.

---

## 2. Transporte y autenticación

### 2.1. Transporte

- Protocolo de aplicación: `HTTP`
- Protección en tránsito: `HTTPS`
- Content type: `application/json`

### 2.2. Endpoint

Se define un único endpoint para el MVP:

- `POST /v1/poll`

Semántica:

- el adapter siempre inicia la comunicación,
- el mismo endpoint sirve tanto para enviar eventos como para hacer heartbeat,
- la respuesta puede traer `ack`, `result` y/o `heartbeat`.

### 2.3. Headers requeridos

Todos los requests al endpoint deben incluir:

- `Content-Type: application/json`
- `Authorization: Bearer <installation_key>`
- `X-Fluxbee-Adapter-Id: <adapter_id>`

### 2.4. Reglas de autenticación

- `adapter_id` se identifica por `X-Fluxbee-Adapter-Id`
- `installation_key` se valida desde el bearer token
- el nodo debe rechazar credenciales inválidas antes de procesar items

Errores HTTP mínimos:

- `401 Unauthorized`: adapter desconocido o key inválida
- `400 Bad Request`: request JSON inválido o schema inválido a nivel envelope
- `413 Payload Too Large`: batch excede límites configurados
- `500 Internal Server Error`: fallo interno del nodo antes de poder devolver respuesta de protocolo

Regla de operación:

- si el request pasa autenticación y envelope general, el nodo debe intentar responder `200 OK` aunque existan errores por item.

---

## 3. Envelope de request

## 3.1. Shape general

```json
{
  "schema_version": 1,
  "request_id": "lhreq_000001",
  "adapter_id": "lh_adapter_sales_01",
  "mode": "events",
  "sent_at": "2026-04-27T15:10:00Z",
  "items": []
}
```

### 3.2. Campos del envelope

| Campo | Tipo | Requerido | Descripción |
|---|---|---:|---|
| `schema_version` | integer | sí | Versión del contrato. MVP = `1` |
| `request_id` | string | sí | ID único del poll |
| `adapter_id` | string | sí | Debe coincidir con `X-Fluxbee-Adapter-Id` |
| `mode` | string | sí | `events` o `heartbeat` |
| `sent_at` | string RFC3339 | sí | Timestamp del adapter |
| `items` | array | sí | Lista de eventos; vacía en `heartbeat` |

### 3.3. Reglas del envelope

- `schema_version` debe ser `1`
- `adapter_id` del body debe coincidir con el header
- si `mode = "heartbeat"`, `items` debe ser `[]`
- si `mode = "events"`, `items` debe contener al menos un item

---

## 4. Items salientes del adapter

## 4.1. Tipos soportados en v1

Los únicos tipos soportados por el MVP son:

- `avatar_create`
- `avatar_update`
- `conversation_message`

El heartbeat se modela a nivel envelope usando `mode = "heartbeat"`.

## 4.2. Campos comunes de item

```json
{
  "item_id": "evt_000001",
  "type": "conversation_message",
  "occurred_at": "2026-04-27T15:09:55Z",
  "adapter_avatar_id": "1",
  "payload": {}
}
```

| Campo | Tipo | Requerido | Descripción |
|---|---|---:|---|
| `item_id` | string | sí | ID único del item dentro del adapter |
| `type` | string | sí | Tipo de evento |
| `occurred_at` | string RFC3339 | sí | Momento del hecho observado |
| `adapter_avatar_id` | string | sí | ID del avatar dentro del adapter |
| `payload` | object | sí | Carga específica del tipo |

Reglas:

- `item_id` debe ser único por adapter
- `adapter_avatar_id` es el identificador canónico del avatar dentro del adapter
- `adapter_avatar_id` debe existir en `avatar_create`, `avatar_update` y `conversation_message`

---

## 5. Payloads de eventos v1

## 5.1. `avatar_create`

```json
{
  "item_id": "evt_avatar_create_001",
  "type": "avatar_create",
  "occurred_at": "2026-04-27T15:10:00Z",
  "adapter_avatar_id": "1",
  "payload": {
    "display_name": "John Doe",
    "email": "john@example.com",
    "company": "Acme",
    "language": "en",
    "timezone": "America/New_York",
    "metadata": {
      "lh_account_name": "sales-east"
    }
  }
}
```

Campos mínimos del payload:

- `display_name`
- `metadata` opcional

Campos opcionales útiles:

- `email`
- `company`
- `language`
- `timezone`
- `metadata`

## 5.2. `avatar_update`

`avatar_update` usa el mismo shape de payload que `avatar_create`, pero con semántica de snapshot completo.

```json
{
  "item_id": "evt_avatar_update_001",
  "type": "avatar_update",
  "occurred_at": "2026-04-27T15:11:00Z",
  "adapter_avatar_id": "1",
  "payload": {
    "display_name": "John Doe",
    "email": "john@example.com",
    "company": "Acme Updated",
    "language": "en",
    "timezone": "America/New_York",
    "metadata": {
      "lh_account_name": "sales-east"
    }
  }
}
```

## 5.3. `conversation_message`

```json
{
  "item_id": "evt_msg_000001",
  "type": "conversation_message",
  "occurred_at": "2026-04-27T15:12:00Z",
  "adapter_avatar_id": "1",
  "payload": {
    "lh_contact_id": "66",
    "contact": {
      "display_name": "Jane Smith",
      "email": null,
      "company": "Globex",
      "metadata": {
        "linkedin_profile_url": "https://linkedin.example/jane"
      }
    },
    "conversation": {
      "external_thread_id": "thread_123",
      "external_message_id": "msg_555"
    },
    "message": {
      "direction": "contact_to_avatar",
      "text": "Hola, me interesa conocer más.",
      "content_type": "text/plain"
    }
  }
}
```

Campos obligatorios del payload:

- `lh_contact_id`
- `contact.display_name`
- `message.direction`
- `message.text`

Restricciones v1:

- `message.direction` debe ser `contact_to_avatar`
- solo se soporta texto inline
- no se soportan adjuntos

### 5.3.1. `external_id` canónico del contacto

El nodo debe formar el `external_id` canónico del contacto como:

- `<adapter_avatar_id>_<lh_contact_id>`

Ejemplo:

- `adapter_avatar_id = "1"`
- `lh_contact_id = "66"`
- `external_id = "1_66"`

---

## 6. Envelope de response

## 6.1. Shape general

```json
{
  "schema_version": 1,
  "request_id": "lhreq_000001",
  "adapter_id": "lh_adapter_sales_01",
  "responded_at": "2026-04-27T15:10:01Z",
  "items": []
}
```

Campos:

| Campo | Tipo | Requerido | Descripción |
|---|---|---:|---|
| `schema_version` | integer | sí | Versión del contrato |
| `request_id` | string | sí | Echo del request |
| `adapter_id` | string | sí | Adapter destinatario |
| `responded_at` | string RFC3339 | sí | Timestamp del nodo |
| `items` | array | sí | ACKs, results y/o heartbeat |

Regla:

- una respuesta puede mezclar elementos originados en el request actual y resultados pendientes de requests anteriores del mismo adapter

---

## 7. Tipos de response v1

## 7.1. `ack`

```json
{
  "type": "ack",
  "request_item_id": "evt_msg_000001",
  "status": "accepted"
}
```

Campos:

- `type = "ack"`
- `request_item_id`
- `status`

Valores permitidos de `status` en v1:

- `accepted`
- `rejected`

Uso:

- `accepted` indica que el nodo aceptó el item para procesamiento
- `rejected` indica rechazo inmediato a nivel de validación item-level

## 7.2. `result`

```json
{
  "type": "result",
  "request_item_id": "evt_msg_000001",
  "status": "success",
  "result_type": "conversation_output",
  "payload": {
    "text": "Gracias por tu mensaje. Te contacto en breve."
  },
  "error_code": null,
  "error_message": null,
  "retryable": false
}
```

Campos:

| Campo | Tipo | Requerido | Descripción |
|---|---|---:|---|
| `type` | string | sí | `result` |
| `request_item_id` | string | sí | Item original correlacionado |
| `status` | string | sí | `success` o `error` |
| `result_type` | string | sí | Tipo semántico de resultado |
| `payload` | object/null | no | Resultado útil |
| `error_code` | string/null | no | Código de error |
| `error_message` | string/null | no | Mensaje de error |
| `retryable` | boolean | no | Hint operativo para el adapter |

`result_type` mínimos de v1:

- `avatar_create_applied`
- `avatar_update_applied`
- `conversation_output`
- `validation_error`
- `identity_error`
- `routing_error`
- `blocked_avatar`

Reglas:

- si `status = "success"`, `payload` debería contener el resultado útil cuando aplique
- si `status = "error"`, deben informarse `error_code` y `error_message` cuando sea posible

## 7.3. `heartbeat`

```json
{
  "type": "heartbeat"
}
```

Uso:

- cuando el nodo no tiene ningún `ack` ni `result` para devolver
- como respuesta mínima a un poll vacío sin pendientes

Regla:

- si la respuesta contiene otros items útiles, no hace falta incluir `heartbeat`

---

## 8. Semántica de procesamiento

## 8.1. Heartbeat

Request:

```json
{
  "schema_version": 1,
  "request_id": "lhreq_000100",
  "adapter_id": "lh_adapter_sales_01",
  "mode": "heartbeat",
  "sent_at": "2026-04-27T15:20:00Z",
  "items": []
}
```

Respuesta sin pendientes:

```json
{
  "schema_version": 1,
  "request_id": "lhreq_000100",
  "adapter_id": "lh_adapter_sales_01",
  "responded_at": "2026-04-27T15:20:00Z",
  "items": [
    {
      "type": "heartbeat"
    }
  ]
}
```

## 8.2. Poll con eventos

Request:

```json
{
  "schema_version": 1,
  "request_id": "lhreq_000101",
  "adapter_id": "lh_adapter_sales_01",
  "mode": "events",
  "sent_at": "2026-04-27T15:21:00Z",
  "items": [
    {
      "item_id": "evt_avatar_create_001",
      "type": "avatar_create",
      "occurred_at": "2026-04-27T15:20:58Z",
      "adapter_avatar_id": "1",
      "payload": {
        "display_name": "John Doe",
        "email": "john@example.com",
        "company": "Acme",
        "language": "en",
        "timezone": "America/New_York",
        "metadata": {}
      }
    },
    {
      "item_id": "evt_msg_000001",
      "type": "conversation_message",
      "occurred_at": "2026-04-27T15:20:59Z",
      "adapter_avatar_id": "1",
      "payload": {
        "lh_contact_id": "66",
        "contact": {
          "display_name": "Jane Smith",
          "email": null,
          "company": "Globex",
          "metadata": {}
        },
        "conversation": {
          "external_thread_id": "thread_123",
          "external_message_id": "msg_555"
        },
        "message": {
          "direction": "contact_to_avatar",
          "text": "Hola, me interesa conocer más.",
          "content_type": "text/plain"
        }
      }
    }
  ]
}
```

Respuesta inmediata posible:

```json
{
  "schema_version": 1,
  "request_id": "lhreq_000101",
  "adapter_id": "lh_adapter_sales_01",
  "responded_at": "2026-04-27T15:21:00Z",
  "items": [
    {
      "type": "ack",
      "request_item_id": "evt_avatar_create_001",
      "status": "accepted"
    },
    {
      "type": "ack",
      "request_item_id": "evt_msg_000001",
      "status": "accepted"
    }
  ]
}
```

Respuesta posterior posible en otro poll:

```json
{
  "schema_version": 1,
  "request_id": "lhreq_000102",
  "adapter_id": "lh_adapter_sales_01",
  "responded_at": "2026-04-27T15:21:10Z",
  "items": [
    {
      "type": "result",
      "request_item_id": "evt_avatar_create_001",
      "status": "success",
      "result_type": "avatar_create_applied",
      "payload": {
        "avatar_ilk": "ilk:11111111-1111-1111-1111-111111111111"
      },
      "retryable": false
    },
    {
      "type": "result",
      "request_item_id": "evt_msg_000001",
      "status": "success",
      "result_type": "conversation_output",
      "payload": {
        "text": "Gracias por tu mensaje. Te contacto en breve."
      },
      "retryable": false
    }
  ]
}
```

---

## 9. Límites y comportamiento mínimo del nodo

Para el MVP, el nodo debe poder configurar al menos:

- límite de items por request,
- límite de bytes por request,
- timeout de request HTTP,
- adapters habilitados,
- installation key por adapter,
- tenant y destino inicial por adapter o por config equivalente.

Reglas mínimas:

- si el envelope general es inválido, se responde error HTTP
- si solo uno o más items son inválidos, se intenta responder `200` con `ack rejected` y/o `result error`
- el nodo no debe mezclar pendientes de distintos adapters
- el nodo no debe avanzar `conversation_message` si no pudo resolver/crear el contacto

---

## 10. Temas explícitamente pendientes

- persistencia durable de la cola de pendientes
- garantía de no pérdida ante reinicio del nodo
- política de retry
- política de deduplicación
- versionado de acciones salientes no conversacionales
- soporte de adjuntos
- edición/cancelación de mensajes
- operación oficial de `ILK_UPDATE` en core

## 11. Nota de robustez del MVP

La primera implementación del mecanismo de pendientes puede operar con cola en memoria.

Eso es aceptable para validar el circuito del MVP, pero tiene limitaciones claras:

- no hay durabilidad ante reinicio del proceso,
- no hay replay confiable tras crash,
- no hay garantía de entrega de resultados ya encolados si el nodo cae.

Por lo tanto:

- esta implementación sirve para desarrollo y validación temprana,
- pero no debe considerarse suficiente para producción.
