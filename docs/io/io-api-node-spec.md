# IO.api - Especificacion Tecnica

**Estado:** propuesta normativa para implementacion
**Fecha:** 2026-04-10
**Audiencia:** arquitectura, backend, desarrollo, ops, integradores
**Tipo de nodo:** `IO.*`
**Runtime recomendado:** `IO.api`
**Alcance:** nodo IO HTTP/API de proposito general para ingreso de mensajes desde sistemas externos hacia Fluxbee

---

## 0. Proposito

`IO.api` define un adapter HTTP general para Fluxbee.

Su funcion es:

- exponer un endpoint HTTP de ingreso;
- autenticar al caller externo;
- validar el request segun el contrato efectivo de la instancia;
- derivar el sujeto conversacional efectivo;
- materializar attachments en blob storage cuando corresponda;
- aplicar relay comun de `io-common`;
- publicar un mensaje canonico al router.

`IO.api` no es:

- una API directa al router;
- una API de administracion del sistema;
- una capa de memoria cognitiva;
- una base de datos de mensajes;
- un reemplazo de `SY.identity`, `SY.storage`, `SY.admin` o `SY.cognition`.

---

## 1. Relacion con la arquitectura general

Dentro de Fluxbee, `IO.api` es un nodo IO mas. El medio externo es HTTP.

Por lo tanto:

- el mundo externo habla HTTP con `IO.api`;
- `IO.api` habla protocolo Fluxbee con el router;
- el router sigue siendo el unico responsable de routing interno;
- cognition no participa del relay del IO;
- identity sigue siendo la fuente de verdad de ILK/ICH/tenant.

### 1.1 Responsabilidades

`IO.api` es responsable de:

- exponer `POST /` como endpoint principal de ingreso;
- exponer `GET /` como endpoint principal de introspeccion;
- mantener `POST /messages` y `GET /schema` como aliases legacy compatibles;
- autenticar y autorizar al caller externo;
- validar el request segun el contrato efectivo de la instancia;
- construir un `ResolveOrCreateInput` segun el `subject_mode` de la instancia;
- delegar la resolucion/provision de identidad al pipeline comun de `io-common`;
- decidir si el sujeto requiere regularizacion por `SY.frontdesk.gov` segun su estado de registro;
- construir `frontdesk_handoff` cuando esa regularizacion sea necesaria;
- construir un mensaje `text/v1` canonico;
- pasar por el relay comun;
- enviar al router por `fluxbee_sdk::NodeSender::send`;
- adjuntar `meta.context.response_envelope` cuando el hop a frontdesk requiere respuesta estructurada;
- consumir respuesta estructurada síncrona de frontdesk sobre payload `text` cuando el request necesita regularizacion por frontdesk.

### 1.2 No-responsabilidades

`IO.api` no debe:

- tomar decisiones de routing de negocio;
- persistir conversacion como fuente de verdad;
- reemplazar la capa de storage;
- reemplazar la capa de identity;
- acoplarse a cognition;
- exponer bypass administrativos a nodos internos.

---

## 2. Principios de diseno

### 2.1 Runtime generico, contrato efectivo por instancia

`IO.api` es un runtime generico capaz de soportar varios modos de ingreso, autenticacion y resolucion de sujeto.

Sin embargo, una instancia concreta debe exponer un contrato HTTP efectivo y deterministico derivado de su configuracion.

### 2.2 Separacion entre caller autenticado y sujeto conversacional

`IO.api` debe distinguir entre:

- caller autenticado: la entidad que invoca el endpoint HTTP;
- sujeto conversacional efectivo: la entidad que sera representada internamente como sujeto del mensaje.
- tenant efectivo: el tenant derivado de la API key autenticada.
- destino efectivo del request: el `dst_node` configurado o overrideado por request.

Ambos pueden coincidir o no.

### 2.3 Identidad por pipeline comun de IO

`IO.api` no implementa una source of truth de identidad separada del resto de los nodos IO.

