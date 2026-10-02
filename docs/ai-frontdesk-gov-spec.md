# SY.frontdesk.gov - Especificacion tecnica (v2)

Estado: vigente (actualizada 2026-10-02: tenant del caso, sin creacion de tenants, merge por email,
nadie se registra en el tenant raiz)

> Nota de deprecacion:
> `frontdesk_result` queda deprecado como contrato de salida de `SY.frontdesk.gov`.
> Estado actual:
> - sin envelope, frontdesk responde `payload.type = "text"`
> - con `meta.context.response_envelope`, responde `payload.type = "text"` con JSON estructurado
> Las menciones a `frontdesk_result` en documentos viejos o notas historicas deben leerse como legacy/superseded.

## 1. Rol

`SY.frontdesk.gov` es el nodo especializado de identity/onboarding del sistema.

Su funcion es:

- recibir casos con identidad `temporary` o incompleta;
- completar registro humano cuando el caso ya trae datos suficientes;
- continuar el flujo conversacional cuando todavia faltan datos;
- ejecutar el upgrade via `ILK_REGISTER`;
- responder con contrato de aplicacion compatible con el consumidor actual.

No reemplaza a un `AI.*` generalista.

## 2. Identidad y naming

- Nombre L2 fijo: `SY.frontdesk.gov`
- Nombre calificado: `SY.frontdesk.gov@<hive_id>`

Regla operativa:

- existe una sola instancia canonica por hive;
- debe considerarse parte del `system default set`;
- puede recibir trafico derivado por nodos IO/AI u otras capas de negocio.

## 3. Responsabilidades

`SY.frontdesk.gov`:

- acepta dos contratos oficiales de input:
  - `payload.type = "text"`
  - `payload.type = "frontdesk_handoff"`
- reutiliza estado por `src_ilk`;
- completa o corrige los datos minimos del humano;
- llama a `ilk_register` cuando el caso ya esta listo, en el tenant del caso (seccion 4.3);
- responde por defecto con `payload.type = "text"`;
- cuando recibe `meta.context.response_envelope`, puede responder con `payload.type = "text"` estructurado compatible con ese envelope.

No debe:

- escribir directo en la identity DB;
- crear tenants. No manda `TNT_CREATE`, y `SY.identity` tampoco se lo permite: `TNT_CREATE`,
  `TNT_UPDATE` y `TNT_SET_SPONSOR` no estan en su allowlist. Un tenant lo crea el operador por
  `SY.admin`, o Fluxbee Cloud con su propio proceso (`create_tenant` de io.cloud, que pasa por
  `SY.admin`);
- tomar el tenant de la persona, del LLM, de su config o de su entorno (seccion 4.3);
- registrar a nadie en el tenant raiz (seccion 4.4);
- inventar ILKs;
- asumir que todos los consumidores requieren el mismo contrato de salida.

## 4. Inputs oficiales

### 4.1 Conversacional

Carrier requerido:

- `meta.src_ilk`
- `meta.thread_id`
- payload `text`

Uso:

- cuando el caso viene de un canal humano normal;
- cuando faltan datos y frontdesk debe pedirlos.

### 4.2 Estructurado

Carrier requerido:

- `meta.src_ilk`
- `meta.thread_id`
- payload `frontdesk_handoff`

Shape canonico:

```json
{
  "type": "frontdesk_handoff",
  "schema_version": 1,
  "operation": "complete_registration",
  "subject": {
    "display_name": "Juan Perez",
    "email": "juan@example.com",
    "phone": "+5491100000001",
    "company_name": "Acme Support",
    "attributes": {
      "crm_customer_id": "crm-123"
    }
  },
  "tenant_id": "tnt:...",
  "context": {
    "source_node": "IO.api.support@motherbee",
    "external_user_id": "crm:123"
  }
}
```

Reglas:

- `operation = "complete_registration"` es la unica operacion cerrada en esta version;
- `subject.display_name` y `subject.email` son requeridos para esa operacion;
- `subject.company_name` es un dato de la persona: nunca elige ni crea un tenant;
- `tenant_id` es opcional. Si viene, tiene que ser el tenant del caso (seccion 4.3); si no lo es,
  no se registra nada (`INVALID_REQUEST`, `error_code = tenant_mismatch`).

