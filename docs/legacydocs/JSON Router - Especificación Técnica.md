# JSON Router - Especificación Técnica

**Estado:** Draft v1.12
**Fecha:** 2025-01-20

---

## Parte I: Fundamentos

### 1. Introducción

Este documento especifica un router de mensajes JSON diseñado para interconectar nodos heterogéneos en un sistema de agentes. Los nodos pueden ser:

- **Agentes AI**: Nodos con capacidad LLM para procesamiento de lenguaje natural.
- **Agentes WF**: Nodos con workflows estáticos y resolución determinística.
- **Nodos IO**: Adaptadores de medio (WhatsApp, email, Instagram, chat, etc.).
- **Nodos SY**: Infraestructura del sistema (tiempo, monitoreo, administración).

El sistema se configura mediante dos mecanismos ortogonales:

- **Tabla de ruteo**: Configuración estática de caminos (IP lógica, forwards, VPNs).
- **Policies OPA**: Reglas de negocio para resolución dinámica por rol/capacidad.

### 2. Principios de Diseño

**El router no almacena datos.** El router mueve mensajes entre colas. No persiste nada. Si necesita buffer de largo plazo, es responsabilidad del nodo.

**Las colas son de los nodos.** Cada nodo tiene su cola de entrada. La cola existe mientras el nodo existe. Si el nodo muere, la cola desaparece.

**Detección de link automática.** El router detecta cuando un nodo muere (link down) por señalización del sistema operativo, no por polling o heartbeat.

**Decisiones instantáneas.** El routing es O(1). Si una decisión toma tiempo, el modelo está mal.

**Separación de capas.** OPA decide qué rol/capacidad necesita un mensaje. El router decide qué nodo específico lo atiende.

### 3. Arquitectura de Dos Capas de Decisión

```
Mensaje llega
    │
    ▼
┌─────────────────────────────────────┐
│ Router recibe, lee header           │
└─────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────┐
│ ¿Tiene destino IP explícito?        │
└─────────────────────────────────────┘
    │ Sí                    │ No
    ▼                       ▼
Forward directo      ┌─────────────────────────────────────┐
                     │ Consulta OPA: ¿qué rol/capacidad?   │
                     └─────────────────────────────────────┘
                            │
                            ▼
                     OPA responde: "rol: X"
                            │
                            ▼
                     ┌─────────────────────────────────────┐
                     │ Router resuelve IP desde tabla      │
                     └─────────────────────────────────────┘
                            │
                            ▼
                     Forward al nodo elegido
```

#### 3.1 Capa OPA (Decisión de Negocio)

- Input: Mensaje y metadata únicamente.
- Output: Rol o capacidad requerida.
- No conoce: IPs, estado de nodos, carga, topología.

#### 3.2 Capa Router (Decisión de Red)

- Input: Rol/capacidad requerida (de OPA) o IP explícita (del mensaje).
- Output: Socket concreto del nodo destino.
- Conoce: Todo el estado de la red via shared memory.

### 4. Componentes del Sistema

| Componente | Responsabilidad |
|------------|-----------------|
| Nodo AI | Procesa mensajes con LLM, responde según perfil/rol. |
| Nodo WF | Ejecuta workflows determinísticos. |
| Nodo IO | Adapta medios externos (WhatsApp, email, etc.) al protocolo interno. |
| Nodo SY | Provee servicios de infraestructura (tiempo, monitoreo, admin). |
| Router | Detecta nodos, conecta sockets, mantiene tabla local, consulta OPA si hace falta, rutea, detecta link down. |
| Shared Memory | Tabla de ruteo y estado de nodos. Compartida entre routers, no accesible por nodos. |
| OPA (WASM) | Evalúa policies de negocio. No accede a estado del sistema. |
| Librería de Nodo | Común a todos los nodos. Maneja protocolo de socket, framing, retry, reconexión. |

### 5. Tecnología Base

- **Router**: Rust (tokio, memmap2, raw-sync, nix, wasmtime)
- **Nodos**: Node.js (o cualquier lenguaje que soporte Unix sockets)
- **Sockets**: Unix domain sockets (`SOCK_STREAM`) con framing manual
- **Shared Memory**: POSIX shm (`shm_open`/`mmap`) con seqlock para sincronización
- **Policies**: OPA compilado a WASM, evaluado in-process
- **Formato de mensaje**: JSON con header de routing, framing con length prefix

### 5.1 Modelo de Red: Islas y Conectividad

El sistema se organiza en **islas**. Una isla es un dominio de routing donde los routers comparten estado via shared memory.

#### Definición de Isla

> **Isla = Host = Shared Memory**
>
> Todos los routers de una isla corren en el **mismo host** y comparten `/dev/shm`. 
> Si necesitás routers en hosts distintos, son **islas distintas** conectadas por WAN (TCP).

```
┌─────────────────────────────────────────────────────────────┐
│                        HOST FÍSICO                          │
│                         (Isla A)                            │
│                                                              │
│   ┌──────────┐   ┌──────────┐   ┌──────────┐              │
│   │ Router 1 │   │ Router 2 │   │ Router 3 │              │
│   └────┬─────┘   └────┬─────┘   └────┬─────┘              │
│        │              │              │                      │
│        └──────────────┼──────────────┘                      │
│                       │                                      │
│                       ▼                                      │
│              ┌─────────────────┐                            │
│              │   /dev/shm      │  ← Memoria compartida      │
│              │                 │    (todas las regiones)    │
│              │ /jsr-router-1   │                            │
│              │ /jsr-router-2   │                            │
│              │ /jsr-router-3   │                            │
│              │ /jsr-config-a   │                            │
│              └─────────────────┘                            │
│                                                              │
│   [Nodos]  [Nodos]  [Nodos]                                │
└─────────────────────────────────────────────────────────────┘
```

#### Conectividad Intra-Isla (Automática)

Dentro de una isla, el forwarding entre routers es **automático y sin configuración**:

- Los routers se descubren leyendo las regiones SHM de los peers
- El forwarding usa Unix sockets entre routers
- No se requiere configurar uplinks ni direcciones
- Es como una "LAN" local: todos ven a todos

```
Router 1 necesita enviar a nodo conectado a Router 2:
  1. Lee /jsr-router-2, ve que el nodo está ahí
  2. Envía por Unix socket a Router 2
  3. Router 2 entrega al nodo
```

**No hay configuración WAN dentro de una isla.**

##### Forwarding Intra-Isla: El Router como Switch

**IMPORTANTE:** Dentro de una isla, los routers actúan como un **switch de capa 2**. El forwarding entre routers es transparente y no agrega saltos de capa 1.

```
Isla A (un solo host)
┌─────────────────────────────────────────────────────────────┐
│                                                              │
│   [Nodo X]                              [Nodo Y]            │
│      │                                      │                │
│      │ socket                               │ socket         │
│      ▼                                      ▼                │
│  ┌──────────┐    fabric (socket)     ┌──────────┐          │
│  │ Router A │◄──────────────────────►│ Router B │          │
│  └──────────┘                        └──────────┘          │
│                                                              │
│                    /dev/shm                                 │
│              (todos leen todo)                              │
└─────────────────────────────────────────────────────────────┘

Mensaje de Nodo X a Nodo Y:
  1. Nodo X envía a Router A (su router local)
  2. Router A lee shm, ve que Nodo Y está en Router B
  3. Router A reenvía a Router B por fabric (Unix socket)
  4. Router B entrega a Nodo Y

El mensaje NO cambia. No se modifica routing.src ni routing.dst.
Router A y Router B actúan como UN SOLO switch lógico.
```

**Reglas del forwarding intra-isla:**

1. **Sin modificación de headers**: El mensaje pasa intacto entre routers
2. **Sin TTL decrement**: No es un salto de capa 1, es forwarding de capa 2
3. **Sin tablas de rutas para fabric**: El router descubre peers por shm, no por configuración
4. **Transparente para los nodos**: Un nodo no sabe si el destino está en su router o en otro de la misma isla

**Esto es diferente de WAN (inter-isla):**

| Aspecto | Intra-Isla (Fabric) | Inter-Isla (WAN) |
|---------|---------------------|------------------|
| Transporte | Unix socket | TCP |
| Configuración | Automática (shm) | Explícita (config.yaml) |
| TTL | No decrementa | Decrementa |
| Visibilidad | Transparente | Salto explícito |
| En tabla de rutas | No (implícito) | Sí (`out_link = 1..N`) |

#### Conectividad Inter-Isla (TCP Configurado)

Entre islas distintas, la conexión es por **TCP** y requiere configuración explícita:

```
┌─────────────────┐                    ┌─────────────────┐
│     Isla A      │                    │     Isla B      │
│    (Host 1)     │                    │    (Host 2)     │
│                 │                    │                 │
│  ┌───────────┐  │      TCP/WAN       │  ┌───────────┐  │
│  │  Router   │◄─┼────────────────────┼─►│  Router   │  │
│  └───────────┘  │   (configurado)    │  └───────────┘  │
│                 │                    │                 │
│   /dev/shm      │                    │   /dev/shm      │
│  (no accesible  │                    │  (no accesible  │
│   desde Host 2) │                    │   desde Host 1) │
└─────────────────┘                    └─────────────────┘
```

La conexión TCP entre islas:
- Requiere configuración en `config.yaml` del router (`wan.listen`, `wan.uplinks`)
- Usa mensajes LSA/SYNC porque no hay acceso a shm del peer
- Propaga rutas y estado via protocolo de mensajes

#### Resumen de Conectividad

| Escenario | Transporte | Configuración | Descubrimiento |
|-----------|------------|---------------|----------------|
| Routers misma isla | Unix socket | Ninguna (automático) | Via shm |
| Routers islas distintas | TCP | `wan.listen`, `wan.uplinks` | Via HELLO/LSA |
| Nodo → Router local | Unix socket | Ninguna | Socket en directorio conocido |

#### ¿Por qué este modelo?

- **Simplicidad:** Dentro de una isla, todo funciona automáticamente
- **Sin ambigüedad:** Si querés aislar routers, usás islas distintas (hosts distintos)
- **Performance:** Intra-isla usa shm + Unix sockets (mínima latencia)
- **Escalabilidad:** Agregás islas conectadas por TCP para crecer

---

## Parte II: Protocolo de Mensajes

### 6. Identificadores

El sistema usa dos capas de identificación independientes.

#### 6.1 Identificador Capa 1 (UUID)

- **Formato**: UUID v4 (128 bits, auto-generado)
- **Propósito**: Identificación única del nodo en la red
- **Generación**: El nodo lo genera al arrancar, sin coordinación central
- **Unicidad**: Garantizada por probabilidad matemática, no por validación
- **Representación**: String estándar UUID o base64url (22 chars) si se requiere compacto

```
Ejemplo: "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
```

#### 6.2 Identificador Capa 2 (Nombre Descriptivo)

- **Formato**: Campos separados por punto (`.`)
- **Máximo**: 10 campos
- **Caracteres permitidos**: Alfanuméricos, guión bajo (`_`), guión medio (`-`). Sin espacios, sin caracteres especiales.
- **Primer campo**: Obligatorio, indica tipo de nodo. Valores válidos: `AI`, `IO`, `WF`, `SY`.
- **Campos siguientes**: Libres, definen perfil/rol/capacidad según dominio.

```
Formato: <tipo>.<campo2>.<campo3>...<campoN>

Ejemplos:
  AI.soporte.l1.español
  AI.ventas.bdr.tecnico.nocturno
  IO.wapp.+5491155551234
  IO.email.soporte
  WF.notify.email
  WF.data.crm.update
  SY.time.primary
```

#### 6.3 Validación en Librería de Comunicación

La librería de nodo DEBE validar antes de registrar:

**Capa 1:**
- UUID válido según RFC 4122

**Capa 2:**
- Mínimo 1 campo, máximo 10 campos
- Primer campo es `AI`, `IO`, `WF`, o `SY`
- Cada campo contiene solo caracteres permitidos
- Ningún campo vacío

Mensajes con identificadores inválidos se rechazan antes de entrar a la red.

#### 6.4 Relación entre Capas

| Capa | Identifica | Único | Quién lo usa |
|------|-----------|-------|--------------|
| Capa 1 (UUID) | Instancia física del nodo | Sí, globalmente | Router para forwarding directo |
| Capa 2 (Nombre) | Perfil/capacidad del nodo | No necesariamente | OPA para decisión de routing |

Un mismo nombre capa 2 puede tener múltiples UUIDs (varios nodos con mismo perfil). El router resuelve cuál de ellos recibe el mensaje.

### 6.5 Caracterización de Nodos por Tipo

La estructura del nombre capa 2 varía según el tipo de nodo. El primer campo siempre indica el tipo (`AI`, `IO`, `WF`, `SY`), pero los campos siguientes siguen convenciones distintas según la naturaleza del nodo.

#### Nodos AI (Agentes LLM)

Los agentes AI emulan personas con roles funcionales. La nomenclatura sigue el modelo de **Recursos Humanos** para descripción de puestos:

```
AI.<área>.<cargo>.<nivel>.<especialización>.<turno>
```

| Campo | Descripción | Ejemplos |
|-------|-------------|----------|
| área | Departamento funcional | soporte, ventas, cobranzas, legal, rrhh, contable |
| cargo | Puesto o función | analista, ejecutivo, asesor |
| nivel | Seniority o tier | l1, l2, jr, sr, lead |
| especialización | Nicho de expertise | tecnico, comercial, enterprise, pyme, español, ingles |
| turno | Disponibilidad | diurno, nocturno, 24-7 |

**Ejemplos:**
```
AI.soporte.analista.l1.tecnico.diurno
AI.soporte.analista.l2.tecnico.nocturno
AI.ventas.ejecutivo.bdr.pyme
AI.ventas.ejecutivo.closer.enterprise
AI.cobranzas.analista.jr.amigable
AI.legal.asesor.sr.contratos
AI.rrhh.analista.recruiting
AI.contable.analista.facturas
```

No todos los campos son obligatorios. Un nodo puede registrarse como `AI.soporte.analista` si no requiere mayor especificidad.

#### Nodos IO (Adaptación de Medio)

Los nodos IO son infraestructura de comunicación. Representan canales, no personas. La nomenclatura describe el medio y el identificador de la cuenta/línea:

```
IO.<medio>.<identificador>
```

| Campo | Descripción | Ejemplos |
|-------|-------------|----------|
| medio | Canal de comunicación | wapp, email, telegram, instagram, sms, voice, chat |
| identificador | Cuenta, número o instancia | número de teléfono, dirección email, handle |

**Ejemplos:**
```
IO.wapp.+5491155551234
IO.wapp.+5491155559999
IO.email.ventas@empresa.com
IO.email.soporte@empresa.com
IO.telegram.@bot_empresa
IO.instagram.@cuenta_oficial
IO.chat.widget_web_01
IO.voice.troncal_001
IO.sms.+5491155550000
```

#### Nodos WF (Workflows Estáticos)

Los nodos WF son infraestructura de proceso. Ejecutan acciones determinísticas. La nomenclatura sigue el patrón de APIs y programación:

```
WF.<verbo>.<objeto>.<variante>
```

| Campo | Descripción | Ejemplos |
|-------|-------------|----------|
| verbo | Acción a ejecutar | send, query, update, validate, count, log |
| objeto | Recurso sobre el que actúa | email, sms, crm, erp, kyc, metrics |
| variante | Especificación adicional | tipo, formato, destino |

**Ejemplos:**
```
WF.send.email
WF.send.sms
WF.send.push
WF.query.crm
WF.update.crm
WF.query.erp.facturas
WF.validate.kyc
WF.validate.fraud
WF.count.mensajes
WF.log.evento
WF.route.escalation
WF.route.handoff
```

#### Nodos SY (Sistema)

Los nodos SY son infraestructura del sistema. Proveen servicios fundamentales para la operación de la red. No procesan mensajes de aplicación.

```
SY.<servicio>.<instancia>
```

| Campo | Descripción | Ejemplos |
|-------|-------------|----------|
| servicio | Función del sistema | time, monitor, admin, log |
| instancia | Identificador de la instancia | primary, backup, region |

**Ejemplos:**
```
SY.time.primary
SY.time.backup
SY.monitor.metrics
SY.admin.console
SY.log.collector
SY.config.routes.primary
SY.config.routes.backup
```

**Nodo SY.time (Time Server):**

El servicio de tiempo es responsabilidad del router o de un nodo SY.time dedicado. Provee tiempo UTC sincronizado a toda la red mediante broadcast periódico.

**Nodo SY.config.routes (Configuración de Rutas):**

Responsable de la configuración centralizada de rutas estáticas y VPNs. Un único proceso escribe en la región de shared memory `/jsr-config-<island>`, que todos los routers leen. Ver **Sección 27** para detalles completos.

#### Resumen de Convenciones

| Tipo | Modelo conceptual | Estructura |
|------|------------------|------------|
| AI | Recursos Humanos (personas con roles) | `AI.<área>.<cargo>.<nivel>.<especialización>.<turno>` |
| IO | Infraestructura de comunicación (canales) | `IO.<medio>.<identificador>` |
| WF | APIs/Programación (acciones) | `WF.<verbo>.<objeto>.<variante>` |
| SY | Infraestructura del sistema (servicios) | `SY.<servicio>.<instancia>` |
| RT | Routers (infraestructura de red) | `RT.<isla>.<rol>` |

#### Nodos RT (Routers)

Los routers también tienen identificador capa 2, siguiendo el patrón de infraestructura:

```
RT.<isla>.<rol>
```

| Campo | Descripción | Ejemplos |
|-------|-------------|----------|
| isla | Identificador de la isla | produccion, staging, desarrollo |
| rol | Función dentro de la isla | primary, secondary, backup |

**Ejemplos:**
```
RT.produccion.primary
RT.produccion.secondary
RT.staging.main
RT.desarrollo.local
```

El nombre capa 2 del router se define en la configuración de la isla y es estable. El UUID (capa 1) se genera automáticamente en el primer arranque y se persiste.

### 7. Estructura del Mensaje

Todo mensaje en la red tiene tres secciones:

```json
{
  "routing": { ... },
  "meta": { ... },
  "payload": { ... }
}
```

#### 7.1 Sección `routing` (Header de Red)

Usado por el router para decisiones de capa 1. El router DEBE poder tomar decisiones leyendo SOLO esta sección.

```json
{
  "routing": {
    "src": "uuid-origen",
    "dst": "uuid-destino | null",
    "ttl": 16,
    "trace_id": "uuid-correlación"
  }
}
```

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `src` | UUID | Sí | Nodo origen del mensaje |
| `dst` | UUID, "broadcast", o null | Sí | Nodo destino. UUID=unicast, "broadcast"=todos, null=resolver via OPA |
| `ttl` | int | Sí | Time-to-live, decrementa en cada hop. Si llega a 0, drop. |
| `trace_id` | UUID | Sí | ID de correlación para trazabilidad |

#### 7.2 Sección `meta` (Metadata para OPA y Sistema)

Usado por OPA para decisiones de capa 2, y por el router para broadcast filtrado.

```json
{
  "meta": {
    "type": "user",
    "target": "AI.soporte.l1.español",
    "priority": "high",
    "context": {
      "cliente_tier": "vip",
      "caso_id": "12345",
      "horario": "nocturno"
    }
  }
}
```

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `type` | string | Sí | Tipo de mensaje: `"user"`, `"system"`, `"admin"` |
| `msg` | string | Sí si type=system | Tipo de mensaje de sistema (ej: `"CONFIG_ANNOUNCE"`, `"TIME_SYNC"`) |
| `target` | string (nombre capa 2) | Condicional | Para OPA: perfil/capacidad requerida (si dst=null). Para broadcast: filtro de destinatarios (si dst="broadcast") |
| `priority` | string | No | Hint de prioridad para OPA |
| `context` | object | No | Datos adicionales para reglas OPA |
| `action` | string | No | Para mensajes admin: acción a ejecutar (ej: `"add_route"`, `"list_routes"`) |

**Uso de `target` según contexto:**

