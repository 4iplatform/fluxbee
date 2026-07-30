# IO.cloud — API para Fluxbee Cloud

> **Para el equipo de desarrollo de Fluxbee Cloud.** No hace falta conocer fluxbee por dentro.
>
> Todo lo que está acá fue **probado contra un hive de producción real desde una máquina externa,
> por internet**, el 2026-07-30. Los ejemplos son las llamadas que se ejecutaron, no pseudocódigo.
> Donde algo no se pudo verificar, está dicho.

---

## 1. Qué es y qué resuelve

Un **hive** de fluxbee es una instalación privada: sus servicios hablan entre sí por una malla
interna que no está expuesta a internet. Fluxbee Cloud necesita poder aprovisionar cosas dentro de
un hive —crear un tenant, cargarle credenciales, levantarle un nodo— sin que eso implique abrir la
malla.

**IO.cloud es la única puerta para eso.** Es un nodo que corre dentro del hive, publica **un solo
endpoint HTTPS**, y traduce cada pedido a una acción administrativa interna. Es deliberadamente
angosto: acepta **tres operaciones** y nada más. Cualquier otra cosa que Cloud pudiera querer pedir
está bloqueada por diseño, no por omisión — hay una lista blanca en el lado del hive que se valida
en cada llamada.

Dicho de otro modo: **Cloud no habla con el hive, habla con IO.cloud**, y IO.cloud solo sabe hacer
tres cosas.

---

## 2. Cómo se conecta

```
Fluxbee Cloud
     │  POST https://<hive>/e/<ich>        Authorization: Bearer <secreto>
     ▼
  SY.edge          ← verifica el bearer y CORTA acá si no coincide (401)
     │               también quita el header Authorization: no viaja hacia adentro
     ▼
  IO.cloud         ← traduce op → acción administrativa
     │
     ▼
  SY.admin         ← valida contra la lista blanca y ejecuta
```

**La URL** tiene la forma `https://<host-del-hive>/e/<ich>`, donde `<ich>` es un identificador
opaco que el hive genera. **Cada hive tiene el suyo** — no es adivinable ni reusable.

**La autenticación** es un bearer compartido, configurado en el hive. No hay otro modo: si el
endpoint está publicado, es shared-secret o nada. **Solo se acepta `POST`**; `GET` y `PUT` devuelven
`405`.

Del entorno de prueba usado para este documento:

```
URL       https://hive-k3m9x7q2.fluxbee.ai/e/ich:14b66389-d425-531c-a140-a591d25e8f39
Bearer    <lo entrega el operador del hive — no se publica acá>
```

---

## 3. ⚠️ El modelo de errores — leer antes de escribir el cliente

**Un error de negocio vuelve con HTTP 200.** El código HTTP solo refleja la capa de transporte y
autenticación; el resultado de la operación está **en el cuerpo**.

| capa | cómo se ve |
|---|---|
| bearer ausente o incorrecto | `HTTP 401` + `{"ok":false,"error_code":"UNAUTHORIZED","message":"missing or invalid bearer secret"}` |
| método distinto de POST | `HTTP 405` |
| **cualquier error de la operación** | **`HTTP 200`** + `{"status":"error","error_detail":"…"}` |
| operación exitosa | `HTTP 200` + `{"status":"ok","result":{…}}` |

```js
// La comprobación correcta
const r = await fetch(url, {...});
if (r.status === 401) throw new Error('bearer invalido');
const body = await r.json();
if (body.status !== 'ok') throw new Error(body.error_detail);   // <-- NO alcanza con r.ok
return body.result;
```

**Toda respuesta trae:**

| campo | qué es |
|---|---|
| `status` | `"ok"` o `"error"` |
| `handled_by` | qué nodo la atendió (`IO.cloud@motherbee`) |
| `ich` | el identificador del canal, eco del pedido |
| `request_id` | UUID por pedido — **úsenlo para correlacionar con los logs del hive** |
| `result` | solo si `status: "ok"` |
| `error_detail` | solo si `status: "error"` |

---

## 4. Las tres operaciones

Forma general del cuerpo:

```json
{ "op": "<operación>", "tenant_id": "<tnt:…>", "params": { … } }
```

`tenant_id` va en la **raíz**, no dentro de `params`.

### 4.1 `create_tenant` — crear (o encontrar) un tenant

Único obligatorio: `params.name`.

```bash
curl -X POST "$URL" -H "Authorization: Bearer $SECRET" -H "Content-Type: application/json" \
  -d '{"op":"create_tenant","params":{"name":"acme-corp"}}'
```

```json
{"status":"ok","op":"create_tenant","request_id":"6b46bc8b-…",
 "result":{"created":true,"matched_by":null,"tenant_id":"tnt:94cd37b8-2137-4d40-a7ee-277a214c11ab",
           "sponsor_tenant_id":null,"status":"ok"}}
```

