# IO.cloud — API para Fluxbee Cloud

> **Para el equipo de desarrollo de Fluxbee Cloud.** No hace falta conocer fluxbee por dentro.
>
> **Procedencia de cada dato** (importa, porque hay afirmaciones que se contradicen entre sí en
> versiones anteriores de este documento):
>
> | marca | significa |
> |---|---|
> | 🟢 **live** | Probado contra `https://hive-k3m9x7q2.fluxbee.ai` desde fuera de la red, por internet. |
> | 🔵 **código** | Leído y verificado línea por línea en el código fuente del hive. Determinístico. |
> | 🟡 **no verificado** | Dicho explícitamente como no comprobado. No lo asuman. |
>
> Última revisión: **2026-07-30**. Donde algo cambió respecto de la versión anterior de este
> documento, está marcado con ⚠️ **CORREGIDO**.

---

## 1. Qué es IO.cloud y qué resuelve

Un **hive** de fluxbee es una instalación privada. Sus servicios internos —el que guarda
credenciales, el que lleva el registro de clientes, el que lanza procesos— hablan entre sí por una
red interna que no está publicada en internet y que no tiene ninguna puerta HTTP. Eso es
deliberado: es lo que hace que un hive sea seguro por defecto. Pero deja un problema práctico:
Fluxbee Cloud necesita poder dar de alta un cliente dentro de un hive, cargarle sus credenciales y
levantarle un nodo, sin que nadie entre por SSH a hacerlo a mano.

**IO.cloud es la única puerta para eso, y es deliberadamente angosta.** Es un componente que corre
dentro del hive, publica **un solo endpoint HTTPS**, y sabe hacer exactamente **tres cosas**: crear
un tenant, guardar una credencial y levantar un nodo. No es un API general del hive con una lista
blanca encima: es un traductor de tres entradas fijas. Cualquier otra operación que Cloud pudiera
querer —leer un secreto, listar tenants, matar un nodo, agregar un servidor— **no está bloqueada
por configuración, no existe como camino**. Hay además un segundo control del lado del hive que
vuelve a validar cada pedido que llega por esta vía, por si el propio IO.cloud estuviera
comprometido. En resumen: Cloud no habla con el hive; habla con IO.cloud, y IO.cloud solo sabe
hacer tres cosas.

---

## 2. Cómo se conecta

```text
  Fluxbee Cloud (ustedes)
        │
        │  POST https://<host>/e/<ich>
        │  Authorization: Bearer <secreto>
        │  Content-Type: application/json
        ▼
  ┌───────────────────────────────────────────────────────────┐
  │ SY.edge   — la única pieza expuesta a internet            │
  │  1. ¿existe el <ich>?          no → 404                   │
  │  2. ¿el método es POST?        no → 405                   │
  │  3. ¿el bearer coincide?       no → 401                   │
  │  4. ¿el cuerpo entra en 64 KiB? no → 413                  │
  │  → borra el header Authorization: NO viaja hacia adentro  │
  └───────────────────────────────────────────────────────────┘
        │  (red interna del hive)
        ▼
  ┌───────────────────────────────────────────────────────────┐
  │ IO.cloud  — traduce {op, tenant_id, params} → 1 acción     │
  │  valida forma, reescribe campos de autoridad              │
  └───────────────────────────────────────────────────────────┘
        │
        ▼
  ┌───────────────────────────────────────────────────────────┐
  │ SY.admin  — vuelve a validar contra la lista blanca       │
  │            y ejecuta contra el servicio que corresponda   │
  └───────────────────────────────────────────────────────────┘
```

**La URL.** Tiene la forma `https://<host-del-hive>/e/<ich>`. El `<ich>` es el identificador del
canal. **Se lo pide al operador del hive** — cada instalación tiene el suyo.

> ⚠️ **CORREGIDO — el ICH no es un secreto y con la configuración por defecto es el mismo en toda
> instalación.** 🔵 código: el ICH se deriva de forma determinística (UUIDv5 de
> `<tenant>:<channel_type>:<address>`); con los valores por defecto (`cloud` / `demo` / tenant raíz)
> da siempre `ich:14b66389-d425-531c-a140-a591d25e8f39`. **La única protección real del endpoint es
> el bearer.** No traten la URL como una credencial.

**La autenticación** es un único bearer estático compartido, el mismo para todas las operaciones y
todos los tenants. No hay OAuth, no hay mTLS de cliente, no hay firma por mensaje, no hay
credencial por tenant.

Detalles del bearer que cuestan tiempo si no se saben (🔵 código, 🟢 live):

- El prefijo aceptado es **`"Bearer "` o `"bearer "`, literal y con el espacio** — nada más.
  `BEARER <token>` en mayúsculas → **401** (🟢 probado).
- La comparación es en tiempo constante; un secreto vacío nunca autoriza.
- Si el operador rota el secreto, el **anterior sigue siendo válido 10 minutos** (ventana de gracia).
- **Solo `POST`.** `GET`, `PUT`, `DELETE` → **405** (🟢 probado). El chequeo de método corre **antes**
  que el de bearer: un `PUT` sin credencial devuelve 405, no 401.

Entorno de producción usado para este documento:

```text
Host      https://hive-k3m9x7q2.fluxbee.ai        (🟢 TLS válido, cadena completa, TLSv1.3)
ICH       ich:14b66389-d425-531c-a140-a591d25e8f39 (🟢 publicado y respondiendo)
URL       https://hive-k3m9x7q2.fluxbee.ai/e/ich:14b66389-d425-531c-a140-a591d25e8f39
Bearer    lo entrega el operador del hive — no se publica acá
```

### 2.1 De dónde sale el bearer, y qué implica

Conviene saberlo porque determina qué pasa cuando hay que rotarlo. 🔵 código, 🟢 confirmado en vivo.

```text
1. ORIGEN      /etc/fluxbee/io-cloud.env  →  IO_CLOUD_SECRET     lo escribe el operador
2. PUBLICACIÓN IO.cloud se lo pasa a SY.admin UNA vez, al publicar el canal
3. GUARDADO    SY.admin lo escribe en el vault del hive bajo `edge_channel_secret:<ich>`
               (resource_type: bearer_token, propiedad del nodo de borde)
4. LA PUERTA   guarda solo el NOMBRE de esa clave en su tabla de rutas.
               El valor vive en memoria y NUNCA se escribe en el disco de la zona expuesta.
5. REINICIO    la puerta vuelve a leer el valor del vault por ese nombre
```

**El archivo de entorno es la semilla, no el almacenamiento.** El registro de verdad es el vault
del hive.

Tres consecuencias prácticas:

- **Sobrevive reinicios.** 🟢 Se reinició el servicio de borde en producción y el endpoint siguió
  respondiendo `200` sin intervención: el valor se re-leyó del vault.
- **Nadie puede recuperarlo.** Una vez guardado, **ni el administrador del hive puede leerlo** — el
  vault no tiene bypass para ese secreto. Si lo pierden, no se recupera: **se rota**.
