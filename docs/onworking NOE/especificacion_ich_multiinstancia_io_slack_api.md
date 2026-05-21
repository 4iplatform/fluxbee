# Especificacion tecnica: ICH propio y multiinstancia para IO Slack e IO API

## 1. Objetivo

Este documento define los cambios necesarios para corregir la semantica de `ICH` en los nodos `IO.slack` e `IO.api`, y preparar ambos nodos para un modelo multiinstancia consistente.

El equipo de desarrollo debe usar este documento como base para generar el plan de trabajo e implementar los cambios en codigo y documentacion.

Queda explicitamente fuera de alcance el nodo LinkedHelper (`IO.linkedhelper`), cuyo estado actual se considera obsoleto para esta decision.

---

## 2. Problema actual

Historicamente se interpreto `ICH` como si representara un canal o identificador del interlocutor externo. Por ejemplo:

```text
slack://U123
whatsapp:+5411...
api:<external-id>
```

Esa interpretacion ya no es valida.

La definicion corregida es:

```text
ICH = endpoint operativo local/direccionable del sistema Fluxbee
```

Por lo tanto, un `ICH` debe quedar asociado al `ILK` interno del nodo IO, no al `ILK` o identidad del usuario/contacto externo.

La relacion objetivo para nodos IO multiinstancia es:

```text
1 nodo IO logico <> 1 ILK interno <> 1 ICH propio
```

Esto implica que el nodo IO no queda operativo hasta tener un `ICH` propio valido y registrado/adquirido en Identity.

---

## 3. Principio general para granularidad de ICH

La granularidad del `ICH` debe ser la menor unidad operativa que Fluxbee necesita direccionar de forma autonoma para inbound y outbound.

No necesariamente coincide con:

- la infraestructura runtime, como `ip:puerto`;
- el interlocutor externo;
- la credencial padre;
- el workspace/cuenta si ese nivel no alcanza para direccionar correctamente.

Regla:

```text
ICH debe identificar un destino/inbox/outbox Fluxbee estable, no un detalle tecnico de deployment ni un contacto externo.
```

---

## 4. CTX y thread_id

El modelo `CTX` ya no debe usarse como base de las nuevas definiciones.

`CTX` queda como concepto historico/legacy. El carrier conversacional canonico actual es:

```text
meta.thread_id
meta.thread_seq
```

Por lo tanto, los cambios sobre `ICH` no deben reintroducir `CTX` ni depender de `ctx`, `ctx_seq` o `ctx_window`.

El `thread_id` puede utilizar material del medio, por ejemplo `workspace_id`, `conversation_id`, `thread_ts`, etc., pero no reemplaza semanticamente al `ICH`.

---

## 5. Cambios comunes a nodos IO

### 5.1. El nodo debe asegurar su propio ICH

Cada nodo IO debe tener un flujo explicito de inicializacion:

1. Cargar configuracion efectiva.
2. Validar que la configuracion minima para su identidad operativa exista.
3. Resolver su `self_ilk_id`.
4. Construir el identificador estable de su `ICH` propio.
5. Registrar/adquirir ese ICH en Identity asociado al `self_ilk_id`.
6. Solo despues quedar operativo.

Si falta configuracion minima, el nodo no debe intentar registrar/adquirir ICH. Debe quedar en estado no operativo, por ejemplo:

```text
UNCONFIGURED
FAILED_CONFIG
NOT_READY
```

El nombre exacto del estado puede ajustarse al modelo actual del repo, pero el comportamiento debe ser ese.

### 5.2. No usar ICH para interlocutores externos

Debe eliminarse o aislarse cualquier flujo que derive `ICH` desde el usuario/contacto externo.

Ejemplo incorrecto:

```text
meta.ich = slack://U123
stable_ich_id(channel="slack", external_id="U123", tenant_id=...)
```

Ese dato puede seguir existiendo como identidad externa, alias, contact handle o external identity, pero no como `ICH`.