**Es idempotente por nombre**, y esto es importante para el diseño del cliente: llamarlo dos veces
con el mismo `name` **no crea un segundo tenant** ni falla — devuelve el mismo `tenant_id` con
`created:false` y `matched_by:"name"`. Verificado:

```json
{"result":{"created":false,"matched_by":"name","tenant_id":"tnt:94cd37b8-…"}}
```

⇒ **Pueden llamarlo sin guardar estado previo**; el campo `created` les dice si fue alta nueva.

Opcionales: `params.status` (default `"active"`), `params.domain`, `params.settings`,
`params.sponsor_tenant_id`.

---

### 4.2 `put_token` — guardar una credencial en el vault del tenant

Obligatorios: `tenant_id` (raíz), `params.key`, `params.value` (objeto),
y `params.resource_type` (o dentro de `params.metadata`).

```bash
curl -X POST "$URL" -H "Authorization: Bearer $SECRET" -H "Content-Type: application/json" \
  -d '{"op":"put_token","tenant_id":"tnt:94cd37b8-…",
       "params":{"key":"openai_main","value":{"api_key":"sk-…"},"resource_type":"openai"}}'
```

```json
{"status":"ok","op":"put_token","result":{"changed":true,"key":"openai_main","version":1,"status":"ok"}}
```

`version` se incrementa en cada escritura de la misma `key`; `changed` indica si el valor cambió.

**La forma de `value` depende del `resource_type`.** Los tipos que el hive conoce incluyen
`openai`, `anthropic`, `postgres`, `slack`, `github`, `stripe`, `aws`, `api_key`, `bearer_token` y
varios más. Ejemplos:

```jsonc
{"resource_type":"openai",   "value":{"api_key":"sk-…"}}
{"resource_type":"slack",    "value":{"app_token":"xapp-…","bot_token":"xoxb-…"}}
{"resource_type":"postgres", "value":{"postgres_url":"postgresql://user:pass@host:5432"}}
                              // ↑ credenciales + host, SIN nombre de base
```

Opcional: `params.owner_node` para restringir la credencial a un nodo concreto. Si se omite, queda
disponible para el pool del tenant.

---

### 4.3 `provision_node` — levantar un nodo en el hive

Obligatorios: `tenant_id` (raíz), `params.node_name`.

```bash
curl -X POST "$URL" -H "Authorization: Bearer $SECRET" -H "Content-Type: application/json" \
  -d '{"op":"provision_node","tenant_id":"tnt:94cd37b8-…",
       "params":{"node_name":"IO.acme","runtime":"io.api","runtime_version":"current"}}'
```

```json
{"status":"ok","op":"provision_node",
 "result":{"hive":"motherbee",
           "config":{"created":true,"config_version":1,"path":"…/IO.acme@motherbee/config.json"},
           "identity":{"register":{"ilk_id":"ilk:07e84b17-…"}}}}
```

> ### ⚠️ **Solo nodos `IO.*`**
>
> ```json
> {"status":"error","error_detail":"provision_node may launch only IO.* nodes"}
> ```
>
> Un `node_name` que no empiece con `IO.` es rechazado. **No se pueden aprovisionar nodos `AI.*`,
> `WF.*` ni `SY.*` por esta vía** — eso lo hace el operador del hive.

Opcionales: `params.runtime`, `params.runtime_version` (usen **`"current"`** salvo que necesiten
fijar una versión), `params.config` (objeto), `params.add_channels`, `params.identity_change_reason`.

**Verificado en vivo:** el nodo quedó `active running` y visible en la malla del hive.

---

## 5. Lo que tiene que hacer el operador del hive

Checklist. Sin esto el endpoint no existe.

**1. Configurar IO.cloud** — el paquete instala un ejemplo en
`/etc/fluxbee/io-cloud.env.example`; se copia a `/etc/fluxbee/io-cloud.env` (permisos `0600`):

```bash
IO_CLOUD_EDGE_NODE=SY.edge@ingress1       # el nodo edge donde publicar
IO_CLOUD_SECRET=<token largo y aleatorio>  # el bearer que va a usar Cloud
IO_CLOUD_ADMIN_HIVE=motherbee
IO_CLOUD_IDENTITY_HIVE=motherbee
IO_CLOUD_INBOUND_FAMILY=user
```

```bash
systemctl restart io-cloud
```

**2. Publicar el canal.** `IO.cloud` genera su ICH solo, pero **hoy su auto-publicación falla**
(ver §7). El operador la hace desde el admin del hive:

```bash
curl -X POST http://127.0.0.1:8080/channels/externalize -H 'Content-Type: application/json' -d '{
  "ich":"<el ICH del log de io-cloud>",
  "edge_node":"SY.edge@ingress1",
  "inbound_family":"user",
  "auth_mode":"shared-secret",
  "secret":"<el mismo IO_CLOUD_SECRET>",
  "methods":["POST"]}'
```

El ICH sale del journal: `journalctl -u io-cloud | grep "own ICH ensured"`.

**3. Entregarle a Cloud** la URL (`https://<host>/e/<ich>`) y el bearer.

---

## 6. Errores comunes

| lo que ven | qué pasó |
|---|---|
| `401 missing or invalid bearer secret` | Bearer ausente o distinto del configurado. **El edge corta acá**, no llega al nodo. |
| `405` | Usaron un método que no es `POST`. |
| `unknown op '…'` | El `op` no es uno de los tres. Ojo con typos: son `create_tenant`, `put_token`, `provision_node`. |
| `missing 'op'` | Falta el campo `op` en la raíz. |
| `missing or invalid params.name` | `create_tenant` sin `params.name`. |
| `put_token requires tenant_id` | Falta `tenant_id` **en la raíz** (no alcanza ponerlo en `params`). |
| `put_token requires params.resource_type…` | Falta el tipo de credencial. |
| `provision_node may launch only IO.* nodes` | El `node_name` no empieza con `IO.`. |
| `IO.cloud may relay only [...]` | Pidieron una operación fuera de la lista blanca. **Es el gate de seguridad del hive**, no un bug. |

Cuando algo no cierre, pásenle el **`request_id`** al operador del hive: con eso encuentra la
traza completa del lado de adentro.

---

## 7. Limitaciones conocidas — léanlo, les ahorra tiempo

**Tres operaciones y nada más.** No hay listar, consultar ni borrar: no se puede preguntar qué
tenants existen, leer un token, ni dar de baja un nodo. Todo eso es del operador del hive. Diseñen
el cliente asumiendo que **Cloud escribe pero no lee**.

**No hay verificación de resultado.** `provision_node` devuelve `ok` cuando el nodo quedó lanzado,
pero no hay forma por esta API de preguntar después si sigue vivo o en qué estado está.

**`create_tenant` es idempotente; los otros dos no.** Un `put_token` repetido sobrescribe (sube
`version`). Un `provision_node` sobre un nodo existente **no fue probado** — asuman que puede
fallar y no lo usen como "asegurar que existe".

**La auto-publicación de IO.cloud está rota.** El nodo intenta publicar su propio canal al arrancar
y el hive se lo rechaza:

```
UNAUTHORIZED: IO.cloud may relay only ["create_tenant","vault_put","run_node"]
              over the mesh, not 'externalize'
```

Es un conflicto interno de fluxbee entre dos controles de seguridad que por separado están bien.
**No los afecta**, pero implica que **el operador tiene que publicar el canal a mano** (§5 paso 2), y
que **si reinstalan o recrean el hive hay que repetirlo**. Está reportado para corregirse.

**Un solo bearer para todo.** No hay credenciales por tenant ni rotación automática: el mismo
secreto habilita las tres operaciones sobre todos los tenants. Rotarlo implica cambiar
`IO_CLOUD_SECRET` y re-publicar el canal.

**No verificado en este ciclo:** el comportamiento con cargas concurrentes, límites de tamaño del
cuerpo, y si `put_token` sobre un tenant inexistente falla o lo crea.

---

## 8. Prueba de humo

Para validar un hive nuevo de punta a punta:

```bash
URL="https://<host>/e/<ich>"; SEC="<bearer>"
post() { curl -sS -m 30 -X POST "$URL" -H "Authorization: Bearer $SEC" \
         -H 'Content-Type: application/json' -d "$1"; echo; }

# 1. rechaza sin credencial  -> 401
curl -s -o /dev/null -w '%{http_code}\n' -X POST "$URL" -d '{}'

# 2. crea un tenant          -> status ok, created true
post '{"op":"create_tenant","params":{"name":"smoke-test"}}'

# 3. idempotencia            -> created false, mismo tenant_id
post '{"op":"create_tenant","params":{"name":"smoke-test"}}'

# 4. guarda una credencial   -> changed true, version 1
post '{"op":"put_token","tenant_id":"<el de arriba>",
       "params":{"key":"smoke","value":{"api_key":"x"},"resource_type":"openai"}}'

# 5. levanta un nodo         -> config.created true, identity.register.ilk_id
post '{"op":"provision_node","tenant_id":"<el de arriba>",
       "params":{"node_name":"IO.smoke","runtime":"io.api","runtime_version":"current"}}'
```

Los cinco pasos se ejecutaron contra producción para escribir este documento.