- **Al rotar hay 10 minutos de gracia.** El secreto anterior sigue siendo aceptado durante ese rato,
  así que un cliente en vuelo no recibe un `401` en el instante del cambio. Pero **no confíen en eso
  como estrategia**: es una red de seguridad para el corte, no una ventana de migración.

> ⚠️ **Cuidado al re-publicar el canal.** Si el operador lo re-externaliza **sin** pasar el mismo
> secreto, el hive **acuña uno nuevo** y el bearer que ustedes tienen deja de servir. Es un residual
> conocido del lado del hive (no pueden detectarlo desde afuera salvo por el `401`). Si les empieza
> a fallar la autenticación de golpe y sin cambios de su lado, **pregunten si alguien re-publicó el
> canal**.

---

---

## 3. ⚠️ El modelo de errores — leer antes de escribir el cliente

Esta sección es la que más tiempo les ahorra. Hay **dos formas de error distintas** y **el código
HTTP casi nunca refleja si la operación salió bien**.

### 3.1 Un error de negocio vuelve con HTTP 200

🔵 código: cuando el pedido llegó a IO.cloud, el hive devuelve el cuerpo de la respuesta tal cual,
**siempre con HTTP 200**, sea éxito o error. El código de estado interno (400, 403, 409…) que el
hive calcula se pierde en el camino y no llega a ustedes. **El resultado está en el cuerpo.**

### 3.2 Las dos formas de error

| forma | quién la emite | cómo se reconoce |
|---|---|---|
| `{"ok":false,"error_code":"…","message":"…"}` | **la puerta** (SY.edge). No llegó al nodo. | tiene `ok`, **no** tiene `status` |
| `{"status":"error","error_detail":"…"}` | **IO.cloud o el hive.** Llegó y falló. | tiene `status`, **no** tiene `ok` |

🟢 live, sin credencial:

```http
$ curl -i -X POST "$URL" -d '{}'
HTTP/1.1 401
{"error_code":"UNAUTHORIZED","message":"missing or invalid bearer secret","ok":false}
```

### 3.3 La comprobación correcta

```js
const r = await fetch(url, { method: 'POST', headers, body });

// 1. errores de la puerta: traen codigo HTTP real y forma {ok:false}
if (r.status !== 200) {
  const e = await r.json().catch(() => ({}));
  throw new EdgeError(r.status, e.error_code, e.message);   // 401 404 405 413 500 502 503 504
}

// 2. errores de negocio: HTTP 200 con status:"error" en el cuerpo
const body = await r.json();
if (body.status !== 'ok') {
  //   error_code puede venir null: ver 3.4
  throw new HiveError(body.error_code ?? null, body.error_detail, body.request_id);
}
return body.result;
```

**No alcanza con `r.ok`.** Un cliente que solo mire el status HTTP va a tomar por exitoso el 100 %
de los errores de negocio.

### 3.4 ⚠️ CORREGIDO — `error_code` **no siempre viene**

🔵 código. Hay **cuatro** categorías de error de negocio y **no tienen la misma forma**:

| categoría | `op` | `error_code` | `error_detail` |
|---|---|---|---|
| validación local de IO.cloud (falta un campo, `op` desconocido) | **ausente** | **ausente** | string libre en inglés |
| error del hive (vault / registro de clientes / lanzador) | presente | presente (ver §5.2) | string |
| rechazo de la lista blanca del hive | presente | `"UNAUTHORIZED"` | string |
| fallo de transporte hacia el hive (timeout) | **ausente** | **ausente** | `"admin call failed: …"` |

Existe además un rechazo propio de IO.cloud (`error_code:"UNAUTHORIZED"`, **sin** `op`), pero 🔵
código solo es alcanzable si el pedido llega apuntando a un canal que no es el suyo — cosa que no
puede pasar usando la URL que les dio el operador. Si lo ven, es un problema de configuración del
hive, no del cliente.

⇒ **No discriminen por `error_code`.** Para los errores de validación no hay más que el texto de
`error_detail`. Los strings exactos están en §5.1 y son estables (verificados en el código); si van
a matchear, matcheen contra esa lista.

### 3.5 Campos comunes a toda respuesta

| campo | siempre | qué es |
|---|---|---|
| `status` | sí | `"ok"` o `"error"` |
| `handled_by` | sí | qué nodo la atendió, p.ej. `"IO.cloud@motherbee"` |
| `ich` | sí | eco del canal |
| `op` | **no** | ausente en errores de validación local y de transporte |
| `request_id` | **casi** | ver abajo |
| `result` | solo en `ok` | payload del servicio interno |
| `error_code` | **no** | ver §3.4 |
| `error_detail` | solo en `error` | string |

⚠️ **CORREGIDO — `request_id`.** 🔵 código: si ustedes lo mandan, vuelve siempre. Si **no** lo
mandan, el hive genera un UUID y lo devuelve en los caminos `ok` y error-del-hive, pero en un error
de **validación local** la respuesta **no va a traer `request_id` en absoluto**. Recomendación:
**manden siempre `request_id`** (un UUID por intento). Es lo único que le permite al operador del
hive encontrar la traza de ese pedido.

⚠️ **CORREGIDO — el detalle estructurado del error se pierde.** 🔵 código: cuando el servicio interno
devuelve un error con datos (por ejemplo, la ruta del archivo que ya existía), IO.cloud se queda
**solo con el string** y descarta el objeto. `error_detail` es siempre texto.

---

## 4. Referencia de las tres operaciones

Sobre común. **`tenant_id` va en la raíz, no dentro de `params`.**

```json
{
  "op":         "create_tenant | put_token | provision_node",   // OBLIGATORIO
  "tenant_id":  "tnt:<uuid>",                                    // ver cada op
  "params":     { },                                             // ver cada op
  "request_id": "<uuid propio>"                                  // opcional pero MUY recomendado
}
```

Reglas del sobre (🔵 código):

- `op` se compara **exacto y case-sensitive, y NO se le hace trim**. `" put_token"` con un espacio
  adelante → `unknown op ' put_token'`.
- `tenant_id` sí se trimea y debe ser **canónico**: `tnt:` + un UUID parseable.
- `params` ausente equivale a `{}`.
- Campos extra en la raíz se ignoran.
- Cuerpo máximo **64 KiB** — y el tope se aplica **dos veces**: al cuerpo crudo y de nuevo al sobre
  interno ya serializado, así que el presupuesto útil es algo menor a 64 KiB.

Variables para los ejemplos:

```bash
URL="https://hive-k3m9x7q2.fluxbee.ai/e/ich:14b66389-d425-531c-a140-a591d25e8f39"
SEC="<el bearer que entrega el operador>"
post() { curl -sS -m 35 -X POST "$URL" \
           -H "Authorization: Bearer $SEC" -H 'Content-Type: application/json' \
           -d "$1"; echo; }
```

