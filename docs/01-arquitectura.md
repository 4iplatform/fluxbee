# JSON Router - 01 Arquitectura

**Estado:** v1.13  
**Fecha:** 2025-01-20  
**Audiencia:** Todos (visión general del sistema)

---

## 1. Introducción

Este documento especifica un router de mensajes JSON diseñado para interconectar nodos heterogéneos en un sistema de agentes. Los nodos pueden ser:

- **Agentes AI**: Nodos con capacidad LLM para procesamiento de lenguaje natural.
- **Agentes WF**: Nodos con workflows estáticos y resolución determinística.
- **Nodos IO**: Adaptadores de medio (WhatsApp, email, Instagram, chat, etc.).
- **Nodos SY**: Infraestructura del sistema (tiempo, monitoreo, administración).

El sistema se configura mediante dos mecanismos ortogonales:

- **Tabla de ruteo**: Configuración estática de caminos y zonas VPN.
- **Policies OPA**: Reglas de negocio para resolución dinámica por rol/capacidad.

---

## 2. Principios de Diseño

**El router no almacena datos.** El router mueve mensajes entre colas. No persiste nada. Si necesita buffer de largo plazo, es responsabilidad del nodo.

**Las colas son de los nodos.** Cada nodo tiene su cola de entrada. La cola existe mientras el nodo existe. Si el nodo muere, la cola desaparece.

**Detección de link automática.** El router detecta cuando un nodo muere (link down) por señalización del sistema operativo, no por polling o heartbeat.

**Decisiones instantáneas.** El routing es O(1). Si una decisión toma tiempo, el modelo está mal.

**Separación de capas.** OPA decide qué rol/capacidad necesita un mensaje. El router decide qué nodo específico lo atiende.

---

## 3. Arquitectura de Dos Capas de Decisión

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

### 3.1 Capa OPA (Decisión de Negocio)

- Input: Mensaje y metadata únicamente.
- Output: Rol o capacidad requerida.
- No conoce: IPs, estado de nodos, carga, topología.

### 3.2 Capa Router (Decisión de Red)

- Input: Rol/capacidad requerida (de OPA) o IP explícita (del mensaje).
- Output: Socket concreto del nodo destino.
- Conoce: Todo el estado de la red via shared memory.

---

## 4. Componentes del Sistema

| Componente | Responsabilidad |
|------------|-----------------|
| Nodo AI | Procesa mensajes con LLM, responde según perfil/rol. |
| Nodo WF | Ejecuta workflows determinísticos. |
| Nodo IO | Adapta medios externos (WhatsApp, email, etc.) al protocolo interno. |
| Nodo SY | Provee servicios de infraestructura (tiempo, monitoreo, admin). |
| Router | Detecta nodos, conecta sockets, mantiene tabla local, consulta OPA si hace falta, rutea, detecta link down. |
| Gateway | Router especial que conecta islas entre sí via TCP/WAN. |
| **SY.orchestrator** | **Proceso raíz de la isla. Levanta router, nodos SY, y gestiona ciclo de vida.** |
| Shared Memory | Tablas de estado compartidas. Tres tipos de regiones (ver sección 7). |
| OPA (WASM) | Evalúa policies de negocio. No accede a estado del sistema. |
| Librería de Nodo | Común a todos los nodos. Maneja protocolo de socket, framing, retry, reconexión. |

### 4.1 Arranque de la Isla

```
Usuario ejecuta: systemctl start sy-orchestrator
                        │
                        ▼
              ┌─────────────────────┐
              │  SY.orchestrator    │ ← Proceso raíz
              └─────────────────────┘
                        │
          ┌─────────────┼─────────────┐
          ▼             ▼             ▼
    RT.gateway     SY.admin     SY.config.routes
    (router)       (API HTTP)   (rutas/VPN)
          │
          ▼
    Isla operativa
```

---

## 5. Tecnología Base

- **Router**: Rust (tokio, memmap2, raw-sync, nix, wasmtime)
- **Nodos**: Node.js (o cualquier lenguaje que soporte Unix sockets)
- **Sockets**: Unix domain sockets (`SOCK_STREAM`) con framing manual
- **Shared Memory**: POSIX shm (`shm_open`/`mmap`) con seqlock para sincronización
- **Policies**: OPA compilado a WASM, evaluado in-process
- **Formato de mensaje**: JSON con header de routing, framing con length prefix

---

## 6. Islas y Naming con @isla

### 6.1 Identidad de Isla

Toda instancia opera dentro de una **isla**, definida por el **único archivo de configuración**:

```
/etc/json-router/island.yaml
```

**Ejemplo mínimo:**

```yaml
island_id: "produccion"
```

**Ejemplo con WAN:**

```yaml
island_id: "produccion"

wan:
  listen: "0.0.0.0:9000"
  uplinks:
    - address: "staging.internal:9000"

admin:
  listen: "127.0.0.1:8080"
```

**Reglas:**
- Es el **único archivo** que el usuario crea/edita.
- Si no existe, el sistema NO arranca.
- Todo lo demás (rutas, VPN, nodos) se configura via API.
- Ver `07-operaciones.md` para detalle completo.

