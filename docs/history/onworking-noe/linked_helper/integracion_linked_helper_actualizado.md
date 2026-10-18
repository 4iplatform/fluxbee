# Replanteo de integración LinkedHelper Adapter / Fluxbee Cloud / Fluxbee

## Objetivo

Reducir el rol operativo de **Fluxbee Cloud** en la integración con LinkedHelper Adapter.

La idea principal es que Fluxbee Cloud deje de funcionar como un componente que monitorea activamente al adapter o decide aspectos runtime, y pase a cumplir un rol más acotado de **control administrativo, enrollment, discovery, provisioning y visibilidad**.

El runtime real debería quedar principalmente entre:

```text
LinkedHelper Adapter <> IO.linkedhelper
```

Fluxbee Cloud interviene cuando hay algo administrativo pendiente, pero no debería quedar “pendiente de todo” durante la operación normal.

---

## Principio rector

```text
El adapter consulta a Fluxbee Cloud solo cuando tiene trabajo administrativo pendiente.
```

Ese trabajo administrativo puede incluir:

```text
- enrollment inicial
- aceptación del adapter
- obtención de token/secreto de comunicación
- reporte de instancias LinkedHelper detectadas
- espera de aprobación de instancias
- espera de provisioning de nodos
- recepción de destinos runtime por instancia
- aparición de nuevas instancias
- recuperación administrativa ante errores de configuración
```

Una vez que las instancias ya tienen destino runtime y el adapter confirmó conexión con sus nodos correspondientes, la comunicación permanente con Fluxbee Cloud deja de ser necesaria.

---

## Cambio respecto al modelo anterior

### Modelo anterior asumido

Se venía asumiendo algo cercano a esto:

```text
Adapter reporta alive periódicamente a Fluxbee Cloud.
Fluxbee Cloud usa ese alive para saber si el adapter está vivo.
Fluxbee Cloud evalúa updates.
Fluxbee Cloud conoce el estado operativo del adapter.
Fluxbee Cloud coordina parte importante del runtime.
```

### Nuevo modelo propuesto

El nuevo modelo debería ser:

```text
Adapter usa Fluxbee Cloud como control plane administrativo.
Fluxbee Cloud registra, aprueba y provisiona.
Fluxbee Cloud entrega destinos runtime.
Adapter opera directamente contra IO.linkedhelper.
Fluxbee Cloud no requiere alive permanente.
```

---

## Nuevo concepto: “qué tenés para mí”

En lugar de pensar en un heartbeat permanente tipo `alive`, conviene pensar en una consulta administrativa genérica del adapter hacia Cloud:

```text
¿Qué tenés para mí?
```

Ese concepto permite cubrir distintos casos sin multiplicar flujos:

```text
- todavía no fuiste aceptado
- te acepté, tomá token/secreto
- recibí tus instancias, están pendientes
- esta instancia fue aprobada
- esta instancia fue rechazada/ignorada
- estoy creando el nodo
- el nodo ya está creado
- para esta instancia conectá contra este destino
- esta instancia está disabled/enabled administrativamente
```

No necesariamente tiene que llamarse `alive`. De hecho, conviene evitar ese nombre si induce a pensar en monitoreo permanente.

Nombres posibles a evaluar más adelante:

```text
admin_poll
control_poll
provisioning_poll
sync
```

Pero conceptualmente el punto importante es:

```text
El adapter consulta a Cloud mientras espera definiciones administrativas.
```

---

## Flujo de alta propuesto

### 1. Adapter se levanta

```text
1. Adapter inicia localmente.
2. Adapter contacta a Fluxbee Cloud.
3. Informa que existe.
4. Fluxbee Cloud lo acepta o rechaza.
5. Si lo acepta, Fluxbee Cloud entrega un token/secreto/canal de comunicación administrativa.
```

Resultado esperado:

```text
Adapter registrado y aceptado administrativamente.
```

---

### 2. Adapter reporta instancias locales

```text
1. Adapter detecta instancias LinkedHelper locales.
2. Adapter informa esas instancias a Fluxbee Cloud.
3. Fluxbee Cloud registra esas instancias.
4. Fluxbee Cloud las deja en estado pendiente, aprobada, rechazada o ignorada según corresponda.
```

Estados conceptuales posibles:

```text
discovered
pending_approval
approved
rejected
ignored
```

---

### 3. Fluxbee Cloud gestiona el alta de nodos

Para cada instancia aprobada:

```text
1. Fluxbee Cloud solicita a Fluxbee el alta/configuración del nodo IO.linkedhelper correspondiente.
2. Fluxbee crea o configura el nodo.
3. Fluxbee devuelve a Fluxbee Cloud la información necesaria para que el adapter pueda conectarse.
```

Estados conceptuales posibles:

```text
approved
provisioning_node
node_provisioned
node_provision_failed
```

---

### 4. Fluxbee Cloud devuelve destinos runtime

Cuando el nodo está listo:

```text
1. Fluxbee Cloud informa al adapter a dónde debe conectar cada instancia.
2. El adapter guarda el mapping entre instancia local y destino runtime.
3. Para esa instancia, la comunicación administrativa con Cloud puede considerarse cerrada.
```

Resultado esperado:

```text
Instancia asignada a un nodo IO.linkedhelper.
```

Estados conceptuales posibles:

```text
assigned_to_node
waiting_node_destination
```

---

### 5. Adapter conecta con el nodo

Luego de recibir el destino:

```text
1. Adapter conecta directamente contra el nodo IO.linkedhelper.
2. El nodo valida identidad, secreto, instancia y compatibilidad.
3. Si el nodo acepta la conexión, queda confirmada la conectividad técnica.
```

Este paso no implica necesariamente que el adapter ya pueda reportar eventos reales.

Estados conceptuales posibles:

```text
node_connection_pending
node_connection_confirmed
node_connection_failed
```

---

### 6. Habilitación operativa

Debe separarse la conectividad técnica de la habilitación para emitir eventos.

Una instancia puede estar:

```text
- aprobada
- provisionada
- asignada a un nodo
- conectada correctamente al nodo
- pero todavía disabled para emitir eventos
```

Por eso se propone separar:

```text
node_connection_confirmed
enabled
```

Regla:

```text
Solo una instancia enabled puede emitir eventos runtime.
```

Estados operativos posibles:

```text
disabled
enabled
paused
blocked
```

---

## Comunicación normal después del alta

Una vez completado el alta:

```text
Adapter aceptado.
Instancias aprobadas.
Nodos provisionados.
Destinos runtime entregados.
Adapter conectado a cada nodo.
Instancias en estado enabled o disabled.
```

Fluxbee Cloud no necesita seguir recibiendo un `alive` permanente del adapter.

La operación diaria queda principalmente en:

```text
LinkedHelper Adapter <> IO.linkedhelper
```

Ahí deberían vivir:

```text
- validación runtime
- compatibilidad de protocolo
- envío de eventos
- recepción de acciones/respuestas
- acks
- errores operativos
- aplicación efectiva de enabled/disabled
```

---

## Cuándo vuelve a hablar el adapter con Fluxbee Cloud

El adapter debería volver a contactar a Fluxbee Cloud cuando exista trabajo administrativo nuevo.

Ejemplos:

```text
- aparece una nueva instancia LinkedHelper local
- una instancia perdió su asignación
- el adapter perdió o invalidó credenciales administrativas
- necesita recuperar configuración administrativa
- necesita informar un cambio relevante de inventario
- necesita resolver una instancia todavía no conocida por Cloud
```

Caso principal:

```text
Adapter detecta nueva instancia
=> contacta Cloud
=> Cloud registra/aprueba/provisiona
=> Cloud devuelve destino
=> adapter conecta con nodo
=> se vuelve a cerrar la comunicación administrativa
```

---

## Estado visible en Fluxbee Cloud

Fluxbee Cloud puede seguir guardando información en su PostgreSQL, pero idealmente debe evitar transformarse en la fuente de verdad de todo el runtime.

### Información razonable para guardar en PostgreSQL de Cloud

```text
- adapter_id
- tenant_id
- datos mínimos de AdapterInstallation
- estado administrativo del adapter
- instancias descubiertas
- decisiones administrativas sobre instancias
- mappings aprobados
- managed_instance_id
- último destino runtime conocido
- última fecha de contacto administrativo
- estado administrativo enabled/disabled
```

### Información que idealmente debería venir desde Fluxbee

Para evitar duplicación y lógica innecesaria en Cloud, la mayor parte del estado operativo debería consultarse o recibirse desde Fluxbee.

Ejemplos:

```text
- estado real del nodo IO.linkedhelper
- conectividad runtime del adapter con el nodo
- último contacto runtime
- errores runtime del nodo
- compatibilidad aceptada o rechazada por el nodo
- health del nodo
- estado operativo efectivo de la instancia
```

Regla deseada:

```text
Fluxbee Cloud puede mostrar estado operativo, pero idealmente no lo calcula ni lo monitorea directamente.
Lo obtiene desde Fluxbee cuando necesita mostrarlo o sincronizarlo.
```

