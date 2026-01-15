# JSON Router - Especificación Técnica

**Estado:** Draft v0.1
**Fecha:** 2025-01-15

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
| Librería de Nodo | Común a todos los nodos. Maneja protocolo de socket, retry, reconexión. |

### 5. Tecnología Base

- **Runtime**: Node.js
- **Sockets**: Unix domain sockets (SOCK_SEQPACKET) para detección automática de link down.
- **Shared Memory**: Addon nativo C++ sobre `shm_open`/`mmap`.
- **Policies**: OPA compilado a WASM, evaluado in-process.
- **Formato de mensaje**: JSON con header de routing.

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
- **Primer campo**: Obligatorio, indica tipo de nodo. Valores válidos: `AI`, `IO`, `WF`.
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

La estructura del nombre capa 2 varía según el tipo de nodo. El primer campo siempre indica el tipo (`AI`, `IO`, `WF`), pero los campos siguientes siguen convenciones distintas según la naturaleza del nodo.

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
```

**Nodo SY.time (Time Server):**

El servicio de tiempo es responsabilidad del router o de un nodo SY.time dedicado. Provee tiempo UTC sincronizado a toda la red mediante broadcast periódico.

#### Resumen de Convenciones

| Tipo | Modelo conceptual | Estructura |
|------|------------------|------------|
| AI | Recursos Humanos (personas con roles) | `AI.<área>.<cargo>.<nivel>.<especialización>.<turno>` |
| IO | Infraestructura de comunicación (canales) | `IO.<medio>.<identificador>` |
| WF | APIs/Programación (acciones) | `WF.<verbo>.<objeto>.<variante>` |
| SY | Infraestructura del sistema (servicios) | `SY.<servicio>.<instancia>` |

Esta separación permite que cada tipo de nodo se describa con el vocabulario natural de su dominio, manteniendo el sistema unificado a través del primer campo de tipo.

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
| `dst` | UUID o null | Sí | Nodo destino. Si null, requiere resolución OPA |
| `ttl` | int | Sí | Time-to-live, decrementa en cada hop. Si llega a 0, drop. |
| `trace_id` | UUID | Sí | ID de correlación para trazabilidad |

#### 7.2 Sección `meta` (Metadata para OPA)

Usado por OPA para decisiones de capa 2. El router pasa esta sección completa a OPA sin interpretarla.

```json
{
  "meta": {
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
| `target` | string (nombre capa 2) | Sí si dst es null | Perfil/capacidad requerida |
| `priority` | string | No | Hint de prioridad para OPA |
| `context` | object | No | Datos adicionales para reglas OPA |

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

### 8. Flujo de Resolución

```
Mensaje llega al router
        │
        ▼
  ┌─────────────────┐
  │ Leer routing.dst │
  └─────────────────┘
        │
        ▼
  ┌─────────────────┐     dst tiene valor
  │ ¿dst es null?   │ ───────────────────────► Forward directo a UUID
  └─────────────────┘
        │ dst es null
        ▼
  ┌─────────────────┐
  │ Pasar meta a OPA │
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
    Forward a UUID elegido
```

---

## Parte III: Infraestructura de Red

### 11. Sockets y Detección de Link

#### 11.1 Tipo de Socket

El sistema usa **Unix domain sockets** con `SOCK_SEQPACKET`:

- Mensajes discretos (no stream): un send = un receive completo
- Detección automática de desconexión: si el nodo muere, el router recibe error
- Sin framing manual: el kernel delimita mensajes
- Límite por mensaje: ~64-200KB (configurable)

#### 11.2 Ubicación de Sockets

Los nodos crean sus sockets en un directorio conocido:

```
/var/run/mesh/nodes/<uuid>.sock
```

Los routers monitorean este directorio con `inotify` para detectar nodos nuevos sin polling.

#### 11.3 Modelo de Conexión

El nodo es pasivo, el router es activo:

**Nodo:**
```
socket() → bind() → listen(fd, 1) → accept()
```

**Router:**
```
inotify detecta socket nuevo → random backoff → connect()
```

El `listen(fd, 1)` con backlog 1 garantiza que solo un router puede conectar. El primero que hace `connect()` gana, los demás reciben error.

#### 11.4 Detección de Link Down

El socket señala automáticamente cuando el nodo muere:

| Evento | Qué pasa | Acción del router |
|--------|----------|-------------------|
| Nodo termina limpio | `close()` del socket | Router recibe EOF en próximo read |
| Nodo crashea | Kernel cierra socket | Router recibe `ECONNRESET` o `EPIPE` |
| Nodo se cuelga | Timeout en operación | Router detecta por inactividad |

No hay heartbeat ni polling. El kernel notifica.

### 12. Shared Memory

#### 12.1 Qué vive en Shared Memory

| Dato | Propósito | Quién escribe | Quién lee |
|------|-----------|---------------|-----------|
| Tabla de ruteo | Rutas estáticas, VPNs, forwards | Admin/Config | Routers |
| Registro de nodos | UUID ↔ nombre capa 2 ↔ router dueño | Router que conectó | Todos los routers |
| Tabla de routers | Routers activos, hops entre ellos | Routers (HELLO/LSA) | Todos los routers |

#### 12.2 Qué NO vive en Shared Memory

| Dato | Por qué no | Dónde vive |
|------|------------|------------|
| Estado del link (up/down) | El socket lo indica | Local en cada router |
| Carga del nodo | Cambia muy rápido, causaría contención | Local en cada router |
| Cola de mensajes | Es del nodo, no de la red | Buffer del socket / nodo |

#### 12.3 Estructura de Registro de Nodo

```c
struct node_entry {
    uuid_t      uuid;           // Capa 1: identificador único
    char        name[256];      // Capa 2: AI.soporte.l1.español
    uuid_t      router_owner;   // Qué router lo tiene conectado
    uint64_t    connected_at;   // Timestamp de conexión
};
```

#### 12.4 Implementación en Node.js

Addon nativo C++ usando:
- `shm_open()` / `mmap()` para la región compartida
- `pthread_rwlock` con `PTHREAD_PROCESS_SHARED` para sincronización

El addon expone funciones simples:
```javascript
shm.getRoutes()
shm.getNodes()
shm.registerNode(uuid, name)
shm.unregisterNode(uuid)
shm.getRouters()
```

### 13. Ciclo de Vida de Nodos

#### 13.1 Registro (Nodo Nuevo)

```
1. Nodo arranca
2. Genera UUID (capa 1)
3. Crea socket en /var/run/mesh/nodes/<uuid>.sock
4. listen(fd, 1)
5. Router detecta socket nuevo (inotify)
6. Router espera random backoff (0-100ms)
7. Router intenta connect()
   - Éxito: Router registra nodo en shared memory
   - Falla: Otro router lo tomó, ignorar
8. Router envía QUERY al nodo
9. Nodo responde ANNOUNCE con su nombre capa 2
10. Router actualiza registro con nombre
11. Router envía BCAST_ANNOUNCE a la red
```

#### 13.2 Operación Normal

```
Nodo ←──socket──→ Router ←──shared memory──→ Otros Routers
         │
         ▼
    Mensajes JSON
```

#### 13.3 Desconexión Limpia

```
1. Nodo decide apagar
2. Nodo envía WITHDRAW al router
3. Nodo hace close() del socket
4. Router detecta cierre
5. Router elimina nodo de shared memory
6. Router envía actualización LSA a otros routers
```

#### 13.4 Desconexión por Falla

```
1. Nodo crashea o se cuelga
2. Kernel cierra socket (o router detecta timeout)
3. Router detecta error en próximo read/write
4. Router elimina nodo de shared memory
5. Router envía UNREACHABLE a nodos que tenían mensajes pendientes
6. Router envía actualización LSA a otros routers
```

#### 13.5 Reconexión

```
1. Nodo se recupera, arranca de nuevo
2. Genera mismo o nuevo UUID (política del nodo)
3. Crea socket, listen()
4. Router detecta, conecta, registra
5. Flujo normal de QUERY/ANNOUNCE
```

### 14. Coordinación entre Routers

#### 14.1 Descubrimiento de Routers

Los routers se descubren entre sí mediante mensajes HELLO en un canal dedicado (socket o multicast en shared memory).

#### 14.2 Random Backoff para Evitar Colisiones

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

#### 14.3 Sincronización de Tablas

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

### 15. Timers del Sistema

Basados en estándares de OSPF y BGP.

#### 15.1 Timers de Mensaje

| Timer | Default | Configurable | Propósito |
|-------|---------|--------------|-----------|
| **TTL** | 16 hops | Sí | Máximo saltos antes de drop |
| **Message Timeout** | 30s | Sí | Tiempo máximo esperando respuesta |
| **Retransmit Interval** | 5s | Sí | Reenviar si no hay ACK |

#### 15.2 Timers de Router

| Timer | Default | Configurable | Propósito |
|-------|---------|--------------|-----------|
| **Hello Interval** | 10s | Sí | Cada cuánto anunciar existencia a otros routers |
| **Dead Interval** | 40s (4x hello) | Sí | Sin hello = router marcado como caído |
| **Route Refresh** | 300s (5min) | Sí | Refrescar tabla de rutas entre routers |
| **Connect Backoff Max** | 100ms | Sí | Máximo random delay antes de conectar a socket nuevo |
| **Time Sync Interval** | 60s | Sí | Cada cuánto emitir TIME_SYNC broadcast |

#### 15.3 Timers de Nodo

| Timer | Default | Configurable | Propósito |
|-------|---------|--------------|-----------|
| **Reconnect Interval** | 5s | Sí | Tiempo entre reintentos de conexión |
| **Reconnect Max Attempts** | 10 | Sí | Intentos antes de desistir |

#### 15.4 Configuración de Timers

Los timers se configuran en el archivo de configuración del router:

```json
{
  "timers": {
    "ttl_default": 16,
    "message_timeout": 30000,
    "retransmit_interval": 5000,
    "hello_interval": 10000,
    "dead_interval": 40000,
    "route_refresh": 300000,
    "connect_backoff_max": 100,
    "time_sync_interval": 60000
  }
}
```

Todos los valores en milisegundos excepto TTL (hops).

### 9. Tipos de Mensaje por Tamaño

El sistema maneja mensajes de diferentes tamaños con estrategias distintas.

#### 9.1 Mensaje Inline (< 64KB)

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

#### 9.2 Mensaje por Referencia (> 64KB)

El payload es un archivo en disco. El JSON lleva el path. El nodo destino lo lee del filesystem compartido.

```json
{
  "routing": { ... },
  "meta": { ... },
  "payload": {
    "type": "file_ref",
    "path": "/var/spool/mesh/blobs/uuid-del-archivo",
    "mime": "image/png",
    "size": 5242880
  }
}
```

**Flujo para mensaje grande:**

1. Nodo origen recibe/genera archivo grande
2. Nodo origen guarda archivo en `/var/spool/mesh/blobs/`
3. Nodo origen manda JSON con `file_ref` al router
4. Router rutea el JSON (chiquito) al nodo destino
5. Nodo destino lee el archivo del path
6. Cleanup externo (proceso de mantenimiento) borra archivos viejos

**Storage de blobs:**

- Directorio compartido entre todos los nodos
- Montaje local o NFS según infraestructura
- Sin HTTP, acceso directo a disco
- Mantenimiento externo, los nodos no borran

#### 9.3 Mensaje de Sistema (< 1KB)

Mensajes de control de la red. Van por el mismo canal pero con `meta.type: "system"`. El router puede procesarlos él mismo en lugar de hacer forward.

### 10. Mensajes de Sistema

Nomenclatura basada en estándares de red existentes.

#### 10.1 Descubrimiento y Registro

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `ANNOUNCE` | Nodo | Router | Nodo anuncia existencia (UUID + nombre capa 2) |
| `WITHDRAW` | Nodo | Router | Nodo anuncia shutdown limpio |
| `QUERY` | Router | Nodo | Router pregunta identidad a socket nuevo |

#### 10.2 Health y Diagnóstico (ICMP-like)

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `ECHO` | Cualquiera | Cualquiera | Ping |
| `ECHO_REPLY` | Cualquiera | Cualquiera | Pong |
| `UNREACHABLE` | Router | Nodo origen | Destino no existe |
| `TTL_EXCEEDED` | Router | Nodo origen | TTL llegó a 0 |
| `SOURCE_QUENCH` | Router/Nodo | Nodo origen | Backpressure, bajar velocidad |

#### 10.3 Routing (entre routers)

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `HELLO` | Router | Routers | Anunciar existencia |
| `LSA` | Router | Routers | Link State Advertisement, compartir nodos conectados |
| `SYNC_REQUEST` | Router | Router | Pedir tabla completa |
| `SYNC_REPLY` | Router | Router | Respuesta con tabla completa |

#### 10.4 Administrativos (unicast)

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

#### 10.5 Administrativos (broadcast)

Mensajes dirigidos a toda la red o grupo de routers.

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `BCAST_SHUTDOWN` | Admin | Todos | Shutdown general |
| `BCAST_RELOAD` | Admin | Todos | Recargar configuración en todos |
| `BCAST_CONFIG` | Admin | Todos | Distribuir nueva configuración |
| `BCAST_ANNOUNCE` | Router | Todos | Anunciar nodo nuevo en la red |
| `TIME_SYNC` | Router/SY.time | Todos | Broadcast de tiempo UTC sincronizado |

#### 10.6 Tiempo del Sistema

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

#### 10.7 Estado

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

#### 10.8 Formato de Mensaje de Sistema

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

### 16. Semántica de Comunicación

#### 16.1 Modelo: Request/Response End-to-End (tipo TCP)

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

#### 16.2 Principios

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

#### 16.3 Contrato del Protocolo

| Campo | Obligatorio | Propósito |
|-------|-------------|-----------|
| `msg_id` | Sí (en REQ) | Identificador único de la solicitud |
| `msg_id` | Sí (en RESP) | Correlation ID para cerrar la sesión |
| `deadline` | Definido por emisor | Timeout de la solicitud |

El emisor controla el timeout, no el router. El router no trackea sesiones "abiertas".

#### 16.4 Señales del Router

Cuando el router no puede aceptar o enrutar, responde inmediatamente con error:

| Señal | Origen | Significado |
|-------|--------|-------------|
| `UNREACHABLE` | Router | No existe ruta, no hay nodos con ese rol, destino no existe |
| `SOURCE_QUENCH` | Router o Nodo | Cola llena, backpressure, bajar velocidad |
| `TTL_EXCEEDED` | Router | Mensaje excedió límite de hops |

Estas señales NO implican que el mensaje fue procesado. Solo indican que el router no pudo completar el forwarding.

#### 16.5 Estados Finales de Sesión

Una sesión request/response termina en uno de estos estados:

| Estado | Qué pasó | Acción típica |
|--------|----------|---------------|
| `SUCCESS` | Recibió RESP con resultado válido | Continuar |
| `REMOTE_ERROR` | Recibió RESP con error de aplicación | Manejar error |
| `ROUTER_ERROR` | Recibió señal del router (UNREACHABLE, etc.) | Retry o abortar |
| `TIMEOUT` | No recibió respuesta antes del deadline | Ambiguo, decidir retry |

#### 16.6 Ambigüedad del Timeout

El timeout es el caso ambiguo. El emisor no puede saber:

| Escenario | Qué pasó realmente | Qué ve el emisor |
|-----------|-------------------|------------------|
| Mensaje nunca llegó a B | Perdido en tránsito | Timeout |
| B recibió, crasheó antes de responder | Puede haber procesado | Timeout |
| B procesó, respuesta se perdió | Procesado pero sin confirmación | Timeout |

En todos los casos el emisor ve lo mismo: timeout. Por eso **el retry es decisión de negocio**: si el emisor reintenta, el destino podría recibir el mensaje dos veces. La aplicación debe manejar idempotencia o aceptar duplicados.

#### 16.7 Comparación con TCP

| Aspecto | TCP | Este sistema |
|---------|-----|--------------|
| ACK de quién | Del extremo final | Del extremo final (RESP) |
| ACK intermedio | No (routers IP no confirman) | No (router no confirma) |
| Retransmisión | Automática por el stack | Manual, decisión de negocio |
| Orden | Garantizado | Garantizado (SOCK_SEQPACKET) |
| Detección de pérdida | Timeout + ACK duplicados | Timeout |

La diferencia clave: TCP retransmite automáticamente porque son bytes sin semántica. Acá son mensajes con posibles efectos secundarios, entonces el retry lo decide la aplicación.

### 17. Blob Store

#### 17.1 Objetivo

Transportar payloads grandes sin fragmentación ni streaming. El payload se consolida completamente en origen antes de ser referenciado y enviado por JSON.

El blob store es un filesystem compartido entre todos los nodos. La infraestructura resuelve el acceso (NFS, mount compartido, etc.).

#### 17.2 Umbral Inline vs Referencia

| Tamaño | Transporte |
|--------|------------|
| `<= MAX_INLINE_BYTES` (64KB) | Inline en JSON |
| `> MAX_INLINE_BYTES` | Referencia `blob_ref` |

#### 17.3 Estructura de Directorios

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

#### 17.4 Identidad del Blob

| Campo | Obligatorio | Descripción |
|-------|-------------|-------------|
| `blob_id` | Sí | Opaco, recomendado: `sha256:<hash>` (content-addressed) |
| `sha256` | Sí | Hash del contenido para validación |
| `size` | Sí | Tamaño en bytes |
| `mime` | Sí | Tipo de contenido para el receptor |
| `spool_day` | Sí | Fecha UTC de creación (YYYY-MM-DD) |

Content-addressed significa: si dos nodos mandan el mismo archivo, es el mismo blob. Deduplicación automática.

#### 17.5 Escritura Atómica (Sellado)

Un blob es válido solo si existe en `objects/`. Nunca leer desde `tmp/`.

```
1. Escribir a ${...}/tmp/<blob_id>.<pid>.<rand>.tmp
2. fsync(file) + fsync(dir)
3. rename() atómico a ${...}/objects/<blob_id>.blob
```

El `rename()` es atómico en POSIX. Si existe en `objects/`, está completo.

#### 17.6 Mensaje blob_ref

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

#### 17.7 Lectura y Validación

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

#### 17.8 Pins (Locks de Uso)

Para evitar que el GC borre blobs en uso:

```
Al comenzar a procesar: crear ${...}/pins/<blob_id>.<node_id>
Al terminar: borrar ese pin
```

El GC no borra días con pins activos.

#### 17.9 Retención y Garbage Collection

Estrategia: borrar días completos (O(#días), no O(#archivos)).

```
RETENTION_DAYS >= 2 (recomendado)

Candidato a borrar: directorio con fecha < today_utc - RETENTION_DAYS
Condición: pins/ vacío o no existe
Acción: borrar directorio del día completo
```

El sweeper corre periódicamente (configurable, ej. cada hora).

#### 17.10 Límites Operativos

| Límite | Propósito |
|--------|-----------|
| `MAX_BLOB_SIZE` | Hard limit por blob individual |
| `MAX_SPOOL_BYTES` | Cuota total del spool |

Si se excede `MAX_SPOOL_BYTES`, rechazar nuevas escrituras con `PAYLOAD_TOO_LARGE`.

#### 17.11 Semántica

- **Inmutable:** write-once, nunca se modifica un blob
- **Best-effort:** el blob store no implica delivery garantizado
- **No streaming:** el payload no se expone al receptor hasta validación completa

#### 17.12 Señales de Error

| Señal | Cuándo |
|-------|--------|
| `PAYLOAD_TOO_LARGE` | Blob excede `MAX_BLOB_SIZE` o spool excede cuota |
| `BLOB_NOT_FOUND` | Receptor no encuentra blob (incluso con fallback ±24h) |
| `BLOB_CORRUPTED` | Blob existe pero no valida `size` o `sha256` |

### 18. Uplink WAN (Router ↔ Router)

#### 18.1 Objetivo

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

#### 18.2 Transporte

| Contexto | Transporte | Framing |
|----------|------------|---------|
| LAN/Local (nodos) | Unix domain socket `SOCK_SEQPACKET` | Automático (kernel) |
| WAN (routers) | TCP stream | Manual: `uint32_be length` + `JSON bytes` |

El framing WAN:
```
┌──────────────┬─────────────────────┐
│ length (4B)  │ JSON message        │
│ big-endian   │ (length bytes)      │
└──────────────┴─────────────────────┘
```

`length` incluye solo el JSON, no el header de 4 bytes.

El contenido del mensaje es el mismo JSON del protocolo (routing/meta/payload).

#### 18.3 Mensajes Reutilizados

El uplink WAN usa los mismos mensajes de routing entre routers ya definidos:

| Mensaje | Propósito en WAN |
|---------|------------------|
| `HELLO` | Anunciar existencia, keepalive |
| `LSA` | Propagar cambios de topología entre islas |
| `SYNC_REQUEST` | Pedir tabla completa al conectar |
| `SYNC_REPLY` | Responder con tabla completa |

No se inventa protocolo nuevo para WAN.

#### 18.4 Mensajes de Handshake WAN

Dos mensajes adicionales para negociación explícita del uplink:

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `UPLINK_ACCEPT` | Router | Router | Confirma aceptación del peer, versión y capabilities |
| `UPLINK_REJECT` | Router | Router | Rechaza conexión con motivo explícito |

#### 18.5 Handshake del Uplink

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

#### 18.6 Payload del HELLO (WAN extendido)

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
| `capabilities` | Qué soporta este router |
| `timers` | Intervalos de hello/dead para sincronizar |

#### 18.7 Payload de UPLINK_ACCEPT

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

#### 18.8 Payload de UPLINK_REJECT

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

#### 18.9 Operación Normal

Una vez establecido el uplink:

- **HELLO periódico** cada `hello_interval_ms`
- **Dead detection**: sin HELLO por `dead_interval_ms` = peer caído, invalidar rutas
- **LSA incremental**: cambios de topología se propagan con LSA
- **TTL**: se decrementa por hop, previene loops entre islas

#### 18.10 Forwarding entre Islas

Cuando un mensaje tiene destino en otra isla:

```
1. Router A recibe mensaje para nodo en Isla B
2. Router A busca en tabla: nodo está en Isla B via Router B
3. Router A decrementa TTL
4. Router A envía mensaje por uplink TCP a Router B
5. Router B recibe, busca nodo local, entrega por Unix socket
```

El mensaje JSON es idéntico. Solo cambia el transporte.

#### 18.11 Seguridad

La autenticación se delega al edge proxy (NGINX, Envoy, etc.):

| Capa | Responsable | Mecanismo |
|------|-------------|-----------|
| Transporte | Edge proxy | TLS/mTLS |
| Autorización | Edge proxy | Allowlist de IPs/certs |
| Rate limiting | Edge proxy | Por conexión/bytes |
| Protocolo | Router | HELLO + UPLINK_ACCEPT/REJECT |

El proxy no interpreta el protocolo. Es transparente L4.

La validación de `island_id` contra lista de islas autorizadas puede hacerse en el router al recibir HELLO, respondiendo `UPLINK_REJECT` si no está autorizado.

---

## Parte IV: Routing

### 16. Tabla de Ruteo Estática

*Por definir: formato de rutas, matching de destinos, prioridades.*

### 17. Policies OPA

*Por definir: estructura de policies, input/output contract, ejemplos.*

### 18. Balanceo entre Nodos del Mismo Rol

*Por definir: estrategia cuando hay múltiples nodos con la capacidad requerida.*

---

## Parte V: Operación

### 19. Estados de la Red

*Por definir: estados posibles de nodos y links, transiciones, eventos.*

### 20. Eventos del Sistema

*Por definir: qué eventos se generan, quién los consume, formato.*

### 21. Broadcast y Multicast

*Por definir: cómo mandar un mensaje a múltiples destinos, casos de uso.*

### 22. Mantenimiento y Administración

*Por definir: herramientas de diagnóstico, limpieza de links huérfanos, monitoreo.*

---

## Parte VI: Implementación

### 23. Router: Loop Principal

*Por definir: pseudocódigo del ciclo epoll/read/route/write.*

### 24. Librería de Nodo

*Por definir: API para que los nodos se comuniquen con el router.*

### 25. Addon de Shared Memory

*Por definir: interfaz del módulo nativo para Node.js.*

---

## Apéndices

### A. Glosario

- **Nodo**: Proceso que procesa mensajes (AI, WF, o IO).
- **Router**: Proceso que mueve mensajes entre nodos.
- **Link**: Conexión socket entre nodo y router.
- **IP lógica**: Identificador único de un nodo en el sistema.
- **Rol**: Capacidad abstracta de un nodo (ej: "soporte", "facturación").

### B. Decisiones de Diseño

*Registro de decisiones tomadas y alternativas descartadas.*

### C. Referencias

*Links a documentación de OPA, Unix domain sockets, shared memory POSIX.*