### 4.3 Tenant del caso

El tenant de un registro es el que `SY.identity` tiene para el ILK temporal del caso
(`meta.src_ilk`). Lo fijo el nodo IO que creo ese ILK con `ILK_PROVISION`:

- un nodo IO provisiona con su propio tenant (`FLUXBEE_NODE_TENANT_ID`, el que le dio el
  orchestrator);
- io.api e io.cloud provisionan con el tenant para el que los llamaron: io.api, el de su
  integracion; io.cloud, el `tenant_id` del sobre de Cloud, el tenant que creo Cloud;
- un nodo IO sin tenant provisiona en el tenant por defecto (`fluxbee`), que es el tenant raiz:
  esa persona no se puede registrar (seccion 4.4).

Reglas:

- el frontdesk lee ese tenant del SHM de identity de su hive (`jsr-identity-<hive>`), que
  `SY.identity` escribe antes de responder el `ILK_PROVISION`;
- `frontdesk_handoff.tenant_id`, si viene, solo se compara con ese tenant;
- en el camino conversacional la unica fuente es el tenant del ILK: el estado del hilo lo escribe
  el LLM, asi que no es fuente de tenant;
- si el tenant del ILK no se puede leer (el ILK no esta en el SHM, o el SHM no se lee), no se manda
  `ILK_REGISTER` y no se crea nada. La respuesta es explicita: `status = "error"`,
  `result_code = TENANT_UNRESOLVED`, `error_code = missing_tenant_id`;
- no hay otras fuentes: la tool no tiene `tenant_id` ni `identity_candidate.tenant_hint`, y no
  existen `effective_config.tenant_id` ni `GOV_IDENTITY_TENANT_ID`;
- un tenant `pending`, `suspended` o borrado no recibe registros: `SY.identity` responde
  `TENANT_PENDING`, `TENANT_SUSPENDED` o `TENANT_DELETED`, que son resultados finales
  (`REGISTER_FAILED`, no reintentables).

### 4.4 Tenant raiz

Nadie se registra en el tenant raiz (`tnt:00000000-0000-0000-0000-000000000001`, el `fluxbee`
del sistema y sus nodos; decision del operador 2026-10-02). Si el tenant del caso es el raiz:

- el frontdesk no llama a `SY.identity` ni guarda estado, y responde un resultado final, no
  reintentable: `status = "error"`, `result_code = TENANT_NOT_REGISTRABLE`,
  `error_code = TENANT_ROOT_NOT_REGISTRABLE`, `registration_status = "temporary"` y el mensaje
  "No puedo completar el registro por este canal: no pertenece a ninguna organización.";
- en el handoff, antes de pedir datos que falten; en el conversacional, antes de abrir la
  conversacion (no hace falta el LLM: responde igual aunque el nodo este `UNCONFIGURED`); la tool
  `ilk_register` tambien lo rechaza;
- `SY.identity` aplica la misma regla para cualquier llamador (`TENANT_ROOT_NOT_REGISTRABLE`), y
  el frontdesk la traduce al mismo resultado.

La persona queda `temporary`: el `ILK_PROVISION` en el tenant raiz sigue funcionando. Es el caso
de quien escribe por un nodo IO que corre en el tenant raiz, como las instancias base que levanta
`fluxbee-firstboot` (`IO.api@motherbee`, `IO.wapp.default`): si su nodo IO enruta por `Resolve`,
cada mensaje suyo vuelve al frontdesk y recibe la misma respuesta. Que hacen los nodos IO en el
tenant raiz esta pendiente (seccion 13).

## 5. Estado por hilo

Mantiene estado por `src_ilk` usando `thread_state_*`.

Store minimo:

- `thread_state_get`
- `thread_state_put`
- `thread_state_delete`

La clave efectiva de estado queda asociada al `src_ilk`.

Estado minimo actual:

```json
{
  "status": "collecting|awaiting_confirmation|completed|completed_error",
  "collected": {
    "name": null,
    "email": null,
    "phone": null,
    "company_name": null
  },
  "tenant_id": null,
  "registration_status": null,
  "register_attempted": false,
  "register_error": null
}
```

`tenant_id` es el tenant del caso. El handoff lo guarda tambien cuando el registro falla
(`completed_error`), asi un reintento sigue informandolo. Es informativo: el tenant de cada intento
se vuelve a leer del ILK (seccion 4.3).