| `dst` | `meta.target` | Comportamiento |
|-------|---------------|----------------|
| `null` | nombre capa 2 | OPA resuelve destino usando target |
| `"broadcast"` | patrón capa 2 | Router filtra: solo entrega a nodos que matchean el patrón |
| `"broadcast"` | ausente | Router entrega a todos los nodos |
| UUID | - | Ignorado (unicast directo) |

**Ejemplo mensaje de sistema (broadcast filtrado):**

```json
{
  "routing": {
    "src": "uuid-sy-config",
    "dst": "broadcast",
    "ttl": 16,
    "trace_id": "uuid"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_ANNOUNCE",
    "target": "SY.config.*"
  },
  "payload": { ... }
}
```

La estructura de `context` es libre y depende de las policies definidas.

#### 7.3 Sección `payload` (Datos de Aplicación)

El contenido del mensaje. Ni el router ni OPA lo leen. Solo el nodo destino lo procesa.

```json
{
  "payload": {
    "type": "text",
    "content": "Hola, necesito ayuda con mi factura",
    "attachments": []
  }
}
```

La estructura interna es libre y la definen los nodos.

#### 7.4 Estructuras Rust del Protocolo

Las siguientes estructuras definen el formato de mensajes. Deben exponerse públicamente en la librería para uso de nodos SY y otros componentes.

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Mensaje completo del protocolo
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub routing: Routing,
    pub meta: Meta,
    #[serde(default)]
    pub payload: Value,
}

/// Header de routing (capa 1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routing {
    pub src: String,                    // UUID del nodo origen
    #[serde(deserialize_with = "deserialize_dst")]
    pub dst: Destination,               // UUID, "broadcast", o null
    pub ttl: u8,
    pub trace_id: String,
}

/// Destino del mensaje
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Destination {
    Unicast(String),                    // UUID específico
    Broadcast,                          // Literal "broadcast"
    Resolve,                            // null - resolver via OPA
}

/// Metadata (capa 2 y sistema)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    #[serde(rename = "type")]
    pub msg_type: String,               // "user", "system", "admin"
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,            // Para system: "CONFIG_ANNOUNCE", "TIME_SYNC", etc.
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,         // Para OPA o broadcast filter
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,         // Para admin: "add_route", "list_routes", etc.
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,       // Hint para OPA
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,         // Datos adicionales para OPA
}
```

**Exposición en librería:**

```rust
// lib.rs
pub mod protocol;  // Expone Message, Routing, Meta, Destination
pub mod socket;    // Expone funciones de framing (read_frame, write_frame)
```

Los módulos `protocol` y `socket` deben ser públicos para que `sy_config_routes` y otros nodos SY puedan reutilizar el código de serialización y framing.

### 8. Framing de Mensajes

**IMPORTANTE:** Todos los sockets del sistema (LAN y WAN) usan el mismo framing.

#### 8.1 Formato del Frame

```
┌──────────────┬─────────────────────┐
│ length (4B)  │ JSON message        │
│ big-endian   │ (length bytes)      │
└──────────────┴─────────────────────┘
```

- `length`: uint32 en big-endian, indica el tamaño del JSON en bytes
- `length` NO incluye los 4 bytes del header
- Máximo tamaño de mensaje: configurable (default 64KB para inline, ver blob_ref para mayores)

#### 8.2 Escritura de Frame

```
1. Serializar mensaje a JSON (UTF-8)
2. Calcular length = bytes del JSON
3. Escribir 4 bytes de length (big-endian)
4. Escribir bytes del JSON
```

#### 8.3 Lectura de Frame

```
1. Leer exactamente 4 bytes
2. Interpretar como uint32 big-endian → length
3. Leer exactamente length bytes
4. Parsear JSON
```

#### 8.4 Pseudocódigo

**Escribir (Rust):**
```rust
async fn write_frame(socket: &mut UnixStream, msg: &[u8]) -> io::Result<()> {
    let len = msg.len() as u32;
    socket.write_all(&len.to_be_bytes()).await?;
    socket.write_all(msg).await?;
    Ok(())
}
```

**Leer (Rust):**
```rust
async fn read_frame(socket: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    socket.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    
    let mut buf = vec![0u8; len];
    socket.read_exact(&mut buf).await?;
    Ok(buf)
}
```

**Escribir (Node.js):**
```javascript
function writeFrame(socket, msg) {
    const json = Buffer.from(JSON.stringify(msg), 'utf8');
    const header = Buffer.alloc(4);
    header.writeUInt32BE(json.length, 0);
    socket.write(Buffer.concat([header, json]));
}
```

**Leer (Node.js):**
```javascript
// Acumular en buffer, parsear cuando hay frame completo
let buffer = Buffer.alloc(0);

socket.on('data', (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    
    while (buffer.length >= 4) {
        const len = buffer.readUInt32BE(0);
        if (buffer.length < 4 + len) break; // frame incompleto
        
        const json = buffer.slice(4, 4 + len);
        buffer = buffer.slice(4 + len);
        
        const msg = JSON.parse(json.toString('utf8'));
        handleMessage(msg);
    }
});
```

### 9. Flujo de Resolución

```
Mensaje llega al router
        │
        ▼
  ┌─────────────────┐
  │ Leer frame      │
  │ (length+JSON)   │
  └─────────────────┘
        │
        ▼
  ┌─────────────────┐
  │ Leer routing.dst│
  └─────────────────┘
        │
        ▼
  ┌─────────────────┐     dst tiene valor
  │ ¿dst es null?   │ ───────────────────────► Forward directo a UUID
  └─────────────────┘
        │ dst es null
        ▼
  ┌─────────────────┐
  │ Pasar meta a OPA│
  └─────────────────┘
        │
        ▼
  ┌─────────────────┐
  │ OPA retorna     │
  │ nombre capa 2   │
  └─────────────────┘
        │
        ▼
  ┌─────────────────────────┐
  │ Router busca en tabla:  │
  │ ¿qué UUIDs tienen ese   │
  │ nombre?                 │
  └─────────────────────────┘
        │
        ▼
  ┌─────────────────┐
  │ Elegir uno      │
  │ (balanceo)      │
  └─────────────────┘
        │
        ▼
  ┌─────────────────┐
  │ Escribir frame  │
  │ al nodo destino │
  └─────────────────┘
```

### 10. Tipos de Mensaje por Tamaño

El sistema maneja mensajes de diferentes tamaños con estrategias distintas.

#### 10.1 Mensaje Inline (< 64KB)

JSON completo con payload incluido. Texto, comandos, metadata, respuestas LLM normales. Va directo por el socket.

```json
{
  "routing": { ... },
  "meta": { ... },
  "payload": {
    "type": "text",
    "content": "Mensaje de texto normal"
  }
}
```

#### 10.2 Mensaje por Referencia (> 64KB)

El payload es un archivo en disco. El JSON lleva el path. El nodo destino lo lee del filesystem compartido.

```json
{
  "routing": { ... },
  "meta": { ... },
  "payload": {
    "type": "blob_ref",
    "blob_id": "sha256:a1b2c3d4...",
    "size": 5242880,
    "sha256": "a1b2c3d4...",
    "mime": "image/png",
    "spool_day": "2025-01-15"
  }
}
```

**Flujo para mensaje grande:**

1. Nodo origen recibe/genera archivo grande
2. Nodo origen guarda archivo en `/var/spool/mesh/blobs/`
3. Nodo origen manda JSON con `blob_ref` al router
4. Router rutea el JSON (chiquito) al nodo destino
5. Nodo destino lee el archivo del path
6. Cleanup externo (proceso de mantenimiento) borra archivos viejos

**Storage de blobs:**

- Directorio compartido entre todos los nodos
- Montaje local o NFS según infraestructura
- Sin HTTP, acceso directo a disco
- Mantenimiento externo, los nodos no borran

#### 10.3 Mensaje de Sistema (< 1KB)

Mensajes de control de la red. Van por el mismo canal pero con `meta.type: "system"`. El router puede procesarlos él mismo en lugar de hacer forward.

### 11. Mensajes de Sistema

Nomenclatura basada en estándares de red existentes.

#### 11.1 Descubrimiento y Registro

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `HELLO` | Nodo | Router | Nodo anuncia existencia (UUID + nombre capa 2) |
| `ANNOUNCE` | Router | Nodo | Router confirma registro del nodo |
| `WITHDRAW` | Nodo | Router | Nodo anuncia shutdown limpio |

#### 11.2 Health y Diagnóstico (ICMP-like)

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `ECHO` | Cualquiera | Cualquiera | Ping |
| `ECHO_REPLY` | Cualquiera | Cualquiera | Pong |
| `UNREACHABLE` | Router | Nodo origen | Destino no existe |
| `TTL_EXCEEDED` | Router | Nodo origen | TTL llegó a 0 |
| `SOURCE_QUENCH` | Router/Nodo | Nodo origen | Backpressure, bajar velocidad |

#### 11.3 Routing (entre routers)

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `HELLO` | Router | Routers | Anunciar existencia |
| `LSA` | Router | Routers | Link State Advertisement, compartir nodos conectados |
| `SYNC_REQUEST` | Router | Router | Pedir tabla completa |
| `SYNC_REPLY` | Router | Router | Respuesta con tabla completa |

#### 11.4 Administrativos (unicast)

Mensajes dirigidos a un nodo o router específico.

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `ADM_SHUTDOWN` | Admin | Nodo/Router | Orden de apagar |
| `ADM_RELOAD` | Admin | Nodo/Router | Recargar configuración |
| `ADM_STATUS` | Admin | Nodo/Router | Solicitar estado |
| `ADM_STATUS_REPLY` | Nodo/Router | Admin | Respuesta con estado propio |
| `ADM_ROUTES` | Admin | Router | Solicitar tabla de ruteo |
| `ADM_ROUTES_REPLY` | Router | Admin | Respuesta con tabla de ruteo |
| `ADM_NODES` | Admin | Router | Solicitar nodos conectados |
| `ADM_NODES_REPLY` | Router | Admin | Respuesta con lista de nodos |

#### 11.5 Administrativos (broadcast)

Mensajes dirigidos a toda la red o grupo de routers.

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `BCAST_SHUTDOWN` | Admin | Todos | Shutdown general |
| `BCAST_RELOAD` | Admin | Todos | Recargar configuración en todos |
| `BCAST_CONFIG` | Admin | Todos | Distribuir nueva configuración |
| `BCAST_ANNOUNCE` | Router | Todos | Anunciar nodo nuevo en la red |
| `TIME_SYNC` | Router/SY.time | Todos | Broadcast de tiempo UTC sincronizado |

#### 11.6 Tiempo del Sistema

El tiempo sincronizado es fundamental para:
- Correlación de eventos y trazas
- Cálculo de TTLs y timeouts
- Timestamps del blob store (spool_day)
- Logs y auditoría

**Todo tiempo en el sistema es UTC.** La conversión a timezone local es responsabilidad de cada agente/aplicación.

**Mensaje TIME_SYNC:**

```json
{
  "routing": {
    "src": "uuid-router",
    "dst": null,
    "ttl": 1,
    "trace_id": "uuid"
  },
  "meta": {
    "type": "system",
    "msg": "TIME_SYNC"
  },
  "payload": {
    "utc": "2025-01-15T10:00:00.000Z",
    "epoch_ms": 1736935200000,
    "source": "SY.time.primary",
    "stratum": 1
  }
}
```

| Campo | Propósito |
|-------|-----------|
| `utc` | Tiempo UTC en ISO-8601 |
| `epoch_ms` | Milliseconds desde Unix epoch (para cálculos) |
| `source` | Quién emite el tiempo (router o nodo SY.time) |
| `stratum` | Nivel de confianza (1 = fuente primaria, 2+ = derivado) |

**Emisión del TIME_SYNC:**

- El router (o nodo SY.time dedicado) emite `TIME_SYNC` periódicamente
- Intervalo recomendado: cada 60 segundos
- TTL = 1 (no se propaga entre routers, cada router/isla tiene su fuente)
- Los nodos pueden usar este tiempo para sincronizar sus relojes internos

**Nota:** Los nodos no están obligados a sincronizar su reloj con TIME_SYNC, pero todos los timestamps en mensajes del protocolo DEBEN ser UTC.

#### 11.7 Estado

Cada componente define su propio mensaje de estado con información relevante a su tipo.

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `STATE` | Nodo/Router | Quien solicitó | Estado interno del componente |

**Contenido de STATE según tipo:**

- **Router:** Nodos conectados, tabla de ruteo, métricas de tráfico, uptime
- **Nodo AI:** Modelo cargado, requests en proceso, memoria usada
- **Nodo IO:** Canal conectado, mensajes en cola, última actividad
- **Nodo WF:** Workflows activos, ejecuciones pendientes
- **Nodo SY:** Estado del servicio específico (ej: SY.time reporta stratum, drift)

#### 11.8 Formato de Mensaje de Sistema

```json
{
  "routing": {
    "src": "uuid-origen",
    "dst": "uuid-destino",
    "ttl": 16,
    "trace_id": "uuid"
  },
  "meta": {
    "type": "system",
    "msg": "ECHO"
  },
  "payload": {
    "timestamp": "2025-01-15T10:00:00Z",
    "seq": 1
  }
}
```

El campo `meta.type: "system"` distingue mensajes de control de mensajes normales.

**Nota:** Todos los timestamps en mensajes del sistema DEBEN ser UTC (ISO-8601 con sufijo Z).

---

## Parte II-B: Librería de Nodos (node-lib)

Esta sección especifica la librería estándar que **todos los nodos** deben usar para comunicarse con el router. La librería encapsula toda la complejidad de conexión, protocolo, y manejo de identidad.

### 11. Objetivos de la Librería

#### 11.1 Problema que Resuelve

Sin librería estándar, cada nodo implementa:
- Generación de UUID
- Conexión al socket del router
- Framing de mensajes
- Handshake HELLO
- Reconexión automática
- Manejo de errores

Esto lleva a código duplicado, bugs inconsistentes, y dificultad de mantenimiento.

#### 11.2 Principio: Un Solo Lugar

**La librería es el único lugar donde se implementa la comunicación con el router.**

| Responsabilidad | Dónde se implementa |
|-----------------|---------------------|
| Generar UUID | Librería |
| Persistir UUID | Librería |
| Conexión socket | Librería |
| Framing (read/write) | Librería |
| Handshake HELLO | Librería |
| Reconexión automática | Librería |
| Enviar mensajes | Librería |
| Recibir mensajes | Librería |

**El nodo solo debe:**
- Configurar nombre L2 e island_id
- Llamar `connect()`
- Usar `send()` y `recv()`
- Implementar su lógica de negocio

### 12. Arquitectura de la Librería

```
┌─────────────────────────────────────────────────────────────────┐
│                         NODO (cualquier tipo)                   │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                    Lógica de Negocio                       │ │
│  │         (AI, WF, IO, SY.config.routes, etc.)               │ │
│  └──────────────────────────┬─────────────────────────────────┘ │
│                             │                                    │
│                             │ send() / recv()                   │
│                             ▼                                    │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                     NODE-LIB                                │ │
│  │                                                              │ │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │ │
│  │  │   Identity   │  │  Connection  │  │   Protocol   │      │ │
│  │  │              │  │              │  │              │      │ │
│  │  │ - gen UUID   │  │ - socket    │  │ - framing    │      │ │
│  │  │ - persist    │  │ - reconnect │  │ - HELLO      │      │ │
│  │  │ - load       │  │ - timeouts  │  │ - serialize  │      │ │
│  │  └──────────────┘  └──────────────┘  └──────────────┘      │ │
│  │                                                              │ │
│  └──────────────────────────┬─────────────────────────────────┘ │
│                             │                                    │
└─────────────────────────────┼────────────────────────────────────┘
                              │
                              ▼
                    Unix Socket al Router
```

### 13. Gestión de Identidad (UUID)

#### 13.1 Generación de UUID

**El nodo genera su propio UUID. El router NO asigna UUIDs.**

```rust
// En la librería
impl NodeIdentity {
    pub fn new_or_load(persistence_path: &Path) -> Self {
        if let Ok(uuid) = Self::load_from_file(persistence_path) {
            // UUID existente, reutilizar
            Self { uuid }
        } else {
            // Primera ejecución, generar nuevo
            let uuid = Uuid::new_v4();
            Self::save_to_file(persistence_path, &uuid);
            Self { uuid }
        }
    }
}
```

#### 13.2 Persistencia de UUID

El UUID se persiste en un archivo para sobrevivir reinicios:

```
/var/lib/json-router/nodes/<nombre>.uuid
```

Ejemplo:
```
/var/lib/json-router/nodes/SY.config.routes.produccion.uuid
```

**Contenido:** UUID en formato texto (36 caracteres).

#### 13.3 Flujo de Identidad

```
Primera ejecución:
  1. Librería busca archivo UUID → no existe
  2. Librería genera UUID v4
  3. Librería guarda en archivo
  4. Librería usa UUID para HELLO

Ejecuciones siguientes:
  1. Librería busca archivo UUID → existe
  2. Librería lee UUID del archivo
  3. Librería usa UUID para HELLO
```

### 14. Handshake: HELLO es el Estándar

#### 14.1 Protocolo Oficial

**HELLO es el único handshake para nodos.** No hay otro mecanismo.

| Mensaje | Dirección | Propósito |
|---------|-----------|-----------|
| `HELLO` | Nodo → Router | Registrar nodo |
| `ANNOUNCE` | Router → Nodo | Confirmar registro |

**Nota:** Cualquier código legacy usando QUERY/ANNOUNCE debe eliminarse (no deprecarse, eliminarse).

#### 14.2 Flujo del Handshake

```
NodeClient.connect():
    │
    ▼
┌─────────────────────────────────────────┐
│ 1. Cargar o generar UUID                │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 2. Conectar al socket del router        │
│    (timeout: 5s)                        │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 3. Enviar HELLO                         │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 4. Esperar ANNOUNCE (timeout: 5s)       │
│    - status: "registered" → OK          │
│    - status: "error" → log y continuar  │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 5. Retornar cliente conectado           │
└─────────────────────────────────────────┘
```

#### 14.3 Mensaje HELLO (enviado por librería)

```json
{
  "routing": {
    "src": "<uuid-nodo>",
    "dst": null,
    "ttl": 1,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "HELLO"
  },
  "payload": {
    "uuid": "<uuid-nodo>",
    "name": "SY.config.routes.produccion",
    "version": "1.0"
  }
}
```

**Campos obligatorios:**
- `uuid`: Generado/cargado por la librería (obligatorio)
- `name`: Nombre L2 del nodo (obligatorio)
- `version`: Versión del protocolo (obligatorio)

#### 14.4 Mensaje ANNOUNCE (respuesta del router)

**Éxito:**
```json
{
  "meta": {
    "type": "system",
    "msg": "ANNOUNCE"
  },
  "payload": {
    "uuid": "<uuid-nodo>",
    "name": "SY.config.routes.produccion",
    "status": "registered"
  }
}
```

**Error:**
```json
{
  "meta": {
    "type": "system",
    "msg": "ANNOUNCE"
  },
  "payload": {
    "status": "error",
    "error": "UUID_CONFLICT",
    "detail": "UUID already registered by another node"
  }
}
```

**Códigos de error en ANNOUNCE:**

| Código | Descripción | Acción de la librería |
|--------|-------------|----------------------|
| `UUID_CONFLICT` | UUID ya registrado | Log error, continuar |
| `INVALID_NAME` | Nombre L2 malformado | Log error, continuar |
| `VERSION_UNSUPPORTED` | Versión no soportada | Log error, continuar |

**Nota:** En caso de error, la librería loguea y continúa operando. No regenera UUID ni aborta.

#### 14.5 Compatibilidad de Versión

```
Política de versiones:
- version "1.0" es la versión actual
- Router acepta versiones menores o iguales
- Si version > soportada → ANNOUNCE con error VERSION_UNSUPPORTED
```

### 15. Lenguajes y Bindings

#### 15.1 Convención: Nodos SY en Rust (obligatorio)

**Todos los nodos SY DEBEN implementarse en Rust.** Esta es una convención del sistema, no una sugerencia.

| Tipo de nodo | Lenguaje | Binding |
|--------------|----------|---------|
| SY.* (system) | **Rust (obligatorio)** | `json_router::node_client` |
| AI.* (agentes) | Rust o Node.js | `json_router` o `json-router-node` |
| WF.* (workflows) | Rust o Node.js | `json_router` o `json-router-node` |
| IO.* (integraciones) | Rust o Node.js | `json_router` o `json-router-node` |

**Razones:**
- Los nodos SY son infraestructura crítica
- Comparten crate con el router (consistencia garantizada)
- Sin overhead de FFI o serialización entre lenguajes
- Un solo binario por nodo SY

#### 15.2 Arquitectura del Crate

El módulo `node_client` es parte del crate `json_router`, no una librería separada:

```
┌─────────────────────────────────────────────────────────────┐
│                    json_router (crate)                       │
│                                                              │
│  ┌────────────────┐  ┌────────────────┐  ┌──────────────┐  │
│  │     Router     │  │   node_client  │  │   protocol   │  │
│  │   (bin/main)   │  │    (módulo)    │  │   (módulo)   │  │
│  │                │  │                │  │              │  │
│  │  - routing     │  │  - NodeClient  │  │  - Message   │  │
│  │  - shm         │  │  - NodeConfig  │  │  - Routing   │  │
│  │  - wan         │  │  - NodeError   │  │  - Meta      │  │
│  └────────────────┘  └────────────────┘  └──────────────┘  │
│                              │                              │
└──────────────────────────────┼──────────────────────────────┘
                               │
              ┌────────────────┼────────────────┐
              │                │                │
              ▼                ▼                ▼
        SY.admin        SY.config.routes   SY.orchestrator
        (binario)          (binario)          (binario)