### 6.2 Definición de Isla

> **Isla = Host = Shared Memory**
>
> Todos los routers de una isla corren en el **mismo host** y comparten `/dev/shm`. 
> Si necesitás routers en hosts distintos, son **islas distintas** conectadas por WAN (TCP).

```
┌─────────────────────────────────────────────────────────────┐
│                        HOST FÍSICO                          │
│                         (Isla A)                            │
│                                                             │
│   ┌──────────┐   ┌──────────┐   ┌──────────┐              │
│   │ Router 1 │   │ Router 2 │   │ Gateway  │              │
│   └────┬─────┘   └────┬─────┘   └────┬─────┘              │
│        │              │              │                     │
│        └──────────────┼──────────────┘                     │
│                       │                                    │
│                       ▼                                    │
│              ┌─────────────────┐                           │
│              │   /dev/shm      │                           │
│              │                 │                           │
│              │ jsr-<uuid-1>    │  ← Región Router 1        │
│              │ jsr-<uuid-2>    │  ← Región Router 2        │
│              │ jsr-<uuid-gw>   │  ← Región Gateway         │
│              │ jsr-config-A    │  ← Región Config          │
│              │ jsr-lsa-A       │  ← Región LSA (remoto)    │
│              └─────────────────┘                           │
│                                                             │
│   [Nodos]  [Nodos]  [Nodos]                               │
└─────────────────────────────────────────────────────────────┘
```

### 6.3 Naming L2 con @isla (OBLIGATORIO)

Todo nombre L2 **incluye isla** con el sufijo `@`:

```
TYPE.campo1.campo2@isla
```

**Ejemplos:**

```
AI.soporte.l1@produccion
WF.invoice.process@staging
SY.config.routes@produccion
RT.primary@produccion
RT.gateway@produccion
```

**Reglas:**
- `TYPE` ∈ {AI, WF, IO, SY, RT}
- La isla es el sufijo después de `@`
- La **librería de nodo agrega @isla automáticamente** desde `island.yaml`
- El nodo solo configura `name: "AI.soporte.l1"`, la librería lo convierte a `AI.soporte.l1@produccion`

### 6.4 Routing Inter-Isla

El routing inter-isla se decide parseando `@isla` del destino:

```
Destino: AI.soporte.l1@staging

1. Parsear @isla → "staging"
2. Si staging == mi_isla → routing intra-isla (SHM local)
3. Si staging != mi_isla → buscar en jsr-lsa, enviar al gateway
```

---

## 7. Regiones de Shared Memory

El sistema usa **tres tipos** de regiones de memoria compartida:

```
/dev/shm/
├── jsr-<router-uuid>        # Una por router (nodos conectados)
├── jsr-config-<island>      # Una por isla (rutas estáticas, VPN)
└── jsr-lsa-<island>         # Una por isla (topología remota)
```

| Región | Quién escribe | Contenido | Quién lee |
|--------|---------------|-----------|-----------|
| `jsr-<uuid>` | Router dueño | Sus nodos CONNECTED | Todos los routers de la isla |
| `jsr-config-<island>` | SY.config.routes | Rutas estáticas, tabla VPN | Todos los routers de la isla |
| `jsr-lsa-<island>` | Gateway | Nodos, rutas, VPNs de **otras islas** | Todos los routers de la isla |

**Principio:** Un solo writer por región. Múltiples readers. Seqlock para sincronización.

---

## 8. Identificadores

El sistema usa dos capas de identificación independientes.

### 8.1 Identificador Capa 1 (UUID)

- **Formato**: UUID v4 (128 bits, auto-generado)
- **Propósito**: Identificación única del nodo en la red
- **Generación**: El nodo lo genera al arrancar, sin coordinación central

```
Ejemplo: "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
```

### 8.2 Identificador Capa 2 (Nombre Descriptivo)

- **Formato**: Campos separados por punto (`.`), con sufijo `@isla`
- **Máximo**: 10 campos (sin contar @isla)
- **Caracteres permitidos**: Alfanuméricos, guión bajo (`_`), guión medio (`-`)
- **Primer campo**: Tipo de nodo: `AI`, `IO`, `WF`, `SY`, `RT`

```
Formato: <tipo>.<campo2>.<campo3>...<campoN>@<isla>

Ejemplos:
  AI.soporte.l1.español@produccion
  IO.wapp.+5491155551234@produccion
  WF.notify.email@produccion
  SY.config.routes@produccion
  RT.gateway@produccion
```

### 8.3 Relación entre Capas

| Capa | Identifica | Único | Quién lo usa |
|------|-----------|-------|--------------|
| Capa 1 (UUID) | Instancia física | Sí, globalmente | Router para forwarding directo |
| Capa 2 (Nombre) | Perfil/capacidad | No necesariamente | OPA para decisión de routing |

---

## 9. Caracterización de Nodos por Tipo

### 9.1 Nodos AI (Agentes LLM)

```
AI.<área>.<cargo>.<nivel>.<especialización>@<isla>
```

