# Especificación técnica — Corrección semántica de ICH en Fluxbee

**Estado:** propuesta de corrección para desarrollo  
**Fecha:** 2026-05-20  
**Objetivo:** aclarar el malentendido detectado sobre `ICH`, alinear documentación core/identity/protocolo y orientar los cambios necesarios en nodos IO e Identity.

---

## 1. Resumen ejecutivo

Durante la revisión del core, Identity y nodos IO se detectó que `ICH` fue interpretado y aplicado de forma ambigua.

La interpretación que quedó aplicada en varios documentos y partes del código fue:

```text
ICH = canal/dirección del interlocutor externo
```

Ejemplos de esa interpretación:

```text
whatsapp:+5411...
email:cliente@dominio.com
slack:user-id
```

Esa lectura ya no debe considerarse válida para el modelo canónico.

La semántica correcta que debe consolidarse es:

```text
ICH = canal operativo propio del sistema Fluxbee, asociado al ILK interno del nodo IO
```

Es decir, un `ICH` representa el asset/entrypoint/inbox/canal local que Fluxbee controla o expone mediante un nodo IO: número de WhatsApp propio, cuenta Gmail propia, app/canal Slack propio, endpoint API propio, etc.

Por lo tanto:

```text
ICH NO representa al interlocutor externo.
ICH NO debe ser creado para ILKs externos.
ICH NO debe ser usado como alias/contact handle de una persona externa.
```

El interlocutor externo debe vivir en `ILK`, aliases, external identities, contact identities o estructuras equivalentes, pero no como `ICH`.

---

## 2. Por qué se produjo la confusión

La documentación actual conserva definiciones antiguas o ambiguas, por ejemplo:

- `docs/10-identity-v2.md` define `ICH` como un canal owned by a single ILK y da ejemplos mixtos como `this person's WhatsApp` y `this agent's API endpoint`.
- `docs/11-context.md` es un documento histórico del modelo `CTX`, pero todavía contiene muchos ejemplos donde `ICH` parece ser un canal del interlocutor externo.
- `docs/02-protocolo.md` todavía describe `meta.ich` como canal por el cual se comunica el interlocutor, sin dejar suficientemente claro si ese canal es externo o propio del sistema.

El documento histórico de incompatibilidades usa `CTX` para explicar el problema, pero `CTX` ya no es el carrier conversacional canónico. Aun así, ese análisis sigue siendo útil para entender la ambigüedad semántica de `ICH`.

---

## 3. Estado actual de CTX vs thread_id

Antes de modificar documentación o código conviene separar dos temas:

1. La semántica de `ICH`.
2. El carrier conversacional.

En la documentación core actual, `CTX` está deprecado/histórico.

El modelo canónico actual es:

```text
meta.thread_id
meta.thread_seq
```

Según la documentación core vigente:

- `docs/02-protocolo.md` marca `thread_id` y `thread_seq` como campos L3 canónicos.
- `docs/02-protocolo.md` marca `ctx`, `ctx_seq` y `ctx_window` como legacy.
- `docs/04-routing.md` indica que el router ya no usa `ctx` como unidad canónica ni agrega `ctx_window`.
- `docs/12-cognition-v2.md` indica que `thread_id` es calculado por SDK/IO y `thread_seq` por router.
- `docs/11-context.md` debe tratarse como documento histórico del modelo anterior `CTX/ctx_window`.

Por lo tanto, al corregir `ICH` no debe reactivarse ni rediseñarse `CTX`.

La forma correcta de expresar el impacto es:

```text
Antes, el problema se manifestaba como CTX = hash(ILK externo + ICH externo).
Hoy, el impacto real está en cómo IO calcula/provee meta.ich y meta.thread_id.
```

`thread_id` reemplaza a `CTX` como carrier técnico/canónico de continuidad conversacional, pero no hereda exactamente su semántica. `CTX` mezclaba identidad, canal y conversación; `thread_id` representa el hilo físico/relacional del medio. La semántica cognitiva vive aparte en cognition v2: scope, context, reason, memory, etc.

---