Sin embargo, para `subject_mode = explicit_subject`, el contrato HTTP puede exigir una resolucion de identidad sincronica antes de devolver respuesta HTTP.

Regla normativa:

1. el nodo autentica al caller;
2. en `caller_is_subject`, mantiene el comportamiento asincrono de ingress;
3. en `explicit_subject`, determina el modo de identificacion desde el payload;
4. en `explicit_subject`, no devuelve respuesta exitosa ni emite al router hasta conocer el resultado de identity para ese request.

Esto no convierte a `IO.api` en source of truth de identidad. Sigue delegando a identity, pero cambia el punto en el que responde HTTP.

### 2.4 Relay pre-router

Todo mensaje de ingreso debe pasar por el relay comun de `io-common`.

La superficie formal de configuracion del relay es:

- `config.io.relay.window_ms`
- `config.io.relay.max_open_sessions`
- `config.io.relay.max_fragments_per_session`
- `config.io.relay.max_bytes_per_session`

Regla:

- `window_ms > 0` implica consolidacion;
- `window_ms = 0` implica passthrough inmediato.

### 2.5 Endpoint parcialmente sincronico segun `subject_mode`

`POST /` no espera el resultado final del sistema, pero tampoco es completamente asincrono en todos los modos.

Regla normativa:

- `caller_is_subject`: mantiene respuesta asincrona de ingress;
- `explicit_subject`: espera el resultado de la etapa de identity requerida por el request antes de devolver respuesta HTTP.

En ningun caso responde con el resultado final del AI/frontdesk/workflow, salvo la excepcion estructurada del camino `SY.frontdesk.gov` definida en la seccion de contrato HTTP.

---

## 3. Naming, runtime e instancias

### 3.1 Runtime

Nombre recomendado:

- `IO.api`

### 3.2 Ejemplos de instancia

- `IO.api.frontdesk@worker-220`
- `IO.api.portal@worker-220`
- `IO.api.partner1@worker-220`
- `IO.api.ingress@motherbee`

### 3.3 Multi-instancia

`IO.api` debe soportar multi-instancia.

Patron recomendado:

- una instancia = un listener HTTP = un contrato efectivo

Esto no cambia el pipeline comun de identidad ni relay. Cada instancia aplica el mismo flujo IO sobre su propio contrato HTTP y su propia configuracion.

---

## 4. Estados del nodo

`IO.api` debe implementar al menos:

- `UNCONFIGURED`
- `CONFIGURED`
- `FAILED_CONFIG`

### 4.1 UNCONFIGURED

El nodo arranco, pero todavia no tiene configuracion efectiva suficiente para aceptar trafico de negocio.

En este estado:

- debe exponer `GET /`;
- no debe aceptar `POST /` para trafico de negocio;
- debe responder `503 Service Unavailable` con `error_code=node_not_configured` en `POST /`.

### 4.2 CONFIGURED

El nodo tiene listener, auth, `subject_mode`, accepted content types y relay definidos, y puede aceptar trafico de negocio.

### 4.3 FAILED_CONFIG

La configuracion es invalida o insuficiente para operar correctamente.

En este estado:

- debe seguir vivo;
- debe exponer `GET /`;
- no debe aceptar `POST /`;
- debe reportar explicitamente el motivo del fallo en schema/status/control plane.

---

## 5. Configuracion de instancia

### 5.1 Modelo vigente

`IO.api` debe alinearse con el modelo general de nodos IO en este repo:

- bootstrap/infrastructure config por orchestrator `config.json`;
- runtime/business config node-owned por `CONFIG_GET` / `CONFIG_SET`;
- `CONFIG_CHANGED` como senal informativa de infraestructura, no como camino canonico de hot reload del adapter.

### 5.2 Config minima para pasar a CONFIGURED

Una instancia no debe pasar a `CONFIGURED` si falta alguno de estos grupos:

- `listen.address`
- `listen.port`
- `auth.mode`
- `ingress.subject_mode`
- `ingress.accepted_content_types`
- `config.io.relay.*` valido

Ademas:

- si `auth.mode = api_key`, debe existir al menos una credencial activa valida;
- si a futuro `outbound.mode = webhook`, debe existir configuracion valida de webhook.

### 5.3 Secrets

Los secretos del nodo no deben quedar expuestos en `${STATE_DIR}` ni en respuestas publicas.

Reglas vigentes:

- `CONFIG_SET` puede aceptar secretos inline;
- `CONFIG_GET` y `CONFIG_RESPONSE` deben devolver secretos redacted;
- la metadata de secretos debe salir via `contract.secrets[*]`;
- el nodo no debe persistir secretos en `${STATE_DIR}`;
- el mecanismo concreto de persistencia local de secretos queda como decision de implementacion del adapter mientras no contradiga el contrato comun.

Ejemplos de secretos de `IO.api`:

- API keys inbound;
- secretos HMAC futuros;
- credenciales de webhook outbound futuras.

### 5.4 Ownership model de API keys en `IO.api`

Para `IO.api`, v1 cierra explicitamente este reparto de responsabilidades:

- la creacion, rotacion y revocacion logica de API keys pertenece al control plane;
- la entrega inicial de la key a la instancia pertenece al flujo de bootstrap por orchestrator o a `CONFIG_SET`;
- la materializacion local utilizable en runtime pertenece al propio nodo `IO.api`;
- `${STATE_DIR}` nunca debe contener secretos;
- `CONFIG_GET` y `CONFIG_RESPONSE` nunca deben devolver el valor crudo.

Lectura operativa:

- `architect`, un operador humano o una futura herramienta admin pueden decidir que keys deben existir;
- el nodo no genera API keys;
- el nodo recibe la key ya provisionada por control plane;
- el nodo la persiste localmente fuera de `${STATE_DIR}` y la usa como fuente de runtime.

### 5.5 Decision v1 para persistencia local de secrets

Para `IO.api`, v1 adopta explicitamente `secrets.json` local como decision de implementacion del adapter, siguiendo el precedente ya documentado en AI Nodes.

Reglas para `IO.api` v1:

- las API keys pueden entrar inline por `CONFIG_SET` o por bootstrap config;
- el nodo debe separarlas de la config visible;
- el nodo debe persistirlas localmente en `secrets.json`;
- `secrets.json` no forma parte de `${STATE_DIR}`;
- `config.json` puede transportar bootstrap config, pero no debe ser la fuente primaria del secreto a largo plazo cuando ya existe materializacion local valida.

Esto debe leerse como una decision explicita de `IO.api` v1, no como una generalizacion automatica a todos los `IO.*`.

---

## 6. Seguridad

### 6.1 Objetivo

`IO.api` debe autenticar al caller externo que invoca el endpoint HTTP.

Esa autenticacion no equivale automaticamente a la identidad conversacional interna.

### 6.2 Modos de autenticacion

Obligatorio en v1:

- `api_key`

Previstos para fases posteriores:

- `hmac_signature`
- `mTLS`
- `allowlist_ip` como control complementario

### 6.3 Transporte recomendado para API key

Se recomienda:

- `Authorization: Bearer <token>`

Ese bearer token puede ser una API key opaca. No implica OAuth ni JWT.

### 6.4 Multiples credenciales activas

Una instancia debe poder soportar multiples credenciales activas simultaneamente.

Eso permite:

- rotacion sin downtime;
- separacion de consumidores;
- revocacion selectiva.

### 6.4.1 Tenant scoping de API keys

En `IO.api`, cada API key debe declarar explicitamente un `tenant_id`.

Reglas:

- la autenticacion valida el token;
- la autorizacion deriva el tenant efectivo desde la key autenticada;
- `IO.api` debe validar fail-closed que ese `tenant_id` exista;
- si el tenant de la key no existe, el request debe rechazarse;
- el request HTTP no define tenancy propia.

### 6.5 TLS

Fuera de dev, el acceso al endpoint debe ocurrir sobre:

- HTTPS, o
- HTTP detras de reverse proxy con terminacion TLS controlada.

