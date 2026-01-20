# JSON Router - 08 Apéndices

**Estado:** v1.13  
**Fecha:** 2025-01-20  
**Audiencia:** Todos (referencia)

---

## A. Glosario (v1.13)

### Conceptos Fundamentales

- **Nodo**: Proceso que procesa mensajes (AI, WF, IO, o SY).

- **Router**: Proceso que mueve mensajes entre nodos, identificado como RT.

- **Link**: Conexión socket entre nodo y router.

- **IP lógica**: Identificador único de un nodo en el sistema (UUID, capa 1).

- **Nombre capa 2**: Identificador descriptivo jerárquico con isla (ej: `AI.soporte.l1@produccion`, `RT.primary@produccion`).

- **Rol**: Capacidad abstracta de un nodo (ej: "soporte", "facturación").

- **Framing**: Delimitación de mensajes en un stream de bytes (length prefix de 4 bytes, big-endian).

### Islas y Conectividad

- **Isla**: Dominio local de routing donde los routers comparten `/dev/shm`. Un host físico = una isla.

- **island.yaml**: Archivo que define la identidad de la isla (`island_id`). Todos los procesos de la isla lo leen.

- **config.yaml**: Configuración real del router (autosuficiente, no depende de island.yaml en runtime).

- **identity.yaml**: Estado de hardware del router con UUID (auto-generado en primer arranque).

- **IRP (Inter-Router Peering)**: Comunicación entre routers de la misma isla via Unix sockets. Es transparente (no modifica TTL).

- **IIL (Inter-Isla Links)**: Comunicación entre islas via TCP. Solo el gateway de cada isla lo maneja.

- **Gateway**: Router único por isla que maneja conexiones TCP a otras islas (v1.13).

### Shared Memory

- **Región shm**: Área de shared memory de un router específico (`/jsr-<uuid>`). Cada router tiene la suya.

- **Región config shm**: Área de shared memory para rutas estáticas y VPN zones (`/jsr-config-<island>`).

- **Seqlock**: Mecanismo de sincronización para un writer y múltiples readers usando contador atómico.

- **Heartbeat**: Timestamp actualizado periódicamente para detectar procesos muertos.

- **Generation**: Contador que incrementa cada vez que se recrea una región shm.

### Routing

- **RIB (Routing Information Base)**: Todas las rutas candidatas (distribuidas en múltiples regiones).

- **FIB (Forwarding Information Base)**: Rutas ganadoras compiladas en memoria local del router.

- **Next-hop**: Router vecino al que enviar un mensaje (no el destino final).

- **LPM (Longest Prefix Match)**: La ruta más específica gana.

- **Admin Distance**: Preferencia por origen de ruta (menor = preferido). CONNECTED=0, STATIC=1, LSA=10.

- **peer_links**: Tabla local que mapea UUID de peer a link_id del uplink.

### VPN (v1.13 - Redefinido)

- **VPN**: Zona lógica (VRF) **dentro de una isla** que aísla visibilidad y routing entre grupos de nodos. NO es túnel inter-isla.

- **vpn_id**: Identificador de la zona. `vpn_id=0` es global (default).

- **Membership rule**: Regla que asigna nodos a una VPN por pattern de nombre L2.

- **Leak rule**: Regla que permite visibilidad/routing explícito entre VPNs.

- **vpn_scope**: Campo de rutas estáticas que indica en qué VPN(s) se instala la ruta.

### Nodos de Sistema

- **SY.config.routes**: Nodo de sistema responsable de escribir la región de config. Uno por isla.

- **SY.opa.rules**: Nodo de sistema responsable de compilar y distribuir policies OPA. Uno por isla.

- **SY.orchestrator**: Nodo de sistema para orquestación de routers y nodos. Uno por isla.

- **SY.admin**: Gateway HTTP REST único para administración de toda la infraestructura.

---

## B. Decisiones de Diseño

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
| Naming con @isla (v1.13) | isla como campo separado | Simplifica routing inter-isla (parsear @isla) |
| Gateway único por isla (v1.13) | Múltiples gateways | Evita ambigüedad de salida, control-plane simple |
| VPN como VRF intra-isla (v1.13) | VPN como túnel inter-isla | Semántica más clara, aislamiento arquitectónico |
| No LSA de nodos entre islas (v1.13) | Propagar nodos | Con @isla solo hace falta conectividad |

---

## C. Cambios v1.13

### C.1 Naming con @isla

- **Antes:** `SY.config.routes.produccion`, `AI.soporte.l1`
- **Ahora:** `SY.config.routes@produccion`, `AI.soporte.l1@produccion`

Todo nombre L2 incluye `@isla` como sufijo.

### C.2 VPN redefinido

- **Antes:** VPN era túnel inter-isla con `ACTION_VPN` y endpoints
- **Ahora:** VPN es zona/VRF **dentro de una isla**

Estructuras nuevas:
- `VpnZoneEntry`
- `VpnMemberRuleEntry`
- `VpnLeakRuleEntry`

Campo nuevo en routing header: `vpn`

### C.3 Gateway único por isla

- **Antes:** Cualquier router podía tener WAN
- **Ahora:** Solo un router por isla es gateway inter-isla

Los demás routers reenvían tráfico inter-isla al gateway por IRP.

### C.4 No LSA de nodos entre islas

- **Antes:** Se podían propagar rutas de nodos entre islas
- **Ahora:** Con @isla, solo hace falta conectividad al gateway remoto

### C.5 Eliminación de ACTION_VPN

- **Antes:** `ACTION_FORWARD`, `ACTION_DROP`, `ACTION_VPN`
- **Ahora:** `ACTION_FORWARD`, `ACTION_DROP`

El forwarding inter-isla se resuelve por `next_hop_island` en rutas estáticas.

---

## D. Referencias

### Estándares y Especificaciones

- [OPA WASM](https://www.openpolicyagent.org/docs/latest/wasm/)
- [Unix domain sockets](https://man7.org/linux/man-pages/man7/unix.7.html)
- [POSIX shared memory](https://man7.org/linux/man-pages/man7/shm_overview.7.html)
- [OSPF Timers](https://datatracker.ietf.org/doc/html/rfc2328)
- [Seqlock](https://en.wikipedia.org/wiki/Seqlock)
- [Linux /proc/pid/stat](https://man7.org/linux/man-pages/man5/proc.5.html)

### Rust

- [Rust Atomics and Locks](https://marabos.nl/atomics/)
- [memmap2 crate](https://docs.rs/memmap2)
- [nix crate](https://docs.rs/nix)

### Configuración

- [YAML 1.2 Spec](https://yaml.org/spec/1.2.2/)
- [UUID RFC 4122](https://datatracker.ietf.org/doc/html/rfc4122)

---

## E. Índice de Documentos

| # | Documento | Descripción |
|---|-----------|-------------|
| 01 | `01-arquitectura.md` | Fundamentos, principios, islas, naming, VPN overview |
| 02 | `02-protocolo.md` | Estructura de mensajes, framing, HELLO, librería de nodos |
| 03 | `03-shm.md` | Shared memory, estructuras Rust, seqlock |
| 04 | `04-routing.md` | RIB/FIB, VPN zones, algoritmos, OPA |
| 05 | `05-conectividad.md` | IRP intra-isla, WAN inter-isla, gateway |
| 06 | `06-config-shm.md` | Región de config, estructuras VPN, API |
| 07 | `07-operaciones.md` | Arranque, YAML files, systemd |
| 08 | `08-apendices.md` | Glosario, decisiones de diseño, referencias |
| -- | `SY_nodes_spec.md` | Nodos SY (config.routes, opa.rules, etc.) |