## 6. Modos internos de trabajo

### 6.1 `register_automatic`

Se activa cuando entra `frontdesk_handoff`.

Regla:

- no abre conversacion innecesaria;
- resuelve primero el tenant del caso (seccion 4.3); sin tenant, con uno que no coincide o con el
  tenant raiz (seccion 4.4), responde el error y no guarda nada;
- mergea con estado previo si corresponde;
- si ya tiene el minimo completo, intenta registrar;
- si sigue incompleto, responde `text` con el mensaje humano correspondiente.

### 6.2 Conversacional

Se activa con `payload.type = "text"`.

Regla:

- un caso del tenant raiz no abre conversacion: recibe el resultado final de la seccion 4.4, sin
  LLM;
- puede recolectar datos faltantes;
- puede pedir confirmacion;
- ante confirmacion positiva llama `ilk_register`;
- responde con `text`.

## 7. Tool de completacion

El nodo usa la tool `ilk_register`.

Payload minimo:

- `src_ilk`
- `identity_candidate.name`
- `identity_candidate.email`

Opcionales:

- `identity_candidate.phone`
- `identity_candidate.company_name`
- `identity_candidate.attributes` (datos libres; van tal cual a `identification.attributes`)

No hay mas campos: el tenant no es un parametro (seccion 4.3). El estado del caso se indexa por
`src_ilk` (seccion 5); la tool no recibe `thread_id`.

Respuesta exitosa:

```json
{
  "status": "ok",
  "registered": true,
  "merged": false,
  "ilk_id": "ilk:...",
  "tenant_id": "tnt:...",
  "identity_payload": { "status": "ok", "ilk_id": "ilk:...", "merged": false }
}
```

### 7.1 Email ya registrado: merge

El email identifica a la persona dentro de su tenant. Si el registro trae un email que ya tiene otro
ILK humano del mismo tenant, la persona ya esta registrada y el registro no falla: `SY.identity` hace
el merge dentro del mismo `ILK_REGISTER` (detalle en `10-identity-v2.md` 6.5):

- el ILK existente conserva su id, su tenant y su estado `complete`;
- los canales del ILK temporal pasan a ese ILK, y el temporal queda como alias suyo hasta que vence
  `merge_alias_ttl_secs` (despues lo marca borrado el GC de alias);
- los campos de `identification` que el ILK existente tiene vacios se completan con los del
  registro; los que ya tienen valor no se tocan;
- `SY.identity` responde `merged: true`, `ilk_id` del ILK existente y `merged_from_ilk_id` del
  temporal; frontdesk responde `MERGED` (seccion 9) y le dice a la persona que ya estaba registrada y
  que este canal quedo asociado a su registro.

Una actualizacion explicita si pisa: un `ILK_REGISTER` del propio ILK ya registrado (por ejemplo un
`register_human` repetido de Cloud para el mismo email) reemplaza su `identification` entera; gana el
ultimo.

Sin prueba de que la persona es duena del email, quien escriba el email de otro hace que su canal
quede asociado al ILK de esa persona. La verificacion (por ejemplo un codigo de un solo uso) esta
pendiente (seccion 13).

## 8. Output por defecto

Salida por defecto cuando no hay envelope:

- `meta.type = "user"`
- `payload.type = "text"`

Shape canónico mínimo:

```json
{
  "type": "text",
  "content": "Necesito tu email para continuar."
}
```

Regla:

- sin envelope, frontdesk no debe exponer un payload estructurado custom como contrato obligatorio del consumidor;
- el mensaje humano debe salir como texto normal.

## 8.1 Output estructurado opt-in por envelope

Cuando el mensaje entrante trae `meta.context.response_envelope`, `SY.frontdesk.gov` puede responder con:

- `meta.type = "user"`
- `payload.type = "text"`
- `payload.content = "<json estructurado>"`

En este primer corte, el shape soportado y validado es:

```json
{
  "success": true,
  "human_message": "Registro completado correctamente."
}
```

o, si hubo bloqueo/error funcional:

```json
{
  "success": false,
  "human_message": "No pude completar el registro en este momento.",
  "error_code": "register_failed"
}
```

Reglas:

- el envelope es opt-in y hop-by-hop;
- si no existe envelope, frontdesk responde `text` normal;
- `success` es `true` solo con `status = "ok"` (`REGISTERED`, `MERGED`, `ALREADY_COMPLETE`);
- `error_code` es el `result_code` en minusculas (seccion 9) y solo va cuando `success = false`;
- `error_code` no debe emitirse como `null` en v1;
- en este corte, frontdesk solo declara soporte explícito para:
  - `success:boolean`
  - `human_message:string`
  - `error_code:string`
- si el envelope es inválido o pide un shape que frontdesk no puede cumplir, debe fallar con `invalid_response_contract`.

## 9. Semantica de resultado

Estados cerrados:

- `ok`
- `needs_input`
- `error`

`result_code` cerrados:

| `result_code` | estado | cuando |
| --- | --- | --- |
| `REGISTERED` | `ok` | el ILK temporal quedo `complete` |
| `MERGED` | `ok` | el email ya era de otro ILK del tenant: este canal paso a ese ILK (seccion 7.1); `ilk_id` es ese ILK |
| `ALREADY_COMPLETE` | `ok` | conversacional: el estado del hilo ya dice `completed` |
| `MISSING_REQUIRED_FIELDS` | `needs_input` | faltan `display_name` y/o `email` |
| `IN_CONVERSATION` | `needs_input` | turno conversacional en el que no hubo registro (saludo, pedido de un dato) |
| `INVALID_REQUEST` | `error` | pedido invalido: operacion no soportada, datos invalidos, `tenant_mismatch`, `INVALID_*` de `SY.identity` |
| `TENANT_UNRESOLVED` | `error` | no hay tenant para el caso (`missing_tenant_id`, seccion 4.3); no se registro ni se creo nada |
| `TENANT_NOT_REGISTRABLE` | `error` | el tenant del caso es el tenant raiz (`TENANT_ROOT_NOT_REGISTRABLE`, seccion 4.4); no se registro nada y la persona sigue `temporary` |
| `REGISTER_FAILED` | `error` | `SY.identity` rechazo el registro con un veredicto final |
| `IDENTITY_UNAVAILABLE` | `error` | falla transitoria, la unica reintentable |

`human_message` es obligatorio siempre.

Fallas de registro (`ILK_REGISTER`):

- `IDENTITY_UNAVAILABLE` solo para fallas transitorias, las unicas reintentables: `SY.identity` no respondio (`UNREACHABLE`, `TTL_EXCEEDED`, `TIMEOUT`, `IDENTITY_ERROR`) o respondio `NOT_PRIMARY`, `DB_NOT_READY` o `DB_WRITE_FAILED`;
- cualquier otro codigo con el que `SY.identity` responde es un veredicto final, no reintentable: `INVALID_*` -> `INVALID_REQUEST`; `TENANT_ROOT_NOT_REGISTRABLE` -> `TENANT_NOT_REGISTRABLE` (el mismo resultado que si lo ve el frontdesk); el resto (`TENANT_PENDING`, `TENANT_SUSPENDED`, `TENANT_DELETED`, `ILK_DELETED`, `ILK_NOT_FOUND`, `SYSTEM_ILK_PROTECTED`, `DUPLICATE_*`, `UNAUTHORIZED_REGISTRAR`, ...) -> `REGISTER_FAILED`;
- `error_code` conserva el codigo de `SY.identity` tal cual (o el de la tool: `missing_src_ilk`, `invalid_identity_candidate`, `missing_tenant_id`, `tenant_mismatch`, y `TENANT_ROOT_NOT_REGISTRABLE`, el mismo codigo que usa `SY.identity`).

`DUPLICATE_EMAIL` queda solo para un email que no se puede mergear: el ILK del registro no es un
temporal humano del mismo tenant (por ejemplo un ILK ya `complete` que quiere el email de otro).

`missing_fields`:

- si `status = "needs_input"`, contiene la lista exacta de faltantes;
- en cualquier otro caso, debe ser `[]`.

## 10. Integracion con consumidores

### 10.1 Consumidores conversacionales

Deben:

- tratar la salida default de frontdesk como texto normal;
- no asumir una única forma de salida si el hop usa envelope.

### 10.2 Consumidores no conversacionales

Deben:

- si no usan envelope, consumir texto normal;
- si usan envelope, consumir la respuesta estructurada definida por ese hop.

### 10.3 `IO.api`

`IO.api` debe:

- construir `frontdesk_handoff`, con el `tenant_id` de su integracion (el mismo con el que provisiono el ILK);
- usar `SY.frontdesk.gov` como paso intermedio cuando el sujeto no esta registrado completamente;
- agregar `meta.context.response_envelope` para el hop síncrono de regularización;
- consumir la respuesta estructurada resultante y:
  - si `success = true`, permitir que el mensaje original continue al `dst_final`;
  - si `success = false`, mapearla a la respuesta HTTP de `IO.api`.

Una instancia de `IO.api` que corre en el tenant raiz (la `IO.api@motherbee` que levanta
`fluxbee-firstboot`) provisiona a sus sujetos `by_data` en el tenant raiz: el frontdesk responde
`success = false`, `error_code = tenant_not_registrable` (seccion 4.4), y el mensaje no sigue.

### 10.4 io.cloud `register_human`

io.cloud provisiona el ILK temporal en el tenant del sobre de Cloud (canal `cloud`, direccion = el
email) y le manda al frontdesk el `frontdesk_handoff` con ese mismo `tenant_id`. Pide `ilk_id` y
`merged` en el envelope, asi su respuesta a Cloud lleva el ILK en el que quedo la persona: despues
de un `MERGED`, el que ya tenia el email (con `merged_from_ilk_id`, el temporal que provisiono).

Un sobre con el tenant raiz se rechaza antes de provisionar nada (`TENANT_ROOT_NOT_REGISTRABLE`),
asi no queda en el tenant raiz un temporal con el email de la persona.

## 11. Configuracion y operacion

`SY.frontdesk.gov` es un nodo de sistema autonomo:

- corre como servicio del sistema (`sy-frontdesk-gov.service`) sin argumentos ni YAML de nodo;
  toma `node_name` de `hive.yaml` como `SY.frontdesk.gov@<hive_id>`;
- el prompt va embebido en el binario; no se configura;
- el proveedor y el modelo de IA son los del hive: seccion `ai` de `hive.yaml` (con un fallback
  horneado si falta);
- la clave del proveedor se lee de `SY.vault` por `resource_type` del proveedor (`openai` o
  `anthropic`), en el tenant raiz, al arrancar y en cada `VAULT_SECRET_CHANGED`. Sin clave el nodo
  queda `UNCONFIGURED`: el camino estructurado sigue andando y el conversacional responde
  `node_not_configured`;
- `CONFIG_SET` no acepta configuracion: rechaza `ai`, `ai_providers`, `behavior`, `api_key` y
  `api_key_ref` con `config_not_accepted`, ignora cualquier otro campo, vuelve a leer la clave del
  vault y no persiste nada;
- `CONFIG_GET` informa el estado, el proveedor y el modelo, y si la clave se resolvio.

Lifecycle: es singleton por hive y se actualiza con el `.deb`, como el resto de los `SY.*`; no se
borra ni se respawnea como un `IO.*`. No guarda config local: su unico estado es el de los hilos
(seccion 5).

## 12. Observabilidad minima

Debe exponer:

- `state`: `UNCONFIGURED|CONFIGURED|FAILED_CONFIG`
- contadores sugeridos (pendientes, seccion 13):
  - `threads_active`
  - `identity_upgrades_ok`
  - `identity_upgrades_error`
  - `frontdesk_handoff_ok`
  - `frontdesk_handoff_needs_input`

Los logs no llevan datos personales: del registro se loguean `ilk_id`, `tenant_id`, los codigos y
que campos vinieron, nunca sus valores.

## 13. Pendientes

- Prueba de que la persona es duena del email (por ejemplo un codigo de un solo uso) antes del merge
  de la seccion 7.1. Hasta entonces, quien conoce el email de otro asocia su canal al ILK de esa
  persona.
- Los nodos IO que corren en el tenant raiz (las instancias base de `fluxbee-firstboot`): las
  personas que llegan por ellos quedan `temporary` (seccion 4.4). Decision del operador pendiente.
- Los contadores de la seccion 12 (G6).
- Los reintentos conversacionales: la regla "no loop" del prompt vuelve terminal un error
  transitorio (G8).