### 6.6 Higiene de logs

El nodo no debe loggear en claro:

- API keys;
- secretos;
- payloads completos con PII;
- attachments;
- headers sensibles.

---

## 7. Modos de resolucion del sujeto

### 7.1 Regla de configuracion

En v1, `subject_mode` es configuracion de instancia, no switch por request.

La instancia concreta elige uno y `GET /` debe reflejarlo.

### 7.2 `explicit_subject`

El request incluye explicitamente la informacion del sujeto final.

Es el modo recomendado para integradores de servicio que hablan en nombre de multiples personas.

`explicit_subject` admite dos modos de identificacion por payload:

- `by_ilk`: el request incluye `subject.ilk`;
- `by_data`: el request no incluye `subject.ilk` util y la identidad se intenta resolver/provisionar a partir de datos del sujeto.

Reglas:

- el objeto `subject` sigue siendo el carrier normativo del sujeto explicito;
- dentro de `subject`, los campos de identidad pasan a ser opcionales a nivel de shape;
- el request debe caer en uno de los dos modos validos;
- si `subject.ilk` viene no vacio, el request entra en `by_ilk`;
- si `subject.ilk` no viene o viene vacio, el request entra en `by_data`;
- esos campos alimentan a identity, pero no reemplazan a `SY.identity`.

#### 7.2.1 Modo `by_ilk`

Cuando `subject.ilk` viene con un valor no vacio:

- `IO.api` debe consultar a identity si ese ILK existe;
- si existe, puede continuar el flujo y responder exito;
- si no existe, debe rechazar el request;
- en este modo no debe hacer provision por datos del sujeto;
- el pipeline interno debe usar ese `ilk` como `src_ilk` efectivo sin volver a resolverlo por `(channel, external_id)`;
- si el payload incluye ademas `display_name`, `email`, `company_name` u otros campos de identidad, esos campos se aceptan pero se ignoran completamente en este modo.

#### 7.2.2 Modo `by_data`

Cuando `subject.ilk` no esta presente o viene vacio:

- `IO.api` debe validar primero que existan los campos minimos requeridos para identity;
- si esos campos faltan o vienen vacios, debe rechazar el request;
- si la validacion minima pasa, debe consultar a identity y esperar el resultado;
- solo si identity responde exito el nodo puede emitir al router y devolver respuesta HTTP exitosa.

Para `IO.api` se cierra, por ahora, esta validacion minima de `by_data`:

- `subject.external_user_id`
- `subject.display_name`
- `subject.email`

Lectura:

- el tenant ya no se toma del body sino de la API key autenticada;
- pueden admitirse campos adicionales, por ejemplo `phone`, pero no reemplazan a los minimos obligatorios.

Si el payload incluye `phone` u otro dato adicional:

- se trata como dato de identidad complementario;
- puede formar parte del payload hacia identity cuando el adapter implemente esa llamada;
- no crea automaticamente un `ICH` nuevo;
- no vincula automaticamente ese telefono a otro canal como WhatsApp o telefonia;
- el `ICH` canonico de `IO.api` sigue siendo el del canal `api` y su referencia estable de ingreso.

Campos opcionales de metadata admitidos en `by_data`:

- `subject.company_name`
- `subject.attributes`

Reglas:

- `company_name` es metadata del humano o de su compania, no tenancy;
- `attributes` debe ser un objeto y permite metadata adicional extensible;
- `subject.tenant_id` y `subject.tenant_hint` no son validos en `IO.api`.

### 7.3 `caller_is_subject`

La identidad del sujeto se deriva del caller autenticado.

Es el modo recomendado para credenciales emitidas por usuario o por sujeto final.

Reglas:

- el payload no necesita `subject`;
- la referencia estable del sujeto se deriva del contexto autenticado;
- si el request incluye `subject` y la instancia no lo admite, debe rechazarse con `422 invalid_payload`.

### 7.4 `mapped_subject`

Modo previsto para fase posterior.

No es obligatorio para v1.