```

**Los nodos SY importan del mismo crate que el router:**

```rust
// Cargo.toml de sy_config_routes
[dependencies]
json_router = { path = "../json_router" }
```

#### 15.3 API Rust (nodos SY)

**Configuración:**

```rust
use json_router::node_client::{NodeClient, NodeConfig, NodeError};
use json_router::protocol::{Message, Meta, Routing};

pub struct NodeConfig {
    /// Nombre L2 del nodo (ej: "SY.config.routes.produccion")
    pub name: String,
    
    /// Island ID (ej: "produccion")
    pub island_id: String,
    
    /// Path al socket del router
    pub router_socket: PathBuf,
    
    /// Directorio para persistir UUID
    pub uuid_persistence_dir: PathBuf,
}
```

**Ejemplo completo de nodo SY:**

```rust
use json_router::node_client::{NodeClient, NodeConfig, NodeError};
use json_router::protocol::{Message, Meta, Routing};
use std::path::PathBuf;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), NodeError> {
    let island_id = std::env::var("ISLAND_ID").unwrap_or("sandbox".into());
    
    // Configuración
    let config = NodeConfig {
        name: format!("SY.config.routes.{}", island_id),
        island_id: island_id.clone(),
        router_socket: PathBuf::from("/var/run/json-router/router.sock"),
        uuid_persistence_dir: PathBuf::from("/var/lib/json-router/nodes/"),
    };
    
    // Conectar (UUID y HELLO automáticos)
    let client = NodeClient::connect(config).await?;
    
    println!("Conectado como {} (UUID: {})", client.name(), client.uuid());
    
    // Loop principal
    loop {
        // Recibir mensaje (bloquea)
        let msg = client.recv().await?;
        
        // Procesar según tipo
        match msg.meta.msg.as_deref() {
            Some("add_route") => {
                // Lógica de negocio...
                let response = Message {
                    routing: Routing {
                        src: Some(client.uuid().to_string()),
                        dst: msg.routing.src,
                        ttl: 16,
                        trace_id: msg.routing.trace_id,
                    },
                    meta: Meta {
                        msg_type: "system".into(),
                        msg: Some("add_route_response".into()),
                        ..Default::default()
                    },
                    payload: serde_json::json!({ "status": "ok" }),
                };
                client.send(response).await?;
            }
            _ => {
                // Mensaje desconocido
            }
        }
    }
}
```

**Métodos de NodeClient:**

```rust
impl NodeClient {
    /// Conectar al router (genera/carga UUID, envía HELLO)
    pub async fn connect(config: NodeConfig) -> Result<Self, NodeError>;
    
    /// UUID del nodo
    pub fn uuid(&self) -> &str;
    
    /// Nombre L2 del nodo
    pub fn name(&self) -> &str;
    
    /// Enviar mensaje (con reconexión automática)
    pub async fn send(&self, msg: Message) -> Result<(), NodeError>;
    
    /// Recibir mensaje (bloquea indefinidamente)
    pub async fn recv(&self) -> Result<Message, NodeError>;
    
    /// Recibir con timeout
    pub async fn recv_timeout(&self, timeout: Duration) -> Result<Message, NodeError>;
    
    /// Cerrar conexión limpiamente (envía WITHDRAW)
    pub async fn close(&self) -> Result<(), NodeError>;
}
```

#### 15.4 API Node.js (nodos AI, WF, IO)

Para nodos que no son SY, existe un binding Node.js como paquete npm separado:

```
┌─────────────────────────────────────┐
│      json-router-node (npm)         │
│                                     │
│  - Port de node_client a JS         │
│  - Misma API, mismo protocolo       │
│  - Para nodos AI, WF, IO            │
└─────────────────────────────────────┘
```

**Ejemplo:**

```javascript
const { NodeClient } = require('json-router-node');

const config = {
    name: 'AI.soporte.l1.español',
    islandId: 'produccion',
    routerSocket: '/var/run/json-router/router.sock',
    uuidPersistenceDir: '/var/lib/json-router/nodes/'
};

const client = await NodeClient.connect(config);

console.log(`Conectado como ${client.name} (UUID: ${client.uuid})`);

// Enviar
await client.send({
    routing: { dst: 'WF.tickets.crear', ttl: 16 },
    meta: { type: 'user' },
    payload: { texto: 'crear ticket' }
});

// Recibir
const msg = await client.recv();

// Recibir con timeout
const msg = await client.recv({ timeout: 30000 });

// Cerrar
await client.close();
```

**Nota:** El paquete `json-router-node` no existe aún. Se implementará cuando haya nodos AI/WF/IO. Los nodos SY usan Rust exclusivamente.

### 16. Parámetros y Límites

#### 16.1 Timeouts

| Parámetro | Valor | Descripción |
|-----------|-------|-------------|
| Connect timeout | 5s | Tiempo máximo para conectar al socket |
| HELLO timeout | 5s | Tiempo máximo esperando ANNOUNCE |
| Read timeout | 0 (infinito) | `recv()` bloquea indefinidamente |
| Read timeout configurable | N/A | Usar `recv_timeout(duration)` |

#### 16.2 Reconexión

| Parámetro | Valor | Descripción |
|-----------|-------|-------------|
| Backoff inicial | 100ms | Delay inicial entre reintentos |
| Backoff máximo | 30s | Delay máximo entre reintentos |
| Backoff factor | 2x | Multiplicador exponencial |
| Reintentos en send | 3 | Intentos antes de propagar error |

#### 16.3 Límites de Mensajes

| Parámetro | Valor | Descripción |
|-----------|-------|-------------|
| Max frame size | 64KB | Tamaño máximo de mensaje |
| Frame header | 4 bytes | uint32 big-endian (length) |

### 17. Modelo de Errores

#### 17.1 Tipos de Error

```rust
pub enum NodeError {
    /// No se pudo conectar al router
    ConnectionFailed { reason: String },
    
    /// Timeout en operación
    Timeout { operation: String },
    
    /// Conexión perdida (EOF o error de socket)
    Disconnected,
    
    /// Mensaje excede 64KB
    FrameTooLarge { size: usize },
    
    /// JSON inválido o estructura incorrecta
    InvalidMessage { detail: String },
    
    /// Error retornado por el router
    RouterError { code: String, detail: String },
    
