# IO.api - Contrato HTTP y ejemplos

**Estado:** draft propuesto
**Audiencia:** desarrollo, integraciones, QA, documentacion oficial
**Documento complementario de:** `io-api-node-spec.md`

---

## 1. Proposito

Este documento complementa la especificacion general de `IO.api` con:

- el contrato de `POST /`;
- la respuesta esperada del endpoint;
- el comportamiento de `GET /`;
- ejemplos minimos por `subject_mode`;
- el camino de attachments desde dia 0.

La especificacion general sigue siendo la fuente de verdad sobre arquitectura, estados, relay, identidad y operacion.

---

## 2. Convenciones generales

### 2.1 Endpoint principal

```text
POST /
```

### 2.2 Endpoint de introspeccion

```text
GET /
```

Aliases legacy compatibles:

```text
POST /messages
GET /schema
```

### 2.3 Content types soportados

Minimos soportados:

- `application/json`
- `multipart/form-data`

Regla:

- `application/json` se usa para requests sin binarios;
- `multipart/form-data` se usa para requests con attachments binarios.

### 2.4 Autenticacion

Cuando la instancia esta configurada con `auth.mode = api_key`, el request debe incluir:

```http
Authorization: Bearer <api-key>
```

La credencial autentica al caller del endpoint.

No autentica automaticamente al sujeto conversacional salvo que la instancia este en `subject_mode = caller_is_subject`.

---

## 3. Respuesta general de `POST /`

`IO.api` no espera el resultado final del procesamiento dentro de Fluxbee, pero `subject_mode = explicit_subject` puede esperar la etapa de identity antes de devolver respuesta.

### 3.1 Caso exitoso

Si el request:

- fue autenticado;
- valido schema;
- supero la etapa de identity requerida por el `subject_mode` efectivo;
- y pudo ingresar al flujo interno del nodo;

el endpoint debe responder:

```http
202 Accepted
```

Body sugerido:

```json
{
  "status": "accepted",
  "request_id": "req_01JXYZ...",
  "trace_id": "4c66b54c-9d53-4b44-8eb0-c9f88f8f4c1e",
  "ilk": "ilk:3e47407a-cd56-4790-8044-aa118e3e3dfc",
  "relay_status": "held",
  "node_name": "IO.api.frontdesk@worker-220"
}
```

Notas:

- `request_id` es identificador HTTP local del nodo;
- `trace_id` es el identificador de correlacion del mensaje interno;
- en `explicit_subject`, `ilk` debe devolverse cuando identity resolvio o creo exitosamente el sujeto;
- `relay_status` puede ser:
  - `held`
  - `flushed_immediately`

### 3.2 Errores frecuentes

#### Request mal formado

```http
400 Bad Request
```

```json
{
  "status": "error",
  "error_code": "invalid_json",
  "error_message": "Request body is not valid JSON"
}
```

#### Credencial ausente o invalida

```http
401 Unauthorized
```

```json
{
  "status": "error",
  "error_code": "unauthorized",
  "error_message": "Missing or invalid bearer token"
}
```

#### Tenant de la key inexistente

```http
403 Forbidden
```

```json
{
  "status": "error",
  "error_code": "tenant_not_found",
  "error_message": "Authenticated API key tenant 'tnt:acme' does not exist"
}
```

#### Payload semantico invalido

```http
422 Unprocessable Entity
```

```json
{
  "status": "error",
  "error_code": "invalid_payload",
  "error_message": "Payload does not define a valid explicit subject identification mode"
}
```

#### ILK inexistente

```http
404 Not Found
```

```json
{
  "status": "error",
  "error_code": "ilk_does_not_exist",
  "error_message": "The provided ILK does not exist"
}
```

#### Timeout de identity

```http
504 Gateway Timeout
```

```json
{
  "status": "error",
  "error_code": "identity_timeout",
  "error_message": "Identity did not respond in time"
}
```

#### Identity unavailable

```http
503 Service Unavailable
```

```json
{
  "status": "error",
  "error_code": "identity_unavailable",
  "error_message": "Identity is currently unavailable"
}
```