---

## 8. Contrato HTTP

### 8.1 Endpoints obligatorios en v1

- `POST /`
- `GET /`

Aliases legacy compatibles:

- `POST /messages`
- `GET /schema`

### 8.2 Naturaleza de `POST /`

`POST /` tiene comportamiento distinto segun `subject_mode`.

#### `caller_is_subject`

Mantiene la semantica asincrona:

- autentica;
- valida;
- acepta el ingress;
- devuelve `202 Accepted` cuando el request fue retenido por relay o emitido hacia el sistema.

#### `explicit_subject`

Tiene una etapa sincronica previa:

- autentica;
- valida el modo de identificacion del payload;
- consulta a identity;
- solo si esa etapa es exitosa puede emitir al router y devolver respuesta HTTP exitosa.

La respuesta exitosa recomendada sigue siendo:

- `202 Accepted`

pero solo despues de la etapa de identity de `explicit_subject`.

En `explicit_subject`, la respuesta HTTP exitosa debe incluir ademas el `ilk` efectivo resuelto o creado, para permitir que el integrador lo reutilice en requests futuros.

Se mantiene `202 Accepted` tambien en `explicit_subject`, incluso cuando identity ya fue resuelta antes de responder, porque sigue siendo el codigo mas estandar para un ingress que acepta y publica trabajo dentro del sistema sin prometer resultado final de negocio.

Excepcion cerrada de esta version:

- si el sujeto de `explicit_subject by_data` no esta registrado completamente, `IO.api` debe intermediar un `frontdesk_handoff` antes de continuar al `dst_final`;
- si ese handoff devuelve un resultado estructurado no exitoso, `IO.api` responde con el envelope HTTP de frontdesk;
- si devuelve `ok`, `IO.api` continua el mensaje original al `dst_final` y responde `202 Accepted`;
- si el caller fija explicitamente `dst_node = SY.frontdesk.gov`, `IO.api` conserva un camino tecnico/terminal que responde directamente con ese mismo envelope estructurado.

### 8.3 Matriz de respuestas HTTP segun regularizacion frontdesk y destino efectivo

`IO.api` tiene dos familias de respuesta:

- camino normal de ingress via router;
- camino estructurado sincronico cuando frontdesk bloquea la continuacion o cuando el request apunta explicitamente a `SY.frontdesk.gov`.

#### 8.3.1 Camino normal via router

Si el request no apunta efectivamente a `SY.frontdesk.gov`, `IO.api` mantiene semantica de ingress:

- responde `202 Accepted` cuando autentico, valido el payload, resolvio identity si hacia falta y pudo publicar o retener el mensaje internamente;
- no espera resultado final de negocio;
- el body de exito incluye metadatos de aceptacion como `request_id`, `trace_id`, `ilk` y `relay_status`.

#### 8.3.2 Camino con regularizacion por frontdesk

Si `IO.api` detecta que el sujeto de `explicit_subject by_data` no esta registrado completamente, debe:

1. construir `frontdesk_handoff`;
2. enviarlo a `SY.frontdesk.gov@<hive>`;
3. esperar respuesta estructurada compatible con el envelope enviado;
4. decidir si continua o no el mensaje original al `dst_final`.

Casos normativos:

- `success = true` -> continuar el mensaje original al `dst_final` y responder `202 Accepted`
- `success = false` y `error_code = "missing_required_fields"` -> `422 Unprocessable Entity`
- `success = false` y `error_code = "invalid_request"` -> `422 Unprocessable Entity`
- `success = false` y `error_code = "identity_unavailable"` -> `503 Service Unavailable`
- `success = false` y `error_code = "register_failed"` -> `502 Bad Gateway`
- `success = false` con cualquier otro `error_code` o sin `error_code` -> `502 Bad Gateway`

El body devuelto por `IO.api` en este camino depende del resultado:

- `ok` -> envelope `accepted` del ingress comun;
- `success = false` -> envelope estructurado HTTP de frontdesk:
  - `success`
  - `human_message`
  - `error_code`