    /// Error de I/O
    Io(std::io::Error),
}
```

#### 17.2 Errores del Router (en mensajes)

Cuando el router no puede entregar un mensaje, responde con error:

```json
{
  "meta": {
    "type": "system",
    "msg": "ERROR"
  },
  "payload": {
    "error": "UNREACHABLE",
    "detail": "No route to destination",
    "original_trace_id": "<trace-id-del-mensaje-original>"
  }
}
```

**Códigos de error del router:**

| Código | Descripción |
|--------|-------------|
| `UNREACHABLE` | No hay ruta al destino |
| `TTL_EXCEEDED` | TTL llegó a 0 |
| `NO_ROUTE` | Destino no encontrado en tabla |
| `QUEUE_FULL` | Cola del destino llena |
| `VPN_NOT_FOUND` | VPN ID referenciado no existe |
| `NO_ROUTE_TO_ISLAND` | No hay uplink hacia la isla destino de VPN |

La librería expone estos como `NodeError::RouterError { code, detail }`.

### 18. Reconexión Automática

La librería maneja reconexión de forma transparente:

```rust
impl NodeClient {
    pub async fn send(&self, msg: Message) -> Result<(), NodeError> {
        let mut attempts = 0;
        loop {
            match self.try_send(&msg).await {
                Ok(_) => return Ok(()),
                Err(e) if e.is_disconnected() && attempts < 3 => {
                    attempts += 1;
                    self.reconnect().await?;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }
    
    async fn reconnect(&self) -> Result<(), NodeError> {
        let mut delay = Duration::from_millis(100);
        loop {
            match self.try_connect().await {
                Ok(_) => {
                    self.send_hello().await?;
                    return Ok(());
                }
                Err(_) => {
                    sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        }
    }
}
```

**Detección de desconexión:**
- `send()` falla con error de socket → reconecta
- `recv()` recibe EOF → reconecta
- No hay heartbeat activo desde la librería

### 19. Módulos Expuestos del Crate

El crate `json_router` expone estos módulos públicos para uso de nodos SY:

```rust
// json_router/lib.rs

/// Estructuras de protocolo (Message, Routing, Meta)
pub mod protocol;

/// Framing de mensajes (read_frame, write_frame)
pub mod framing;

/// Cliente de nodo (NodeClient, NodeConfig, NodeError)
pub mod node_client;
```

**Uso típico en un nodo SY:**

```rust
// Cargo.toml
[package]
name = "sy_config_routes"
version = "0.1.0"
edition = "2021"

[dependencies]
json_router = { path = "../json_router" }
tokio = { version = "1", features = ["full"] }
serde_json = "1"
```

```rust
// main.rs
use json_router::node_client::{NodeClient, NodeConfig, NodeError};
use json_router::protocol::{Message, Meta, Routing};
```

### 20. Resumen de Responsabilidades

| Componente | Responsabilidad |
|------------|-----------------|
| **Crate json_router** | Router + node_client + protocol (todo en uno) |
| **node_client** | UUID, conexión, HELLO/ANNOUNCE, framing, reconexión, errores |
| **Nodo SY** | Configurar nombre/isla, lógica de negocio |
| **Router** | Aceptar HELLO, registrar, rutear, responder ANNOUNCE |

**Reglas:**
1. Nodos SY **DEBEN** ser Rust
2. Nodos SY **DEBEN** usar `json_router::node_client`
3. Si un nodo toca sockets, framing, o UUIDs directamente → refactorizar

---

## Parte III: Infraestructura de Red

### 12. Sockets y Detección de Link

#### 12.1 Tipo de Socket

El sistema usa **Unix domain sockets** con `SOCK_STREAM`:

- Stream de bytes con framing manual (length prefix)
- Detección automática de desconexión: si el nodo muere, el router recibe EOF o error
- Compatible con Node.js y cualquier lenguaje
- Framing consistente con uplink WAN (mismo código)

#### 12.2 Ubicación de Sockets

Los nodos crean sus sockets en un directorio conocido:

```
/var/run/mesh/nodes/<uuid>.sock
```

Los routers monitorean este directorio con `inotify` para detectar nodos nuevos sin polling.

#### 12.3 Modelo de Conexión

El nodo es pasivo (servidor), el router es activo (cliente):

**Nodo:**
```
socket(AF_UNIX, SOCK_STREAM) → bind() → listen(1) → accept()
```

**Router:**
```
inotify detecta socket nuevo → random backoff → connect()
```

El `listen(1)` con backlog 1 garantiza que solo un router puede conectar. El primero que hace `connect()` gana, los demás reciben error.

#### 12.4 Detección de Link Down

El socket señala automáticamente cuando el nodo muere:

| Evento | Qué pasa | Acción del router |
|--------|----------|-------------------|
| Nodo termina limpio | `close()` del socket | Router recibe EOF (read retorna 0) |
| Nodo crashea | Kernel cierra socket | Router recibe `ECONNRESET` o `EPIPE` |
| Nodo se cuelga | Timeout en operación | Router detecta por inactividad |

No hay heartbeat ni polling. El kernel notifica.

### 13. Shared Memory

La shared memory es el mecanismo de coordinación entre routers. Cada router tiene su propia región que solo él escribe, y todos los demás leen.

#### 13.1 Modelo: Una Región por Router

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           Isla "produccion"                              │
│                                                                          │
│  ┌──────────────┐     ┌──────────────┐     ┌──────────────┐             │
│  │  Router A    │     │  Router B    │     │  Router C    │             │
│  │              │     │              │     │              │             │
│  │ ┌──────────┐ │     │ ┌──────────┐ │     │ ┌──────────┐ │             │
│  │ │ SHM-A    │ │     │ │ SHM-A    │ │     │ │ SHM-A    │ │             │
│  │ │(WRITER)  │◄├────►├─┤(reader)  │◄├────►├─┤(reader)  │ │             │
│  │ └──────────┘ │     │ └──────────┘ │     │ └──────────┘ │             │
│  │ ┌──────────┐ │     │ ┌──────────┐ │     │ ┌──────────┐ │             │
│  │ │ SHM-B    │ │     │ │ SHM-B    │ │     │ │ SHM-B    │ │             │
│  │ │(reader)  │◄├────►├─┤(WRITER)  │◄├────►├─┤(reader)  │ │             │
│  │ └──────────┘ │     │ └──────────┘ │     │ └──────────┘ │             │
│  │ ┌──────────┐ │     │ ┌──────────┐ │     │ ┌──────────┐ │             │
│  │ │ SHM-C    │ │     │ │ SHM-C    │ │     │ │ SHM-C    │ │             │
│  │ │(reader)  │◄├────►├─┤(reader)  │◄├────►├─┤(WRITER)  │ │             │
│  │ └──────────┘ │     │ └──────────┘ │     │ └──────────┘ │             │
│  └──────────────┘     └──────────────┘     └──────────────┘             │
│                                                                          │
│  Cada router:                                                            │
│  - ESCRIBE solo en SU región (es el único writer)                       │
│  - LEE las regiones de TODOS los otros routers                          │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

**Principio clave:** Cada región tiene exactamente un writer (su dueño) y múltiples readers (todos los demás). Esto permite usar seqlock sin conflictos.

#### 13.2 Naming de Regiones

El nombre de la región incluye el UUID del router dueño:

```
/jsr-<router_uuid>
```

Ejemplo:
```
/jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

#### 13.3 Descubrimiento de Regiones via HELLO

Los routers descubren las regiones de sus peers mediante el mensaje HELLO. El HELLO incluye el nombre de la región shm:

```json
{
  "meta": {
    "type": "system",
    "msg": "HELLO"
  },
  "payload": {
    "router_id": "uuid-router-a",
    "island_id": "produccion",
    "shm_name": "/jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "...": "..."
  }
}
```

**Flujo de descubrimiento:**

```
1. Router A arranca, crea su región /jsr-<uuid-A>
2. Router A envía HELLO a la red (broadcast o a peers conocidos)
3. Router B recibe HELLO de A
4. Router B extrae shm_name del HELLO
5. Router B mapea /jsr-<uuid-A> en modo read-only
6. Router B ahora puede leer los nodos y rutas de A
7. Router B responde con su propio HELLO (incluye su shm_name)
8. Router A mapea /jsr-<uuid-B>
```

#### 13.4 Contenido de Cada Región

Cada router escribe en SU región únicamente:

| Dato | Descripción |
|------|-------------|
| **Sus nodos** | Nodos conectados directamente a este router |
| **Sus rutas CONNECTED** | Rutas automáticas por nodos locales |
| **Sus rutas STATIC** | Rutas configuradas en este router |
| **Su estado** | owner_pid, heartbeat, timestamps |

**NO escribe:**
- Rutas de otros routers (esas las lee de sus regiones)
- Nodos de otros routers (esas las lee de sus regiones)

#### 13.5 Vista Consolidada (FIB)

Cada router construye su FIB **en memoria local** (no en shm) leyendo todas las regiones:

```
FIB local = merge(
    mi_shm.nodes,
    mi_shm.routes,
    peer_A_shm.nodes,    // Se convierten en rutas con admin_distance = AD_LSA
    peer_A_shm.routes,
    peer_B_shm.nodes,
    peer_B_shm.routes,
    ...
)
```

Los nodos de otros routers se tratan como rutas aprendidas (LSA) con `admin_distance = AD_LSA`.

#### 13.6 Conceptos de Redes Aplicados

| Concepto | En redes IP | En este sistema |
|----------|-------------|-----------------|
| **RIB** (Routing Information Base) | Todas las rutas candidatas | Datos en todas las regiones shm |
| **FIB** (Forwarding Information Base) | Rutas ganadoras para forwarding rápido | Cache en memoria local del router |
| **Next-hop** | Router vecino al que enviar | `next_hop_router` en RouteEntry |
| **Admin distance** | Preferencia por origen de ruta | `admin_distance` (CONNECTED < STATIC < LSA) |
| **LPM** (Longest Prefix Match) | Matcheo más específico gana | `prefix_len` + `match_kind` |

#### 13.7 Layout de la Región

```
┌─────────────────────────────────────────────────────────────┐
│ ShmHeader (192 bytes)                                       │
│ - magic, version, owner info, seqlock, timestamps           │
├─────────────────────────────────────────────────────────────┤
│ NodeEntry[MAX_NODES] (1024 entries)                         │
│ - Nodos conectados a ESTE router                            │
├─────────────────────────────────────────────────────────────┤
│ RouteEntry[MAX_ROUTES] (256 entries)                        │
│ - Rutas CONNECTED y STATIC de ESTE router                   │
└─────────────────────────────────────────────────────────────┘
```

**Nota:** No hay `RouterEntry[]` en cada región. La lista de routers se construye dinámicamente a partir de los HELLOs recibidos y las regiones mapeadas.

#### 13.8 Sincronización: Seqlock

Cada región tiene exactamente un writer (su dueño), lo que permite usar seqlock sin conflictos:

- Header contiene `seq: AtomicU64` (explícitamente atómico)
- Sin locks, sin deadlocks, sin contención
- Usa memory orderings para coherencia entre procesos

**Protocolo del Writer (solo el router dueño):**
```rust
// Comenzar escritura (seq queda impar = "escribiendo")
header.seq.fetch_add(1, Ordering::Relaxed);

// Escribir datos en la región
// ...

// Finalizar escritura (seq queda par = "snapshot consistente")
atomic::fence(Ordering::Release);
header.seq.fetch_add(1, Ordering::Relaxed);
```

**Protocolo del Reader (todos los demás):**
```rust
loop {
    // Leer seq
    let s1 = header.seq.load(Ordering::Acquire);
    
    // Si impar, el writer está escribiendo, reintentar
    if s1 & 1 != 0 {
        std::hint::spin_loop();
        continue;
    }
    
    // Barrera antes de leer datos
    atomic::fence(Ordering::Acquire);
    
    // Copiar datos que necesita
    let data = /* copiar snapshot */;
    
    // Barrera después de leer datos
    atomic::fence(Ordering::Acquire);
    
    // Verificar que seq no cambió durante la lectura
    let s2 = header.seq.load(Ordering::Acquire);
    if s1 == s2 {
        break; // Snapshot consistente
    }
    // Si cambió, descartar y reintentar
}
```

**Importante:** `seq` debe estar alineado a 8 bytes para garantizar atomicidad en todas las arquitecturas.

#### 13.9 Inicialización y Recuperación

##### ¿Quién crea la región?

Cada router crea su propia región al arrancar.

##### Detección de región stale (crash anterior)

El header incluye campos para detectar si el dueño anterior crasheó:

```rust
pub struct ShmHeader {
    // ... otros campos ...
    pub owner_pid: u32,           // PID del proceso dueño
    pub owner_start_time: u64,    // Epoch ms cuando arrancó el proceso
    pub generation: u64,          // Incrementa en cada recreación de la región
    pub heartbeat: u64,           // Epoch ms, actualizado periódicamente
}
```

**Campos de detección:**
- `owner_pid`: PID del proceso dueño
- `owner_start_time`: Timestamp de inicio del proceso (evita falsos positivos por PID reciclado)
- `generation`: Contador que incrementa cada vez que se recrea la región
- `heartbeat`: Actualizado periódicamente por el dueño

**Al arrancar:**
```
1. Intentar abrir región existente con mi UUID
2. Si existe:
   a. Leer owner_pid y owner_start_time
   b. Verificar si el proceso está vivo:
      - kill(owner_pid, 0) == OK
      - Y owner_start_time coincide con el start_time real del proceso
   c. Si el proceso NO existe o start_time no coincide → región stale
   d. Si heartbeat > HEARTBEAT_STALE_MS → región stale
   e. Si región stale:
      - shm_unlink(shm_name)
      - Crear región nueva con generation++
   f. Si proceso vivo y heartbeat reciente → ERROR: otro proceso usa mi UUID
3. Si no existe:
   a. Crear región nueva
   b. Inicializar header con magic, version, mi pid, start_time, generation=1
```

**Obtener start_time del proceso:**

La verificación de `owner_start_time` previene falsos positivos cuando el PID se recicla rápidamente.

```rust
/// Obtiene el tiempo de inicio del proceso.
/// Retorna None si no se puede obtener (proceso no existe o OS no soportado).
fn get_process_start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // Leer /proc/<pid>/stat, campo 22 (starttime en ticks)
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
        let fields: Vec<&str> = stat.split_whitespace().collect();
        let start_ticks: u64 = fields.get(21)?.parse().ok()?;
        Some(start_ticks)
    }
    
    #[cfg(target_os = "macos")]
    {
        // En macOS se podría usar sysctl KERN_PROC o libproc
        // Por ahora, fallback a None (solo usa heartbeat)
        None
    }
    
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

fn is_owner_alive(header: &ShmHeader) -> bool {
    // 1. Verificar que el proceso existe
    if unsafe { libc::kill(header.owner_pid as i32, 0) } != 0 {
        return false;
    }
    
    // 2. Verificar start_time si está disponible (Linux)
    //    En otros OS, confiar solo en heartbeat
    match get_process_start_time(header.owner_pid) {
        Some(actual_start) => header.owner_start_time == actual_start,
        None => true,  // OS no soporta, asumir OK y confiar en heartbeat
    }
}
```

**Nota de portabilidad:**
- **Linux:** Verificación completa usando `/proc/<pid>/stat`
- **macOS:** Solo `kill(pid, 0)` + heartbeat (start_time no implementado en v1)
- **Otros OS:** Solo `kill(pid, 0)` + heartbeat

El heartbeat de 30s provee protección adicional en todos los casos.

##### Comportamiento de shm_unlink y readers mapeados

Cuando se hace `shm_unlink(shm_name)`:
- El nombre se elimina del filesystem `/dev/shm/`
- Los procesos que ya tenían la región mapeada **continúan viendo su snapshot** hasta que hagan `munmap()`
- La nueva región con el mismo nombre es independiente

**Implicaciones para readers (shm-watch, otros routers):**
- Deben verificar periódicamente `generation` o `heartbeat`
- Si detectan región stale o `generation` cambió, deben:
  1. `munmap()` la región vieja
  2. Re-abrir por nombre para obtener la nueva generación

##### Heartbeat

El router dueño actualiza `heartbeat` periódicamente (cada 5 segundos):

```rust
// En el loop principal del router
loop {
    // ... procesar mensajes ...
    
    if tiempo_desde_ultimo_heartbeat > HEARTBEAT_INTERVAL_MS {
        seqlock_write_begin();
        header.heartbeat = now_epoch_ms();
        seqlock_write_end();
    }
}
```

Los readers pueden verificar el heartbeat para detectar regiones de routers muertos.

##### Detección de peer muerto (desde reader)

Cuando un router lee la región de un peer:

```
1. Verificar heartbeat del peer
2. Si heartbeat > HEARTBEAT_STALE_MS (30s):
   a. Considerar peer como muerto
   b. Dejar de usar sus rutas en la FIB
   c. munmap() la región
   d. Intentar re-abrir periódicamente (puede haber reiniciado)
3. Si generation cambió desde la última lectura:
   a. munmap() y re-abrir para obtener nueva generación
```

#### 13.10 Constantes

```rust
// Identificación
pub const SHM_MAGIC: u32 = 0x4A535352;  // "JSSR" en ASCII
pub const SHM_VERSION: u32 = 1;
pub const SHM_NAME_PREFIX: &str = "/jsr-";

// Capacidades por router
pub const MAX_NODES: u32 = 1024;    // Nodos por router
pub const MAX_ROUTES: u32 = 256;    // Rutas por router
pub const MAX_PEERS: usize = 16;    // Máximo de peers mapeados

// Tamaños de campos
pub const NAME_MAX_LEN: usize = 256;
pub const ISLAND_ID_MAX_LEN: usize = 64;
pub const SHM_NAME_MAX_LEN: usize = 64;

// Flags de estado
pub const FLAG_ACTIVE: u16 = 0x0001;
pub const FLAG_DELETED: u16 = 0x0002;  // Marcado para reusar slot

// Match kinds (para rutas)
pub const MATCH_EXACT: u8 = 0;   // "AI.soporte.l1" matchea solo eso
pub const MATCH_PREFIX: u8 = 1;  // "AI.soporte" matchea "AI.soporte.*"
pub const MATCH_GLOB: u8 = 2;    // Patterns con wildcards

// Tipos de ruta (route_type)
pub const ROUTE_CONNECTED: u8 = 0;  // Nodo conectado directamente a este router
pub const ROUTE_STATIC: u8 = 1;     // Configurada manualmente
pub const ROUTE_LSA: u8 = 2;        // Aprendida de otro router (via shm o WAN)

// Admin distances (menor = preferido)
pub const AD_CONNECTED: u16 = 0;    // Siempre preferir nodos locales
pub const AD_STATIC: u16 = 1;       // Rutas manuales
pub const AD_LSA: u16 = 10;         // Rutas aprendidas de otros routers

// Identificadores de link
pub const LINK_LOCAL: u32 = 0;      // Nodo conectado localmente
// LINK 1..N = uplinks WAN a otros routers

// Timers de shared memory
pub const HEARTBEAT_INTERVAL_MS: u64 = 5_000;    // Actualizar heartbeat cada 5s
pub const HEARTBEAT_STALE_MS: u64 = 30_000;      // Heartbeat > 30s = stale
```

#### 13.11 Estructuras de Datos

Todas las estructuras usan `#[repr(C)]` para layout determinístico en memoria.

##### ShmHeader

```rust
use std::sync::atomic::AtomicU64;

#[repr(C)]
pub struct ShmHeader {
    // === IDENTIFICACIÓN (8 bytes) ===
    pub magic: u32,                 // SHM_MAGIC para validar
    pub version: u32,               // SHM_VERSION para compatibilidad
    
    // === OWNER (40 bytes) ===
    pub router_uuid: [u8; 16],      // UUID del router dueño
    pub owner_pid: u32,             // PID del proceso dueño
    pub _pad0: u32,                 // Padding para alineación
    pub owner_start_time: u64,      // Epoch ms cuando arrancó el proceso (anti PID-reuse)
    pub generation: u64,            // Incrementa en cada recreación de la región
    
    // === SEQLOCK (8 bytes, alineado) ===
    pub seq: AtomicU64,             // Impar=escribiendo, par=consistente
    
    // === CONTADORES (8 bytes) ===
    pub node_count: u32,            // Nodos activos en esta región
    pub route_count: u32,           // Rutas activas en esta región
    
    // === CAPACIDADES (8 bytes) ===
    pub node_max: u32,              // MAX_NODES
    pub route_max: u32,             // MAX_ROUTES
    
    // === TIMESTAMPS (24 bytes) ===
    pub created_at: u64,            // Epoch ms cuando se creó la región
    pub updated_at: u64,            // Epoch ms última modificación de datos
    pub heartbeat: u64,             // Epoch ms último heartbeat del owner
    
    // === ISLA (66 bytes) ===
    pub island_id: [u8; 64],        // "produccion", "staging", etc.
    pub island_id_len: u16,
    
    // === OPA POLICY (12 bytes) ===
    pub opa_policy_version: u64,    // Versión del policy cargado (0 = no cargado)
    pub opa_load_status: u8,        // 0=OK, 1=ERROR, 2=LOADING
    pub _pad1: [u8; 3],             // Padding
    
    // === RESERVADO (18 bytes para llegar a 192) ===
    pub _reserved: [u8; 18],
}
// Total: 192 bytes (alineado a 64)
```

**Campos clave:**
- `seq`: AtomicU64 para seqlock, debe estar alineado a 8 bytes
- `owner_start_time`: Previene falsos positivos cuando el PID se recicla
- `generation`: Permite a readers detectar que la región fue recreada

##### NodeEntry

Registro de un nodo conectado a este router.

```rust
#[repr(C)]
pub struct NodeEntry {
    // === IDENTIFICACIÓN (16 bytes) ===
    pub uuid: [u8; 16],          // UUID del nodo (capa 1)
    
    // === NOMBRE CAPA 2 (258 bytes) ===
    pub name: [u8; 256],         // "AI.soporte.l1.español" (UTF-8)
    pub name_len: u16,           // Longitud en bytes
    
    // === ESTADO (10 bytes) ===
    pub flags: u16,              // FLAG_ACTIVE, FLAG_DELETED
    pub connected_at: u64,       // Epoch ms cuando conectó
    
    // === RESERVADO (28 bytes) ===
    pub _reserved: [u8; 28],
}
// Total: 312 bytes
// Con 1024 entries: ~312 KB
```

**Nota:** `router_uuid` no está en NodeEntry porque todos los nodos en una región pertenecen al router dueño de esa región.

##### RouteEntry

Entrada en la tabla de ruteo. Solo rutas CONNECTED y STATIC de este router.

```rust
#[repr(C)]
pub struct RouteEntry {
    // === MATCHING (260 bytes) ===
    pub prefix: [u8; 256],       // Pattern a matchear: "AI.soporte.*" (UTF-8)
    pub prefix_len: u16,         // Longitud en bytes del prefix
    pub match_kind: u8,          // MATCH_EXACT, MATCH_PREFIX, MATCH_GLOB
    pub route_type: u8,          // ROUTE_CONNECTED, ROUTE_STATIC
    
    // === FORWARDING (24 bytes) ===
    pub next_hop_router: [u8; 16], // UUID del router next-hop (para STATIC via WAN)
    pub out_link: u32,           // 0=local, 1..N=uplink WAN id
    pub metric: u32,             // Costo de esta ruta
    
    // === SELECCIÓN (4 bytes) ===
    pub admin_distance: u16,     // AD_CONNECTED, AD_STATIC
    pub flags: u16,              // FLAG_ACTIVE, FLAG_DELETED
    
    // === METADATA (16 bytes) ===
    pub installed_at: u64,       // Epoch ms cuando se instaló
    pub _reserved: [u8; 8],
}
// Total: 304 bytes
// Con 256 entries: ~76 KB
```

**Campos de matching:**
- `prefix`: Pattern contra el que se matchea (UTF-8).
- `prefix_len`: Longitud en bytes, permite comparaciones rápidas.
- `match_kind`: EXACT, PREFIX, o GLOB.

**Campos de forwarding:**
- `next_hop_router`: Para rutas STATIC que van por WAN, el router vecino.
- `out_link`: 0 = nodo local, 1..N = uplink WAN.
- `metric`: Costo para selección entre rutas equivalentes.

#### 13.12 Encoding de Strings

**UTF-8** para `name`, `prefix`, e `island_id`.

- Los nombres pueden contener caracteres Unicode: "AI.soporte.español", "日本語"
- `name_len`, `prefix_len`, `island_id_len` son longitud en **bytes**, no caracteres
- La validación de caracteres permitidos aplica a **code points**:

```rust
fn validate_name(name: &str) -> bool {
    name.chars().all(|c| {
        c.is_alphanumeric() || c == '_' || c == '-' || c == '.'
    })
}
```

#### 13.13 Qué vive en Shared Memory (por región)

| Dato | Quién escribe | Descripción |
|------|---------------|-------------|
| Header | Router dueño | Identificación, seqlock, heartbeat |
| Nodos | Router dueño | Nodos conectados a este router |
| Rutas CONNECTED | Router dueño | Una por cada nodo local |
| Rutas STATIC | Router dueño | Configuradas manualmente |

#### 13.14 Qué NO vive en Shared Memory

| Dato | Por qué no | Dónde vive |
|------|------------|------------|
| Lista de routers peers | Dinámica, via HELLOs | Memoria local del router |
| FIB compilada | Derivada de todas las regiones | Memoria local del router |
| Estado del socket | El socket lo indica | Local en cada router |
| Rutas de otros routers | Cada uno las tiene en su región | Se leen, no se copian |

#### 13.15 Construcción de la FIB

El router construye su FIB en memoria local leyendo todas las regiones.

##### Tabla peer_links

El router mantiene una tabla local (en memoria, no en shm) que mapea cada peer a su link:

```rust
// Tabla local del router
peer_links: HashMap<Uuid, u32>  // peer_uuid → link_id
```

**Cómo se llena:**
- Al recibir un HELLO por un uplink, el router registra por qué conexión llegó
- `peer_links[peer_uuid] = link_id` del uplink por donde llegó el HELLO
- Los `link_id` van de 1..N para uplinks (WAN o inter-router local)

**Resolución de uplink_to_peer:**
```rust
fn get_link_for_peer(peer_uuid: &Uuid) -> Option<u32> {
    peer_links.get(peer_uuid).copied()
}
```

##### Algoritmo de construcción

```
FIB = []

// 1. Agregar mis rutas CONNECTED (nodos locales, prioridad máxima)
for route in mi_region.routes:
    if route.flags & FLAG_ACTIVE:
        FIB.add(route)
        // Estas rutas tienen out_link = LINK_LOCAL (0)
        // porque los nodos están conectados a MIS sockets

// 2. Agregar nodos de peers como rutas LSA
for peer_region in peers_mapeados:
    if peer_region.heartbeat es reciente:
        let link_id = peer_links.get(peer_region.router_uuid)
        if link_id.is_none():
            continue  // No tenemos uplink al peer, ignorar
        
        for node in peer_region.nodes:
            if node.flags & FLAG_ACTIVE:
                // Crear ruta hacia el ROUTER peer, no hacia el nodo
                FIB.add(RouteEntry {
                    prefix: node.name,
                    prefix_len: node.name_len,
                    match_kind: MATCH_EXACT,
                    next_hop_router: peer_region.router_uuid,
                    out_link: link_id,        // Uplink al peer router (1..N)
                    admin_distance: AD_LSA,
                    route_type: ROUTE_LSA,
                    metric: 1,
                    ...
                })

// 3. Agregar rutas STATIC de peers (propagación)
for peer_region in peers_mapeados:
    if peer_region.heartbeat es reciente:
        let link_id = peer_links.get(peer_region.router_uuid)
        if link_id.is_none():
            continue
        
        for route in peer_region.routes:
            if route.flags & FLAG_ACTIVE && route.route_type == ROUTE_STATIC:
                // Propagar con admin_distance de LSA
                FIB.add(RouteEntry {
                    ...route,
                    admin_distance: AD_LSA,
                    out_link: link_id,
                    next_hop_router: peer_region.router_uuid,
                })

// 4. Ordenar FIB para lookup rápido
FIB.sort_by(|a, b| {
    // LPM: más específico primero (prefix_len desc)
    // Luego admin_distance menor
    // Luego metric menor
})
```

##### Semántica de LINK_LOCAL

**LINK_LOCAL (0) significa:** El nodo está conectado directamente a MIS sockets. Solo aplica a rutas CONNECTED en MI región.

**Para nodos de otros routers (peers):** Siempre se usa un `link_id` de uplink (1..N), incluso si el peer está en el mismo host. El mensaje debe ir al router dueño del nodo, que lo entregará por su socket.

```
┌─────────────────────────────────────────────────────────────┐
│                      Mismo host                              │
│                                                              │
│  Router A                          Router B                  │
│  ┌──────────┐    uplink 1         ┌──────────┐              │
│  │ SHM-A    │◄───────────────────►│ SHM-B    │              │
│  │          │    (Unix socket     │          │              │
│  └──────────┘     o TCP local)    └──────────┘              │
│       │                                 │                    │
│       │ LINK_LOCAL                      │ LINK_LOCAL         │
│       ▼                                 ▼                    │
│  [Nodo 1]                          [Nodo 2]                  │
│                                                              │
│  Si Router A quiere enviar a Nodo 2:                        │
│  - Lee de SHM-B que Nodo 2 existe                           │
│  - out_link = 1 (uplink a Router B), NO LINK_LOCAL          │
│  - Envía por uplink 1 a Router B                            │
│  - Router B recibe, out_link = LINK_LOCAL, entrega a Nodo 2 │
└─────────────────────────────────────────────────────────────┘
```

##### Conectividad entre Routers

**Intra-isla (mismo host, misma /dev/shm):**
- Conexión automática por Unix socket
- Leen nodos y rutas de las regiones shm de los peers
- No requiere configuración de uplinks
- Ver sección 5.1 para detalles

**Inter-isla (hosts distintos, TCP/WAN):**
- Conexión TCP configurada explícitamente
- Propagan nodos y rutas via mensajes LSA/SYNC (la shm no es accesible entre hosts)
- Requiere `wan.listen` y `wan.uplinks` en config

En ambos casos, el forwarding a nodos de un peer **siempre** va por el uplink al router peer.

#### 13.16 Algoritmo de Selección de Ruta

Cuando el router necesita encontrar destino para un mensaje:

```
1. Buscar en FIB todas las rutas que matchean el destino
2. Filtrar solo FLAG_ACTIVE
3. Ordenar por:
   a. prefix_len descendente (LPM: más específico primero)
   b. admin_distance ascendente (menor = preferido)
   c. metric ascendente (menor costo)
4. Tomar la primera (ganadora)
5. Según action de la ruta:
   - ACTION_FORWARD (0): continuar a paso 6
   - ACTION_DROP (1): descartar mensaje, fin
   - ACTION_VPN (2): resolver VPN (ver 13.16.1)
6. Si out_link == LINK_LOCAL:
   → Nodo conectado a MI socket, entregar directamente
7. Si out_link > 0:
   → Enviar por ese uplink al next_hop_router
```

#### 13.16.1 Resolución de VPN

Cuando una ruta tiene `action == ACTION_VPN`, el router debe resolver el túnel VPN para determinar el uplink de salida:

```
Entrada: RouteEntry con action = ACTION_VPN, vpn_id = X

1. Buscar VPNEntry en config SHM donde vpn_id == X
2. Si no existe VPNEntry:
   → Responder UNREACHABLE al origen
   → Log error: "VPN not found: vpn_id=X"
   → Fin

3. Leer remote_island de VPNEntry
4. Buscar uplink activo hacia remote_island:
   - Escanear uplinks WAN conectados
   - Buscar uno cuyo peer.island_id == remote_island

5. Si no hay uplink directo hacia remote_island:
   → Buscar ruta transitiva via routing inter-isla (ver 19.14)
   → Si no hay ruta transitiva: UNREACHABLE

6. Enviar mensaje por el uplink encontrado
```

**Diagrama de resolución:**

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│ RouteEntry  │     │  VPNEntry   │     │   Uplinks   │
│             │     │             │     │             │
│ vpn_id: 5 ──┼────►│ vpn_id: 5   │     │ link_1: B   │
│             │     │ remote: "B" ├────►│ link_2: C   │
└─────────────┘     └─────────────┘     └─────────────┘
                                              │
                                              ▼
                                        Enviar por link_1
```

#### 13.16.1.1 Mapeo island_id → uplink

El router mantiene una tabla de uplinks activos con su `island_id` asociado:

```rust
struct UplinkEntry {
    link_id: u32,                    // ID interno del link
    peer_router_uuid: [u8; 16],      // UUID del router peer
    peer_island_id: String,          // Island del peer (de HELLO WAN)
    socket: TcpStream,               // Conexión activa
    state: UplinkState,              // CONNECTING, CONNECTED, DEAD
}
```

**El `peer_island_id` se obtiene del HELLO WAN:**

```
1. Router A conecta a Router B por uplink WAN
2. B envía HELLO con island_id = "staging"
3. A guarda: uplinks[link_id] = { peer_island_id: "staging", ... }
4. Cuando A necesita enviar a isla "staging", busca en uplinks
```

**Pseudocódigo de búsqueda:**

```rust
fn find_uplink_to_island(&self, island_id: &str) -> Option<&UplinkEntry> {
    self.uplinks
        .iter()
        .find(|u| u.peer_island_id == island_id && u.state == UplinkState::CONNECTED)
}
```

**Pseudocódigo completo de resolve_vpn:**

```rust
fn resolve_vpn(route: &RouteEntry, config_shm: &ConfigRegion) -> Result<LinkId, RouterError> {
    // 1. Buscar VPN por ID
    let vpn = config_shm.vpns
        .iter()
        .find(|v| v.vpn_id == route.vpn_id && v.flags & FLAG_ACTIVE != 0)
        .ok_or(RouterError::VpnNotFound(route.vpn_id))?;
    
    // 2. Obtener isla destino
    let remote_island = vpn.remote_island_as_str();
    
    // 3. Buscar uplink hacia esa isla
    let uplink = self.find_uplink_to_island(remote_island)
        .ok_or(RouterError::NoRouteToIsland(remote_island.to_string()))?;
    
    Ok(uplink.link_id)
}
```

**Tabla de resolución de acciones:**

| action | Valor | Resolución |
|--------|-------|------------|
| ACTION_FORWARD | 0 | Usar `out_link` directamente de RouteEntry |
| ACTION_DROP | 1 | Descartar mensaje silenciosamente |
| ACTION_VPN | 2 | `vpn_id` → `VPNEntry.remote_island` → uplink por island |

#### 13.16.2 Forwarding Completo

Flujo completo de forwarding de un mensaje:

```
Mensaje entrante con dst = "AI.soporte.l1"
    │
    ▼
┌─────────────────────────────────────────┐
│ 1. Buscar ruta en FIB (LPM)             │
│    Resultado: RouteEntry                │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 2. Evaluar action                       │
│    - FORWARD → paso 3                   │
│    - DROP → descartar, fin              │
│    - VPN → resolver VPN, luego paso 3   │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 3. Determinar link de salida            │
│    - out_link == 0 → entrega local      │
│    - out_link > 0 → uplink WAN          │
│    - VPN resuelto → uplink de VPN       │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 4. Decrementar TTL                      │
│    - TTL == 0 → TTL_EXCEEDED, fin       │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 5. Enviar por link determinado          │
│    - Local: write al socket del nodo    │
│    - WAN: write al socket TCP del uplink│
└─────────────────────────────────────────┘
```

#### 13.19 Operaciones de Escritura (solo el router dueño)

**Registrar nodo (cuando conecta):**
```
1. Buscar slot libre en NodeEntry[] (flags == 0 o FLAG_DELETED)
2. seqlock_begin_write()  // seq++
3. Escribir uuid, name, name_len, connected_at
4. flags = FLAG_ACTIVE
5. node_count++
6. updated_at = now()
7. seqlock_end_write()    // seq++
8. Crear RouteEntry CONNECTED para el nodo
```

**Desregistrar nodo (cuando desconecta):**
```
1. Buscar NodeEntry por uuid
2. seqlock_begin_write()
3. flags = FLAG_DELETED
4. node_count--
5. Marcar RouteEntry asociada como FLAG_DELETED
6. route_count--
7. updated_at = now()
8. seqlock_end_write()
```

**Actualizar heartbeat:**
```
1. seqlock_begin_write()
2. heartbeat = now()
3. seqlock_end_write()
```

#### 13.20 Cleanup de Slots FLAG_DELETED

**Política:** No compactar en tiempo real. Reusar slots.

```rust
fn find_free_slot<T>(entries: &[T]) -> Option<usize> 
where T: HasFlags 
{
    entries.iter().position(|e| {
        let flags = e.flags();
        flags == 0 || (flags & FLAG_DELETED) != 0
    })
}
```

Si no hay slots libres (`node_count == node_max`), rechazar nuevas conexiones con error.

**Compactación offline (opcional):** Una herramienta puede compactar cuando el router está apagado.

#### 13.19 Tamaño Total de la Región

```
ShmHeader:     192 bytes
NodeEntry[]:   312 * 1024 = 319,488 bytes (~312 KB)
RouteEntry[]:  304 * 256  =  77,824 bytes (~76 KB)
─────────────────────────────────────────────────────
Total:                      397,504 bytes (~388 KB)
```

Redondeado a **512 KB** para tener margen.

### 14. Ciclo de Vida de Nodos

#### 14.1 Registro (Nodo Nuevo)

```
1. Nodo arranca
2. Librería carga o genera UUID
3. Librería conecta al socket del router
4. Librería envía HELLO (uuid, name, version)
5. Router valida y registra nodo en shared memory
6. Router responde ANNOUNCE (status: registered)
7. Router crea RouteEntry tipo CONNECTED
8. Router envía LSA a otros routers
```

#### 14.2 Operación Normal

```
Nodo ←──socket──→ Router ←──shared memory──→ Otros Routers
         │
         ▼
    Mensajes JSON (con framing)
```

#### 14.3 Desconexión Limpia

```
1. Nodo decide apagar
2. Nodo envía WITHDRAW al router
3. Nodo hace close() del socket
4. Router detecta cierre (EOF)
5. Router elimina nodo de shared memory
6. Router elimina RouteEntry asociada
7. Router envía actualización LSA a otros routers
```

#### 14.4 Desconexión por Falla

```
1. Nodo crashea o se cuelga
2. Kernel cierra socket (o router detecta timeout)
3. Router detecta error en próximo read/write
4. Router elimina nodo de shared memory
5. Router elimina RouteEntry asociada
6. Router envía UNREACHABLE a nodos que tenían mensajes pendientes
7. Router envía actualización LSA a otros routers
```

#### 14.5 Reconexión

```
1. Nodo se recupera, arranca de nuevo
2. Librería carga UUID persistido (mismo UUID)
3. Librería conecta al router
4. Flujo normal de HELLO/ANNOUNCE
```

### 15. Coordinación entre Routers

#### 15.1 Descubrimiento de Routers

El descubrimiento de routers depende del tipo de conexión:

**Intra-isla (mismo host, fabric automático):**
- Los routers escanean `/dev/shm/jsr-*` para descubrir regiones de peers
- No requiere HELLO para descubrimiento
- La conexión fabric (Unix socket) se establece automáticamente

**Inter-isla (WAN, TCP configurado):**
- Los uplinks WAN se configuran explícitamente en `config.yaml`
- Al conectar, se intercambian mensajes HELLO
- HELLO incluye `shm_name` para que el peer sepa qué región leer (aunque en WAN no puede leerla directamente, sirve para identificación)

```
Intra-isla:  Scan /dev/shm/jsr-* → Conectar fabric → Leer shm peer
Inter-isla:  Config uplink → TCP connect → HELLO handshake → LSA/SYNC
```

#### 15.2 Random Backoff para Evitar Colisiones

Cuando múltiples routers detectan un socket nuevo simultáneamente:

```
Router recibe inotify "socket nuevo"
    │
    ▼
Espera random(0, connect_backoff_max) ms
    │
    ▼
Intenta connect()
    │
    ├─ Éxito → Registra en shared memory
    │
    └─ Falla (ECONNREFUSED) → Otro router ganó, ignorar
```

Este mecanismo está probado en redes (CSMA/CD en Ethernet, CSMA/CA en WiFi).

#### 15.3 Sincronización de Tablas

```
Router nuevo arranca
    │
    ▼
Envía HELLO a otros routers
    │
    ▼
Envía SYNC_REQUEST
    │
    ▼
Recibe SYNC_REPLY con tabla completa
    │
    ▼
Actualiza su vista local
    │
    ▼
Comienza a enviar HELLO periódico
```

### 16. Timers del Sistema

Basados en estándares de OSPF y BGP.

#### 16.1 Timers de Mensaje

| Timer | Default | Configurable | Propósito |
|-------|---------|--------------|-----------|
| **TTL** | 16 hops | Sí | Máximo saltos antes de drop |
| **Message Timeout** | 30s | Sí | Tiempo máximo esperando respuesta |
| **Retransmit Interval** | 5s | Sí | Reenviar si no hay ACK |

#### 16.2 Timers de Router

| Timer | Default | Configurable | Propósito |
|-------|---------|--------------|-----------|
| **Heartbeat Interval** | 5s | Sí | Actualizar heartbeat en SHM (CRÍTICO) |
| **Heartbeat Stale** | 30s | Sí | Considerar peer muerto si heartbeat > este valor |
| **Hello Interval** | 10s | Sí | Cada cuánto anunciar existencia a otros routers |
| **Dead Interval** | 40s (4x hello) | Sí | Sin hello = router marcado como caído |
| **Route Refresh** | 300s (5min) | Sí | Refrescar tabla de rutas entre routers |
| **Connect Backoff Max** | 100ms | Sí | Máximo random delay antes de conectar a socket nuevo |
| **Time Sync Interval** | 60s | Sí | Cada cuánto emitir TIME_SYNC broadcast |

**IMPORTANTE:** El heartbeat en SHM es crítico para el funcionamiento del fabric intra-isla. Si el router no actualiza su heartbeat, los peers lo considerarán muerto después de 30s y dejarán de usar sus rutas. Esto causa pérdida de conectividad entre routers de la misma isla.

#### 16.3 Timers de Nodo

| Timer | Default | Configurable | Propósito |
|-------|---------|--------------|-----------|
| **Reconnect Interval** | 5s | Sí | Tiempo entre reintentos de conexión |
| **Reconnect Max Attempts** | 10 | Sí | Intentos antes de desistir |

#### 16.4 Configuración de Timers

Los timers se configuran en el archivo de configuración del router:

```toml
[timers]
ttl_default = 16
message_timeout_ms = 30000
retransmit_interval_ms = 5000
heartbeat_interval_ms = 5000      # CRÍTICO: actualizar SHM
heartbeat_stale_ms = 30000        # CRÍTICO: considerar peer muerto
hello_interval_ms = 10000
dead_interval_ms = 40000
route_refresh_ms = 300000
connect_backoff_max_ms = 100
time_sync_interval_ms = 60000
```

### 17. Semántica de Comunicación

#### 17.1 Modelo: Request/Response End-to-End (tipo TCP)

El sistema implementa comunicación request/response con confirmación end-to-end. La respuesta del nodo destino ES la confirmación de recepción, igual que en TCP donde el ACK viene del extremo final, no de intermediarios.

```
Nodo A ──REQ──► Router ──────► Nodo B
                               │
                               ▼
                          Nodo B recibe
                               │
                               ▼
Nodo A ◄──RESP── Router ◄───── Nodo B responde
```

**No hay ACK intermedio del router.** El router es transparente: mueve bytes, no confirma recepción.

#### 17.2 Principios

1. **El router es un medio de transporte, no un broker.**
   - No almacena mensajes con fines de durabilidad
   - No implementa reintentos automáticos
   - No confirma recepción en nombre del destino

2. **La respuesta es el único ACK.**
   - En request/response, la respuesta confirma que el destino recibió Y procesó
   - Si no hay respuesta dentro del timeout, la sesión se considera fallida

3. **Retry y contingencia son decisiones de negocio (nodos).**
   - Reintentar, reroutear, degradar, escalar son políticas de aplicación
   - El router no intenta "resolver" fallas mediante reenvíos automáticos
   - Los mensajes pueden tener efectos o costos; el router no puede decidir reenviar

#### 17.3 Contrato del Protocolo

| Campo | Obligatorio | Propósito |
|-------|-------------|-----------|
| `msg_id` | Sí (en REQ) | Identificador único de la solicitud |
| `msg_id` | Sí (en RESP) | Correlation ID para cerrar la sesión |
| `deadline` | Definido por emisor | Timeout de la solicitud |

El emisor controla el timeout, no el router. El router no trackea sesiones "abiertas".

#### 17.4 Señales del Router

Cuando el router no puede aceptar o enrutar, responde inmediatamente con error:

| Señal | Origen | Significado |
|-------|--------|-------------|
| `UNREACHABLE` | Router | No existe ruta, no hay nodos con ese rol, destino no existe |
| `SOURCE_QUENCH` | Router o Nodo | Cola llena, backpressure, bajar velocidad |
| `TTL_EXCEEDED` | Router | Mensaje excedió límite de hops |

Estas señales NO implican que el mensaje fue procesado. Solo indican que el router no pudo completar el forwarding.

#### 17.5 Estados Finales de Sesión

Una sesión request/response termina en uno de estos estados:

| Estado | Qué pasó | Acción típica |
|--------|----------|---------------|
| `SUCCESS` | Recibió RESP con resultado válido | Continuar |
| `REMOTE_ERROR` | Recibió RESP con error de aplicación | Manejar error |
| `ROUTER_ERROR` | Recibió señal del router (UNREACHABLE, etc.) | Retry o abortar |
| `TIMEOUT` | No recibió respuesta antes del deadline | Ambiguo, decidir retry |

#### 17.6 Ambigüedad del Timeout

El timeout es el caso ambiguo. El emisor no puede saber:

| Escenario | Qué pasó realmente | Qué ve el emisor |
|-----------|-------------------|------------------|
| Mensaje nunca llegó a B | Perdido en tránsito | Timeout |
| B recibió, crasheó antes de responder | Puede haber procesado | Timeout |
| B procesó, respuesta se perdió | Procesado pero sin confirmación | Timeout |

En todos los casos el emisor ve lo mismo: timeout. Por eso **el retry es decisión de negocio**: si el emisor reintenta, el destino podría recibir el mensaje dos veces. La aplicación debe manejar idempotencia o aceptar duplicados.

#### 17.7 Comparación con TCP

| Aspecto | TCP | Este sistema |
|---------|-----|--------------|
| ACK de quién | Del extremo final | Del extremo final (RESP) |
| ACK intermedio | No (routers IP no confirman) | No (router no confirma) |
| Retransmisión | Automática por el stack | Manual, decisión de negocio |
| Orden | Garantizado | Garantizado (framing sobre SOCK_STREAM) |
| Detección de pérdida | Timeout + ACK duplicados | Timeout |

La diferencia clave: TCP retransmite automáticamente porque son bytes sin semántica. Acá son mensajes con posibles efectos secundarios, entonces el retry lo decide la aplicación.

### 18. Blob Store

#### 18.1 Objetivo

Transportar payloads grandes sin fragmentación ni streaming. El payload se consolida completamente en origen antes de ser referenciado y enviado por JSON.

El blob store es un filesystem compartido entre todos los nodos. La infraestructura resuelve el acceso (NFS, mount compartido, etc.).

#### 18.2 Umbral Inline vs Referencia

| Tamaño | Transporte |
|--------|------------|
| `<= MAX_INLINE_BYTES` (64KB) | Inline en JSON |
| `> MAX_INLINE_BYTES` | Referencia `blob_ref` |

#### 18.3 Estructura de Directorios

Root configurable: `BLOB_ROOT` (ej. `/var/lib/router/blobs`)

Organización por día UTC:

```
${BLOB_ROOT}/YYYY/MM/DD/tmp/        # Escrituras incompletas
${BLOB_ROOT}/YYYY/MM/DD/objects/    # Blobs sellados (válidos)
${BLOB_ROOT}/YYYY/MM/DD/pins/       # Locks de uso (evita borrado)
${BLOB_ROOT}/YYYY/MM/DD/manifest.log # Opcional, auditoría
```

Sharding recomendado para alto volumen (>10K blobs/día):

```
${BLOB_ROOT}/YYYY/MM/DD/objects/aa/bb/<blob_id>.blob
```

Los primeros caracteres del sha256 dan buena distribución.

#### 18.4 Identidad del Blob

| Campo | Obligatorio | Descripción |
|-------|-------------|-------------|
| `blob_id` | Sí | Opaco, recomendado: `sha256:<hash>` (content-addressed) |
| `sha256` | Sí | Hash del contenido para validación |
| `size` | Sí | Tamaño en bytes |
| `mime` | Sí | Tipo de contenido para el receptor |
| `spool_day` | Sí | Fecha UTC de creación (YYYY-MM-DD) |

Content-addressed significa: si dos nodos mandan el mismo archivo, es el mismo blob. Deduplicación automática.

#### 18.5 Escritura Atómica (Sellado)

Un blob es válido solo si existe en `objects/`. Nunca leer desde `tmp/`.

```
1. Escribir a ${...}/tmp/<blob_id>.<pid>.<rand>.tmp
2. fsync(file) + fsync(dir)
3. rename() atómico a ${...}/objects/<blob_id>.blob
```

El `rename()` es atómico en POSIX. Si existe en `objects/`, está completo.

#### 18.6 Mensaje blob_ref

El router transporta solo el JSON, no el blob.

```json
{
  "routing": { ... },
  "meta": { ... },
  "payload": {
    "type": "blob_ref",
    "blob_id": "sha256:a1b2c3d4...",
    "size": 5242880,
    "sha256": "a1b2c3d4...",
    "mime": "image/png",
    "spool_day": "2025-01-15"
  }
}
```

Los nodos no envían paths. El path se deriva localmente:

```
${BLOB_ROOT}/${spool_day}/objects/${blob_id}.blob
```

#### 18.7 Lectura y Validación

El receptor:

1. Construye path desde `BLOB_ROOT` + `spool_day` + `blob_id`
2. Si no existe, fallback ±24 horas (orden: `spool_day` → `-1 día` → `+1 día`)
3. Si no existe en ninguno, error `BLOB_NOT_FOUND`
4. Lee archivo, valida `size` + `sha256`
5. Si no valida, error (blob corrupto)
6. Solo si valida, procesa el payload completo

El fallback de ±24 horas cubre el caso de blob escrito a las 23:59 UTC y procesado a las 00:01 UTC.

**Retry por latencia de filesystem compartido:**

Si el blob no se encuentra inmediatamente, el nodo puede reintentar con backoff corto (ej. 100ms, 200ms, 400ms) antes de declarar `BLOB_NOT_FOUND`. Esto es responsabilidad del nodo, no hay comunicación adicional.

#### 18.8 Pins (Locks de Uso)

Para evitar que el GC borre blobs en uso:

```
Al comenzar a procesar: crear ${...}/pins/<blob_id>.<node_id>
Al terminar: borrar ese pin
```

El GC no borra días con pins activos.

#### 18.9 Retención y Garbage Collection

Estrategia: borrar días completos (O(#días), no O(#archivos)).

```
RETENTION_DAYS >= 2 (recomendado)

Candidato a borrar: directorio con fecha < today_utc - RETENTION_DAYS
Condición: pins/ vacío o no existe
Acción: borrar directorio del día completo
```

El sweeper corre periódicamente (configurable, ej. cada hora).

#### 18.10 Límites Operativos

| Límite | Propósito |
|--------|-----------|
| `MAX_BLOB_SIZE` | Hard limit por blob individual |
| `MAX_SPOOL_BYTES` | Cuota total del spool |

Si se excede `MAX_SPOOL_BYTES`, rechazar nuevas escrituras con `PAYLOAD_TOO_LARGE`.

#### 18.11 Semántica

- **Inmutable:** write-once, nunca se modifica un blob
- **Best-effort:** el blob store no implica delivery garantizado
- **No streaming:** el payload no se expone al receptor hasta validación completa

#### 18.12 Señales de Error

| Señal | Cuándo |
|-------|--------|
| `PAYLOAD_TOO_LARGE` | Blob excede `MAX_BLOB_SIZE` o spool excede cuota |
| `BLOB_NOT_FOUND` | Receptor no encuentra blob (incluso con fallback ±24h) |
| `BLOB_CORRUPTED` | Blob existe pero no valida `size` o `sha256` |

### 19. Uplink WAN (Router ↔ Router)

#### 19.1 Objetivo

Conectar "islas" (dominios locales) mediante uplink persistente entre routers. El uplink transporta mensajes de sistema (control-plane) y forwarding de mensajes entre islas, manteniendo el diseño best-effort.

```
┌─────────────────┐                    ┌─────────────────┐
│     Isla A      │                    │     Isla B      │
│                 │                    │                 │
│  [Nodo]──[Router A]════TCP/TLS════[Router B]──[Nodo]  │
│  [Nodo]─┘       │      (WAN)         │       └─[Nodo] │
└─────────────────┘                    └─────────────────┘
```

El uplink es siempre punto a punto: un router conecta con otro router. Un router puede tener múltiples uplinks a diferentes islas.

#### 19.2 Transporte

| Contexto | Transporte | Framing |
|----------|------------|---------|
| LAN/Local (nodos) | Unix domain socket `SOCK_STREAM` | `uint32_be length` + `JSON` |
| WAN (routers) | TCP stream | `uint32_be length` + `JSON` |

**El framing es idéntico en ambos casos.** El mismo código de lectura/escritura funciona para LAN y WAN.

#### 19.3 Mensajes Reutilizados

El uplink WAN usa los mismos mensajes de routing entre routers ya definidos:

| Mensaje | Propósito en WAN |
|---------|------------------|
| `HELLO` | Anunciar existencia, keepalive |
| `LSA` | Propagar cambios de topología entre islas |
| `SYNC_REQUEST` | Pedir tabla completa al conectar |
| `SYNC_REPLY` | Responder con tabla completa |

No se inventa protocolo nuevo para WAN. La propagación de configuración (rutas estáticas, VPNs) es responsabilidad del nodo `SY.config.routes`, no del router.

#### 19.4 Mensajes de Handshake WAN

Dos mensajes adicionales para negociación explícita del uplink:

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `UPLINK_ACCEPT` | Router | Router | Confirma aceptación del peer, versión y capabilities |
| `UPLINK_REJECT` | Router | Router | Rechaza conexión con motivo explícito |

#### 19.5 Handshake del Uplink

Secuencia al establecer conexión TCP:

```
Router A                              Router B
    │                                     │
    │◄────────TCP connect─────────────────┤
    │                                     │
    ├─────────HELLO──────────────────────►│
    │◄────────HELLO───────────────────────┤
    │                                     │
    │         (validación de HELLO)       │
    │                                     │
    ├─────────UPLINK_ACCEPT──────────────►│  (o UPLINK_REJECT)
    │◄────────UPLINK_ACCEPT───────────────┤  (o UPLINK_REJECT)
    │                                     │
    ├─────────SYNC_REQUEST───────────────►│
    │◄────────SYNC_REPLY──────────────────┤
    │                                     │
    │◄────────SYNC_REQUEST────────────────┤
    ├─────────SYNC_REPLY─────────────────►│
    │                                     │
    │         (operación normal)          │
    │                                     │
    ├─────────HELLO periódico────────────►│
    │◄────────HELLO periódico─────────────┤
    │                                     │
```

#### 19.6 Payload del HELLO (WAN extendido)

```json
{
  "routing": {
    "src": "uuid-router-a",
    "dst": "uuid-router-b",
    "ttl": 16,
    "trace_id": "uuid"
  },
  "meta": {
    "type": "system",
    "msg": "HELLO"
  },
  "payload": {
    "timestamp": "2025-01-15T10:00:00Z",
    "seq": 1,
    "protocol": "json-router/1",
    "router_id": "uuid-router-a",
    "island_id": "isla-produccion",
    "shm_name": "/jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "capabilities": {
      "sync": true,
      "lsa": true,
      "forwarding": true
    },
    "timers": {
      "hello_interval_ms": 10000,
      "dead_interval_ms": 40000
    }
  }
}
```

| Campo | Propósito |
|-------|-----------|
| `protocol` | Versión del protocolo para compatibilidad |
| `router_id` | UUID del router |
| `island_id` | Identificador de la isla (dominio) |
| `shm_name` | Nombre de la región de shared memory del router (para descubrimiento) |
| `capabilities` | Qué soporta este router |
| `timers` | Intervalos de hello/dead para sincronizar |

**Descubrimiento de shared memory via HELLO:**

Al recibir un HELLO de un peer, el router:
1. Extrae `shm_name` del payload
2. Mapea la región en modo read-only: `shm_open(shm_name, O_RDONLY)`
3. Valida magic y version
4. Comienza a leer nodos y rutas del peer para construir su FIB

#### 19.7 Payload de UPLINK_ACCEPT

```json
{
  "meta": {
    "type": "system",
    "msg": "UPLINK_ACCEPT"
  },
  "payload": {
    "timestamp": "2025-01-15T10:00:01Z",
    "peer_router_id": "uuid-router-b",
    "negotiated": {
      "protocol": "json-router/1",
      "hello_interval_ms": 10000,
      "dead_interval_ms": 40000
    }
  }
}
```

#### 19.8 Payload de UPLINK_REJECT

```json
{
  "meta": {
    "type": "system",
    "msg": "UPLINK_REJECT"
  },
  "payload": {
    "timestamp": "2025-01-15T10:00:01Z",
    "reason": "PROTOCOL_MISMATCH",
    "message": "Minimum supported version is json-router/1",
    "min_version": "json-router/1"
  }
}
```

Razones de rechazo:

| Reason | Descripción |
|--------|-------------|
| `PROTOCOL_MISMATCH` | Versión de protocolo no soportada |
| `ISLAND_NOT_AUTHORIZED` | Island ID no está en lista de permitidos |
| `CAPABILITY_MISSING` | Falta capability requerida |
| `OVERLOADED` | Router no puede aceptar más uplinks |

#### 19.9 Operación Normal

Una vez establecido el uplink:

- **HELLO periódico** cada `hello_interval_ms`
- **Dead detection**: sin HELLO por `dead_interval_ms` = peer caído, invalidar rutas
- **LSA incremental**: cambios de topología se propagan con LSA
- **TTL**: se decrementa por hop, previene loops entre islas

#### 19.10 Forwarding entre Islas

Cuando un mensaje tiene destino en otra isla:

```
1. Router A recibe mensaje para nodo en Isla B
2. Router A busca en tabla: nodo está en Isla B via Router B
3. Router A decrementa TTL
4. Router A envía mensaje por uplink TCP a Router B
5. Router B recibe, busca nodo local, entrega por Unix socket
```

El mensaje JSON es idéntico. Solo cambia el transporte subyacente.

#### 19.11 Seguridad

La autenticación se delega al edge proxy (NGINX, Envoy, etc.):

| Capa | Responsable | Mecanismo |
|------|-------------|-----------|
| Transporte | Edge proxy | TLS/mTLS |
| Autorización | Edge proxy | Allowlist de IPs/certs |
| Rate limiting | Edge proxy | Por conexión/bytes |
| Protocolo | Router | HELLO + UPLINK_ACCEPT/REJECT |

El proxy no interpreta el protocolo. Es transparente L4.

La validación de `island_id` contra lista de islas autorizadas puede hacerse en el router al recibir HELLO, respondiendo `UPLINK_REJECT` si no está autorizado.

#### 19.12 Topología WAN: Modelo Bus

La topología recomendada es **bus lineal** (similar a fibra óptica). Cada isla solo configura su uplink al "siguiente" en la cadena:

```
Isla A ──► Isla B ──► Isla C ──► Isla D
```

**Configuración mínima por isla:**

```yaml
# Isla A (primera)
wan:
  listen: "0.0.0.0:9000"
  uplink: "10.0.1.2:9000"    # → B

# Isla B
wan:
  listen: "0.0.0.0:9000"
  uplink: "10.0.2.2:9000"    # → C

# Isla C
wan:
  listen: "0.0.0.0:9000"
  uplink: "10.0.3.2:9000"    # → D

# Isla D (última)
wan:
  listen: "0.0.0.0:9000"
  # Sin uplink, es el final del bus
```

**Ventajas:**
- Cada isla configura solo UN uplink (excepto la última)
- Agregar isla nueva: tocar solo 2 configs (nueva + anterior)
- Simple de entender y mantener

**Variante anillo (más resiliente):**

```
Isla A ──► Isla B ──► Isla C ──► Isla D
   ▲                                │
   └────────────────────────────────┘
```

La última isla (D) configura uplink de vuelta a la primera (A):

```yaml
# Isla D (cierra el anillo)
wan:
  listen: "0.0.0.0:9000"
  uplink: "10.0.0.2:9000"    # → A (cierra el anillo)
```

#### 19.13 Conexiones Bidireccionales Automáticas

Cuando A conecta a B por uplink:
- A conoce a B (su uplink configurado)
- B conoce a A (conexión entrante aceptada)

**No es necesario configurar ambos lados.** La conexión entrante se acepta automáticamente si pasa validación de HELLO.

```
A config: uplink → B
B config: (nada hacia A)

Resultado:
  A ────────► B
    (TCP)
  
A sabe de B: por su config
B sabe de A: por conexión entrante
```

#### 19.14 Routing Transitivo Inter-Isla

Si A quiere enviar a isla C pero solo tiene uplink a B:

```
A ──► B ──► C

1. A recibe mensaje para nodo en Isla C
2. A no tiene uplink directo a C
3. A consulta tabla inter-isla: "C via B"
4. A envía a B (su uplink)
5. B recibe, ve que destino es Isla C
6. B tiene uplink a C, reenvía
7. C recibe, entrega al nodo local
```

**La tabla de rutas inter-isla se construye por LSA:**

```
Cada router anuncia via LSA:
  - Su island_id
  - Islas a las que tiene uplink directo

Los routers calculan rutas:
  - "Para llegar a C desde A, ir por B"
```

**TTL previene loops:** El mensaje decrementa TTL en cada hop inter-isla.

#### 19.15 Requisitos de Deployment

**Router principal por isla:**
- Cada isla tiene UN router con WAN activa (levantado por systemd)
- Este router es el punto de entrada/salida WAN de la isla
- No se puede matar via API (protección en SY.orchestrator)

**SY.admin:**
- Debe correr en la primera isla del bus (o cualquiera con conectividad a todas)
- Requiere rutas (directas o transitivas) a todas las islas que administra
- Deriva automáticamente los nombres L2 por isla:
  - `SY.orchestrator.{isla}`
  - `SY.config.routes.{isla}`
  - `SY.opa.rules.{isla}`

---

## Parte IV: Routing

### 20. Tabla de Ruteo (RIB)

La tabla de ruteo sigue el modelo estándar de redes. Ver sección 13 para el formato de `RouteEntry`.

#### 20.1 Tipos de Ruta

| Tipo | route_type | admin_distance | Origen |
|------|------------|----------------|--------|
| CONNECTED | 0 | 0 | Nodo conectado directamente al router |
| STATIC | 1 | 1 | Configurada manualmente |
| LSA | 2 | 10 | Aprendida de otro router via LSA |

#### 20.2 Selección de Mejor Ruta

Cuando hay múltiples rutas al mismo destino:

1. **Longest Prefix Match (LPM)**: La ruta más específica gana
2. **Admin Distance**: Menor valor gana (CONNECTED < STATIC < LSA)
3. **Metric**: Menor costo gana
4. **Installed At**: La más vieja gana (estabilidad)

#### 20.3 Rutas CONNECTED (automáticas)

Cuando un nodo conecta, el router crea automáticamente una ruta CONNECTED:

```
prefix: <nombre capa 2 del nodo>
prefix_len: longitud exacta
match_kind: EXACT
next_hop_router: <uuid del router local>
out_link: LINK_LOCAL (0)
admin_distance: AD_CONNECTED (0)
route_type: ROUTE_CONNECTED
```

#### 20.4 Rutas STATIC (configuradas)

Las rutas estáticas se configuran centralizadamente mediante el nodo `SY.config.routes`. Ver **Sección 27: Configuración Centralizada de Rutas** para detalles completos.

**Resumen:** Un nodo SY escribe las rutas estáticas en una región de shared memory dedicada (`/jsr-config-<island>`). Todos los routers de la isla leen esta región y actualizan su FIB.

```
SY.config.routes → /jsr-config-<island> → Routers leen → FIB actualizada
```

**Nota:** La propagación de configuración entre islas es responsabilidad del nodo `SY.config.routes`, no del router. El router solo lee la configuración local. Ver documento "JSON Router - Nodos SY" para detalles de propagación.

#### 20.5 Rutas LSA (aprendidas)

Recibidas de otros routers via mensajes LSA. El router instala estas rutas con `admin_distance = AD_LSA`.

### 21. Policies OPA

Las policies OPA permiten decisiones de routing dinámicas basadas en metadata del mensaje.

#### 21.1 Arquitectura

```
                                    ┌─────────────────────┐
                                    │   SY.opa.rules      │
                                    │                     │
Nodo AI ──► API (Rego) ──────────► │ 1. Compila Rego     │
                                    │ 2. Guarda WASM      │
                                    │ 3. Broadcast RELOAD │
                                    └──────────┬──────────┘
                                               │
                                               ▼
                              /var/lib/json-router/policy.wasm
                                               │
                    ┌──────────────────────────┼──────────────────────────┐
                    │                          │                          │
                    ▼                          ▼                          ▼
              ┌──────────┐              ┌──────────┐              ┌──────────┐
              │ Router A │              │ Router B │              │ Router C │
              │ WASM     │              │ WASM     │              │ WASM     │
              │ en RAM   │              │ en RAM   │              │ en RAM   │
              └──────────┘              └──────────┘              └──────────┘
```

#### 21.2 Lenguaje Rego

Las policies se escriben en **Rego**, el lenguaje estándar de OPA:

```rego
package router

default target = null

# Clientes VIP van a soporte L2
target = "AI.soporte.l2" {
    input.meta.context.cliente_tier == "vip"
}

# Clientes standard van a soporte L1
target = "AI.soporte.l1" {
    input.meta.context.cliente_tier == "standard"
}

# Horario nocturno va a equipo nocturno
target = "AI.soporte.l1.nocturno" {
    input.meta.context.horario == "nocturno"
}
```

**Input disponible para policies:**
```json
{
  "meta": { ... },           // Sección meta completa del mensaje
  "routing": {
    "src": "uuid-origen"     // Solo src, no dst (eso es lo que resolvemos)
  }
}
```

**Output esperado:**
```json
{
  "target": "AI.soporte.l1"  // Nombre capa 2 del destino
}
```

#### 21.3 Compilación y Distribución

El archivo `.wasm` se genera compilando Rego:

```bash
# Compilar policy.rego a policy.wasm
opa build -t wasm -e router/target policy.rego -o bundle.tar.gz
tar -xzf bundle.tar.gz /policy.wasm
```

**Directorio estándar:**
```
/var/lib/json-router/policy.wasm    # Runtime (WASM compilado)
/etc/json-router/policy.rego        # Fuente (opcional, para referencia)
```

#### 21.4 Carga en el Router

Al iniciar, el router:
1. Lee `/var/lib/json-router/policy.wasm`
2. Carga WASM en memoria (Wasmtime)
3. Actualiza `opa_policy_version` en SHM
4. `opa_load_status = 0` (OK)

Si falla:
- `opa_load_status = 1` (ERROR)
- Router continúa sin OPA (solo routing directo por UUID)

#### 21.5 Reload Atómico (OPA_RELOAD)

Cuando el router recibe mensaje `OPA_RELOAD`:

```
1. opa_load_status = 2 (LOADING)
2. Entrar en modo drain:
   - Dejar de aceptar mensajes nuevos
   - Esperar que terminen los en vuelo (max 1s timeout)
3. Recargar policy.wasm desde disco
4. Si éxito:
   - opa_policy_version = version del mensaje
   - opa_load_status = 0 (OK)
5. Si falla:
   - opa_load_status = 1 (ERROR)
   - Mantener policy anterior si es posible
6. Salir de modo drain, continuar operación
```

**Mensaje OPA_RELOAD:**
```json
{
  "routing": {
    "src": "uuid-sy-opa-rules",
    "dst": "broadcast",
    "ttl": 16,
    "trace_id": "uuid"
  },
  "meta": {
    "type": "system",
    "msg": "OPA_RELOAD"
  },
  "payload": {
    "version": 42,
    "hash": "sha256-del-wasm"
  }
}
```

#### 21.6 Estado OPA en SHM

```rust
// En ShmHeader
pub opa_policy_version: u64,    // Versión cargada (0 = no cargado)
pub opa_load_status: u8,        // 0=OK, 1=ERROR, 2=LOADING
```

| opa_load_status | Significado |
|-----------------|-------------|
| 0 | OK - Policy cargado y funcionando |
| 1 | ERROR - Falló la carga, sin OPA |
| 2 | LOADING - Recargando, en modo drain |

Esto permite a `SY.opa.rules` verificar que todos los routers cargaron la policy correctamente.

### 22. Balanceo entre Nodos del Mismo Rol

*Por definir: estrategia cuando hay múltiples nodos con la capacidad requerida.*

---

## Parte V: Operación

### 23. Estados de la Red

*Por definir: estados posibles de nodos y links, transiciones, eventos.*

### 24. Eventos del Sistema

*Por definir: qué eventos se generan, quién los consume, formato.*

### 25. Broadcast y Multicast

El sistema soporta mensajes dirigidos a múltiples destinos. El router maneja esto de forma transparente.

#### 25.1 Tipos de Destino

| `dst` | `meta.target` | Comportamiento |
|-------|---------------|----------------|
| UUID | - | Unicast: entregar a ese nodo específico |
| `null` | nombre capa 2 | Resolver via OPA, luego unicast |
| `"broadcast"` | - | Broadcast: entregar a todos los nodos |
| `"broadcast"` | patrón capa 2 | Multicast: entregar a nodos que matchean el patrón |

**Importante:** `dst: null` y `dst: "broadcast"` son flujos completamente distintos. No se mezclan.

#### 25.2 Broadcast Global

Cuando `dst = "broadcast"` sin filtro:

```json
{
  "routing": {
    "src": "uuid-origen",
    "dst": "broadcast",
    "ttl": 16,
    "trace_id": "uuid"
  },
  "meta": { ... },
  "payload": { ... }
}
```

**Comportamiento del router:**

```
1. Verificar trace_id en cache → Si existe, DROP (loop)
2. Agregar trace_id al cache (TTL 60s)
3. Comparar routing.src con mi UUID → No entregar al origen
4. Para cada nodo conectado localmente:
   - Copia el mensaje
   - Entrega al nodo
5. Si TTL > 1:
   - Decrementa TTL
   - Reenvía a routers peers (misma isla via fabric)
6. Si TTL > 2:
   - Reenvía a routers WAN (otras islas)
```

#### 25.3 Multicast (Broadcast Filtrado)

Cuando `dst = "broadcast"` con `meta.target`:

```json
{
  "routing": {
    "src": "uuid-origen",
    "dst": "broadcast",
    "ttl": 16,
    "trace_id": "uuid"
  },
  "meta": {
    "type": "system",
    "target": "SY.config.*"
  },
  "payload": { ... }
}
```

**Comportamiento del router:**

```
1-3. (Igual que broadcast global: cache, loop check, no al origen)
4. Para cada nodo conectado localmente:
   - Si nombre capa 2 matchea meta.target → Entrega
   - Si no matchea → Skip
5-6. (Igual: propagar según TTL)
```

**El filtro es por nombre capa 2.** El router ya conoce el nombre de cada nodo (registrado en HELLO).

#### 25.4 Algoritmo de Match para meta.target

```rust
fn target_match(pattern: &str, node_name: &str) -> bool {
    if pattern.ends_with(".*") {
        // Prefix match: "SY.config.*" matchea "SY.config.routes"
        let prefix = &pattern[..pattern.len() - 2];
        node_name.starts_with(prefix)
    } else {
        // Exact match
        pattern == node_name
    }
}
```

| Pattern | Node Name | Match |
|---------|-----------|-------|
| `SY.config.*` | `SY.config.routes` | ✓ |
| `SY.config.*` | `SY.time` | ✗ |
| `AI.ventas.*` | `AI.ventas.l1.diurno` | ✓ |
| `AI.ventas.l1` | `AI.ventas.l1` | ✓ |
| `AI.ventas.l1` | `AI.ventas.l1.diurno` | ✗ |

#### 25.5 TTL en Broadcast

El TTL controla el alcance del broadcast:

| TTL | Alcance |
|-----|---------|
| 1 | Solo nodos conectados a ESTE router |
| 2 | Toda la isla (este router + peers via fabric) |
| 3+ | Cruza WAN a otras islas |

**Decremento:**
- Al reenviar por fabric: TTL - 1
- Al reenviar por WAN: TTL - 1

**Importante:** Broadcast WAN (TTL > 2) puede generar mucho tráfico. Usar con cuidado.

#### 25.6 Prevención de Loops

Cada router mantiene un cache de `trace_id` por separado:

```rust
struct BroadcastCache {
    seen: HashMap<Uuid, Instant>,  // trace_id → timestamp
    ttl: Duration,                  // 60 segundos
}

impl BroadcastCache {
    fn check_and_add(&mut self, trace_id: Uuid) -> bool {
        self.cleanup_expired();
        if self.seen.contains_key(&trace_id) {
            false  // Ya visto, DROP
        } else {
            self.seen.insert(trace_id, Instant::now());
            true   // Nuevo, procesar
        }
    }
}
```

**El cache es por router.** No se comparte entre routers. Cada uno filtra independientemente.

#### 25.7 No Entregar al Origen

El router **nunca** entrega un broadcast al nodo que lo originó:

```rust
if msg.routing.src == node.uuid {
    continue;  // Skip, es el origen
}
```

Esto aplica tanto para nodos locales como para reenvío a otros routers.

#### 25.8 Casos de Uso

| Mensaje | Target | TTL | Propósito |
|---------|--------|-----|-----------|
| `TIME_SYNC` | - | 1 | Tiempo a nodos de este router |
| `CONFIG_ANNOUNCE` | `SY.config.*` | 16 | Anunciar rutas entre islas |
| `OPA_RELOAD` | - | 16 | Recargar policies OPA en todos los routers |
| `BCAST_SHUTDOWN` | - | 16 | Shutdown de toda la red |
| `BCAST_RELOAD` | - | 2 | Recargar config en isla |

#### 25.9 Broadcast y VPN

**Broadcast de sistema (SY.\*):** Siempre se entrega, sin filtro de VPN. Son mensajes de infraestructura esenciales.

**Broadcast de negocio:** Por ahora no hay filtro por VPN. Todos los nodos que matchean el target reciben el mensaje.

**Nota:** VPN en este sistema es un **túnel/camino** entre islas, no un mecanismo de segmentación de broadcast. Si en el futuro se requiere segmentación lógica, se definirá un concepto separado.

### 26. Mantenimiento y Administración

#### 26.1 Herramienta shm-watch

Herramienta de línea de comandos para visualizar las regiones de shared memory en tiempo real:

```bash
# Ver todas las regiones detectadas
shm-watch --discover --refresh 1s

# Ver una región específica
shm-watch --shm /jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890 --refresh 1s

# Ver todas las regiones de una isla
shm-watch --island produccion --refresh 1s
```

Muestra por cada región:
- Header: router_uuid, owner_pid, heartbeat, seq, counts
- Estado: ALIVE / STALE (basado en heartbeat)
- Tabla de nodos activos
- Tabla de rutas

**Modo consolidado:**
```bash
shm-watch --all --refresh 1s
```
Muestra la vista consolidada (FIB) combinando todas las regiones.

#### 26.2 Limpieza de Recursos

*Por definir: cleanup de sockets huérfanos, entries marcadas DELETED, etc.*

### 27. Configuración Centralizada de Rutas

Las rutas estáticas y VPNs se configuran centralizadamente mediante un nodo de sistema `SY.config.routes`. Este nodo es el único que escribe en una región de shared memory dedicada, que todos los routers leen.

**Separación de responsabilidades:**
- **Router:** Solo lee configuración local. No propaga configuración.
- **SY.config.routes:** Escribe configuración y la propaga entre islas.

#### 27.1 Principio de Diseño

**Router = Data Plane (solo lee)**

```
┌─────────────────────────────────────────────────────────────┐
│                   SY.config.routes                          │
│                                                              │
│  - Único proceso que escribe config de rutas                │
│  - API para administración (REST/socket)                    │
│  - Persiste cambios en disco                                │
│  - Propaga cambios entre islas (via mensajes a otros SY)   │
└─────────────────────────────────────────────────────────────┘
                           │
                           │ escribe
                           ▼
┌─────────────────────────────────────────────────────────────┐
│              /jsr-config-<island_id>                        │
│              Región SHM de Configuración                     │
│                                                              │
│  - StaticRouteEntry[] - Rutas estáticas de esta isla       │
│  - VPNEntry[] - Configuración de VPNs                       │
│  - Seqlock para sincronización                              │
└─────────────────────────────────────────────────────────────┘
                           │
            ┌──────────────┼──────────────┐
            │ leen         │ leen         │ leen
            ▼              ▼              ▼
       Router A       Router B       Router C
       
       Cada router:
       1. Lee /jsr-config-<island>
       2. Instala rutas STATIC en su FIB
       (NO propaga - eso lo hace SY.config.routes)
```

**Ventajas:**
- Consistencia: todos los routers ven las mismas rutas estáticas
- Simplicidad: router no tiene lógica de propagación
- Auditabilidad: un solo lugar donde ver/modificar rutas
- Separación clara: router mueve mensajes, SY maneja config

#### 27.2 Nodo SY.config.routes

El nodo de configuración sigue el patrón de nodos SY:

```
SY.config.routes.<instancia>

Ejemplos:
  SY.config.routes.primary
  SY.config.routes.backup
```

**Responsabilidades:**
1. Proveer API para CRUD de rutas estáticas y VPNs
2. Persistir configuración en disco (YAML/JSON)
3. Escribir en `/jsr-config-<island_id>` al iniciar y al cambiar
4. Mantener heartbeat en la región de config
5. **Propagar cambios a SY.config.routes de otras islas** (via mensajes normales)

**No es responsabilidad del router:**
- Propagar configuración entre islas
- Entender el contenido de la configuración (solo la lee y aplica)

Ver documento "JSON Router - Nodos SY" para detalles de la propagación entre islas.

#### 27.3 Región SHM de Configuración

Nueva región de shared memory dedicada a configuración global.

**Nombre:** `/jsr-config-<island_id>`

**Ejemplo:** `/jsr-config-produccion`

##### Layout de la región

```
┌─────────────────────────────────────────────────────────────┐
│                     ConfigHeader (192 bytes)                │
├─────────────────────────────────────────────────────────────┤
│                 StaticRouteEntry[MAX_STATIC_ROUTES]         │
│                       (360 * 256 = 92,160 bytes)            │
├─────────────────────────────────────────────────────────────┤
│                     VPNEntry[MAX_VPNS]                      │
│                       (256 * 64 = 16,384 bytes)             │
└─────────────────────────────────────────────────────────────┘
Total: ~106 KB
```

##### ConfigHeader

```rust
#[repr(C)]
pub struct ConfigHeader {
    // === IDENTIFICACIÓN (80 bytes) ===
    pub magic: u32,                   // CONFIG_MAGIC = 0x4A534343 ("JSCC")
    pub version: u32,                 // Versión del formato
    pub island_id: [u8; 64],          // ID de la isla
    pub island_id_len: u16,
    pub _pad1: [u8; 6],
    
    // === OWNERSHIP (40 bytes) ===
    pub owner_pid: u64,               // PID del SY.config.routes
    pub owner_start_time: u64,        // Para detectar PID reuse
    pub owner_uuid: [u8; 16],         // UUID del nodo SY.config.routes (binario)
    pub heartbeat: u64,               // Epoch ms, actualizado periódicamente
    
    // === SEQLOCK (8 bytes) ===
    pub seq: u64,                     // Seqlock counter (AtomicU64)
    
    // === CONTADORES (16 bytes) ===
    pub static_route_count: u32,      // Rutas estáticas activas
    pub vpn_count: u32,               // VPNs activas
    pub config_version: u64,          // Versión de la config (incrementa con cada cambio)
    
    // === TIMESTAMPS (16 bytes) ===
    pub created_at: u64,              // Epoch ms de creación
    pub updated_at: u64,              // Epoch ms de última modificación
    
    // === RESERVED (32 bytes para alinear a 192) ===
    pub _reserved: [u8; 32],
}
// Total: 192 bytes
```

##### StaticRouteEntry

```rust
#[repr(C)]
pub struct StaticRouteEntry {
    // === MATCHING (260 bytes) ===
    pub prefix: [u8; 256],            // Pattern: "AI.soporte.*"
    pub prefix_len: u16,
    pub match_kind: u8,               // MATCH_EXACT, MATCH_PREFIX, MATCH_GLOB
    pub _pad1: u8,
    
    // === FORWARDING (76 bytes) ===
    pub next_hop_island: [u8; 64],    // Island destino (para VPN/WAN)
    pub next_hop_island_len: u16,
    pub action: u8,                   // ACTION_FORWARD, ACTION_DROP, ACTION_VPN
    pub _pad2: u8,
    pub vpn_id: u32,                  // Si action == ACTION_VPN
    pub metric: u32,
    
    // === ESTADO (8 bytes) ===
    pub flags: u16,                   // FLAG_ACTIVE, FLAG_DELETED
    pub priority: u16,                // Para ordenar (menor = más prioritario)
    pub _pad3: [u8; 4],
    
    // === METADATA (16 bytes) ===
    pub installed_at: u64,
    pub _reserved: [u8; 8],
}
// Total: 360 bytes
```

**Acciones:**

| action | Valor | Descripción |
|--------|-------|-------------|
| ACTION_FORWARD | 0 | Rutear normalmente |
| ACTION_DROP | 1 | Descartar (blackhole) |
| ACTION_VPN | 2 | Enviar por VPN específica |

##### VPNEntry

```rust
#[repr(C)]
pub struct VPNEntry {
    // === IDENTIFICACIÓN (72 bytes) ===
    pub vpn_id: u32,                  // ID único de la VPN
    pub vpn_name: [u8; 64],           // Nombre descriptivo
    pub vpn_name_len: u16,
    pub _pad1: [u8; 2],
    
    // === DESTINO (72 bytes) ===
    pub remote_island: [u8; 64],      // Isla destino
    pub remote_island_len: u16,
    pub _pad2: [u8; 6],
    
    // === ENDPOINTS (98 bytes) ===
    pub endpoint_count: u32,
    pub endpoints: [[u8; 46]; 2],     // Hasta 2 endpoints (IP:puerto)
    pub _pad3: [u8; 2],
    
    // === ESTADO (14 bytes) ===
    pub flags: u16,
    pub _pad4: [u8; 4],
    pub created_at: u64,
}
// Total: 256 bytes
```

**Uso de VPNEntry:** El router usa `vpn_id` de RouteEntry para buscar VPNEntry, luego usa `remote_island` para determinar el uplink de salida. Ver **sección 13.16.1** para el algoritmo completo de resolución de VPN.

#### 27.4 Constantes

```rust
// Región de configuración
pub const CONFIG_MAGIC: u32 = 0x4A534343;  // "JSCC"
pub const CONFIG_VERSION: u32 = 1;
pub const CONFIG_SHM_PREFIX: &str = "/jsr-config-";

// Capacidades
pub const MAX_STATIC_ROUTES: u32 = 256;    // Rutas estáticas por isla
pub const MAX_VPNS: u32 = 64;              // VPNs por isla

// Acciones de ruta
pub const ACTION_FORWARD: u8 = 0;
pub const ACTION_DROP: u8 = 1;
pub const ACTION_VPN: u8 = 2;

// Timers (heredados de router)
pub const CONFIG_HEARTBEAT_INTERVAL_MS: u64 = 5_000;
pub const CONFIG_HEARTBEAT_STALE_MS: u64 = 30_000;
```

#### 27.5 API del Nodo SY.config.routes

El nodo expone una API via socket Unix para administración:

**Socket:** `/var/run/mesh/nodes/SY.config.routes.primary.sock`

##### Mensajes de API

**Listar rutas estáticas:**
```json
{
  "routing": { "src": "admin-uuid", "dst": "sy-config-uuid", ... },
  "meta": { "type": "admin", "action": "list_routes" },
  "payload": {}
}
```

**Respuesta:**
```json
{
  "payload": {
    "routes": [
      {
        "prefix": "AI.soporte.*",
        "match_kind": "PREFIX",
        "action": "FORWARD",
        "next_hop_island": "produccion-us",
        "metric": 10,
        "priority": 100
      }
    ],
    "config_version": 42
  }
}
```

**Agregar ruta estática:**
```json
{
  "meta": { "type": "admin", "action": "add_route" },
  "payload": {
    "prefix": "AI.ventas.*",
    "match_kind": "PREFIX",
    "action": "FORWARD",
    "next_hop_island": "produccion-us",
    "metric": 10,
    "priority": 100
  }
}
```

**Eliminar ruta estática:**
```json
{
  "meta": { "type": "admin", "action": "delete_route" },
  "payload": {
    "prefix": "AI.ventas.*"
  }
}
```

**Listar VPNs:**
```json
{
  "meta": { "type": "admin", "action": "list_vpns" },
  "payload": {}
}
```

Respuesta:
```json
{
  "payload": {
    "status": "ok",
    "vpns": [
      {
        "vpn_id": 1,
        "vpn_name": "vpn-staging",
        "remote_island": "staging",
        "endpoints": ["10.0.1.100:9000"]
      }
    ]
  }
}
```

**Agregar VPN:**
```json
{
  "meta": { "type": "admin", "action": "add_vpn" },
  "payload": {
    "vpn_name": "vpn-staging",
    "remote_island": "staging",
    "endpoints": ["10.0.1.100:9000", "10.0.1.101:9000"]
  }
}
```

Respuesta (vpn_id generado automáticamente):
```json
{
  "payload": {
    "status": "ok",
    "vpn_id": 1
  }
}
```

**Eliminar VPN:**
```json
{
  "meta": { "type": "admin", "action": "delete_vpn" },
  "payload": {
    "vpn_id": 1
  }
}
```

**Nota sobre VPN y rutas:** Para usar una VPN, se debe crear una ruta con `action: "VPN"` y `vpn_id` referenciando la VPN creada. El router resuelve `vpn_id → remote_island → uplink` automáticamente (ver sección 13.16.1).
```

#### 27.6 Flujo del Router al Leer Config

Cada router lee la región de config además de las regiones de otros routers:

```rust
// Al iniciar y periódicamente
fn update_fib_from_config(&mut self) {
    // 1. Abrir región de config si no está mapeada
    let config_shm_name = format!("{}{}", CONFIG_SHM_PREFIX, self.island_id);
    if self.config_region.is_none() {
        self.config_region = map_config_region(&config_shm_name);
    }
    
    // 2. Leer con seqlock
    let config = match &self.config_region {
        Some(region) => seqlock_read(region),
        None => return,  // Config no disponible, continuar sin rutas estáticas
    };
    
    // 3. Verificar heartbeat (SY.config.routes vivo)
    if is_config_stale(&config.header) {
        log::warn!("Config region stale, SY.config.routes may be down");
        // Las rutas existentes se mantienen hasta que expire
    }
    
    // 4. Si config_version cambió, actualizar FIB
    if config.header.config_version != self.last_config_version {
        self.install_static_routes(&config.static_routes);
        self.last_config_version = config.header.config_version;
        // Fin - el router NO propaga config, solo la lee
    }
}
```

**El router NO propaga configuración.** Solo lee y aplica. La propagación entre islas es responsabilidad de `SY.config.routes`.

#### 27.7 Ejemplo: Agregar Ruta Estática

```
1. Admin/AI conecta al socket de SY.config.routes (cualquier isla)
2. Envía mensaje add_route
3. SY.config.routes:
   a. Valida el request
   b. Persiste en disco (routes.yaml)
   c. Escribe en /jsr-config-<isla> con seqlock
   d. Incrementa config_version
   e. Responde OK
   f. Propaga a SY.config.routes de otras islas (si aplica)
4. Routers de la isla detectan cambio de config_version:
   a. Leen nueva ruta de la región
   b. Instalan en FIB
   (NO propagan - eso lo hace SY.config.routes)
```

Ver documento "JSON Router - Nodos SY" para detalles de propagación entre islas.

#### 27.8 Persistencia

El nodo SY.config.routes persiste la configuración en disco:

**Archivo:** `/etc/json-router/routes.yaml`

```yaml
# Rutas estáticas de la isla
static_routes:
  - prefix: "AI.soporte.*"
    match_kind: PREFIX
    action: FORWARD
    next_hop_island: produccion-us
    metric: 10
    priority: 100
    
  - prefix: "AI.ventas.interno"
    match_kind: EXACT
    action: DROP
    
vpns:
  - vpn_id: 1
    vpn_name: vpn-staging
    remote_island: staging
    endpoints:
      - "10.0.1.100:9000"
      - "10.0.1.101:9000"
```

**Ejemplo: Ruta que usa VPN:**

```yaml
static_routes:
  # Tráfico a AI.staging.* va por VPN a isla staging
  - prefix: "AI.staging.*"
    match_kind: PREFIX
    action: VPN
    vpn_id: 1          # Referencia a vpn-staging
    metric: 10

vpns:
  - vpn_id: 1
    vpn_name: vpn-staging
    remote_island: staging
    endpoints:
      - "10.0.1.100:9000"
```

**Flujo de resolución:**
```
1. Mensaje llega con dst = "AI.staging.worker1"
2. Router busca en FIB → matchea "AI.staging.*" con action=VPN, vpn_id=1
3. Router busca VPNEntry con vpn_id=1 → remote_island="staging"
4. Router busca uplink hacia isla "staging"
5. Router envía mensaje por ese uplink
```

**Flujo al iniciar:**
1. Leer `routes.yaml`
2. Crear/reclamar región `/jsr-config-<island>`
3. Escribir todas las rutas y VPNs en la región
4. Iniciar heartbeat loop

#### 27.9 Alta Disponibilidad

Para HA, se puede correr un nodo backup:

```
SY.config.routes.primary   (activo, escribe en shm)
SY.config.routes.backup    (standby, monitorea)
```

**Estrategia simple:** El backup monitorea el heartbeat del primary. Si detecta que está stale:
1. Intenta reclamar la región (verifica que el primary realmente murió)
2. Si lo logra, se convierte en primary
3. Lee `routes.yaml` y reescribe la región

El archivo `routes.yaml` es la fuente de verdad. Ambos nodos lo leen.

#### 27.10 Eliminación de Rutas Locales en Routers

Con el sistema centralizado, **los routers ya no tienen rutas STATIC propias**. Solo tienen:

- **CONNECTED:** Nodos locales (automáticas)
- **LSA:** Aprendidas de otros routers

Las rutas estáticas vienen siempre de la región de config.

**Impacto en `RouteEntry` de cada router:**
- `route_type` solo puede ser `ROUTE_CONNECTED` o `ROUTE_LSA`
- `ROUTE_STATIC` solo existe en la región de config

**Impacto en la FIB:**
- La FIB combina: nodos locales + rutas de peers + rutas de config
- Las rutas de config tienen `admin_distance = AD_STATIC (1)`


---

## Parte VI: Configuración y Arranque

Esta sección describe la estructura completa de configuración del sistema. Hay una separación clara entre:

- **island.yaml**: Template y autorización (solo se lee si el router no tiene config propio)
- **config.yaml**: Configuración real del router (autosuficiente una vez creado)
- **identity.yaml**: Estado de hardware/runtime (UUID, shm, auto-generado)

### 27. Principio de Diseño

**El router arranca con su propio config, no depende de island.yaml.**

Una vez que un router tiene su `config.yaml`, es autosuficiente. El `island.yaml` solo se usa como template para crear el config de un router nuevo, y como lista de routers autorizados.

```
Primera vez:
  island.yaml → genera → config.yaml
  Luego genera → identity.yaml

Siguientes veces:
  config.yaml + identity.yaml (island.yaml no se toca)
```

### 28. Estructura de Directorios

```
/etc/json-router/                      # Configuración
├── island.yaml                        # Template + autorización
└── routers/                           # Config por router
    ├── RT.produccion.primary/
    │   └── config.yaml                # Config real (auto-generado o editado)
    └── RT.produccion.secondary/
        └── config.yaml

./state/                               # Estado de runtime (auto-generado)
└── RT.produccion.primary/
    └── identity.yaml                  # UUID y datos de HW
```

### 29. Archivo: island.yaml

Template y lista de routers autorizados. **No contiene WAN** - eso es específico de cada router.

```yaml
# /etc/json-router/island.yaml
# Template para crear routers nuevos + lista de autorizados

island:
  id: produccion

# Defaults para routers nuevos
defaults:
  paths:
    state_dir: ./state
    node_socket_dir: /var/run/mesh/nodes
    shm_prefix: /jsr-
  
  timers:
    hello_interval_ms: 10000
    dead_interval_ms: 40000
    heartbeat_interval_ms: 5000
    heartbeat_stale_ms: 30000

# Lista de routers autorizados en esta isla
routers:
  - name: RT.produccion.primary
  - name: RT.produccion.secondary
```

#### 29.1 Campos de island.yaml

| Campo | Obligatorio | Descripción |
|-------|-------------|-------------|
| `island.id` | Sí | Identificador único de la isla |
| `routers[].name` | Sí | Lista de routers autorizados |
| `defaults.*` | No | Valores por defecto para config de routers nuevos |

### 30. Archivo: config.yaml (Router)

Configuración real y completa del router. **Autosuficiente** - no necesita island.yaml para operar.

```yaml
# /etc/json-router/routers/RT.produccion.primary/config.yaml
# Configuración completa del router

router:
  name: RT.produccion.primary
  island_id: produccion

paths:
  state_dir: ./state
  node_socket_dir: /var/run/mesh/nodes
  shm_prefix: /jsr-

timers:
  hello_interval_ms: 10000
  dead_interval_ms: 40000
  heartbeat_interval_ms: 5000
  heartbeat_stale_ms: 30000

# WAN es específica de este router
wan:
  listen: "10.0.1.1:9000"              # Opcional: donde escuchar conexiones entrantes
  uplinks:                              # Opcional: peers a los que conectar
    - address: "10.0.1.2:9000"
    - address: "staging.internal:9000"

# Peers locales (shm) que este router va a leer
local_peers:
  shm:
    - /jsr-a1b2c3d4e5f67890ab
    - /jsr-b1c2d3e4f5a67890cd

# Rutas estáticas (opcional)
routing:
  static_routes:
    - pattern: "WF.echo"
      match_kind: exact
      next_hop_router: "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
      link_id: 1
    - pattern: "AI.soporte"
      match_kind: prefix
      next_hop_router: "b1c2d3e4-f5a6-7890-bcde-fa2345678901"
      link_id: 2
```

#### 30.1 Campos de config.yaml

| Campo | Obligatorio | Descripción |
|-------|-------------|-------------|
| `router.name` | Sí | Nombre capa 2 del router |
| `router.island_id` | Sí | Isla a la que pertenece |
| `paths.state_dir` | Sí | Directorio para identity.yaml |
| `paths.node_socket_dir` | Sí | Directorio donde los nodos crean sockets |
| `paths.shm_prefix` | Sí | Prefijo para nombre de shared memory |
| `timers.*` | Sí | Todos los timers deben estar definidos |
| `wan.listen` | No | IP:puerto para conexiones WAN entrantes |
| `wan.uplinks[]` | No | Lista de peers a los que conectar |
| `local_peers.shm[]` | No | Lista de regiones shm de peers locales |
| `routing.static_routes[]` | No | Rutas estáticas con next-hop y link_id |

**Nota:** El config.yaml debe ser completo. No hay merge con island.yaml en runtime.

### 31. Archivo: identity.yaml (Estado HW)

Auto-generado en el primer arranque. Contiene el UUID y datos de hardware/runtime.

```yaml
# ./state/RT.produccion.primary/identity.yaml
# AUTO-GENERADO - NO EDITAR MANUALMENTE

layer1:
  uuid: a1b2c3d4-e5f6-7890-abcd-ef1234567890

layer2:
  name: RT.produccion.primary

shm:
  name: /jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890

created_at: 2025-01-17T10:30:00Z
created_at_ms: 1737110400000

system:
  hostname: server-prod-01
  pid_at_creation: 12345
```

#### 31.1 Campos de identity.yaml

| Campo | Descripción |
|-------|-------------|
| `layer1.uuid` | UUID único del router (capa 1), nunca cambia |
| `layer2.name` | Nombre capa 2, debe coincidir con config |
| `shm.name` | Nombre de la región de shared memory |
| `created_at` | Timestamp ISO-8601 de creación |
| `created_at_ms` | Timestamp en epoch milliseconds |
| `system.hostname` | Hostname del servidor (diagnóstico) |
| `system.pid_at_creation` | PID del proceso que creó el identity |

#### 31.2 shm.name en macOS

En macOS el nombre de shared memory está limitado a 31 caracteres. El sistema usa automáticamente un formato truncado:

```
Linux:   /jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890  (40 chars)
macOS:   /jsr-a1b2c3d4e5f67890ab                    (22 chars)
```

El valor guardado en `shm.name` es el nombre real usado en el sistema.

### 32. CLI y Variables de Entorno

```bash
json-router --name RT.produccion.primary [--config /etc/json-router] [--log-level info]
```

#### 32.1 Parámetros

| Parámetro | CLI | Env | Default | Obligatorio |
|-----------|-----|-----|---------|-------------|
| Router name (capa 2) | `--name` | `JSR_ROUTER_NAME` | - | **Sí** |
| Config directory | `--config` | `JSR_CONFIG_DIR` | `/etc/json-router` | No |
| Log level | `--log-level` | `JSR_LOG_LEVEL` | `info` | No |

**Prioridad:** CLI > Env > Default

**Nota:** Solo 3 variables de entorno. El UUID (capa 1) nunca se configura por env - es hardware, se genera y persiste en identity.yaml.

#### 32.2 Log Levels

| Level | Descripción |
|-------|-------------|
| `error` | Solo errores |
| `warn` | Errores y warnings |
| `info` | Operación normal (default) |
| `debug` | Debug detallado |
| `trace` | Todo, incluyendo mensajes |

### 33. Flujo de Arranque

```
json-router --name RT.produccion.primary --config /etc/json-router
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 1. Buscar config del router                                 │
│    Ruta: <config_dir>/routers/<name>/config.yaml            │
│                                                             │
│    ┌─ Existe ─────────────────────────────────────────┐    │
│    │  Usarlo directamente                              │    │
│    │  → Ir a paso 3                                    │    │
│    └───────────────────────────────────────────────────┘    │
│                                                             │
│    ┌─ No existe ──────────────────────────────────────┐    │
│    │  → Ir a paso 2 (crear desde island.yaml)         │    │
│    └───────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 2. Crear config desde island.yaml (solo si no existe)       │
│    a. Leer <config_dir>/island.yaml                        │
│    b. Validar que <name> está en routers[]                 │
│       - Si no está → ERROR: router no autorizado           │
│    c. Crear directorio routers/<name>/                     │
│    d. Generar config.yaml con:                             │
│       - router.name = <name>                               │
│       - router.island_id = island.id                       │
│       - Copiar defaults de island.yaml                     │
│       - wan vacío (admin debe configurar)                  │
│    e. Continuar con el config recién creado                │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. Validar config.yaml                                      │
│    - Todos los campos obligatorios presentes               │
│    - router.name coincide con --name                       │
│    - Paths válidos                                          │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. Cargar o crear identity                                  │
│    Ruta: <state_dir>/<name>/identity.yaml                  │
│                                                             │
│    ┌─ Existe ─────────────────────────────────────────┐    │
│    │  - Leer UUID                                      │    │
│    │  - Validar layer2.name == router.name            │    │
│    └───────────────────────────────────────────────────┘    │
│                                                             │
│    ┌─ No existe ──────────────────────────────────────┐    │
│    │  - Generar UUID nuevo                            │    │
│    │  - Calcular shm.name (truncar en macOS)          │    │
│    │  - Crear directorio y escribir identity.yaml     │    │
│    │  - El router "nace"                              │    │
│    └───────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 5. Inicializar shared memory                                │
│    - Usar shm.name del identity                            │
│    - Verificar si existe región previa (crash recovery)    │
│    - Crear o reclamar región                               │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 6. Iniciar servicios                                        │
│    - Escuchar en node_socket_dir (inotify)                 │
│    - Si wan.listen definido: bind en ese puerto            │
│    - Si wan.uplinks definido: conectar a cada peer         │
│    - Iniciar loop principal                                 │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
         Router operativo
```

### 34. Validaciones y Errores

| Condición | Error | Exit Code |
|-----------|-------|-----------|
| `--name` no proporcionado | "Router name required (--name)" | 1 |
| `config_dir` no existe | "Config directory not found: <path>" | 1 |
| `config.yaml` no existe Y `island.yaml` no existe | "Neither config.yaml nor island.yaml found" | 1 |
| `--name` no está en `island.routers[]` | "Router not authorized: <name>" | 1 |
| `config.yaml` incompleto (falta campo obligatorio) | "Missing required field: <field>" | 1 |
| `router.name` en config no coincide con `--name` | "Config name mismatch: expected <name>" | 1 |
| `identity.yaml` existe pero `layer2.name` no coincide | "Identity name mismatch" | 1 |
| `node_socket_dir` no existe | "Node socket directory not found: <path>" | 1 |
| `state_dir` no existe | Crear automáticamente | - |
| Región shm existe y owner está vivo | "Another process owns this router" | 1 |

### 35. Reset de Identidad

Para que un router "nazca de nuevo" con UUID diferente:

```bash
# Detener el router
systemctl stop json-router@RT.produccion.primary

# Borrar identity (esto borra el UUID)
rm -rf ./state/RT.produccion.primary/

# Reiniciar (generará nuevo UUID)
systemctl start json-router@RT.produccion.primary
```

**Advertencia:** Esto hace que el router sea una entidad nueva para la red. Los peers tendrán referencias al UUID viejo que quedarán huérfanas hasta que expiren.

### 36. Reset Completo de un Router

Para eliminar un router completamente y recrearlo desde cero:

```bash
# Detener
systemctl stop json-router@RT.produccion.primary

# Borrar config (forzará regeneración desde island.yaml)
rm -rf /etc/json-router/routers/RT.produccion.primary/

# Borrar identity
rm -rf ./state/RT.produccion.primary/

# Reiniciar (regenerará config desde island.yaml + nuevo UUID)
systemctl start json-router@RT.produccion.primary
```

### 37. Ejemplo Completo

#### Escenario: Isla "produccion" con dos routers

**Paso 1: Admin crea island.yaml**

```yaml
# /etc/json-router/island.yaml
island:
  id: produccion

defaults:
  paths:
    state_dir: ./state
    node_socket_dir: /var/run/mesh/nodes
    shm_prefix: /jsr-
  timers:
    hello_interval_ms: 10000
    dead_interval_ms: 40000
    heartbeat_interval_ms: 5000
    heartbeat_stale_ms: 30000

routers:
  - name: RT.produccion.primary
  - name: RT.produccion.secondary
```

**Paso 2: Primer arranque de RT.produccion.primary**

```bash
json-router --name RT.produccion.primary --config /etc/json-router
```

El sistema:
1. No encuentra `routers/RT.produccion.primary/config.yaml`
2. Lee `island.yaml`, valida que el router está autorizado
3. Crea `routers/RT.produccion.primary/config.yaml`:

```yaml
# Auto-generado desde island.yaml
router:
  name: RT.produccion.primary
  island_id: produccion

paths:
  state_dir: ./state
  node_socket_dir: /var/run/mesh/nodes
  shm_prefix: /jsr-

timers:
  hello_interval_ms: 10000
  dead_interval_ms: 40000
  heartbeat_interval_ms: 5000
  heartbeat_stale_ms: 30000

wan:
  # Configurar manualmente si se necesita WAN
  # listen: "0.0.0.0:9000"
  # uplinks:
  #   - address: "10.0.1.2:9000"
```

4. Genera `state/RT.produccion.primary/identity.yaml`:

```yaml
layer1:
  uuid: a1b2c3d4-e5f6-7890-abcd-ef1234567890
layer2:
  name: RT.produccion.primary
shm:
  name: /jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890
created_at: 2025-01-17T10:30:00Z
created_at_ms: 1737110400000
system:
  hostname: server-prod-01
  pid_at_creation: 12345
```

5. Arranca el router

**Paso 3: Admin configura WAN (si necesita)**

```bash
# Editar el config generado
vi /etc/json-router/routers/RT.produccion.primary/config.yaml
```

```yaml
# Agregar WAN
wan:
  listen: "10.0.1.1:9000"
  uplinks:
    - address: "10.0.1.2:9000"
    - address: "staging.internal:9000"
```

```bash
# Reiniciar para aplicar
systemctl restart json-router@RT.produccion.primary
```

**Paso 4: Siguientes arranques**

El router usa directamente `config.yaml` + `identity.yaml`. El `island.yaml` no se toca.

## Parte VII: Implementación

### 38. Router: Loop Principal

*Por definir: pseudocódigo del ciclo epoll/read/route/write.*

### 39. Librería de Nodo (Node.js)

*Por definir: API para que los nodos se comuniquen con el router.*

### 40. Librería de Nodo (Rust)

*Por definir: crate compartido para nodos escritos en Rust.*

---

## Apéndices

### A. Glosario

- **Nodo**: Proceso que procesa mensajes (AI, WF, IO, o SY).
- **Router**: Proceso que mueve mensajes entre nodos, identificado como RT.
- **Link**: Conexión socket entre nodo y router.
- **IP lógica**: Identificador único de un nodo en el sistema (UUID, capa 1).
- **Nombre capa 2**: Identificador descriptivo jerárquico (ej: AI.soporte.l1, RT.produccion.primary).
- **Rol**: Capacidad abstracta de un nodo (ej: "soporte", "facturación").
- **Framing**: Delimitación de mensajes en un stream de bytes (length prefix).
- **Isla**: Dominio local con sus routers, conectado a otras islas via WAN.
- **island.yaml**: Template + lista de routers autorizados (solo se lee para crear routers nuevos).
- **config.yaml**: Configuración real del router (autosuficiente, no depende de island.yaml en runtime).
- **identity.yaml**: Estado de hardware del router con UUID (auto-generado en primer arranque).
- **Región shm**: Área de shared memory de un router específico (cada router tiene la suya).
- **Región config shm**: Área de shared memory para rutas estáticas y VPNs (`/jsr-config-<island>`).
- **SY.config.routes**: Nodo de sistema responsable de escribir la región de config y propagar entre islas.
- **RIB**: Routing Information Base - todas las rutas candidatas (distribuidas en múltiples regiones).
- **FIB**: Forwarding Information Base - rutas ganadoras compiladas en memoria local.
- **Next-hop**: Router vecino al que enviar un mensaje (no el destino final).
- **LPM**: Longest Prefix Match - la ruta más específica gana.
- **Admin Distance**: Preferencia por origen de ruta (menor = preferido).
- **Seqlock**: Mecanismo de sincronización para un writer y múltiples readers usando contador atómico.
- **Heartbeat**: Timestamp actualizado periódicamente para detectar procesos muertos.
- **Generation**: Contador que incrementa cada vez que se recrea una región shm.
- **peer_links**: Tabla local que mapea UUID de peer a link_id del uplink.
- **VPN**: Túnel lógico entre islas con endpoints definidos.

### B. Decisiones de Diseño

| Decisión | Alternativa descartada | Razón |
|----------|----------------------|-------|
| `SOCK_STREAM` con framing | `SOCK_SEQPACKET` | Compatibilidad con Node.js, consistencia con WAN |
| Rust para router | Node.js | Performance, acceso a syscalls POSIX |
| Framing `uint32_be + JSON` | Delimitador de línea | Mensajes pueden contener newlines, binary-safe |
| Una región shm por router | Una región compartida por todos | Evita conflictos de escritura, seqlock funciona sin locks |
| Descubrimiento de shm via HELLO | Directorio fijo / scanning | Mecanismo natural, ya existe HELLO |
| Seqlock con AtomicU64 | RwLock / Mutex | RwLock no es process-shared en Rust, seqlock sin deadlocks |
| Acquire/Release orderings | SeqCst | Suficiente para coherencia, mejor performance |
| owner_start_time para stale | Solo PID | PID puede ser reciclado en Linux |
| start_time Linux-only + fallback | Implementar para todos los OS | Simplicidad, heartbeat cubre otros OS |
| shm.name truncado en macOS | Mismo formato que Linux | macOS limita a 31 chars, usar primeros 16 hex del UUID |
| generation en header | Ninguna | Permite detectar región recreada sin re-abrir |
| shm_unlink + recrear | Sobrescribir in-place | Readers viejos mantienen snapshot, más seguro |
| peer_links tabla local | En shm | Dinámica, depende de qué uplinks tengo |
| LINK_LOCAL solo para nodos propios | LINK_LOCAL para peers locales | Un nodo solo es accesible por el router que tiene su socket |
| Forwarding siempre via router dueño | Acceso directo a nodos de peers | Solo el router dueño tiene el socket del nodo |
| ROUTE_LSA para rutas remotas | ROUTE_REMOTE | Consistente con terminología de redes (LSA) |
| Shared memory para tablas | Redis/etcd | Latencia, sin dependencias externas |
| RouteEntry con next-hop | Destino final | Modelo estándar de redes, routing hop-by-hop |
| Strings con length explícito | Null-terminated | Comparaciones rápidas, evita escanear 256 bytes |
| UTF-8 para strings | ASCII estricto | Soporte internacional (español, etc.) |
| YAML para configuración | TOML | Más estándar, mejor soporte en editores |
| island.yaml como template, no runtime | Merge en runtime | Router autosuficiente, simplifica lógica |
| config.yaml completo y autosuficiente | Override parcial de island | Sin ambigüedades de merge, fácil de auditar |
| WAN en config.yaml del router | WAN en island.yaml | Cada router decide su propia conectividad WAN |
| UUID solo en identity.yaml | UUID por env/CLI | Capa 1 es hardware, nunca configurable manualmente |
| Solo 3 env vars (name, config, log) | Múltiples env vars | Simplicidad, evitar configuración dispersa |
| Router name capa 2 (RT.*) | Solo UUID | Consistente con nomenclatura de nodos, legible |
| state_dir relativo (./state) | Path absoluto fijo | Flexible, permite múltiples instancias en desarrollo |
| Región config shm separada | Rutas estáticas en cada router | Un escritor (SY), consistencia garantizada |
| SY.config.routes como nodo | Servicio externo | Integrado al sistema de mensajes, misma infra |
| SY propaga config, no router | Router propaga config | Separación data plane / control plane |
| Routers no escriben STATIC propias | Cada router con sus estáticas | Centralizado, fácil de auditar, sin conflictos |

### C. Referencias

- [OPA WASM](https://www.openpolicyagent.org/docs/latest/wasm/)
- [Unix domain sockets](https://man7.org/linux/man-pages/man7/unix.7.html)
- [POSIX shared memory](https://man7.org/linux/man-pages/man7/shm_overview.7.html)
- [OSPF Timers](https://datatracker.ietf.org/doc/html/rfc2328)
- [Seqlock](https://en.wikipedia.org/wiki/Seqlock)
- [Linux /proc/pid/stat](https://man7.org/linux/man-pages/man5/proc.5.html)
- [Rust Atomics and Locks](https://marabos.nl/atomics/)
- [memmap2 crate](https://docs.rs/memmap2)
- [nix crate](https://docs.rs/nix)
- [YAML 1.2 Spec](https://yaml.org/spec/1.2.2/)