### 5.3. Revisar nombres peligrosos

En codigo comun IO existen nombres como:

```text
channel
external_id
ich_id
```

Si esos campos se usan para identificar interlocutores externos, no deben llamarse ni tratarse como `ICH`.

Se recomienda separar conceptualmente:

```text
own_ich / local_channel / local_endpoint
```

versus:

```text
external_identity / external_handle / contact_address
```

### 5.4. meta.ich en mensajes

En mensajes producidos por un nodo IO, `meta.ich` debe apuntar al `ICH` propio del nodo IO que recibio/emite por ese canal local.

No debe apuntar al sender externo.

Incorrecto:

```text
meta.ich = slack://U123
```

Correcto, ejemplo Slack:

```text
meta.ich = ICH.slack.<workspace_id>.<conversation_id>
```

Correcto, ejemplo API:

```text
meta.ich = ICH.api.<integration_id>
```

---

## 6. IO API

### 6.1. Problema actual

Actualmente se contemplo o se implemento la posibilidad de usar `listen_addr` como material para el `ICH`, por ejemplo:

```text
channel_type = io_api_instance
address = 0.0.0.0:8080
```

Eso no debe considerarse canonico.

`listen_addr` es infraestructura runtime. Puede cambiar por deploy, container, reverse proxy, puerto, host o topologia. No representa una integracion estable.

### 6.2. Definicion objetivo

El `ICH` de `IO.api` debe estar ligado a una entidad estable de integracion o canal API.

Formas aceptables:

```text
IO.api.<integration_id>@motherbee
ICH.api.<integration_id>
```

O:

```text
IO.api.<api_channel_id>@motherbee
ICH.api.<api_channel_id>
```

El nombre exacto (`integration_id` o `api_channel_id`) debe definirse en la documentacion/codigo, pero debe representar una integracion estable y no un bind tecnico.

### 6.3. Configuracion minima para quedar operativo

`IO.api` no debe intentar registrar/adquirir ICH si no tiene configuracion minima valida.

Minimo esperado:

```text
self_ilk_id
tenant_id / sponsor / ownership aplicable
integration_id o api_channel_id estable
modo API efectivo
configuracion inbound valida
configuracion de auth si aplica
configuracion de outbound callback si aplica
```

`listen_addr` puede ser requerido para levantar HTTP, pero no debe ser el identificador canonico del `ICH`.

### 6.4. Webhook externo de respuesta

Si `IO.api` puede responder a un webhook externo, ese webhook debe pertenecer a la configuracion de la integracion/canal API.

Ejemplo:

```text
ICH.api.acme_orders
  integration_id = acme_orders
  inbound_path = /api/acme/orders
  callback_url = https://acme.example/callback
```

El webhook externo no es el `ICH` del interlocutor externo. Es parte del endpoint operativo configurado en Fluxbee.

### 6.5. Multiinstancia IO API

El modelo objetivo permite multiples instancias:

```text
IO.api.acme_orders@motherbee
IO.api.globex_support@motherbee
IO.api.internal_alerts@motherbee
```

Cada una debe tener su propio:

```text
self_ilk_id
ICH propio
integration_id/api_channel_id
config efectiva
```

Riesgo transitorio aceptado:

```text
Dos nodos API pueden fallar si intentan bindear el mismo puerto.
```

Ese riesgo queda aceptado temporalmente, pero debe documentarse como deuda tecnica.

Recomendacion futura:

- validar unicidad de bind efectivo;
- evitar que dos integraciones tomen el mismo `listen_addr`;
- mover el ingreso HTTP hacia un router/reverse proxy externo cuando corresponda;
- hacer que `listen_addr` sea runtime/config, no identidad.

---

## 7. IO Slack

### 7.1. Problema actual

La granularidad por workspace solamente no alcanza para outbound proactivo.

Ejemplo:

```text
IO.slack.<workspace_id>@motherbee
```