> 🟡 Los cuerpos de respuesta de éxito que siguen provienen de una corrida contra este mismo hive
> el 2026-07-30 (registrada en el logbook del proyecto) más la lectura del código. **En esta
> revisión no se pudieron re-ejecutar las llamadas autenticadas** (no se dispone del bearer), así
> que están marcadas 🔵. Lo que sí se re-verificó en vivo hoy es todo lo de §2 y §3.2.

---

### 4.1 `create_tenant` — dar de alta (o encontrar) un cliente

| campo | obligatorio | notas |
|---|---|---|
| `tenant_id` (raíz) | ❌ **se IGNORA** | esta op no lo usa |
| `params.name` | ✅ **sí** | string, no vacío |
| `params.domain` | opcional | string. **Sirve como clave de deduplicación, con prioridad sobre `name`** |
| `params.status` | opcional | string; **solo** `"pending"`, `"active"` o `"suspended"`. Default que aplica IO.cloud: `"active"` |
| `params.settings` | opcional | objeto JSON libre |
| `params.sponsor_tenant_id` | opcional | `tnt:<uuid>` de un tenant **que ya exista** |

Cualquier otro campo dentro de `params` **se descarta silenciosamente** (🔵 IO.cloud copia solo esos
cinco; es a propósito, porque el servicio de abajo rechaza campos desconocidos).

```bash
post '{"op":"create_tenant","request_id":"11111111-1111-1111-1111-111111111111",
       "params":{"name":"acme-corp","domain":"acme.com"}}'
```

```json
{"status":"ok","op":"create_tenant","request_id":"11111111-…",
 "handled_by":"IO.cloud@motherbee","ich":"ich:14b66389-…",
 "result":{"status":"ok",
           "tenant_id":"tnt:94cd37b8-2137-4d40-a7ee-277a214c11ab",
           "created":true,
           "matched_by":null,
           "sponsor_tenant_id":null}}
```

#### Idempotencia — y la trampa que trae

🔵 código. **Es idempotente**, pero por un mecanismo que hay que entender bien:

1. Si mandaron `domain` y **ya existe un tenant con ese dominio** (normalizado), devuelve **ese**,
   con `created:false` y `matched_by:"domain"`.
2. Si no, si **ya existe un tenant con ese `name`** (trim + minúsculas), devuelve **ese**, con
   `created:false` y `matched_by:"name"`.
3. Si no, crea uno nuevo: `created:true`, `matched_by:null`.

En los casos 1 y 2 **no se modifica nada del tenant existente**: ni el `status`, ni el `domain`, ni
los `settings` que ustedes mandaron. Se ignoran.

> ⚠️ **La deduplicación por `name` es global al hive, no por cliente de Cloud.** Dos altas
> distintas con `name:"Acme"` y `name:"acme "` reciben **el mismo `tenant_id`**, que puede
> pertenecer a otro cliente. **Recomendación fuerte: manden siempre `domain`**, o usen un `name`
> con un prefijo propio y único. Y **siempre miren `created` y `matched_by`** antes de asumir que
> el tenant es suyo.

#### Errores — `create_tenant`

| condición | `error_detail` / `error_code` |
|---|---|
| falta `params.name` | `"missing or invalid params.name"` (sin `error_code`) |
| `status` fuera de los tres valores | `error_code:"INVALID_REQUEST"` |
| `sponsor_tenant_id` que no existe | `error_code:"INVALID_SPONSOR_TENANT"` |
| `status`/`domain`/`sponsor_tenant_id` que no son string | `error_code:"INVALID_REQUEST"` |

> ⚠️ **CORREGIDO — en esta op el `error_detail` no sirve para diagnosticar.** 🔵 código: para
> cualquier error de negocio de `create_tenant`, el texto es siempre la constante
> **`"failed to create tenant"`**. Toda la información está en `error_code`. Es la única de las tres
> ops donde esto pasa.

---

### 4.2 `put_token` — guardar una credencial en el vault

| campo | obligatorio | notas |
|---|---|---|
| `tenant_id` (raíz) | ✅ **sí**, canónico `tnt:<uuid>` | |
| `params.key` | ✅ **sí** | **charset restringido, ver abajo** |
| `params.value` | ✅ **sí**, cualquier JSON no-`null` | ≤ 1 MiB serializado |
| `params.resource_type` | ✅ **sí** (o dentro de `params.metadata`) | se **normaliza**, ver abajo |
| `params.owner_node` | opcional | debe empezar con `IO.`; **el nodo tiene que existir ya** |
| `params.metadata` | opcional | **esquema fijo**, ver abajo |

```bash
post '{"op":"put_token","tenant_id":"tnt:94cd37b8-…",
       "request_id":"22222222-2222-2222-2222-222222222222",
       "params":{"key":"acme_openai_main",
                 "value":{"api_key":"sk-…"},
                 "resource_type":"openai"}}'
```

```json
{"status":"ok","op":"put_token","request_id":"22222222-…",
 "handled_by":"IO.cloud@motherbee","ich":"ich:14b66389-…",
 "result":{"status":"ok","key":"acme_openai_main","version":1,"changed":true}}
```

#### ⚠️ `params.key` — charset estricto y **espacio de nombres GLOBAL**

🔵 código. Dos cosas que no son evidentes y que rompen en producción:

**(a) Charset.** 1 a 256 bytes; el primer byte debe ser `[a-z0-9]`; todos los bytes deben estar en
`[a-z0-9:_-]`. **Nada de mayúsculas, puntos, barras, arrobas ni espacios.**
`"wapp_token:acme@corp"` es ilegal → `error_code:"INVALID_REQUEST"`,
`error_detail:"invalid key format"` (o `"invalid key prefix"` / `"invalid key length"`).

**(b) La clave es global al hive, no por tenant.** El almacenamiento indexa por `key` y nada más.
Si dos tenants usan `key:"openai_main"`, **el segundo pisa el secreto del primero** y además le
reescribe el `tenant_id` del dueño. **Namespaceen ustedes la clave**, p.ej.
`t94cd37b8_openai_main` o `acme-corp_openai_main`.

#### ⚠️ CORREGIDO — `version` y `changed`: un re-`put` con el mismo valor **no hace nada**

🔵 código. La versión anterior de este documento decía *"`version` se incrementa en cada escritura"*.
Es falso:

- valor **distinto** al guardado → escribe, `version` sube en 1, `changed:true`;
- valor **idéntico** → **no escribe nada**, devuelve la `version` actual sin cambiarla y
  `changed:false`.

Y hay una consecuencia importante: en el caso `changed:false` **la metadata tampoco se guarda**.
⇒ **No se puede re-scopear un secreto existente** (cambiar `owner_node`, `description`, `tags`)
reenviándolo con el mismo `value`. La llamada devuelve `ok` y no hace nada. Si necesitan cambiar la
metadata, tienen que cambiar también el valor.

#### `resource_type` — se normaliza, no se guarda literal