En particular, `needs_input` no se trata como exito HTTP en `IO.api`, porque el modo API no continua conversacionalmente ni puede completar el registro interactuando con el caller.

#### 8.3.3 Camino tecnico con `dst_node = SY.frontdesk.gov`

Si el caller fija explicitamente `options.routing.dst_node = "SY.frontdesk.gov@..."`, `IO.api` conserva un camino tecnico/terminal:

- construye `frontdesk_handoff`;
- agrega `meta.context.response_envelope`;
- espera respuesta estructurada compatible;
- responde directamente con ese payload estructurado;
- no reinyecta luego el mensaje original a otro `dst_final`.

Este camino sirve para debug, validacion o integraciones muy especificas, pero no es el flujo canonico de producto.

#### 8.3.4 Errores de transporte o integracion en el camino frontdesk

Si existe handoff a frontdesk, tambien existen errores previos o laterales al `frontdesk_result`:

- timeout esperando la reply de frontdesk -> `504 Gateway Timeout` con `error_code = "frontdesk_timeout"`
- reply recibida pero con payload no parseable como respuesta estructurada valida -> `502 Bad Gateway` con `error_code = "invalid_frontdesk_response"`
- imposibilidad de enviar el handoff al router o indisponibilidad del canal de salida -> `503 Service Unavailable` con `error_code = "router_unavailable"`
- cierre inesperado del waiter de reply antes de recibir respuesta valida -> `503 Service Unavailable` con `error_code = "frontdesk_unavailable"`

Estos casos no representan resultado funcional de frontdesk; representan fallo de transporte o correlacion del handoff.

### 8.4 Codigos de error recomendados

- `400 Bad Request`
- `401 Unauthorized`
- `403 Forbidden`
- `404 Not Found` para `ilk_does_not_exist`
- `413 Payload Too Large`
- `415 Unsupported Media Type`
- `422 Unprocessable Entity`
- `503 Service Unavailable`
- `504 Gateway Timeout`

### 8.5 Tipificacion inicial de errores HTTP

Errores que conviene estandarizar desde esta fase:

- `invalid_json`
- `unauthorized`
- `tenant_not_found`
- `node_not_configured`
- `invalid_payload`
- `subject_identification_mode_invalid`
- `subject_data_incomplete`
- `ilk_does_not_exist`
- `identity_unavailable`
- `identity_timeout`
- `frontdesk_timeout`
- `invalid_frontdesk_response`
- `router_unavailable`
- `frontdesk_unavailable`
- `unsupported_attachment_mime`
- `attachment_too_large`
- `attachments_too_large`

Tratamiento recomendado:

- `identity_timeout`: `504 Gateway Timeout`, cuando identity no responde dentro del tiempo esperado;
- `identity_unavailable`: `503 Service Unavailable`, cuando identity esta inalcanzable, devuelve `UNREACHABLE`/`TTL_EXCEEDED` o el adapter no puede establecer la llamada requerida.

---

## 9. `GET /`

### 9.1 Objetivo

`GET /` debe describir el contrato efectivo de la instancia en ejecucion.

No debe devolver una union ambigua de todos los modos posibles del runtime.

### 9.2 Contenido minimo esperado

En estado configurado, debe devolver al menos:

- estado efectivo;
- nombre de instancia;
- runtime;
- `subject_mode` efectivo;
- auth efectiva;
- accepted content types;
- campos requeridos y opcionales;
- limites de tamano;
- metadata de attachments;
- metadata de relay;
- metadata de secretos redacted;
- fuente del tenant del request cuando aplique;
- informacion suficiente para construir un `CONFIG_SET` valido o para integrar contra `POST /`.

### 9.3 Relacion con Control Plane

`GET /` es introspeccion HTTP del contrato de ingreso.

No reemplaza:

- `CONFIG_GET`
- `CONFIG_SET`
- `CONFIG_RESPONSE`

El control plane sigue siendo el camino canonico para runtime/business config node-owned.

---

## 10. Formas de request soportadas