Ese nodo conoce la instalacion/workspace, pero no necesariamente sabe a que canal o DM debe publicar una alerta, mensaje automatico o resultado de workflow.

Pasar `channel_id` dentro del payload de cada workflow acoplaria los workflows a detalles especificos de Slack.

### 7.2. Definicion objetivo

La unidad operativa direccionable de Slack debe ser:

```text
workspace_id + conversation_id
```

Donde `conversation_id` puede representar:

- canal publico;
- canal privado;
- DM;
- MPIM / group DM;
- otra conversacion Slack soportada por la API.

Forma canonica recomendada:

```text
IO.slack.<workspace_id>.<conversation_id>@motherbee
ICH.slack.<workspace_id>.<conversation_id>
```

Ejemplo:

```text
IO.slack.T123.C999@motherbee
ICH.slack.T123.C999
```

### 7.3. Workspace como recurso padre

La instalacion Slack en workspace no desaparece. Debe modelarse como recurso padre de autenticacion.

Ejemplo conceptual:

```text
SlackInstallation
  workspace_id = T123
  token = vault://...
  scopes = ...
  enterprise_id = ... opcional
```

El canal/conversacion es el endpoint operativo direccionable:

```text
SlackConversationBinding
  workspace_id = T123
  conversation_id = C999
  node_ilk = IO.slack.T123.C999@motherbee
  ich = ICH.slack.T123.C999
  vault_resource = slack installation T123
  enabled = true
```

El token pertenece a la instalacion/workspace. El `ICH` pertenece al binding operativo `workspace + conversation_id`.

### 7.4. Uso de vault

El hecho de que varios nodos Slack logicos compartan credenciales no debe bloquear el modelo.

Varios nodos pueden resolver la misma credencial padre desde vault si tienen ownership/autorizacion correcta.

Ejemplo:

```text
IO.slack.T123.C999@motherbee
IO.slack.T123.C888@motherbee
```

ambos pueden usar:

```text
vault resource: slack installation T123
```

Debe quedar claro que el nodo por canal/conversacion no es duenio independiente del token. Es duenio de un binding operativo.

### 7.5. Outbound Slack

Con la nueva granularidad, el workflow no necesita enviar `channel_id` como destino primario.

Flujo esperado:

```text
workflow
  -> IO.slack.T123.C999@motherbee
  -> nodo resuelve workspace_id = T123
  -> nodo resuelve conversation_id = C999
  -> nodo obtiene token desde vault
  -> nodo publica en Slack conversation_id = C999
```

El payload puede incluir metadata opcional, por ejemplo:

```text
thread_ts
reply_broadcast
formatting
blocks
attachments
```

Pero `conversation_id/channel_id` no debe ser obligatorio como destino primario porque ya forma parte del nodo/ICH.

### 7.6. Inbound Slack: estado transitorio con Socket Mode

Hoy, mientras `IO.slack` use Socket Mode, existe un problema estructural:

```text
Socket Mode entrega eventos a nivel app/workspace.
El nodo objetivo deseado es workspace_id + conversation_id.
```

Sin un router externo previo, no hay una forma limpia de que cada nodo granular reciba solo sus eventos.

Por lo tanto, debe documentarse el estado transitorio:

```text
Socket Mode queda permitido temporalmente, pero no es el modelo objetivo para granularidad por conversation_id.
```

Problema concreto:

```text
El evento Slack llega a nivel workspace/app y debe filtrarse o redistribuirse por workspace_id + conversation_id.
```

### 7.7. Inbound Slack: modelo objetivo con Request URL + router externo

El modelo objetivo es migrar Slack inbound a:

```text
Slack Request URL
  -> router externo previo a Fluxbee core
  -> interpretacion del payload Slack
  -> resolucion de workspace_id + conversation_id
  -> redirect al nodo IO.slack.<workspace_id>.<conversation_id>@motherbee
```