#### Instancia no configurada

```http
503 Service Unavailable
```

```json
{
  "status": "error",
  "error_code": "node_not_configured",
  "error_message": "IO.api instance is not configured yet"
}
```

### 3.3 Excepcion: destino `SY.frontdesk.gov`

Si el destino efectivo del request es `SY.frontdesk.gov`, `IO.api` responde con el payload estructurado que devuelva frontdesk.

Matriz resumida:

- `frontdesk_result.status = "ok"` -> `200 OK`
- `frontdesk_result.status = "needs_input"` -> `422 Unprocessable Entity`
- `frontdesk_result.status = "error"` y `result_code = "INVALID_REQUEST"` -> `422 Unprocessable Entity`
- `frontdesk_result.status = "error"` y `result_code = "IDENTITY_UNAVAILABLE"` -> `503 Service Unavailable`
- `frontdesk_result.status = "error"` y `result_code = "REGISTER_FAILED"` -> `502 Bad Gateway`
- timeout esperando reply de frontdesk -> `504 Gateway Timeout`
- payload de reply invalido -> `502 Bad Gateway`
- fallo de transporte hacia router/frontdesk -> `503 Service Unavailable`

Caso `needs_input`:

```http
422 Unprocessable Entity
```

```json
{
  "type": "frontdesk_result",
  "schema_version": 1,
  "status": "needs_input",
  "result_code": "MISSING_REQUIRED_FIELDS",
  "human_message": "Necesito tu email para continuar.",
  "missing_fields": ["email"],
  "error_code": null,
  "error_detail": null,
  "ilk_id": "ilk:123",
  "tenant_id": "tnt:acme",
  "registration_status": "temporary"
}
```

Caso `ok`:

```http
200 OK
```

```json
{
  "type": "frontdesk_result",
  "schema_version": 1,
  "status": "ok",
  "result_code": "REGISTERED",
  "human_message": "Registro completado correctamente.",
  "missing_fields": [],
  "error_code": null,
  "error_detail": null,
  "ilk_id": "ilk:123",
  "tenant_id": "tnt:acme",
  "registration_status": "complete"
}
```

Caso `error` por request invalido de frontdesk:

```http
422 Unprocessable Entity
```

```json
{
  "type": "frontdesk_result",
  "schema_version": 1,
  "status": "error",
  "result_code": "INVALID_REQUEST",
  "human_message": "No pude procesar el registro con los datos recibidos.",
  "missing_fields": [],
  "error_code": "invalid_request",
  "error_detail": "display_name is required",
  "ilk_id": null,
  "tenant_id": "tnt:acme",
  "registration_status": "not_started"
}
```

Caso `error` por identity unavailable reportado por frontdesk:

```http
503 Service Unavailable
```

```json
{
  "type": "frontdesk_result",
  "schema_version": 1,
  "status": "error",
  "result_code": "IDENTITY_UNAVAILABLE",
  "human_message": "No puedo completar el registro en este momento.",
  "missing_fields": [],
  "error_code": "identity_unavailable",
  "error_detail": "SY.identity did not respond",
  "ilk_id": null,
  "tenant_id": "tnt:acme",
  "registration_status": "temporary"
}
```

Caso `error` por fallo de registro:

```http
502 Bad Gateway
```

```json
{
  "type": "frontdesk_result",
  "schema_version": 1,
  "status": "error",
  "result_code": "REGISTER_FAILED",
  "human_message": "No pude completar el alta.",
  "missing_fields": [],
  "error_code": "register_failed",
  "error_detail": "ILK_REGISTER returned failure",
  "ilk_id": "ilk:123",
  "tenant_id": "tnt:acme",
  "registration_status": "temporary"
}
```

Caso timeout esperando `frontdesk_result`:

```http
504 Gateway Timeout
```

```json
{
  "status": "error",
  "error_code": "frontdesk_timeout",
  "error_message": "Timed out waiting for frontdesk reply"
}
```

Caso payload invalido en la reply de frontdesk:

```http
502 Bad Gateway
```