---

## Implicancias para `alive`

El concepto de `alive` contra Fluxbee Cloud debe revisarse.

Ya no debería entenderse como:

```text
heartbeat permanente obligatorio
```

Tampoco debería ser necesario para:

```text
- detectar updates
- decidir compatibilidad con LinkedHelper
- decidir compatibilidad con el nodo
- determinar si el adapter puede emitir eventos
```

En el nuevo modelo, el antiguo `alive` se reemplaza o redefine como una consulta administrativa mientras haya pendientes.

Concepto nuevo:

```text
El adapter consulta a Cloud mientras espera algo de Cloud.
```

Por lo tanto:

```text
La ausencia de alive reciente en Fluxbee Cloud no significa necesariamente que el adapter esté caído.
Puede significar que no tiene trámites administrativos pendientes.
```

---

## Compatibilidad y updates

También se redefine lo que Cloud debe decidir.

Fluxbee Cloud no debería ser quien determina en runtime:

```text
- si LinkedHelper es compatible con el adapter
- si el adapter debe actualizarse
- si el adapter puede hablar con el nodo
- si el protocolo runtime es compatible
```

Esas responsabilidades deberían separarse así:

```text
Adapter:
- detecta versión/schema/capacidades de LinkedHelper local
- decide si puede leer esa instalación
- si no puede, pausa emisión de eventos

IO.linkedhelper:
- valida protocolo, capabilities, identidad y autorización del adapter
- acepta o rechaza comunicación runtime
- aplica enabled/disabled efectivo

Fluxbee Cloud:
- puede registrar/reportar versiones como dato administrativo o de diagnóstico
- no debería gobernar el runtime de compatibilidad
```

Regla importante:

```text
Si el adapter detecta incompatibilidad con LinkedHelper o el nodo rechaza compatibilidad runtime, el adapter debe dejar de emitir eventos nuevos hasta recuperar compatibilidad.
```

---

## Enabled / disabled

El concepto `enabled/disabled` debe quedar explícito.

Hay que separar estos estados:

```text
- instancia descubierta
- instancia aprobada
- nodo provisionado
- destino asignado
- conexión con nodo confirmada
- instancia habilitada para emitir eventos
```

Una instancia conectada no necesariamente está habilitada.

Ejemplo válido:

```text
approved = true
node_provisioned = true
node_connection_confirmed = true
enabled = false
```

En ese caso:

```text
El adapter puede confirmar conexión con el nodo.
El nodo puede reconocer la instancia.
Pero no deben emitirse eventos de negocio.
```

La decisión administrativa de `enabled/disabled` puede administrarse desde Fluxbee Cloud, pero el enforcement efectivo debería ocurrir en Fluxbee / IO.linkedhelper, no depender de un heartbeat contra Cloud.

---

## Nueva distribución de responsabilidades

### Fluxbee Cloud

```text
Control administrativo liviano.

Responsabilidades:
- registrar adapters
- aceptar/rechazar adapters
- entregar credenciales administrativas iniciales
- recibir discovery de instancias
- permitir aprobación/rechazo/ignorar instancias
- pedir a Fluxbee el provisioning de nodos
- recibir destinos runtime
- entregar destinos runtime al adapter
- guardar estado administrativo mínimo
- mostrar estado en UI
- consultar a Fluxbee para estado operativo cuando corresponda

No debería:
- monitorear permanentemente al adapter
- decidir compatibilidad runtime
- decidir updates del adapter
- recibir eventos LinkedHelper de negocio
- ser intermediario permanente entre adapter y nodo
```

---

### LinkedHelper Adapter

```text
Agente local.

Responsabilidades:
- iniciar comunicación administrativa con Cloud cuando corresponda
- enrolarse
- detectar instancias LinkedHelper locales
- reportar instancias nuevas a Cloud
- consultar “qué tenés para mí” mientras haya pendientes administrativos
- recibir destino runtime por instancia
- conectar cada instancia con su nodo IO.linkedhelper
- validar compatibilidad local con LinkedHelper
- pausar eventos si no hay compatibilidad
- emitir eventos solo contra el nodo y solo si la instancia está enabled
```

---

### Fluxbee / IO.linkedhelper

```text
Runtime real.

Responsabilidades:
- recibir conexiones del adapter
- validar adapter_id / secret / managed_instance_id / instancia
- validar compatibilidad de protocolo y capabilities
- aplicar enabled/disabled operativo
- recibir eventos
- devolver acciones/respuestas
- manejar acks
- reportar estado runtime hacia Fluxbee y, eventualmente, hacia Cloud
```