**Ejemplos:**
```
AI.soporte.analista.l1.tecnico@produccion
AI.ventas.ejecutivo.closer@produccion
```

### 9.2 Nodos IO (Adaptación de Medio)

```
IO.<medio>.<identificador>@<isla>
```

**Ejemplos:**
```
IO.wapp.+5491155551234@produccion
IO.email.ventas@empresa.com@produccion
```

### 9.3 Nodos WF (Workflows Estáticos)

```
WF.<verbo>.<objeto>@<isla>
```

**Ejemplos:**
```
WF.send.email@produccion
WF.query.crm@produccion
```

### 9.4 Nodos SY (Sistema)

```
SY.<servicio>@<isla>
```

**Ejemplos:**
```
SY.config.routes@produccion
SY.opa.rules@produccion
SY.time@produccion
```

**IMPORTANTE:** Los nodos SY.* son **inmunes a filtros VPN**. Pueden ver y comunicarse con todos los nodos de la isla, independientemente del VPN.

### 9.5 Nodos RT (Routers)

```
RT.<rol>@<isla>
```

**Ejemplos:**
```
RT.primary@produccion
RT.secondary@produccion
RT.gateway@produccion
```

---

## 10. VPN = Zonas de Aislamiento

### 10.1 Definición

Una **VPN** es una subred aislada dentro de una isla. Sirve para que sistemas que corren en la misma isla (ej: contable y soporte) **nunca se vean entre sí**.

- Es como tener "mini-islas" dentro de la isla física
- El aislamiento es **arquitectónico**, no policy
- Sin configuración de leak, el aislamiento es total

### 10.2 Modelo Simple

Solo una tabla de asignación en `jsr-config-<island>`:

```
pattern           → vpn_id
─────────────────────────────
AI.soporte.*      → 10
AI.ventas.*       → 20
WF.crm.*          → 20
(default)         → 0
```

**Reglas:**
- Cuando un nodo conecta, el router busca en la tabla qué VPN le corresponde
- Si no matchea ningún pattern → `vpn_id = 0` (global)
- Al rutear, el nodo solo ve nodos de su mismo VPN
- **Excepción:** Nodos SY.* ignoran VPN, ven todo

### 10.3 Ejemplo Visual

```
┌─────────────────────────────────────────────────────────────┐
│                    Isla "produccion"                        │
│                                                             │
│   ┌─────────────────────┐    ┌─────────────────────┐       │
│   │   VPN 0 (global)    │    │   VPN 10 (soporte)  │       │
│   │                     │    │                     │       │
│   │  SY.config.routes   │    │  AI.soporte.l1      │       │
│   │  SY.time            │    │  AI.soporte.l2      │       │
│   │  RT.gateway         │    │  WF.ticket          │       │
│   └─────────────────────┘    └─────────────────────┘       │
│            │                           │                    │
│            │  SY.* ve todo             │ No se ven         │
│            │◄──────────────────────────┤                    │
│            │                           │                    │
│   ┌─────────────────────┐              │                    │
│   │   VPN 20 (ventas)   │◄─────────────┘                    │
│   │                     │   (aislados entre sí)             │
│   │  AI.ventas.closer   │                                   │
│   │  WF.crm             │                                   │
│   └─────────────────────┘                                   │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

---

## 11. Gateway e Inter-Isla

### 11.1 Gateway Único por Isla

Cada isla tiene **un único router gateway** que:

- Mantiene conexiones TCP a otras islas
- Recibe LSA de otras islas y lo escribe en `jsr-lsa-<island>`
- Envía LSA de su isla a los gateways remotos

### 11.2 LSA entre Gateways

El gateway consolida toda la información de su isla y la envía a los peers:

```
Gateway lee:
  - Todas las jsr-<router-uuid> → nodos locales
  - jsr-config-<island> → rutas y VPNs

Gateway envía LSA con:
  - Lista de nodos (uuid, name@isla, vpn)
  - Lista de rutas estáticas
  - Lista de asignaciones VPN

Gateway remoto recibe y escribe en:
  - jsr-lsa-<island> → topología de islas remotas
```

### 11.3 Visibilidad Inter-Isla

Todos los routers de la isla pueden leer `jsr-lsa-<island>` y saber:

- Qué nodos existen en @staging, @desarrollo, etc.
- Qué rutas estáticas tienen
- Cómo están configuradas sus VPNs

Esto permite routing informado sin consultar al gateway.

---

## 12. Referencias a Otros Documentos

| Tema | Documento |
|------|-----------|
| Estructura de mensajes, framing, HELLO | `02-protocolo.md` |
| Shared memory, estructuras Rust | `03-shm.md` |
| Routing, FIB, algoritmos | `04-routing.md` |
| IRP intra-isla, WAN inter-isla, LSA | `05-conectividad.md` |
| Región config y LSA, estructuras | `06-regiones.md` |
| Arranque, YAML, systemd | `07-operaciones.md` |
| Glosario, decisiones de diseño | `08-apendices.md` |
| Nodos SY | `SY_nodes_spec.md` |