🔵 código: se pasa a minúsculas y todo lo no alfanumérico se colapsa a `_`.
`"OpenAI"` → `openai`; `"Google Calendar"` → `google_calendar`; `"linked-helper"` → `linked_helper`.
Rechaza: vacío tras normalizar, solo dígitos, más de 64 caracteres.

No hay una lista cerrada de tipos válidos — cualquier string custom pasa. Pero **la forma de
`value` no se valida en ningún punto del camino**: se guarda cualquier JSON. Si la forma no es la
que espera el consumidor, el error aparece recién cuando ese nodo intente usar la credencial. Formas
que los nodos del hive esperan hoy:

```jsonc
{"resource_type":"openai",   "value":{"api_key":"sk-…"}}
{"resource_type":"anthropic","value":{"api_key":"sk-ant-…"}}
{"resource_type":"slack",    "value":{"app_token":"xapp-…","bot_token":"xoxb-…"}}
{"resource_type":"postgres", "value":{"postgres_url":"postgresql://user:pass@host:5432"}}
                              // ↑ sin nombre de base
```

#### ⚠️ CORREGIDO — `params.metadata` es un **esquema fijo**, no un bag libre

🔵 código. Las claves que sobreviven son exactamente:
`description` (string), `tags` (array de strings), `created_by`, `created_at`, `updated_at`.
**Cualquier otra clave se descarta en silencio.** Y son tipadas: mandar `description: 123` o
`tags: "a"` hace fallar la escritura entera con `error_code:"INVALID_VALUE"`.

Además IO.cloud **borra siempre** de la metadata que ustedes manden los campos de autoridad
`tenant_id`, `ilk`, `owner_ilk`, `owner_l2`, `owner_node`, y reinyecta los suyos a partir del
`tenant_id` de la raíz. No intenten fijarlos: además hay un segundo control del lado del hive que
rechazaría el pedido con `UNAUTHORIZED`.

#### `owner_node` — y el orden obligatorio

Si lo omiten, el secreto queda en el **pool del tenant**: cualquier nodo de ese tenant puede leerlo.
Si lo ponen, el secreto queda restringido a ese nodo.

⚠️ 🔵 código: `owner_node` **debe apuntar a un nodo que ya exista en el hive**. La resolución
consulta el registro en el momento de escribir y si el nodo no está registrado falla con:

```text
error_code: "INVALID_REQUEST"
error_detail: "metadata.owner_node 'IO.x@motherbee' is not registered in identity;
               register/spawn it first or omit owner_node for a tenant-pool secret"
```

⇒ **El orden es `provision_node` ANTES de `put_token` con `owner_node`.** Si la credencial la
necesita el nodo en su primer arranque, hay un problema de huevo y gallina: guárdenla **sin**
`owner_node` (pool del tenant).

Y un `owner_node` que no empiece con `IO.` es **error duro**, no un descarte silencioso:
`"put_token owner_node must be an IO.* node"`.

#### ⚠️ CORREGIDO — `put_token` **no verifica que el tenant exista**

🔵 código: solo se valida el formato `tnt:<uuid>`. Un `put_token` contra un tenant inventado
**devuelve `ok`**. No lo usen como comprobación de que el `create_tenant` funcionó.

#### Errores — `put_token`

| condición | respuesta |
|---|---|
| falta `tenant_id` en la raíz | `"put_token requires tenant_id"` (sin `error_code`) |
| `tenant_id` mal formado | `"put_token requires canonical tenant_id tnt:<uuid>"` |
| falta `params.key` | `"missing or invalid params.key"` |
| falta `params.value` o es `null` | `"missing params.value"` |
| falta el tipo | `"put_token requires params.resource_type or params.metadata.resource_type"` |
| `owner_node` no-`IO.*` | `"put_token owner_node must be an IO.* node"` |
| `key` con charset inválido | `INVALID_REQUEST` / `"invalid key format"` |
| `value` > 1 MiB | `INVALID_REQUEST` / `"secret value exceeds 1 MiB"` |
| metadata con tipos equivocados | `INVALID_VALUE` |
| `owner_node` no registrado | `INVALID_REQUEST` (texto arriba) |
| fallos del almacenamiento | `STORAGE_ERROR`, `ENCRYPTION_ERROR`, `MASTER_KEY_NOT_AVAILABLE` |

---

### 4.3 `provision_node` — levantar un nodo en el hive

| campo | obligatorio | notas |
|---|---|---|
| `tenant_id` (raíz) | ✅ **sí**, canónico | |
| `params.node_name` | ✅ **sí** | **debe empezar con `IO.`**; el sufijo `@hive` elige servidor |
| `params.runtime` | opcional (ver abajo) | qué implementación levantar |
| `params.runtime_version` | opcional | default `"current"` |
| `params.config` | opcional | objeto. IO.cloud **pisa** `config.tenant_id` con el de la raíz |
| `params.add_channels` | opcional | array; forma exacta abajo |
| `params.identity_change_reason` | opcional | string descriptivo |

Cualquier otro campo dentro de `params` se descarta.

```bash
post '{"op":"provision_node","tenant_id":"tnt:94cd37b8-…",
       "request_id":"33333333-3333-3333-3333-333333333333",
       "params":{"node_name":"IO.acme","runtime":"io.api","runtime_version":"current"}}'
```

```json
{"status":"ok","op":"provision_node","request_id":"33333333-…",
 "handled_by":"IO.cloud@motherbee","ich":"ich:14b66389-…",
 "result":{"status":"ok",
           "node_name":"IO.acme@motherbee",
           "runtime":"io.api","version":"…","requested_version":"current",
           "hive":"motherbee","target":"motherbee",
           "unit":"fluxbee-node-…",
           "identity":{"register":{"ilk_id":"ilk:07e84b17-…"}},
           "config":{"created":true,"config_version":1,"path":"…/IO.acme@motherbee/config.json"}}}
```

#### ⚠️ CORREGIDO — **NO es idempotente**, nunca

🔵 código, verificado en el orden real de las comprobaciones. Un segundo `provision_node` con el
mismo `node_name` **siempre falla**:

```json
{"status":"error","op":"provision_node","error_code":"NODE_ALREADY_EXISTS",
 "error_detail":"node config already exists: …/IO.acme@motherbee/config.json"}
```

La comprobación de "ya existe" corre **antes** que cualquier otra cosa, así que no hay ningún caso
en que un re-provision devuelva `ok`. **No lo usen como "asegurar que existe".** Si necesitan esa
semántica, tienen que llevar ustedes el registro de qué nodos ya crearon. (Existe internamente una
respuesta `state:"already_running"`, pero solo es alcanzable en un caso de recuperación interna del
hive; desde Cloud no se llega a ella.)

#### ⚠️ CORREGIDO — `node_name` con `@hive` **elige el servidor de destino**