```json
{
  "status": "error",
  "error_code": "invalid_frontdesk_response",
  "error_message": "Frontdesk reply did not match frontdesk_result contract"
}
```

Caso fallo de transporte hacia router/frontdesk:

```http
503 Service Unavailable
```

```json
{
  "status": "error",
  "error_code": "router_unavailable",
  "error_message": "Unable to dispatch request to router/frontdesk"
}
```

#### Payload demasiado grande

```http
413 Payload Too Large
```

---

## 4. `GET /`

## 4.1 Objetivo

`GET /` devuelve el contrato efectivo de la instancia actual.

No debe devolver una union abstracta de todos los modos del runtime.

## 4.2 Caso: instancia no configurada

```http
200 OK
```

```json
{
  "status": "unconfigured",
  "node_name": "IO.api.frontdesk@worker-220",
  "runtime": "IO.api",
  "contract_version": 1,
  "effective_schema": null,
  "required_configuration": [
    "listen.address",
    "listen.port",
    "auth.mode",
    "ingress.subject_mode",
    "ingress.accepted_content_types",
    "config.io.relay.window_ms"
  ]
}
```

## 4.3 Caso: instancia configurada en `explicit_subject`

```json
{
  "status": "configured",
  "node_name": "IO.api.frontdesk@worker-220",
  "runtime": "IO.api",
  "contract_version": 1,
  "auth": {
    "mode": "api_key",
    "transport": "Authorization: Bearer <token>"
  },
  "ingress": {
    "subject_mode": "explicit_subject",
    "accepted_content_types": [
      "application/json",
      "multipart/form-data"
    ]
  },
  "required_fields": {
    "json": ["subject", "message"],
    "multipart": ["metadata"]
  },
  "subject": {
    "required": false,
    "allowed": true,
    "lookup_key_field": "external_user_id",
    "tenant_source": "authenticated_api_key",
    "optional_identity_candidates": ["display_name", "email", "company_name", "attributes"]
  },
  "attachments": {
    "supported": true,
    "mode": "multipart"
  },
  "relay": {
    "config_path": "config.io.relay.*"
  }
}
```

## 4.4 Caso: instancia configurada en `caller_is_subject`

```json
{
  "status": "configured",
  "node_name": "IO.api.portal@worker-220",
  "runtime": "IO.api",
  "contract_version": 1,
  "auth": {
    "mode": "api_key",
    "transport": "Authorization: Bearer <token>"
  },
  "ingress": {
    "subject_mode": "caller_is_subject",
    "accepted_content_types": [
      "application/json",
      "multipart/form-data"
    ]
  },
  "required_fields": {
    "json": ["message"],
    "multipart": ["metadata"]
  },
  "subject": {
    "required": false,
    "allowed": false,
    "resolution": "derived_from_authenticated_caller"
  },
  "attachments": {
    "supported": true,
    "mode": "multipart"
  },
  "relay": {
    "config_path": "config.io.relay.*"
  }
}
```

---

## 5. Shape normativo de `POST /` en `application/json`

## 5.1 Envelope comun

```json
{
  "subject": { },
  "message": {
    "text": "...",
    "attachments": []
  },
  "options": {
    "relay": {
      "final": false
    },
    "routing": {
      "dst_node": "AI.chat@motherbee"
    },
    "metadata": {
      "source_system": "crm-dotnet"
    }
  }
}
```

Reglas:

- `subject` puede ser obligatorio, opcional o invalido segun `subject_mode`;
- `message` es obligatorio;
- `options` es opcional;
- `options.routing.dst_node` es opcional y, si viene, overridea el `config.io.dst_node` efectivo para ese request;
- si `options.routing.dst_node` falta o viene vacio, el nodo usa su `dst_node` configurado y, si tampoco existe, cae en `resolve`;
- `options.metadata` es metadata auxiliar del integrador y no reemplaza campos de protocolo Fluxbee.

## 5.2 Campos de `message`

Campos recomendados:

```json
{
  "text": "string",
  "external_message_id": "string opcional",
  "timestamp": "RFC3339 opcional",
  "attachments": []
}
```

Reglas:

- `text` es opcional solo si hay attachments;
- debe existir al menos uno de:
  - `message.text`
  - uno o mas attachments;
- `external_message_id` sirve para dedup y trazabilidad del lado IO.

## 5.3 Campos de `options`

```json
{
  "relay": {
    "final": false
  },
  "routing": {
    "dst_node": "AI.chat@motherbee"
  },
  "metadata": {
    "source_system": "crm-dotnet",
    "tags": ["frontdesk", "api"]
  }
}
```

Reglas:

- `relay.final=true` puede usarse como hint al relay comun;
- no se incluye `relay.bypass` como contrato normativo v1 hasta definir politica clara por instancia;
- `routing.dst_node` permite fijar un destino Fluxbee puntual para ese request sin reconfigurar la instancia;
- `routing.dst_node` debe ser string cuando esta presente;
- `metadata` es informacion auxiliar del integrador.

---

## 6. `subject_mode = explicit_subject`

## 6.1 Cuando se usa

Se usa cuando el caller autenticado no es necesariamente el interlocutor final.

Ejemplo tipico:

- un servicio .NET con una sola API key
- que reenvia mensajes de multiples clientes/personas

## 6.2 Shape del campo `subject`

```json
{
  "ilk": "ilk:123 opcional",
  "external_user_id": "crm:client-12345",
  "display_name": "Juan Perez",
  "email": "juan@acme.com",
  "company_name": "Acme Support",
  "phone": "+5491155551234",
  "attributes": {
    "crm_customer_id": "crm:client-12345"
  }
}
```

Reglas:

- si `ilk` viene no vacio, el request entra en modo `by_ilk`;
- en `by_ilk`, si ademas llegan `display_name`, `email`, `company_name` u otros campos, se aceptan pero se ignoran;
- si `ilk` no viene o viene vacio, el request entra en modo `by_data`;
- en `by_data`, la validacion minima requerida es:
  - `external_user_id`
  - `display_name`
  - `email`
- `phone` y otros datos adicionales pueden venir como complemento;
- esos datos adicionales no crean automaticamente un `ICH` nuevo ni vinculan otro canal;
- `display_name` y `email` son identity candidates auxiliares;
- `company_name` y `attributes` son metadata;
- `subject.tenant_id` y `subject.tenant_hint` no son validos;
- el nodo usa estos datos para hablar con identity antes de emitir al router.

## 6.3 Ejemplo JSON minimo por `by_ilk`

```http
POST /messages
Authorization: Bearer fb_api_xxxxx
Content-Type: application/json
```

```json
{
  "subject": {
    "ilk": "ilk:3e47407a-cd56-4790-8044-aa118e3e3dfc"
  },
  "message": {
    "text": "Necesito ayuda con mi alta"
  }
}
```

## 6.4 Ejemplo JSON recomendado por `by_data`

```json
{
  "subject": {
    "external_user_id": "crm:client-12345",
    "display_name": "Juan Perez",
    "email": "juan@acme.com",
    "company_name": "Acme Support",
    "attributes": {
      "crm_customer_id": "crm:client-12345"
    }
  },
  "message": {
    "text": "Necesito ayuda con mi alta",
    "external_message_id": "crm-msg-998877"
  },
  "options": {
    "metadata": {
      "source_system": "crm-dotnet"
    }
  }
}
```

## 6.5 Comportamiento esperado

El nodo debe:

1. autenticar al caller;
2. determinar si el request esta en `by_ilk` o `by_data`;
3. validar el modo elegido;
4. consultar a identity y esperar respuesta;
5. solo si identity responde exito, aplicar relay si corresponde;
6. publicar un mensaje `text/v1` al router;
7. devolver respuesta HTTP exitosa solo despues de esa etapa.

## 6.6 Errores tipicos de `explicit_subject`

- `subject_identification_mode_invalid`
- `subject_data_incomplete`
- `ilk_does_not_exist`
- `identity_unavailable`
- `identity_timeout`

Mapeo recomendado:

- `ilk_does_not_exist` -> `404 Not Found`
- `identity_timeout` -> `504 Gateway Timeout`
- `identity_unavailable` -> `503 Service Unavailable`