## 4. Nueva definición canónica de ICH

### 4.1 Definición

`ICH` significa, para el modelo canónico actual:

```text
Interaction Channel Handle
```

Debe entenderse como:

```text
Identificador del canal operativo local del sistema Fluxbee usado por un nodo IO.
```

Un `ICH` representa un entrypoint/inbox/asset propio del sistema, no un destino externo.

Ejemplos correctos:

```text
ICH.whatsapp.soporte.<numero-propio>
ICH.whatsapp.ventas.<numero-propio>
ICH.gmail.<cuenta-propia>
ICH.slack.<workspace/app/channel-propio>
ICH.api.<asset-or-endpoint-id>
```

El formato físico puede seguir siendo `ich:<uuid-v4>` si esa es la decisión de Identity, pero la semántica del registro debe apuntar al canal local del sistema.

### 4.2 Ownership

Un `ICH` debe estar asociado al `ILK` interno del nodo IO que lo opera.

En la etapa actual:

```text
ILK interno del nodo IO -> ICH
```

En una etapa posterior, cuando los nodos IO sean plenamente multiinstancia, la relación esperada será:

```text
ILK interno de instancia IO <> ICH
```

con cardinalidad efectiva:

```text
1 instancia IO = 1 ILK interno = 1 ICH operativo
```

Esta cardinalidad describe la instancia concreta, no necesariamente el tipo de nodo global. Por ejemplo, puede haber muchas instancias `IO.whatsapp`, pero cada instancia concreta opera su propio `ICH`.

El criterio de diseño a consolidar es que los nodos IO apunten a un modelo multiinstancia limpio donde:

```text
1 instancia concreta -> 1 ILK interno -> 1 ICH operativo principal
```

Si en un caso particular una implementación necesitara manejar más de un asset local, deberá explicitarse ese caso y definirse cuál es el `ICH` operativo correcto o si en realidad corresponde modelarlo como múltiples instancias.

### 4.3 Qué NO es un ICH

No son `ICH`:

- El teléfono del usuario final.
- El email del usuario final.
- El Slack user id del usuario final.
- Un alias externo de una persona.
- Un identificador de contacto remoto.
- Un `thread_id`.
- Un tenant.
- Un `src_ilk` externo.

Esos datos pueden formar parte de identity/contact/alias/thread material, pero no definen el ownership de `ICH`.

---

## 5. Relación entre nodo IO, ILK interno, ICH e interlocutor externo

La separación esperada es:

| Concepto | Representa | Ownership |
|---|---|---|
| Nodo IO | Runtime/adaptador que conecta Fluxbee con un medio | Sistema Fluxbee |
| ILK interno del nodo IO | Identidad L3 del nodo/instancia IO | Sistema Fluxbee |
| ICH | Canal/asset/entrypoint operado por ese IO | Sistema Fluxbee |
| Interlocutor externo | Persona/cuenta/contacto que habla con Fluxbee | Externo |
| ILK externo | Identidad L3 que representa al interlocutor externo cuando corresponda | Identity |
| Alias/contact handle externo | Datos para reconocer al interlocutor externo en un medio | Identity/contact layer |
| thread_id | Hilo físico/relacional de conversación | Calculado por IO/SDK |

El nodo IO puede necesitar conocer tanto su `ICH` local como el identificador remoto del interlocutor para procesar un mensaje entrante. Pero ambos datos tienen roles distintos:

```text
ICH local = por dónde entró/sale la comunicación en Fluxbee
remote handle = quién habló desde afuera
```

---

## 6. Ciclo de vida operativo esperado del ICH

### 6.1 Estado actual

Por ahora, el nodo recibe la información necesaria para su `ICH` mediante configuración, por ejemplo vía `config set`.

No se descarta que a futuro esa información venga desde el spawn del nodo, pero ese camino no debe asumirse como implementado ahora.

### 6.2 Regla de operación

Un nodo IO no queda operativo hasta tener un `ICH` propio asignado/registrado.

Al levantar, cada nodo IO debe ejecutar una secuencia equivalente a:

```text
1. Leer su ILK interno.
2. Leer configuración del asset/canal local que debe operar.
3. Verificar si ya existe un ICH asociado a su ILK interno y a ese asset/canal.
4. Si existe, usarlo.
5. Si no existe, y tiene configuración suficiente, registrar/declarar el ICH ante Identity.
6. Si no puede resolver/registrar ICH, quedar no operativo y reportar estado claro.
```

La no-operatividad no implica necesariamente que el proceso deba caer. El nodo puede permanecer vivo para:

- control-plane;
- observabilidad;
- recuperación operativa;

pero no debe aceptar ni procesar tráfico operativo real del canal mientras no tenga su `ICH` resuelto/configurado.

### 6.3 Responsabilidad del nodo IO

Cada tipo de nodo IO conoce cómo derivar o declarar su propio `ICH`.

Ejemplos:

| Nodo | Material local que puede definir su ICH |
|---|---|
| `IO.whatsapp` | número propio, phone_number_id, business account id, asset id |
| `IO.gmail` | cuenta propia, mailbox id, asset id |
| `IO.slack` | app/workspace/channel/inbox propio, asset id |
| `IO.api` | endpoint/asset/client config propio; inicialmente puede atarse al puerto/entrypoint local y refinarse luego el material exacto de unicidad |

El nodo IO es responsable de proveer a Identity el dato correcto. Identity no debería inferir que el `ICH` pertenece al interlocutor externo.

---

## 7. Impacto sobre mensajes y thread_id

### 7.1 `meta.ich`

En mensajes producidos por IO, `meta.ich` debe apuntar al `ICH` local del sistema.

Ejemplo conceptual:

```json
{
  "meta": {
    "src_ilk": "ilk:<persona-externa>",
    "ich": "ich:<canal-local-whatsapp-soporte>",
    "thread_id": "thread:sha256:<...>"
  }
}
```

En una respuesta del sistema hacia afuera, el mismo `ICH` permite saber qué nodo/canal local debe despachar la respuesta.

### 7.2 `thread_id`

`thread_id` sigue siendo el carrier canónico de continuidad conversacional.

Debe calcularse por el productor que conoce el medio, típicamente el nodo IO o el SDK usado por ese nodo.

El criterio actual es mantener la forma en que cada nodo ya resuelve hoy su `thread_id`. Si algún nodo no lo tiene correctamente resuelto o lo tiene resuelto con material ambiguo, ese caso debe revisarse puntualmente en vez de forzar una redefinición general desde este documento.

El material para calcular `thread_id` debe incluir el `ICH` local cuando corresponda, para evitar colisiones entre assets locales distintos.

Ejemplo:

```text
Mismo usuario externo escribe a WhatsApp soporte -> thread_id A
Mismo usuario externo escribe a WhatsApp ventas  -> thread_id B
```

La diferencia no debe salir de un `ICH` externo del usuario, sino del `ICH` local del canal Fluxbee que recibió el mensaje.

### 7.3 CTX

No deben agregarse nuevos usos de `ctx`, `ctx_seq` o `ctx_window`.

Si aparece `CTX` en documentación de análisis o documentos históricos, debe marcarse explícitamente como analogía o antecedente histórico, no como contrato activo.

---

## 8. Cambios requeridos en documentación

### 8.1 Documentos core a corregir

#### `docs/10-identity-v2.md`

Actualizar la sección de `ICH` para eliminar la ambigüedad.

Cambiar ideas como:

```text
this person's WhatsApp
ILK has many ICHs
new ICH for existing person/new channel
```

por una definición explícita:

```text
ICH is the local/system-owned communication channel operated by an IO node.
ICH belongs to the internal ILK of the IO node/instance that owns that channel.
External interlocutor handles are not ICHs.
```

También revisar secciones de:

- lifecycle de ICH;
- `ILK_PROVISION`;
- `ILK_ADD_CHANNEL`;
- SHM IchEntry/IchMappingEntry;
- ejemplos de first contact;
- ejemplos de merge alias;
- constraints de duplicidad.

Hay que evitar que `ICH` parezca un canal agregado a una persona externa.