El router externo mencionado en este documento no es el router interno core de Fluxbee.

Responsabilidad del router externo:

1. Recibir requests del proveedor externo.
2. Validar/verificar la request segun proveedor.
3. Interpretar el payload.
4. Identificar la unidad operativa Fluxbee destino.
5. Redirigir al nodo IO correcto.

Para Slack:

```text
team_id / workspace_id
conversation_id / channel_id
api_app_id / enterprise_id si aplica
```

Con eso debe resolver:

```text
IO.slack.<workspace_id>.<conversation_id>@motherbee
```

### 7.8. Requisito de parametrizacion Slack

`IO.slack` debe nacer/configurarse explicitamente con:

```text
workspace_id
conversation_id
self_ilk_id
ich_id o material estable para generarlo
referencia a vault resource de Slack installation
```

Si falta `workspace_id` o `conversation_id`, el nodo no debe adquirir ICH ni quedar operativo como nodo granular.

### 7.9. Thread_id Slack

El `thread_id` debe seguir derivandose del hilo real de Slack, no del viejo CTX.

Material posible:

```text
workspace_id
conversation_id
thread_ts o message_ts
```

Ejemplo conceptual:

```text
thread_id = hash(slack + workspace_id + conversation_id + thread_ts)
```

Esto no reemplaza al `ICH`.

Diferencia:

```text
ICH = endpoint operativo local direccionable
thread_id = hilo conversacional dentro de ese endpoint
```

---

## 8. Cambios esperados en codigo

### 8.1. Codigo comun IO

Revisar funciones/estructuras que hoy mezclan `channel/external_id` con `ICH`.

Acciones esperadas:

- separar provisioning de ICH propio del nodo;
- separar resolucion/provisioning de identidad externa;
- evitar que `stable_ich_id(channel, external_id, tenant_id)` se use para contactos externos;
- evitar que `meta.ich` se construya como `channel://sender_id`;
- introducir o consolidar helper comun tipo `ensure_own_ich` para nodos IO.

### 8.2. Identity

El alta/asociacion de ICH propio debe usar un flujo equivalente a:

```text
ILK_ADD_CHANNEL(self_ilk_id, own_ich_material)
```

No debe usar un flujo que cree/provisione ILK externo en base al supuesto ICH del usuario.

### 8.3. IO API

Cambios esperados:

- agregar `integration_id` o `api_channel_id` como configuracion estable;
- usar ese identificador para construir/adquirir ICH propio;
- dejar `listen_addr` solo como runtime config;
- impedir adquisicion de ICH si falta config minima;
- revisar outbound webhook como parte de la integracion, no como identidad externa.

### 8.4. IO Slack

Cambios esperados:

- agregar parametrizacion obligatoria `workspace_id + conversation_id` para nodo granular;
- construir/adquirir ICH como `ICH.slack.<workspace_id>.<conversation_id>`;
- resolver token via vault usando instalacion/workspace padre;
- outbound publica al `conversation_id` propio del nodo;
- no exigir `channel_id` como destino primario en outbound;
- documentar Socket Mode como transitorio y limitado;
- preparar contrato para Request URL + router externo.

---

## 9. Cambios esperados en documentacion

Actualizar docs core y docs de nodos IO para reflejar:

1. `ICH` no representa interlocutores externos.
2. `ICH` representa endpoint operativo local/direccionable.
3. `ICH` queda asociado al `ILK` interno del nodo IO.
4. `CTX` es legacy y no debe usarse para nuevas decisiones.
5. `thread_id` es el carrier conversacional canonico.
6. `IO.api` usa `integration_id/api_channel_id`, no `ip:puerto`, como identidad de ICH.
7. `IO.slack` usa `workspace_id + conversation_id` como unidad operativa.
8. Slack workspace/installation es recurso padre de credenciales, no necesariamente ICH final.
9. Socket Mode en Slack es transitorio si no existe router externo granular.
10. Vault puede ser usado por multiples nodos logicos para resolver credenciales compartidas.