🔵 código. `"node_name":"IO.acme"` (sin sufijo) → se crea en el servidor principal del hive.
`"node_name":"IO.acme@ingress1"` → **se crea en el servidor `ingress1`**, y el pedido se reenvía
allí. Si no saben qué servidores tiene el hive, **omitan el sufijo**. (Las otras dos operaciones no
tienen esta palanca: siempre van al servidor principal.)

#### `runtime` — opcional pero de facto obligatorio

🔵 código: si lo omiten, se deriva de los **dos primeros segmentos separados por punto** del
`node_name`. `IO.api.support` → runtime `io.api`. Pero `IO.acme` → runtime `io.acme`, que
**probablemente no exista** → `error_code:"RUNTIME_NOT_AVAILABLE"`. Y `IO.acme` sin un segundo
segmento ni siquiera es derivable → `INVALID_REQUEST` con
`"missing runtime (or runtime not derivable from node_name)"`.

**Recomendación: manden `runtime` siempre, explícito.**

`runtime_version`: usen `"current"` salvo que necesiten fijar una versión.

#### `add_channels` — forma exacta

Array de objetos, cada uno con los tres campos: `ich_id` (`"ich:<uuid>"`), `type`, `address`.
Si falta alguno: `"invalid add_channels entry: …"`.

#### Solo nodos `IO.*`

```json
{"status":"error","error_detail":"provision_node may launch only IO.* nodes"}
```

🔵 código: se valida en IO.cloud **y otra vez** del lado del hive. No se pueden crear nodos `AI.*`,
`WF.*` ni `SY.*` por esta vía. Eso lo hace el operador.

#### Errores — `provision_node`

| `error_code` | qué pasó |
|---|---|
| `NODE_ALREADY_EXISTS` | ya existe un nodo con ese nombre |
| `RUNTIME_NOT_AVAILABLE` | el runtime o la versión pedidos no se pueden resolver |
| `RUNTIME_MANIFEST_MISSING` / `MANIFEST_INVALID` | el catálogo de runtimes del hive está roto |
| `INVALID_REQUEST` | `node_name` inválido, runtime no derivable, `runtime_version` inválido |
| `IDENTITY_REGISTER_FAILED` / `IDENTITY_UPDATE_FAILED` | falló el alta en el registro del hive |
| `CONFIG_WRITE_FAILED` | no se pudo escribir la configuración |
| `SERVICE_FAILED` / `SPAWN_FAILED` | el proceso no arrancó |

En los fallos posteriores al alta, el hive hace *rollback* (borra la configuración y revierte el
registro) para que un reintento no choque con `NODE_ALREADY_EXISTS`. 🟡 No verificado en vivo.

---

### 4.4 `register_human` — dar de alta un humano (crear su ilk)

🔵 código, **NUEVO (0.1.23), no re-ejecutado en vivo** (no se dispone del bearer). Es la primera
acción **local**: IO.cloud la resuelve él mismo (no relaya a `SY.admin`). Provisiona un ilk humano
`temporary`, se lo entrega al **frontdesk** (que valida y registra), y devuelve el veredicto + el
`ilk_id` que creó.

**El sobre:** `tenant_id` en la **raíz** (como put_token/provision_node), y **`params` ES un
`frontdesk_handoff`** con la data del humano.

| campo | obligatorio | notas |
|---|---|---|
| `tenant_id` (raíz) | ✅ **sí**, canónico `tnt:<uuid>` **existente y no-pending** | |
| `params.type` | ✅ `"frontdesk_handoff"` | fijo |
| `params.schema_version` | ✅ `1` (número) | fijo |
| `params.operation` | ✅ `"complete_registration"` | fijo |
| `params.subject.display_name` | ✅ **sí** | nombre |
| `params.subject.email` | ✅ **sí**, email real (con `@`) | **clave única del humano** (dedup por cloud+email+tenant) |
| `params.subject.phone` | opcional | se guarda en el ilk |
| `params.subject.company_name` | opcional | se guarda en el ilk |
| `params.subject.attributes` | opcional, objeto JSON libre | extras, se guardan verbatim en el ilk |

```bash
post '{"op":"register_human","tenant_id":"tnt:94cd37b8-…",
       "request_id":"44444444-4444-4444-4444-444444444444",
       "params":{"type":"frontdesk_handoff","schema_version":1,"operation":"complete_registration",
                 "subject":{"display_name":"Juan Perez","email":"juan@acme.com",
                            "phone":"+54…","company_name":"ACME","attributes":{"crm_id":"x"}}}}'
```

```json
{"status":"ok","op":"register_human","request_id":"44444444-…",
 "handled_by":"IO.cloud@motherbee","ich":"ich:14b66389-…",
 "ilk_id":"ilk:<el ilk humano creado>","registration_status":"complete",
 "success":true,"human_message":"…"}
```

Si el frontdesk no puede registrar → `status:"error"`, `success:false`, `error_code` + `human_message`,
y el ilk queda `temporary`. **Es idempotente:** repetir el mismo `register_human` de un humano ya
registrado vuelve a dar éxito (no duplica el ilk).

**Errores — `register_human`:**

| condición | respuesta |
|---|---|
| falta `tenant_id` en la raíz / no canónico | `"register_human requires tenant_id"` / `"…canonical tenant_id tnt:<uuid>"` |
| `params` no es objeto | `"register_human requires an object 'params' (the frontdesk_handoff payload)"` |
| `params.type` ≠ `frontdesk_handoff` | `"params.type must be \"frontdesk_handoff\""` |
| `params.schema_version` ≠ 1 | `"params.schema_version must be 1"` |
| `params.operation` ≠ `complete_registration` | `"params.operation must be \"complete_registration\""` |
| `params.subject.email` ausente / sin `@` | `"params.subject.email is required and must be an email address"` |
| falla la provisión del ilk | `error_code:"IDENTITY_UNAVAILABLE"` |
| el frontdesk no responde | `error_code:"FRONTDESK_UNAVAILABLE"` |

### 4.5 `list_cloud_actions` — descubrir qué ofrece IO.cloud

🔵 código, **NUEVO (0.1.23).** Acción **local**. Devuelve el catálogo de acciones que IO.cloud ofrece
(relay + locales) con una descripción de cada una — para descubrir la superficie sin depender de este
documento.

```bash
post '{"op":"list_cloud_actions","request_id":"55555555-…"}'
```

```json
{"status":"ok","op":"list_cloud_actions","request_id":"55555555-…",
 "handled_by":"IO.cloud@motherbee","ich":"ich:14b66389-…",
 "result":{"actions":[
   {"op":"create_tenant","category":"relay","summary":"…"},
   {"op":"put_token","category":"relay","summary":"…"},
   {"op":"provision_node","category":"relay","summary":"…"},
   {"op":"register_human","category":"local","summary":"…"},
   {"op":"list_cloud_actions","category":"local","summary":"…"}]}}
```

- `category:"relay"` = IO.cloud la traduce a una acción de `SY.admin`.
- `category:"local"` = IO.cloud la resuelve él mismo (no toca `SY.admin`).

---

## 4bis. Lectura de identidad (fase 2)