## 6.7 Respuesta exitosa en `by_data`

Cuando `by_data` resuelve un sujeto existente o crea uno nuevo, la respuesta HTTP exitosa debe incluir el `ilk` efectivo:

```json
{
  "status": "accepted",
  "request_id": "req_01JXYZ...",
  "trace_id": "4c66b54c-9d53-4b44-8eb0-c9f88f8f4c1e",
  "ilk": "ilk:3e47407a-cd56-4790-8044-aa118e3e3dfc",
  "relay_status": "flushed_immediately",
  "node_name": "IO.api.frontdesk@worker-220"
}
```

El codigo de exito recomendado sigue siendo `202 Accepted` tambien en `explicit_subject`, para mantener semantica uniforme de ingress.

## 6.8 Desvio explicito a frontdesk

Si `options.routing.dst_node` apunta a `SY.frontdesk.gov`, `IO.api` deja de usar el camino normal de `202 Accepted` y usa el contrato estructurado de frontdesk.

Ejemplo de request:

```json
{
  "subject": {
    "external_user_id": "crm:client-12345",
    "display_name": "Juan Perez",
    "email": "juan@acme.com",
    "company_name": "Acme Support",
    "attributes": {
      "crm_customer_id": "crm:client-12345"
    }
  },
  "message": {
    "text": "Necesito ayuda con mi alta",
    "external_message_id": "crm-msg-998877"
  },
  "options": {
    "routing": {
      "dst_node": "SY.frontdesk.gov@motherbee"
    },
    "metadata": {
      "source_system": "crm-dotnet"
    }
  }
}
```

Comportamiento esperado:

1. `IO.api` resuelve/provisiona `src_ilk`
2. construye `frontdesk_handoff`
3. envia el handoff a `SY.frontdesk.gov`
4. espera `frontdesk_result`
5. responde HTTP con ese payload estructurado

---

## 7. `subject_mode = caller_is_subject`

## 7.1 Cuando se usa

Se usa cuando la credencial autenticada representa directamente al sujeto conversacional.

## 7.2 Reglas

- el payload no debe requerir `subject`;
- la identidad efectiva del sujeto se deriva del caller autenticado;
- si el request incluye `subject` y la instancia no lo admite, el nodo debe rechazarlo con `422 invalid_payload`.

## 7.3 Ejemplo JSON minimo

```http
POST /messages
Authorization: Bearer fb_user_yyyyy
Content-Type: application/json
```

```json
{
  "message": {
    "text": "Necesito ayuda con mi alta"
  }
}
```

## 7.4 Comportamiento esperado

El nodo debe:

1. autenticar al caller;
2. derivar el sujeto efectivo desde la credencial/config de auth;
3. construir `ResolveOrCreateInput`;
4. delegar identity al pipeline comun;
5. calcular `meta.thread_id`;
6. aplicar relay si corresponde;
7. publicar el mensaje interno.

---

## 8. Attachments desde dia 0

## 8.1 Regla general

La primera version debe estar preparada para recibir attachments desde dia 0.

La forma normativa de ingreso de attachments es:

```text
multipart/form-data
```

## 8.2 Shape de request multipart

El request multipart debe contener:

- una parte `metadata` con JSON UTF-8;
- una o mas partes de archivo.

## 8.3 Ejemplo de `metadata` para `explicit_subject`

```json
{
  "subject": {
    "external_user_id": "crm:client-12345",
    "display_name": "Juan Perez",
    "email": "juan@acme.com"
  },
  "message": {
    "text": "Adjunto documentacion",
    "external_message_id": "crm-msg-112233"
  },
  "options": {
    "metadata": {
      "source_system": "crm-dotnet"
    }
  }
}
```

## 8.4 Comportamiento del nodo

El nodo debe:

1. validar metadata;
2. validar cantidad, tamano y tipos MIME segun policy;
3. materializar cada archivo en blob storage;
4. construir `blob_ref` por attachment;
5. publicar el mensaje interno con `attachments[]`.

Regla:

- el mensaje interno no debe transportar el binario inline;
- debe transportar referencias `blob_ref` bajo el contrato canonico `text/v1`.

---

## 9. Ejemplo de attachment materializado

```json
{
  "type": "blob_ref",
  "blob_name": "dni_frente_a1b2c3d4e5f6a7b8.png",
  "size": 532112,
  "mime": "image/png",
  "filename_original": "dni_frente.png",
  "spool_day": "2026-04-09"
}
```

---

## 10. Reglas de validacion recomendadas

Configurable por instancia:

- `max_attachments_per_request`
- `max_attachment_size_bytes`
- `max_total_attachment_bytes`
- `allowed_mime_types`

El nodo debe rechazar cuando:

- no hay `message.text` ni attachments;
- el archivo excede limite individual;
- el total excede limite agregado;
- el MIME esta prohibido;
- falta `metadata` en multipart;
- falta `subject` en `explicit_subject`;
- aparece `subject` en `caller_is_subject` y la instancia no lo admite.

---

## 11. Ejemplo conceptual de mensaje interno Fluxbee

Este documento no fija el JSON interno exacto, pero el resultado logico esperado preserva:

- `routing.trace_id`
- `meta.src_ilk`
- `meta.thread_id`
- `meta.context.io.*`
- payload `text/v1`

Ejemplo conceptual:

```json
{
  "routing": {
    "src": "<uuid-io-api>",
    "dst": null,
    "ttl": 16,
    "trace_id": "4c66b54c-9d53-4b44-8eb0-c9f88f8f4c1e"
  },
  "meta": {
    "src_ilk": "ilk:...",
    "ich": "ich:...",
    "thread_id": "thread:sha256:...",
    "context": {
      "io": {
        "channel": "api",
        "relay": {
          "applied": true,
          "reason": "window_elapsed",
          "parts": 2,
          "window_ms": 2500
        }
      }
    }
  },
  "payload": {
    "type": "text",
    "content": "Adjunto documentacion",
    "attachments": [
      {
        "type": "blob_ref",
        "blob_name": "dni_frente_a1b2c3d4e5f6a7b8.png",
        "size": 532112,
        "mime": "image/png",
        "filename_original": "dni_frente.png",
        "spool_day": "2026-04-09"
      }
    ]
  }
}
```

Nota:

- los carriers reales finales deben seguir el protocolo vigente del repo;
- este ejemplo es conceptual y no debe interpretarse como licencia para apartarse de `io-common` o de `NodeSender::send`.

---

## 12. Casos de uso recomendados por modo

### `explicit_subject`

Usar cuando:

- una credencial representa a muchos sujetos finales;
- el caller es un servicio intermediario;
- el nodo necesita recibir identidad candidata por request.

### `caller_is_subject`

Usar cuando:

- una credencial representa a un unico emisor final;
- no se quiere repetir identidad en cada request;
- el sujeto puede derivarse del contexto autenticado.

---

## 13. Lo que este documento deja fuera

No define todavia:

- `mapped_subject` como contrato obligatorio;
- webhook outbound detallado;
- `GET /healthz`;
- `GET /`;
- mTLS o HMAC como auth obligatoria;
- query/polling de respuestas.

---

## 14. Decisiones cerradas por este documento

1. `POST /` es el endpoint de ingreso normativo.
2. `GET /` es el endpoint normativo de introspeccion.
3. `POST /` responde `202 Accepted` al aceptar el request; en `explicit_subject` esa aceptacion ocurre solo despues de la etapa de identity.
4. `application/json` y `multipart/form-data` son los content types minimos soportados.
5. `multipart/form-data` es el camino normativo para attachments binarios.
6. `explicit_subject` y `caller_is_subject` son los modos de sujeto normativos de v1.
7. El contrato efectivo de la instancia se obtiene por configuracion y se refleja en `GET /`.
8. `GET /` describe el contrato efectivo de la instancia.
9. `POST /messages` y `GET /schema` pueden mantenerse como aliases legacy compatibles.
9. Los attachments deben materializarse como blobs y viajar internamente como `blob_ref`.
10. La credencial Bearer autentica al caller del endpoint.