---

## 10. Criterios de aceptacion

### 10.1. Generales

- Ningun nodo IO nuevo debe registrar/adquirir ICH asociado a un interlocutor externo.
- Todo nodo IO operativo debe tener `self_ilk_id` y `own_ich`.
- Si falta configuracion minima, el nodo queda no operativo y no registra ICH.
- `meta.ich` en eventos IO debe apuntar al ICH propio del nodo IO.
- `ctx*` no se usa para nuevas decisiones.

### 10.2. IO API

- `ICH` de API se deriva de `integration_id` o `api_channel_id` estable.
- `listen_addr` no se usa como identidad canonica de ICH.
- Dos integraciones distintas no comparten el mismo ICH.
- El webhook externo de respuesta forma parte de la configuracion de la integracion.
- El nodo no adquiere ICH si falta configuracion minima.

### 10.3. IO Slack

- `ICH` de Slack se deriva de `workspace_id + conversation_id`.
- El nodo Slack granular conoce su `conversation_id` por configuracion, no por payload outbound.
- Outbound puede publicar sin recibir `channel_id` como destino primario.
- El token se resuelve via vault desde la instalacion/workspace padre.
- Socket Mode queda documentado como transitorio/limitado.
- El modelo objetivo queda documentado como Request URL + router externo previo.

---

## 11. Ejemplos canonicos

### 11.1. IO API

```text
integration_id = acme_orders
node_ilk = IO.api.acme_orders@motherbee
ich = ICH.api.acme_orders
listen_addr = 0.0.0.0:8080
callback_url = https://acme.example/webhook/result
```

Interpretacion:

```text
listen_addr es runtime.
integration_id identifica el endpoint operativo Fluxbee.
callback_url es configuracion de la integracion.
```

### 11.2. IO Slack canal

```text
workspace_id = T123
conversation_id = C999
node_ilk = IO.slack.T123.C999@motherbee
ich = ICH.slack.T123.C999
vault_resource = slack_installation_T123
```

Outbound:

```text
workflow -> IO.slack.T123.C999@motherbee
```

El workflow no necesita pasar `channel_id` como destino primario.

### 11.3. IO Slack DM

```text
workspace_id = T123
conversation_id = D456
node_ilk = IO.slack.T123.D456@motherbee
ich = ICH.slack.T123.D456
vault_resource = slack_installation_T123
```

---

## 12. Deudas tecnicas reconocidas

1. Definir formalmente el router externo previo para proveedores HTTP/webhook.
2. Migrar Slack inbound de Socket Mode a Request URL cuando exista el router externo.
3. Agregar validaciones fuertes para evitar conflictos de puerto en IO API.
4. Revisar y renombrar estructuras comunes IO que todavia mezclan `external_id` con `ICH`.
5. Revisar documentacion legacy que todavia sugiera `ICH = canal del interlocutor externo`.
6. Definir lifecycle de SlackConversationBinding: alta, baja, canal archivado, app removida, permisos insuficientes.
7. Definir si el deployment fisico sera 1 proceso por nodo logico o si un proceso podra alojar multiples bindings. Esta decision no debe alterar el contrato logico `1 nodo IO logico <> 1 ICH`.

---

## 13. Decision final resumida

```text
ICH es el endpoint operativo local/direccionable de Fluxbee.
No es el interlocutor externo.
No es necesariamente la credencial padre.
No es infraestructura runtime.
```

Para este alcance:

```text
IO API:
  ICH = integration_id / api_channel_id estable

IO Slack:
  ICH = workspace_id + conversation_id

Vault:
  resuelve credenciales compartidas cuando corresponda

Socket Mode Slack:
  permitido solo como transicion, con limitaciones conocidas

Modelo objetivo Slack inbound:
  Request URL + router externo previo + redirect al nodo granular correcto
```