🔵 código, **NUEVO (0.1.29), no re-ejecutado en vivo** (sin bearer). Dos velocidades: las **rápidas**
(`get_ilk`, `get_tenant`, `list_ilks`) las resuelve IO.cloud **desde la memoria compartida (SHM) del
hive, sin round-trip** — devuelven existencia + un **subset**; la **completa** (`get_ilk_details`) va
por relay a `SY.admin` y trae **todo** (identification: phone/company/attributes + canales + tenant).

> ⚠️ Igual que las escrituras: **el tenant es aseverado por el caller y no se chequea ownership**
> (MVP). Un holder del bearer puede leer cualquier id que nombre.

### 4.6 `get_ilk` — ¿existe el ilk? + subset (rápido, SHM)

**Dos selectores, exactamente uno:**

| campo | oblig. | |
|---|---|---|
| `params.ilk_id` | ✅ (opción A) | `ilk:<uuid>` |
| `params.email` | ✅ (opción B) | el email del humano; `params.tenant_id` **opcional** (ver abajo) |

```bash
post '{"op":"get_ilk","request_id":"66666666-…","params":{"ilk_id":"ilk:a6c7d60d-…"}}'
# — por email dentro de un tenant (probe O(1)) —
post '{"op":"get_ilk","request_id":"66666667-…","params":{"email":"juan@acme.com","tenant_id":"tnt:560fcb25-…"}}'
# — por email SOLO (primer login del website: el email es lo único que hay) —
post '{"op":"get_ilk","request_id":"66666668-…","params":{"email":"juan@acme.com"}}'
```

**Email SIN `tenant_id` — búsqueda cross-tenant.** La respuesta agrega `matches`: **un** ilk por
cada tenant donde ese email existe (cada subset trae su `tenant_id` — de acá el website saca el
tenant; si un estado transitorio de identity dejara dos ilks con el mismo email en un tenant, se
devuelve el que respondería el probe con tenant, nunca dos entradas del mismo tenant). `ilk`
viene poblado **solo si el match es único** (el caso común). Ojo: `tenant_id` **presente pero
inválido** (número, string vacío, formato no `tnt:<uuid>`) es **error fuerte**, no un scan:

```json
// 1 match (caso común):
{"result":{"exists":true, "ilk":{…subset…}, "matches":[{…subset…}]}}
// N matches (misma persona en 2 empresas → selector de empresa en el website):
{"result":{"exists":true, "ilk":null, "matches":[{…}, {…}]}}
// 0 matches (primera vez → create_tenant + register_human):
{"result":{"exists":false, "ilk":null, "matches":[]}}
```
```json
{"status":"ok","op":"get_ilk","result":{
  "exists":true,
  "ilk":{"ilk_id":"ilk:a6c7d60d-…","ilk_type":"human","registration_status":"complete",
         "tenant_id":"tnt:560fcb25-…","display_name":"Juan Perez"}}}
```
Si no existe: `result:{"exists":false,"ilk":null}`. Para phone/company/attributes/canales → `get_ilk_details`.

> **Por qué `tenant_id` es obligatorio con `email`:** el mismo email puede existir como ilks
> distintos en dos tenants (la unicidad de identidad es por `(canal, dirección, tenant)`, nunca
> global). El match es case-insensitive **ASCII** (se normaliza igual que al registrar; un email
> con mayúsculas no-ASCII, p.ej. `JOSÉ@…`, solo matchea con el mismo casing) y resuelve por el
> canal `cloud` del ilk — el que `register_human` provisiona con el email como dirección. Es un
> probe O(1) del índice de la SHM: más rápido incluso que buscar por `ilk_id`.

### 4.7 `list_ilks` — los ilks de un tenant (rápido, SHM)

| campo | oblig. | |
|---|---|---|
| `params.tenant_id` | ✅ | `tnt:<uuid>` |

```json
{"status":"ok","op":"list_ilks","result":{"tenant_id":"tnt:560fcb25-…","count":2,
  "ilks":[{"ilk_id":"ilk:…","ilk_type":"human","registration_status":"complete","display_name":"…"}]}}
```

### 4.8 `get_tenant` — ¿existe el tenant? (rápido, SHM)

| campo | oblig. | |
|---|---|---|
| `params.tenant_id` | ✅ | `tnt:<uuid>` |

```json
{"status":"ok","op":"get_tenant","result":{"exists":true,"tenant_id":"tnt:560fcb25-…","ilk_count":2}}
```

### 4.9 `get_ilk_details` — TODA la data del ilk (relay a admin)

**Dos selectores, exactamente uno (los mismos de `get_ilk`):**

| campo | oblig. | |
|---|---|---|
| `params.ilk_id` | ✅ (opción A) | `ilk:<uuid>` |
| `params.email` + `params.tenant_id` | ✅ (opción B) | el email del humano + `tnt:<uuid>` |

Relay a `SY.admin get_ilk` → el registro completo desde la DB: `identification` (name/email/phone/
company/attributes) + `channels` + el `tenant`. Más lento que `get_ilk` (lee la DB). La respuesta va
en `result` con la forma que devuelve admin (`{ilk:{…, identification:{…}, channels:[…]}, tenant:{…}}`).

Con `email`, io.cloud resuelve primero email→`ilk_id` localmente (mismo índice SHM que `get_ilk`)
y relaya por el id canónico — admin/identity no ven el email como selector. Si el email no existe
en ese tenant: `{"status":"error","error_code":"ILK_NOT_FOUND",…}` sin tocar admin.

---

## 5. Errores comunes y qué significan

### 5.1 De la puerta (SY.edge) — HTTP real, forma `{"ok":false,…}`

| HTTP | `error_code` | qué pasó | qué hacer |
|---|---|---|---|
| 401 | `UNAUTHORIZED` | bearer ausente, distinto, o con un prefijo que no es `"Bearer "` ni `"bearer "` | 🟢 revisar la credencial y el prefijo |
| 404 | `NOT_FOUND` | el ICH de la URL no está publicado en ese hive | 🟢 pedirle la URL al operador; ver §6 paso 2 |
| 405 | `METHOD_NOT_ALLOWED` | usaron algo que no es `POST` | 🟢 |
| 413 | `BODY_TOO_LARGE` / `REQ_TOO_LARGE` | el cuerpo supera 64 KiB | partir el pedido |
| 500 | `REGISTRY_UNAVAILABLE` | problema interno de la puerta | reintentar |
| **503** | `SERVICE_UNAVAILABLE` | **la puerta está al tope de pedidos concurrentes** | **backoff exponencial + reintento** |
| 502 | `HANDLER_UNREACHABLE`, `ROUTER_DISCONNECTED`, `ROUTER_ERROR`, `HANDLER_TTL_EXCEEDED`, `INVALID_HANDLER_RESPONSE`, `HANDLER_ERROR` | el nodo interno no está o respondió mal | avisar al operador |
| **504** | `HANDLER_TIMEOUT` | **el nodo no contestó en 30 s** | ver la advertencia de §5.3 |