---

## Redefinición central

```text
El alta de una instancia LinkedHelper no es lo mismo que su operación runtime.

El alta ocurre entre:
Adapter <> Fluxbee Cloud <> Fluxbee

La operación ocurre entre:
Adapter <> IO.linkedhelper

Fluxbee Cloud solo vuelve a intervenir cuando hay cambios administrativos, nuevas instancias o necesidad de reprovisioning.
```

---

## Reglas a llevar al diseño técnico

```text
1. No asumir heartbeat permanente Adapter <> Fluxbee Cloud.
2. Reemplazar o redefinir alive como consulta administrativa tipo “qué tenés para mí”.
3. Cloud debe cerrar su participación normal cuando entrega destino runtime por instancia.
4. Confirmar conexión con nodo no implica emitir eventos.
5. Agregar estado enabled/disabled por instancia.
6. La emisión de eventos solo ocurre si la instancia está enabled y el nodo acepta runtime.
7. Cloud puede guardar estado administrativo en PostgreSQL.
8. El estado operativo debería venir preferentemente desde Fluxbee.
9. Nuevas instancias reabren el ciclo administrativo solo para esas instancias.
10. Compatibilidad y updates no deberían depender de Cloud como monitor activo.
```

---

## Estado de implementación (2026-07-03)

Implementado y stageado en el repo `fluxbee` (nodo + adapter; el Cloud no requirió
cambios de correctitud). El contrato del canal de control vive en
`contrato_auth_vault_io_linkedhelper_v1.md` §8/§9.

| Regla | Estado |
|---|---|
| 1. No heartbeat permanente Adapter↔Cloud | ✅ El loop del adapter contacta Cloud **on-demand** (`has_pending_admin_work`): solo con trámite pendiente (esperando approval/provisioning, o un reject del nodo lo pidió). |
| 2. Redefinir `alive` como consulta administrativa | ✅ El `alive` a Cloud pasó a on-demand; el poll continuo es contra el **nodo** (`/v1/poll`). |
| 3. Cloud cierra su participación al entregar destino | ✅ Con todas las instancias bound → `cloudContacted=false`; el adapter solo poletea el nodo. |
| 4. Confirmar conexión ≠ emitir eventos | ✅ Separado: `operational_state` (enabled/disabled) es independiente de la conectividad; el reject `pause`/`instance_disabled` corta emisión sin cortar el status-poll. |
| 5. Estado enabled/disabled por instancia | 🟡 **Seam cableado** a nivel nodo/managed-instance (1 cuenta = 1 nodo) + reject `pause`; el **toggle administrativo diferido** (default `enabled`). |
| 6. Eventos solo si enabled y el nodo acepta | ✅ En modo `events`, instancia `disabled` → `409 instance_disabled` + `pause`. |
| 7. Cloud guarda estado administrativo en PostgreSQL | ✅ (sin cambios). |
| 8. Estado operativo desde Fluxbee | ⏸️ **Diferido** (Cloud sigue mostrando lo último reportado). |
| 9. Nuevas instancias reabren el ciclo solo para ellas | ✅ El nodo devuelve `reprovision`/`reenroll` → el adapter reabre el ciclo administrativo por esa instancia (`needs_admin_sync`). |
| 10. Compatibilidad/updates no dependen de Cloud como monitor | ✅ Updates se evalúan solo cuando el adapter contacta Cloud; se agregó un knob opcional de re-sync lento (`--admin-resync-seconds`, default off) para que Cloud pueda empujar updates a un adapter mudo sin volver a un heartbeat de alta frecuencia. |

**Mecanismo central (regla 1/9):** cada respuesta de `/v1/poll` lleva un bloque
`control { operational_state, directive, reason, retry_after_seconds }`. El nodo
—no un heartbeat contra Cloud— le dice al adapter si seguir (`continue`), pausar
(`pause`), o reabrir el ciclo administrativo (`reenroll` / `reprovision`).

**Validado E2E (2026-07-03):** instalación "de usuario" en container Ubuntu limpio
(descarga del binario **solo con el token** → enroll → discovery) → aprobación en
la UI → provisioning del nodo `IO.linkedhelper` nuevo → el adapter reporta al nodo
(`200` + `control{enabled, continue}`) con `cloudContacted=false`.

**Pendiente (fase 2):** Windows (build target + `windows.rs` real + instalador/
servicio + artefacto de release); reemplazo del `direct_node_http` por el Edge;
toggle enabled/disabled administrativo + su fuente; estado operativo leído desde
Fluxbee.