### 10.1 `application/json`

Debe soportarse para casos sin archivos binarios.

### 10.2 `multipart/form-data`

Debe soportarse desde dia 0 para permitir attachments directos.

Camino recomendado:

- una parte `metadata` con JSON;
- una o mas partes de archivo.

### 10.3 Base64 en JSON

No es el camino recomendado para v1.

---

## 11. Estructura logica del request

La estructura logica recomendada es:

- `subject` o equivalente, segun `subject_mode`
- `message`
- `options` opcional

Reglas:

- `message` es obligatorio;
- `subject` puede ser obligatorio, opcional o invalido segun `subject_mode`;
- `options.routing.dst_node` puede overridear el destino Fluxbee por request;
- `options.routing.dst_node` define el `dst_final` del request cuando viene presente;
- `IO.api` puede intermediar frontdesk antes de llegar a ese `dst_final` si el sujeto no esta completo;
- debe existir al menos uno de:
  - `message.text`
  - uno o mas attachments

---

## 12. Attachments y blobs

### 12.1 Principio

Los archivos no deben circular internamente como binario inline una vez ingresados al sistema.

`IO.api` debe materializarlos en blob storage y emitir referencias canonicas en el mensaje interno.

### 12.2 Flujo

1. el request HTTP ingresa con uno o mas archivos;
2. `IO.api` valida limites y tipos;
3. escribe en staging/local segun la implementacion de blobs vigente;
4. promueve a estado consumible;
5. construye `blob_ref` canonico;
6. emite el mensaje interno con `attachments[]`.

### 12.3 Regla de contrato

El mensaje interno no debe transportar el binario inline.

Debe transportar referencias canonicas compatibles con `text/v1`.

---

## 13. Construccion del mensaje interno

### 13.1 Regla general

Luego de autenticar, validar y derivar el sujeto, `IO.api` debe construir un mensaje Fluxbee canonico.

### 13.2 Carriers vigentes

La implementacion debe seguir los carriers reales del repo:

- identidad en `meta.src_ilk`
- thread en `meta.thread_id`
- metadata de canal bajo `meta.context.io.*`
- payload textual canonico `text/v1`

`IO.api` no debe usar wording conceptual viejo como sustituto de esos carriers reales.

### 13.3 Salida al router

El mensaje debe salir al router por `fluxbee_sdk::NodeSender::send`.

`IO.api` no debe reimplementar localmente la decision `content` vs `content_ref` para `text/v1`.

### 13.4 Relay antes del router

El mensaje construido no se envia inmediatamente al router si la politica de relay indica consolidacion.

Primero pasa por el relay comun de `io-common`.

### 13.5 Camino especial de `frontdesk_handoff`

Cuando `IO.api` detecta que el sujeto de `explicit_subject by_data` no esta registrado completamente, usa un contrato interno distinto antes de continuar al `dst_final`:

- `payload.type = "frontdesk_handoff"`
- `payload.schema_version = 1`
- `payload.operation = "complete_registration"`
- `payload.subject.*` con los datos del humano
- `payload.tenant_id` con el tenant derivado de la API key
- `payload.context` con metadata de integracion

El carrier conserva:

- `meta.src_ilk`
- `meta.thread_id`
- `meta.context.io.*`

En este camino:

- no se usa `text/v1` como payload hacia frontdesk;
- `IO.api` agrega `meta.context.response_envelope` al mensaje hacia frontdesk;
- `IO.api` espera una reply `payload.type = "text"` con JSON estructurado compatible con ese envelope;
- si `success = true`, continua luego el mensaje original al `dst_final`;
- si `success = false`, devuelve ese resultado estructurado por HTTP.

Envelope concreto validado en este primer corte:

```json
{
  "kind": "json_object_v1",
  "required": ["success", "human_message"],
  "properties": {
    "success": { "type": "boolean" },
    "human_message": { "type": "string" },
    "error_code": {
      "type": "string",
      "enum": [
        "missing_required_fields",
        "invalid_request",
        "identity_unavailable",
        "register_failed",
        "unknown"
      ]
    }
  }
}
```