> ⚠️ Un **504** o un **404** persistentes casi siempre significan que **IO.cloud está mal
> configurado del lado del hive**, no que el pedido esté mal. Ver §6 y §7.

### 5.2 Del hive — HTTP **200**, forma `{"status":"error",…}`

| `error_detail` (texto exacto) | qué pasó |
|---|---|
| `missing 'op'` | falta `op`, o el cuerpo no es un objeto JSON |
| `unknown op '<x>'` | typo. Son **exactamente** `create_tenant`, `put_token`, `provision_node`. Y **no se trimea**: cuidado con espacios |
| `missing or invalid params.name` | `create_tenant` sin `name` |
| `<op> requires tenant_id` | falta `tenant_id` **en la raíz** — no alcanza ponerlo en `params` |
| `<op> requires canonical tenant_id tnt:<uuid>` | el `tenant_id` no tiene la forma `tnt:` + UUID |
| `missing or invalid params.key` | `put_token` sin `key` |
| `missing params.value` | `put_token` sin `value` (o `value: null`) |
| `put_token requires params.resource_type or params.metadata.resource_type` | falta el tipo de credencial |
| `put_token owner_node must be an IO.* node` | el `owner_node` no empieza con `IO.` |
| `provision_node may launch only IO.* nodes` | el `node_name` no empieza con `IO.` |
| `params.config must be an object` | mandaron `config` que no es objeto |
| `admin call failed: …` | **el pedido llegó pero el hive no contestó a tiempo.** Ver §5.3 |
| `IO.cloud may relay only ["create_tenant","vault_put","run_node"] …` | pidieron algo fuera de la lista blanca. **Es el control de seguridad del hive funcionando**, no un bug |

`error_code` cuando viene: `UNAUTHORIZED`, `INVALID_REQUEST`, `INVALID_VALUE`,
`INVALID_SPONSOR_TENANT`, `NODE_ALREADY_EXISTS`, `RUNTIME_NOT_AVAILABLE`,
`RUNTIME_MANIFEST_MISSING`, `MANIFEST_INVALID`, `IDENTITY_REGISTER_FAILED`,
`IDENTITY_UPDATE_FAILED`, `CONFIG_WRITE_FAILED`, `SERVICE_FAILED`, `SPAWN_FAILED`,
`STORAGE_ERROR`, `ENCRYPTION_ERROR`, `MASTER_KEY_NOT_AVAILABLE`, `KEY_NOT_FOUND`,
`IDENTITY_UNAVAILABLE`, `TIMEOUT`, `TRANSPORT_ERROR`.

### 5.3 ⚠️ `admin call failed: …timeout…` **no es un rollback**

🔵 código. Hay una ventana de tiempos desalineados:

```text
ustedes / la puerta esperan   30 s
IO.cloud espera al hive       20 s  (25 s para provision_node)
el hive espera al servicio    30 s  (45 s si reenvía a otro servidor)
```

⇒ Si una operación tarda entre 20/25 s y 30 s, **ustedes reciben un error mientras la operación
puede estar completándose adentro**. Consecuencias prácticas:

- **`create_tenant`**: reintentar es seguro (es idempotente). Manden siempre `domain` o un `name`
  único, y lean `created`.
- **`put_token`**: reintentar es seguro si el `value` es el mismo (el segundo intento devuelve
  `changed:false`).
- **`provision_node`**: ⚠️ **reintentar da `NODE_ALREADY_EXISTS`, que es engañoso** — puede
  significar tanto "ya lo habías creado antes" como "el intento que te dio timeout sí funcionó".
  **No hay clave de idempotencia**: `request_id` se propaga para trazas pero el hive **no lo usa
  para deduplicar**. Ante un timeout en `provision_node`, tratenlo como *estado desconocido* y
  pídanle al operador que verifique.

---

## 6. Checklist para el operador del hive

Sin estos pasos el endpoint no existe. Todo se hace **en el servidor principal (`motherbee`)** del
hive, salvo donde se indique.

**1. Configurar IO.cloud.** El paquete instala una plantilla en `/etc/fluxbee/io-cloud.env.example`
(el archivo real **no** se crea solo). Copiarla a `/etc/fluxbee/io-cloud.env` y ponerle permisos
`0600`:

```bash
IO_CLOUD_EDGE_NODE=SY.edge@ingress1        # el nodo expuesto donde publicar
IO_CLOUD_SECRET=<token largo y aleatorio>  # ES el bearer que va a usar Cloud
IO_CLOUD_ADMIN_HIVE=motherbee
IO_CLOUD_IDENTITY_HIVE=motherbee
IO_CLOUD_INBOUND_FAMILY=user
```

- `IO_CLOUD_SECRET` es **obligatorio** si `IO_CLOUD_EDGE_NODE` está seteado; si falta, el proceso
  no arranca (queda reiniciándose en bucle).
- **`IO_CLOUD_EDGE_NODE` tiene que quedar seteado aunque el canal se publique a mano** (paso 2). Si
  no lo está, IO.cloud **descarta todos los pedidos en silencio** y Cloud recibe **504** sin
  explicación.
- Opcional pero recomendado: `IO_CLOUD_CHANNEL_ADDRESS=<valor aleatorio>` para que la URL no sea
  la misma que la de cualquier otra instalación (ver §7).
- No hay recarga en caliente: **todo cambio requiere reiniciar**.

```bash
systemctl restart io-cloud
```

**2. ⚠️ Publicar el canal a mano.** IO.cloud intenta publicarse solo y **hoy el hive se lo
rechaza** (bug conocido, §7). El operador lo hace desde el admin local:

```bash
# el ICH sale del journal del nodo:
journalctl -u io-cloud | grep "own channel ICH enabled"

curl -X POST http://127.0.0.1:8080/channels/externalize -H 'Content-Type: application/json' -d '{
  "ich":"<el ICH del log>",
  "edge_node":"SY.edge@ingress1",
  "inbound_family":"user",
  "auth_mode":"shared-secret",
  "secret":"<el mismo IO_CLOUD_SECRET>",
  "methods":["POST"]}'
```

La respuesta trae `url` (relativa, `/e/<ich>`) y `token`. **Hay que repetir este paso cada vez que
se reinstale o recree el hive.**

**3. Verificar que quedó publicado:**

```bash
curl 'http://127.0.0.1:8080/channels/externalized?edge_node=SY.edge@ingress1'
```

Debe aparecer una fila con `owner_l2_name: "IO.cloud@motherbee"`.

**4. Entregarle a Cloud** dos cosas: la URL completa `https://<host-público>/e/<ich>` y el valor de
`IO_CLOUD_SECRET`.

**5. Prueba de humo** (los tres primeros pasos no necesitan credencial y son 🟢 verificados):

