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
| Nodo | Crea su socket, espera conexión, procesa mensajes. Maneja su propio buffer largo si lo necesita. |
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
- Primer campo es `AI`, `IO`, o `WF`
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

#### Resumen de Convenciones

| Tipo | Modelo conceptual | Estructura |
|------|------------------|------------|
| AI | Recursos Humanos (personas con roles) | `AI.<área>.<cargo>.<nivel>.<especialización>.<turno>` |
| IO | Infraestructura de comunicación (canales) | `IO.<medio>.<identificador>` |
| WF | APIs/Programación (acciones) | `WF.<verbo>.<objeto>.<variante>` |

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
    "connect_backoff_max": 100
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

#### 10.6 Estado

Cada componente define su propio mensaje de estado con información relevante a su tipo.

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `STATE` | Nodo/Router | Quien solicitó | Estado interno del componente |

**Contenido de STATE según tipo:**

- **Router:** Nodos conectados, tabla de ruteo, métricas de tráfico, uptime
- **Nodo AI:** Modelo cargado, requests en proceso, memoria usada
- **Nodo IO:** Canal conectado, mensajes en cola, última actividad
- **Nodo WF:** Workflows activos, ejecuciones pendientes

#### 10.7 Formato de Mensaje de Sistema

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
