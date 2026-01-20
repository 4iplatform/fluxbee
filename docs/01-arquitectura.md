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

- **Tabla de ruteo**: Configuración estática de caminos (IP lógica, forwards, zonas VPN).
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
| Shared Memory | Tabla de ruteo y estado de nodos. Compartida entre routers, no accesible por nodos. |
| OPA (WASM) | Evalúa policies de negocio. No accede a estado del sistema. |
| Librería de Nodo | Común a todos los nodos. Maneja protocolo de socket, framing, retry, reconexión. |

---

## 5. Tecnología Base

- **Router**: Rust (tokio, memmap2, raw-sync, nix, wasmtime)
- **Nodos**: Node.js (o cualquier lenguaje que soporte Unix sockets)
- **Sockets**: Unix domain sockets (`SOCK_STREAM`) con framing manual
- **Shared Memory**: POSIX shm (`shm_open`/`mmap`) con seqlock para sincronización
- **Policies**: OPA compilado a WASM, evaluado in-process
- **Formato de mensaje**: JSON con header de routing, framing con length prefix

---

## 6. Islas y Naming con @isla (v1.13)

### 6.1 Identidad de Isla

Toda instancia (routers y nodos) opera dentro de una **isla**, definida por el archivo:

```
/etc/json-router/island.yaml
```

Contenido mínimo:

```yaml
island_id: "produccion"
```

**Regla:**
- Todos los procesos de la isla leen este archivo.
- La isla no se infiere por hostname ni por parámetros sueltos.

### 6.2 Definición de Isla

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

### 6.3 Naming L2 con @isla (v1.13 - OBLIGATORIO)

Todo nombre L2 **incluye isla** con el sufijo `@`:

```
TYPE.campo1.campo2@isla
```

**Ejemplos:**

```
AI.soporte.l1@produccion
WF.invoice.1@staging
SY.config.routes@produccion
SY.orchestrator@produccion
RT.primary@produccion
```

**Reglas:**
- `TYPE` ∈ {AI, WF, IO, SY, RT}
- La isla es el sufijo luego de `@`
- **No se usa** el formato viejo `SY.xxx.{isla}` (deprecado)

### 6.4 Consecuencia sobre Routing Inter-Isla

El routing inter-isla se decide parseando `@isla` del destino. No hay propagación de rutas de nodos por broadcast entre islas.

```
Destino: AI.soporte.l1@staging

1. Parsear @isla → "staging"
2. Si staging == local_island → routing intra-isla
3. Si staging != local_island → enviar al gateway inter-isla
```

---

## 7. Identificadores

El sistema usa dos capas de identificación independientes.

### 7.1 Identificador Capa 1 (UUID)

- **Formato**: UUID v4 (128 bits, auto-generado)
- **Propósito**: Identificación única del nodo en la red
- **Generación**: El nodo lo genera al arrancar, sin coordinación central
- **Unicidad**: Garantizada por probabilidad matemática, no por validación

```
Ejemplo: "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
```

### 7.2 Identificador Capa 2 (Nombre Descriptivo)

- **Formato**: Campos separados por punto (`.`), con sufijo `@isla`
- **Máximo**: 10 campos (sin contar @isla)
- **Caracteres permitidos**: Alfanuméricos, guión bajo (`_`), guión medio (`-`). Sin espacios.
- **Primer campo**: Obligatorio, indica tipo de nodo: `AI`, `IO`, `WF`, `SY`, `RT`.

```
Formato: <tipo>.<campo2>.<campo3>...<campoN>@<isla>

Ejemplos:
  AI.soporte.l1.español@produccion
  AI.ventas.bdr.tecnico@staging
  IO.wapp.+5491155551234@produccion
  WF.notify.email@produccion
  SY.time.primary@produccion
  RT.primary@produccion
```

### 7.3 Relación entre Capas

| Capa | Identifica | Único | Quién lo usa |
|------|-----------|-------|--------------|
| Capa 1 (UUID) | Instancia física del nodo | Sí, globalmente | Router para forwarding directo |
| Capa 2 (Nombre) | Perfil/capacidad del nodo | No necesariamente | OPA para decisión de routing |

Un mismo nombre capa 2 puede tener múltiples UUIDs (varios nodos con mismo perfil). El router resuelve cuál de ellos recibe el mensaje.

---

## 8. Caracterización de Nodos por Tipo

### 8.1 Nodos AI (Agentes LLM)

Los agentes AI emulan personas con roles funcionales. La nomenclatura sigue el modelo de **Recursos Humanos**:

```
AI.<área>.<cargo>.<nivel>.<especialización>.<turno>@<isla>
```