```bash
URL="https://<host>/e/<ich>"; SEC="<bearer>"
curl -s -o /dev/null -w 'sin bearer  -> %{http_code}  (debe ser 401)\n' -X POST "$URL" -d '{}'
curl -s -o /dev/null -w 'GET         -> %{http_code}  (debe ser 405)\n' "$URL"
curl -s -o /dev/null -w 'ICH falso   -> %{http_code}  (debe ser 404)\n' -X POST "https://<host>/e/ich:00000000-0000-0000-0000-000000000000" -d '{}'

post() { curl -sS -m 35 -X POST "$URL" -H "Authorization: Bearer $SEC" \
         -H 'Content-Type: application/json' -d "$1"; echo; }
post '{"op":"create_tenant","params":{"name":"smoke-test-'"$RANDOM"'"}}'   # -> created:true
```

**Rotar el bearer:** cambiar `IO_CLOUD_SECRET`, `systemctl restart io-cloud`, y **repetir el paso 2**
con el secreto nuevo. El anterior sigue funcionando 10 minutos.
⚠️ 🔵 código: sobreescribir el secreto directamente en el vault **no rota el bearer vivo** — la
puerta solo lo relee cuando se republica el canal.

**Diagnóstico rápido:**

| síntoma en Cloud | causa más probable del lado del hive |
|---|---|
| **404** | el canal nunca se publicó (paso 2), o se perdió tras una reinstalación |
| **401** siempre, con el bearer correcto | la puerta no pudo leer el secreto del vault; buscar `could not fetch channel secret from vault` en el log del nodo expuesto |
| **504** | `IO_CLOUD_EDGE_NODE` sin setear, o IO.cloud caído, o la familia (`IO_CLOUD_INBOUND_FAMILY`) no coincide con la usada al publicar |

---

## 7. Lo que NO está implementado o tiene limitaciones

Honestidad total. Esto es lo que les ahorra días.

**7.1 Tres operaciones de escritura y nada más. Cloud escribe, no lee.**
No hay listar, consultar, actualizar ni borrar. No se puede preguntar qué tenants existen, releer
un token (ni siquiera el hive puede: están cifrados y el escritor no tiene permiso de lectura), ni
dar de baja un nodo. **Diseñen el cliente asumiendo que el hive es un destino de escritura ciego** y
que ustedes tienen que llevar su propio registro de lo que crearon.

**7.2 No hay API de descubrimiento.**
Existe internamente una consulta `list_cloud_actions` que devuelve el catálogo de lo permitido, pero
🔵 código: **no es alcanzable desde Cloud** — no es una `op` válida y, aunque lo fuera, el control de
seguridad la rechazaría. Este documento **es** el catálogo. No hay forma programática de
descubrirlo.

**7.3 No hay forma de consultar el estado de IO.cloud, ni por API ni desde el hive.**
🔵 código: IO.cloud no implementa el plano de configuración del resto del hive. No tiene un estado
consultable; la acción de estado estándar del hive devuelve `UNKNOWN` contra él (busca un nombre de
servicio que no le corresponde); y el chequeo de salud **devuelve siempre `HEALTHY`, incluso si el
canal nunca se publicó y el nodo es inservible**. **La única fuente de verdad es
`journalctl -u io-cloud`** en el servidor. Si algo no cierra, el operador tiene que mirar el log.

**7.4 ⚠️ La auto-publicación de IO.cloud está rota (bug conocido, reproducido en producción).**
Al arrancar, el nodo pide publicar su propio canal y el hive lo rechaza:

```text
WARN  IO.cloud -> SY.admin externalize REJECTED  error_code=UNAUTHORIZED
      IO.cloud may relay only ["create_tenant","vault_put","run_node"]
      over the mesh, not 'externalize' (Fluxbee Cloud provisioning gate)
```

Es un choque entre dos controles de seguridad que por separado están bien. **No los afecta en
tiempo de ejecución**, pero implica que el operador debe publicar el canal a mano (§6 paso 2) **y
repetirlo en cada reinstalación**. Además, el nodo **no reintenta**: un rechazo de este tipo lo
abandona para siempre, y el nodo queda vivo, aparentemente sano, descartando pedidos.
Está reportado y pendiente de arreglo.

**7.5 ⚠️ IO.cloud atiende de a UN pedido por vez.**
🔵 código: no hay paralelismo — cada pedido se procesa hasta el final antes de tomar el siguiente.
Dos llamadas concurrentes se encolan, y la segunda puede consumir los 30 s de la puerta y devolver
**504**. La puerta admite hasta 1024 conexiones simultáneas, pero **el cuello de botella real es 1**.
⇒ **Serialicen las llamadas del lado de Cloud.** No paralelicen el aprovisionamiento.

**7.6 No hay límite de tasa, ni cuotas, ni bloqueo por intentos fallidos.**
La única protección es el tope global de concurrencia de la puerta (503). Nada limita por IP ni por
tenant.

**7.7 Un solo bearer, y `tenant_id` es un dato en el que se confía.**
No hay credenciales por tenant ni identidad de usuario final. **Quien tenga el bearer puede operar
sobre cualquier tenant.** Y `create_tenant` **no está acotado por tenant**: permite crear tenants
arbitrarios eligiendo su `status` (`active` incluido) y su `sponsor_tenant_id`, sin validación
adicional del lado del hive. Es una decisión consciente de esta etapa, pero significa que **el
bearer es una credencial de nivel plataforma: protéjanlo como tal.**

**7.8 La URL es adivinable con la configuración por defecto.**
Ver §2. Toda la seguridad descansa en el bearer. Recomiéndenle al operador que cambie
`IO_CLOUD_CHANNEL_ADDRESS` a un valor aleatorio.

**7.9 No hay transacción compuesta ni rollback entre operaciones.**
`create_tenant` → `put_token` → `provision_node` son tres llamadas independientes. Si la tercera
falla, las dos primeras quedan hechas. **Y hay un orden obligatorio**: `provision_node` antes de un
`put_token` con `owner_node` (§4.2).

**7.10 No hay clave de idempotencia.**
`request_id` sirve para trazas, **no para deduplicar**. Ver §5.3.

**7.11 La forma de `value` en `put_token` no se valida nunca.**
Un token con la estructura equivocada para su `resource_type` se guarda con `ok` y falla recién
cuando el nodo consumidor intenta usarlo, posiblemente días después. No hay validación temprana.

**7.12 Lo que sigue sin verificar en vivo (🟡):**

- El comportamiento bajo carga concurrente real (§7.5 es lectura de código, no medición).
- El rollback de `provision_node` ante un fallo de arranque.
- Los cuerpos exactos de respuesta de las tres operaciones en **esta** revisión: provienen de una
  corrida contra este hive el 2026-07-30 más la lectura del código, pero **no se re-ejecutaron hoy**
  (no se dispone del bearer). Lo re-verificado en vivo hoy es: TLS, ICH publicado, 401 sin bearer,
  401 con bearer inválido, 401 con prefijo `BEARER`, 405 en `GET`/`PUT`, 404 con ICH inexistente.