#### `docs/02-protocolo.md`

Actualizar descripción de `meta.ich`.

La descripción actual debe reemplazarse por algo equivalente a:

```text
ICH local/sistémico por el cual ingresó o debe salir la comunicación.
Debe corresponder al canal operativo del nodo IO, no al handle remoto del interlocutor externo.
```

También actualizar la sección de campos canónicos para aclarar:

```text
ich + thread_id + thread_seq
```

no significa:

```text
interlocutor external channel + ctx replacement
```

sino:

```text
local channel handle + physical/relational conversation thread + per-thread sequence
```

#### `docs/11-context.md`

Este documento ya se marca como histórico, pero contiene ejemplos que pueden seguir confundiendo.

Opciones posibles:

1. Mantenerlo como histórico, pero agregar un warning al inicio:

```text
Este documento describe el modelo histórico CTX/ctx_window. No usar sus ejemplos de ICH como semántica actual. En el modelo actual, ICH representa el canal local del sistema operado por un nodo IO, y la continuidad conversacional usa thread_id/thread_seq.
```

2. O moverlo a una carpeta `docs/legacy/` si el equipo prefiere separar documentación activa de histórica.

#### `docs/12-cognition-v2.md`

Revisar ejemplos de `thread_id` para asegurar que `ich_id` se entienda como canal local del sistema.

Ejemplo deseado:

```text
DM / 1:1 = hash(remote participant identity + local ich_id)
Group/persistent channel = hash(local ich_id + native channel/group id si aplica)
Medium-native thread = hash(local ich_id + native_thread_id)
```

El punto central es que `ich_id` no sea leído como un identificador del usuario externo.

No todos los medios ni todos los nodos necesariamente usarán exactamente el mismo material de entrada, pero la regla general es:

- `thread_id` se calcula, no se recibe como verdad externa;
- el cálculo queda a cargo del nodo IO o del SDK que conoce el medio;
- si un nodo todavía no incorpora correctamente el `ICH` local en ese cálculo cuando corresponde, ese caso debe tratarse como gap puntual de implementación.

#### `docs/04-routing.md`

Agregar una aclaración en el contrato operativo:

```text
El router usa thread_id/thread_seq como carrier conversacional. meta.ich identifica el canal local/sistémico involucrado y puede usarse para routing/dispatch, pero no representa al interlocutor externo.
```

#### Documentación de nodos IO

Revisar todas las specs de nodos IO para reemplazar patrones tipo:

```text
external_id -> ICH -> ILK externo
```

por:

```text
asset/canal local -> ICH del nodo IO
remote external_id -> resolución/creación de ILK externo o alias/contact identity
```

---

## 9. Cambios requeridos en código

Esta sección no prescribe implementación exacta, sino los puntos que el equipo debería revisar para generar el plan técnico.

### 9.1 SDK / Identity

Revisar `crates/fluxbee_sdk/src/identity.rs`.

Puntos observados:

- Existen estructuras `IchProvisionRequest`, `IdentityIchOption`, `ResolvedIdentityOption`, etc. con campos `ich_id`, `type`, `address`, `tenant_id`, `owner_l2_name`, `enabled`.
- La resolución actual tiene funciones como `resolve_ilk_from_shm_name(...)` y variantes que resuelven por `(channel_type, address, tenant_id)`.
- Ese patrón puede seguir siendo útil si `address` representa el asset/canal local del sistema, pero es incorrecto si `address` representa el handle remoto del usuario final.

Acción esperada:

- Renombrar o documentar mejor `address` si queda ambiguo.
- Evaluar si conviene separar explícitamente:
  - `local_channel_address` / `asset_address` / `asset_key`;
  - `remote_address` / `external_handle`.
- Asegurar que el mapping de `ICH` se use para canales propios del sistema.
- Evitar APIs que incentiven resolver `ILK externo` desde `ICH remoto`.

### 9.2 Identity service (`src/bin/sy_identity.rs`)

Revisar handlers de provisioning/registro/asociación de ICH.

Acciones esperadas:

- Validar que `ICH` se registre contra el ILK interno propietario del nodo IO.
- Evitar flujos donde first contact cree un `ICH` para la persona externa.
- Revisar constraints de duplicidad. La unicidad debería proteger el asset/canal local, no el handle remoto del interlocutor.
- Revisar eventos/auditoría para que el ownership quede claro.

### 9.3 Nodos IO

Para cada nodo IO actual o futuro:

- Al boot, resolver su ILK interno.
- Leer config del asset/canal local.
- Verificar existencia de `ICH` propio.
- Registrar/declarar `ICH` si falta y hay config suficiente.
- No quedar operativo si no hay `ICH`.
- Al recibir mensajes entrantes:
  - usar `meta.ich = ICH local`;
  - resolver/crear/identificar `src_ilk` externo por otro mecanismo;
  - calcular `thread_id` usando material del medio + `ICH` local.
- Al despachar respuestas:
  - usar `meta.ich` para seleccionar canal local de salida;
  - usar identidad/contact handle externo para seleccionar destinatario remoto.

### 9.4 Thread SDK

Revisar `crates/fluxbee_sdk/src/thread.rs` y usos de `compute_thread_id(...)`.

Acción esperada:

- Confirmar que los inputs incluyen `ich_id` local, no remoto.
- Agregar tests explícitos:

```text
same remote participant + same local ich => same thread_id
same remote participant + different local ich => different thread_id
same local ich + different native thread id => different thread_id
```

### 9.5 Tests/e2e

Actualizar o agregar tests para cubrir:

- Nodo IO sin ICH no queda operativo.
- Nodo IO con config suficiente registra/obtiene ICH al boot.
- Mensaje entrante usa `meta.ich` local.
- Interlocutor externo no genera ICH propio.
- Dos assets locales distintos generan threads distintos aunque el usuario externo sea el mismo.
- Dispatch de respuesta usa `ICH` local para seleccionar nodo/canal de salida.
- `ctx*` no se usa en paths nuevos.

---

## 10. Ejemplos de comportamiento esperado

### 10.1 WhatsApp soporte vs ventas

Configuración:

```text
IO.whatsapp.soporte@motherbee
  ILK interno: ilk:<io-whatsapp-soporte>
  ICH local:  ich:<whatsapp-soporte>
  asset:      +5411123

IO.whatsapp.ventas@motherbee
  ILK interno: ilk:<io-whatsapp-ventas>
  ICH local:  ich:<whatsapp-ventas>
  asset:      +5411234
```

Usuario externo:

```text
Teléfono remoto: +5411345
ILK externo:     ilk:<persona-juan>
```

Si Juan escribe a soporte:

```text
src_ilk = ilk:<persona-juan>
ich = ich:<whatsapp-soporte>
thread_id = hash(material del medio + ich:<whatsapp-soporte> + remoto relevante)
```

Si Juan escribe a ventas:

```text
src_ilk = ilk:<persona-juan>
ich = ich:<whatsapp-ventas>
thread_id = hash(material del medio + ich:<whatsapp-ventas> + remoto relevante)
```

Resultado esperado:

```text
Misma persona externa.
Distinto ICH local.
Distinto thread_id.
Distinto contexto operativo/cognitivo.
Dispatch de respuesta correcto.
```

### 10.2 Gmail

Cuenta propia:

```text
soporte@empresa.com
```

Debe ser parte del asset/canal local que define el `ICH` del nodo Gmail.

Email remoto:

```text
cliente@dominio.com
```

No es un `ICH`. Es un handle externo/contact identity asociado al interlocutor externo.

### 10.3 IO.api

Un cliente externo llama a un endpoint/API asset de Fluxbee.

El `ICH` representa el endpoint/asset local del sistema, no el identificador del cliente remoto.

El cliente remoto puede resolverse como ILK externo, tenant, sponsor, alias, api consumer identity o lo que corresponda según el modelo Identity, pero no como `ICH`.

---

## 11. Riesgos si no se corrige

Si se mantiene la interpretación vieja:

- El mismo usuario externo puede colisionar entre soporte/ventas/marcas.
- El sistema no puede determinar de forma confiable desde qué asset local responder.
- `thread_id` puede calcularse con material incorrecto.
- Identity mezcla contact handles externos con canales propios del sistema.
- Los nodos IO quedan obligados a guardar runtime dispatch data fuera del modelo canónico.
- Multiinstancia de nodos IO se vuelve confusa o inconsistente.
- Fluxbee Cloud no tendrá un modelo limpio para spawnear nodos desde assets.

---

## 12. Criterios de aceptación

El trabajo puede considerarse alineado cuando se cumplan estos criterios:

### Documentación

- La definición canónica de `ICH` no permite interpretarlo como canal del interlocutor externo.
- `docs/10-identity-v2.md` explica ownership de `ICH` por ILK interno de nodo IO/instancia IO.
- `docs/02-protocolo.md` aclara que `meta.ich` es local/sistémico.
- `docs/11-context.md` queda marcado como legacy o movido fuera de documentación activa.
- Specs de nodos IO separan asset/canal local de handle remoto.
- Toda mención a `CTX` en documentos nuevos queda marcada como histórica/no canónica.

### Código

- Los nodos IO no crean ICHs para interlocutores externos.
- Cada nodo IO valida/obtiene/registra su ICH antes de operar.
- `meta.ich` en mensajes IO apunta al canal local del sistema.
- `thread_id` se calcula con `ICH` local cuando corresponde.
- Los handles remotos se modelan fuera de ICH.
- No se agregan nuevos usos de `ctx*` en paths nuevos.

### Tests

- Existen tests unitarios/e2e que prueban separación entre:
  - ICH local;
  - interlocutor externo;
  - thread_id;
  - dispatch de respuesta.
- Hay tests de no-operatividad sin ICH.
- Hay tests de multiasset/multiinbox: mismo usuario externo + distinto asset local => distinto thread.

---

## 13. Glosario recomendado

Para evitar repetir la ambigüedad, se recomienda usar estos términos en documentación nueva:

| Término | Uso recomendado |
|---|---|
| `ICH` | Canal/asset/entrypoint local del sistema Fluxbee |
| `local channel` | Sinónimo explicativo de ICH cuando ayude |
| `asset` | Recurso dado de alta por Fluxbee Cloud o config: número, cuenta, endpoint |
| `remote handle` | Dirección/identificador externo del interlocutor |
| `contact identity` | Identidad o alias externo del interlocutor |
| `ILK interno` | ILK del nodo/instancia IO |
| `ILK externo` | ILK de persona/cuenta/contacto externo |
| `thread_id` | Hilo físico/relacional canónico |
| `ctx*` | Campos legacy/históricos |

Evitar en adelante:

```text
ICH del usuario
ICH externo
ICH de la persona
canal del interlocutor = ICH
```

Preferir:

```text
ICH local
ICH del nodo IO
ICH del asset
canal sistémico
entrypoint operativo
remote handle del interlocutor
```

---

## 14. Recomendación de orden de trabajo

Orden sugerido para que el equipo derive un plan:

1. Actualizar documentación core para fijar semántica y vocabulario.
2. Actualizar specs de nodos IO con separación `ICH local` vs `remote handle`.
3. Revisar contratos SDK/Identity y nombres ambiguos como `address`.
4. Ajustar boot lifecycle de nodos IO para exigir ICH antes de operar.
5. Ajustar producción de mensajes IO: `meta.ich` local + `thread_id` canónico.
6. Ajustar dispatch de respuestas para usar `ICH` como canal local de salida.
7. Agregar tests unitarios/e2e de no regresión.
8. Revisar/migrar documentación histórica de `CTX` para que no vuelva a inducir el error.

---

## 15. Decisión conceptual final

La decisión a consolidar es:

```text
ICH pertenece al sistema Fluxbee, no al interlocutor externo.
ICH se vincula al ILK interno del nodo IO/instancia IO que opera el canal.
El interlocutor externo se identifica por ILK/aliases/contact identities, no por ICH.
La continuidad conversacional canónica se expresa con thread_id/thread_seq, no con CTX.
```