| Campo | Descripción | Ejemplos |
|-------|-------------|----------|
| área | Departamento funcional | soporte, ventas, cobranzas, legal |
| cargo | Puesto o función | analista, ejecutivo, asesor |
| nivel | Seniority o tier | l1, l2, jr, sr, lead |
| especialización | Nicho de expertise | tecnico, comercial, enterprise |
| turno | Disponibilidad | diurno, nocturno, 24-7 |

**Ejemplos:**
```
AI.soporte.analista.l1.tecnico.diurno@produccion
AI.ventas.ejecutivo.closer.enterprise@produccion
AI.cobranzas.analista.jr.amigable@produccion
```

### 8.2 Nodos IO (Adaptación de Medio)

Los nodos IO son infraestructura de comunicación. Representan canales, no personas:

```
IO.<medio>.<identificador>@<isla>
```

**Ejemplos:**
```
IO.wapp.+5491155551234@produccion
IO.email.ventas@empresa.com@produccion
IO.telegram.@bot_empresa@produccion
```

### 8.3 Nodos WF (Workflows Estáticos)

Los nodos WF ejecutan acciones determinísticas:

```
WF.<verbo>.<objeto>.<variante>@<isla>
```

**Ejemplos:**
```
WF.send.email@produccion
WF.query.crm@produccion
WF.validate.kyc@produccion
WF.route.handoff@produccion
```

### 8.4 Nodos SY (Sistema)

Los nodos SY proveen servicios fundamentales para la operación de la red:

```
SY.<servicio>@<isla>
```

**Ejemplos:**
```
SY.config.routes@produccion
SY.opa.rules@produccion
SY.orchestrator@produccion
SY.admin@produccion
SY.time@produccion
```

### 8.5 Nodos RT (Routers)

Los routers también tienen identificador capa 2:

```
RT.<rol>@<isla>
```

**Ejemplos:**
```
RT.primary@produccion
RT.secondary@produccion
RT.gateway@staging
```

### 8.6 Resumen de Convenciones

| Tipo | Modelo conceptual | Estructura |
|------|------------------|------------|
| AI | Recursos Humanos (personas con roles) | `AI.<área>.<cargo>.<nivel>@<isla>` |
| IO | Infraestructura de comunicación | `IO.<medio>.<identificador>@<isla>` |
| WF | APIs/Programación (acciones) | `WF.<verbo>.<objeto>@<isla>` |
| SY | Infraestructura del sistema | `SY.<servicio>@<isla>` |
| RT | Routers (infraestructura de red) | `RT.<rol>@<isla>` |

---

## 9. VPN = Zonas / VRF dentro de una Isla (v1.13)

### 9.1 Definición

Una **VPN** es una zona lógica (VRF-like) **dentro de una isla**. Es una "isla lógica" dentro de la isla física.

- Cada VPN tiene su tabla de visibilidad/routing separada.
- Los nodos en una VPN solo pueden rutear hacia nodos visibles en esa VPN.
- El aislamiento es **arquitectónico**, no "policy best effort".
- OPA es otra capa; VPN existe sin OPA.

**Dominio efectivo de un nodo:**

```
domain = (island_id, vpn_id)
```

**Convención:**
- `vpn_id = 0` es "global" (default, todos se ven)
- `vpn_id != 0` son zonas aisladas

### 9.2 Ejemplo Visual

```
┌─────────────────────────────────────────────────────────────┐
│                    Isla "produccion"                         │
│                                                              │
│   ┌─────────────────────┐    ┌─────────────────────┐       │
│   │     VPN 0 (global)  │    │     VPN 10 (soporte)│       │
│   │                     │    │                     │       │
│   │  AI.ventas@prod     │    │  AI.soporte.l1@prod │       │
│   │  WF.crm@prod        │    │  AI.soporte.l2@prod │       │
│   │                     │    │  WF.ticket@prod     │       │
│   └─────────────────────┘    └─────────────────────┘       │
│              │                         │                    │
│              │    (leak rules)         │                    │
│              └─────────────────────────┘                    │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

### 9.3 Relación con Inter-Isla

**VPN es intra-isla.** No cruza entre islas.

- Para comunicación entre islas se usa el gateway inter-isla (IIL).
- VPN aísla dentro de una isla; WAN conecta entre islas.

---

## 10. Referencias a Otros Documentos

| Tema | Documento |
|------|-----------|
| Estructura de mensajes, framing, HELLO | `02-protocolo.md` |
| Shared memory, estructuras Rust | `03-shm.md` |
| Routing, FIB, VPN zones, algoritmos | `04-routing.md` |
| IRP intra-isla, Inter-Isla WAN | `05-conectividad.md` |
| Región config, estructuras VPN | `06-config-shm.md` |
| Arranque, YAML, systemd | `07-operaciones.md` |
| Glosario, decisiones de diseño | `08-apendices.md` |
| Nodos SY (config.routes, opa.rules, etc.) | `SY_nodes_spec.md` |
