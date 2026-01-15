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

### 6. Estructura del Mensaje

*Por definir: formato del header de routing, metadata requerida, payload.*

### 7. Header de Routing

*Por definir: campos mínimos para que el router tome decisiones sin parsear payload.*

### 8. Metadata para OPA

*Por definir: qué campos del mensaje se exponen a OPA para decisión de rol/capacidad.*

---

## Parte III: Infraestructura de Red

### 9. Sockets y Detección de Link

*Por definir: protocolo de conexión nodo-router, comportamiento en link down, reconexión.*

### 10. Shared Memory

*Por definir: estructura de la tabla de ruteo, estado de nodos, sincronización entre routers.*

### 11. Ciclo de Vida de Nodos

*Por definir: registro de nodo, asignación a router, desconexión, cleanup.*

---

## Parte IV: Routing

### 12. Tabla de Ruteo Estática

*Por definir: formato de rutas, matching de destinos, prioridades.*

### 13. Policies OPA

*Por definir: estructura de policies, input/output contract, ejemplos.*

### 14. Balanceo entre Nodos del Mismo Rol

*Por definir: estrategia cuando hay múltiples nodos con la capacidad requerida.*

---

## Parte V: Operación

### 15. Estados de la Red

*Por definir: estados posibles de nodos y links, transiciones, eventos.*

### 16. Eventos del Sistema

*Por definir: qué eventos se generan, quién los consume, formato.*

### 17. Broadcast y Multicast

*Por definir: cómo mandar un mensaje a múltiples destinos, casos de uso.*

### 18. Mantenimiento y Administración

*Por definir: herramientas de diagnóstico, limpieza de links huérfanos, monitoreo.*

---

## Parte VI: Implementación

### 19. Router: Loop Principal

*Por definir: pseudocódigo del ciclo epoll/read/route/write.*

### 20. Librería de Nodo

*Por definir: API para que los nodos se comuniquen con el router.*

### 21. Addon de Shared Memory

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