Regla de semántica:

- `error_code` es opcional por ausencia;
- en v1 no debe enviarse como `null`;
- si el campo está presente, debe ser `string`.

---

## 14. Outbound futuro

El foco inicial es ingress.

Se deja previsto outbound por webhook como fase posterior, pero no es obligatorio para la primera entrega.

---

## 15. Operacion y despliegue

### 15.1 Modelo

`IO.api` debe desplegarse como business node estandar del sistema.

Cada instancia debe crearse con:

- un `node_name` concreto;
- el runtime `IO.api`;
- su bootstrap config;
- sus secretos asociados.

### 15.2 Multi-instancia

Para soportar multiples contratos simultaneos se recomienda levantar multiples instancias, no multiplexar contratos radicalmente distintos en una sola instancia.

---

## 16. Observabilidad

`IO.api` deberia exponer como minimo:

- requests aceptados;
- requests rechazados por auth;
- requests rechazados por schema;
- relay holds y relay flushes;
- attachments recibidos;
- bytes ingresados;
- errores de blob;
- errores outbound futuros.

Logs recomendados:

- `request_id`
- `trace_id`
- `node_name`
- motivo de rechazo o aceptacion

Sin incluir payload sensible completo ni secretos.

---

## 17. Decisiones cerradas en esta version

1. `IO.api` sera un nuevo tipo de nodo IO oficial.
2. Su runtime sera generico, pero el contrato efectivo sera por configuracion de instancia.
3. Debe soportar multiples instancias.
4. Se recomienda una instancia por listener/contrato.
5. Debe usar el relay comun de `io-common`.
6. Debe soportar `api_key` como modo obligatorio en v1.
7. El carrier recomendado para la API key es `Authorization: Bearer <token>`.
8. Debe distinguir caller autenticado de sujeto conversacional.
9. Debe soportar `explicit_subject` y `caller_is_subject` en v1.
10. `subject_mode` es configuracion de instancia, no switch por request en v1.
11. Debe exponer `POST /` y `GET /`.
12. `POST /` es asincrono y responde `202 Accepted` cuando acepta el ingreso.
13. `GET /` describe el contrato efectivo de la instancia.
14. `POST /messages` y `GET /schema` pueden mantenerse como aliases legacy.
14. Debe soportar attachments desde dia 0.
15. El camino recomendado para attachments es `multipart/form-data`.
16. Internamente, los archivos deben convertirse a blobs canonicos.
17. La identidad sigue el pipeline comun de `io-common`.
18. La configuracion del relay sigue `config.io.relay.*`.
19. La configuracion runtime/business del nodo sigue `CONFIG_GET` / `CONFIG_SET`.
20. Los carriers del mensaje interno deben seguir el protocolo real vigente del repo.
21. Cada API key de `IO.api` es tenant-scoped y define el tenant efectivo del request.
22. `tenant_hint` deja de ser parte del contrato HTTP de `IO.api`.
23. `company_name` y `attributes` se tratan como metadata, no como tenancy.

---

## 18. Fuera de alcance para esta version

- detalle exhaustivo del contrato de webhook outbound;
- `mapped_subject` completo;
- HMAC y mTLS como auth obligatoria;
- firma detallada de callbacks outbound;
- policy/OPA especifica para `IO.api`;
- persistencia historica de respuestas outbound;
- discovery remoto dinamico de mappings externos de sujetos.

---

## 19. Recomendacion de implementacion por fases

### Fase 1

- runtime `IO.api`
- configuracion de instancia
- `POST /`
- `GET /`
- auth `api_key`
- `subject_mode=explicit_subject`
- `subject_mode=caller_is_subject`
- `application/json`
- `multipart/form-data`
- attachments -> `blob_ref`
- relay comun de `io-common`
- sin outbound aun

### Fase 2

- outbound webhook
- HMAC outbound
- retries/idempotencia outbound
- `mapped_subject`
- auth adicionales
