# JSON Router - 08 Apéndices

**Estado:** v1.16  
**Fecha:** 2026-02-04  
**Audiencia:** Todos (referencia)

---

## A. Glosario

### Conceptos Fundamentales

- **Nodo**: Proceso que procesa mensajes (AI, WF, IO, o SY).

- **Router**: Proceso que mueve mensajes entre nodos, identificado como RT.

- **Gateway**: Router especial que conecta islas entre sí via TCP/WAN. Único por isla.

- **Link**: Conexión socket entre nodo y router.

- **UUID (Capa 1)**: Identificador único de un nodo (128 bits, auto-generado).

- **Nombre L2 (Capa 2)**: Identificador descriptivo con isla. Formato: `TYPE.campo1.campo2@isla`.

- **ILK (Capa 3)**: Interlocutor Key. Identificador único de entidades que conversan (personas, agentes, sistemas). Formato: `ilk:<uuid>`.

- **ICH (Capa 3)**: Interlocutor Channel. Canal de comunicación asociado a un ILK (WhatsApp, Slack, email, etc.). Formato: `ich:<uuid>`.

- **CTX (Capa 3)**: Context. Identificador de conversación, calculado como `hash(ilk + ich)`. Formato: `ctx:<hash>`.

- **Turn**: Un mensaje individual dentro de un contexto/conversación.

- **Framing**: Delimitación de mensajes (4 bytes length prefix, big-endian).

### Mensajes de Sistema

- **CONFIG_CHANGED**: Broadcast emitido por SY.admin para distribuir cambios de configuración. Contiene `subsystem`, `version` y `config`.

- **CONFIG_RESPONSE**: Unicast de respuesta a CONFIG_CHANGED. Confirma aplicación (`status: ok`) o reporta error (`status: error`). Obligatorio para todos los nodos SY que reciben configuración.

### Islas y Conectividad

- **Isla**: Dominio local de routing donde los routers comparten `/dev/shm`. Un host físico = una isla.

- **Motherbee**: Isla madre. Contiene PostgreSQL, SY.storage, SY.orchestrator, SY.admin. Cerebro del cluster.

- **Worker**: Isla hija. Stateless, reconstruible. Contiene router, NATS, SY.cognition, nodos AI/IO/WF. Músculo del cluster.

- **hive.yaml**: Archivo que define la identidad de la isla. Campo `role: motherbee` o `role: worker`.

- **@isla**: Sufijo en nombres L2 que indica la isla. Agregado automáticamente por la librería de nodo.

- **IRP (Inter-Router Peering)**: Comunicación entre routers de la misma isla via Unix sockets.

- **WAN**: Comunicación entre islas via TCP. Solo entre gateways. Workers conectan a Motherbee.

- **LSA (Link State Advertisement)**: Mensaje que intercambian los gateways con topología completa de su isla.

### Shared Memory

- **jsr-<uuid>**: Región de memoria compartida de un router. Contiene sus nodos conectados.

- **jsr-config-<hive>**: Región de configuración. Contiene rutas estáticas y tabla VPN. Writer: SY.config.routes.

- **jsr-lsa-<hive>**: Región de topología remota. Contiene nodos/rutas/VPNs de otras islas. Writer: Gateway.

- **jsr-identity-<hive>**: Región de identidad. Contiene ILKs, ICHs, modules, degrees. Writer: SY.identity.

- **jsr-memory-<hive>**: Región de índice cognitivo. Contiene inverted index de tags → event_ids. Writer: SY.cognition.

- **Epoch/RCU**: Mecanismo de sincronización para un writer y múltiples readers. Writer usa shadow region + epoch bump + swap. Readers nunca copian, leen in-place.

- **Heartbeat**: Timestamp actualizado periódicamente para detectar procesos muertos.

### Routing

- **FIB (Forwarding Information Base)**: Tabla de rutas compilada en memoria local del router.

- **Next-hop**: Router o isla destino del siguiente salto.

- **LPM (Longest Prefix Match)**: La ruta más específica gana.

- **Admin Distance**: Preferencia por origen de ruta. LOCAL=0, STATIC=1, REMOTE=2.

### VPN

- **VPN**: Zona de aislamiento dentro de una isla. Los nodos en distintas VPNs no se ven entre sí.

- **vpn_id**: Identificador de la zona. 0 = global (default).

- **Tabla VPN**: Mapping de patterns a vpn_id. Vive en jsr-config-<hive>.

- **Inmunidad SY.***: Los nodos SY.* y RT.* ignoran filtros VPN, ven todo.

### Contexto y Conversaciones

- **ctx_client**: Librería para acceder a historia completa de contextos. Uso raro (cuando ctx_window no es suficiente).

- **ctx_seq**: Número de secuencia del último mensaje conocido en un contexto.

- **ctx_window**: Array con los últimos 20 turns de un contexto. Agregado por el router al mensaje. Permite que el nodo responda sin consultar DB.

- **Persistencia de turns**: El router persiste cada mensaje con ctx en PostgreSQL (async).

### Cognición y Memoria

- **Event (Episodio)**: Unidad de experiencia consolidada. Rango de turns con boundaries claros.

- **Highlight**: Turns verbatim seleccionados que capturan la esencia de un episodio.

- **Memory Item**: Conclusión accionable derivada de un episodio (fact, procedure, constraint, preference).

- **Cue (Tag)**: Etiqueta normalizada para indexación y recuperación. Ej: `intent:billing.issue`, `kw:doble_cobro`.

- **activation_strength**: Probabilidad de que un evento/item sea recuperado. Sube con uso exitoso.

- **context_inhibition**: Supresión contextual de un evento/item. Permite reemplazo funcional sin borrado.

- **jsr-memory-<hive>**: Región SHM con índice invertido de tags → eventos. Writer: SY.cognition.

- **LanceDB**: Base de datos embebida para episodios y items. Cache reconstruible desde PostgreSQL.

- **MemoryPackage**: Paquete de antecedentes relevantes que el router agrega al mensaje.

- **SY.cognition**: Proceso que consolida evidencia en memoria episódica. Single-writer de jsr-memory y LanceDB.

- **Cold Start**: Reconstrucción de memoria desde PostgreSQL cuando LanceDB está vacío/corrupto.

### Persistencia y NATS

- **NATS embebido**: Servidor NATS JetStream embebido en el router (si config). Buffer local con persistencia.

- **JetStream**: Capa de persistencia de NATS. Garantiza entrega at-least-once.

- **fire & forget**: Patrón donde el router publica a NATS sin esperar confirmación de PostgreSQL (~1ms).

- **SY.storage**: Proceso que consume NATS y escribe a PostgreSQL. Solo corre en isla madre.

- **WAN bridge**: Funcionalidad del router que consume NATS local y forward por TCP a isla madre.

- **WorkQueue**: Tipo de stream NATS donde cada mensaje es entregado a un solo consumer.

- **Idempotencia**: Propiedad de las escrituras: `ON CONFLICT DO NOTHING` permite re-envíos sin duplicados.

- **Batch**: Agrupación de turns antes de enviar por WAN (100 turns o 100ms).

---

## B. Decisiones de Diseño v1.16

| Decisión | Razón |
|----------|-------|
| Naming con @isla obligatorio | Simplifica routing inter-isla (parsear @isla del destino) |
| Librería agrega @isla automáticamente | El nodo no necesita saber su isla, la lee de hive.yaml |
| Gateway único por isla | Evita ambigüedad de salida, simplifica modelo |
| Seis regiones SHM separadas | Un writer por región, evita conflictos |
| jsr-lsa para topología remota | Todos los routers ven islas remotas sin consultar al gateway |
| LSA incluye nodos + rutas + VPNs | Visibilidad completa de islas remotas |
| VPN como tabla simple | Sin jerarquías ni leaks, aislamiento total |
| SY.* inmune a VPN | Nodos de sistema deben poder operar sin restricciones |
| Sin LSA de nodos individuales | Con @isla, solo hace falta saber que la isla existe |
| Rutas estáticas como override | Prioridad absoluta sobre routing normal |
| PostgreSQL centralizado (madre) | Source of truth, auditoría, backup único |
| LanceDB local por isla | Latencia baja, serverless, cache reconstruible |
| Tags exactos + semánticos async | Router no bloquea; SY.cognition enriquece después |
| Prioridad dinámica sin borrado | Evidencia inmutable, solo cambia activación/inhibición |
| NATS embebido en router | Sin servidor externo, mismo binario, config decide |
| Router no espera a DB | Fire & forget a NATS (~1ms), SY.storage persiste async |
| SY.storage solo en madre | Único punto de escritura a PostgreSQL, simplicidad |
| WAN bridge en router con config | Mismo binario, NATS consume → forward TCP |
| Múltiples NATS independientes OK | DB deduplica por (ctx, seq), idempotencia |
| Orchestrator solo en Motherbee | Workers son stateless, systemd local basta |
| Workers reconstruibles | LanceDB/jsr-memory se regeneran desde PostgreSQL |

### Distribución Motherbee / Worker

| Componente | Motherbee | Worker |
|------------|:---------:|:------:|
| PostgreSQL | ✓ | - |
| SY.storage | ✓ | - |
| SY.orchestrator | ✓ | - |
| SY.admin | ✓ | - |
| Router + NATS | ✓ | ✓ |
| SY.identity/config/opa | ✓ | ✓ (cache) |
| SY.cognition + LanceDB | ✓ | ✓ |
| AI/IO/WF nodes | opcional | ✓ |

---

## C. Cambios vs Versiones Anteriores

### vs v1.15

| Aspecto | v1.12 | v1.13 |
|---------|-------|-------|
| Naming | Sin @isla | Con @isla obligatorio |
| VPN | Túnel inter-isla | Zona intra-isla |
| Regiones SHM | 2 (router + config) | 3 (router + config + lsa) |
| LSA | No definido | Gateway ↔ Gateway |
| Gateway | Múltiples posibles | Único por isla |

### Estructuras Eliminadas (v1.12)

- `ACTION_VPN` en rutas
- `VpnZoneEntry` con jerarquía
- `VpnLeakRuleEntry`
- `next_hop_router` en rutas estáticas (reemplazado por `next_hop_hive`)

### Estructuras Nuevas (v1.13)

- `jsr-lsa-<hive>` región
- `LsaHeader`, `RemoteHiveEntry`, `RemoteNodeEntry`
- `is_gateway` flag en ShmHeader
- `router_name` en ShmHeader

---

## D. Límites del Sistema

| Límite | Valor | Notas |
|--------|-------|-------|
| MAX_NODES | 1024 | Por router |
| MAX_STATIC_ROUTES | 256 | Por isla |
| MAX_VPN_ASSIGNMENTS | 256 | Por isla |
| MAX_REMOTE_HIVES | 16 | En jsr-lsa |
| MAX_REMOTE_NODES | 1024 | Total entre todas las islas remotas |
| MAX_REMOTE_ROUTES | 256 | Total |
| MAX_REMOTE_VPNS | 256 | Total |
| Nombre L2 max | 256 bytes | UTF-8 |
| Hive ID max | 64 bytes | ASCII recomendado |
| Mensaje inline max | 64 KB | Usar blob_ref para mayores |

---

## E. Constantes Importantes

```rust
// Regiones
pub const SHM_MAGIC: u32 = 0x4A535352;      // "JSSR" - Router
pub const CONFIG_MAGIC: u32 = 0x4A534343;   // "JSCC" - Config
pub const LSA_MAGIC: u32 = 0x4A534C41;      // "JSLA" - LSA

// Match kinds
pub const MATCH_EXACT: u8 = 0;
pub const MATCH_PREFIX: u8 = 1;
pub const MATCH_GLOB: u8 = 2;

// Acciones de ruta
pub const ACTION_FORWARD: u8 = 0;
pub const ACTION_DROP: u8 = 1;

// Flags
pub const FLAG_ACTIVE: u16 = 0x0001;
pub const FLAG_DELETED: u16 = 0x0002;
pub const FLAG_STALE: u16 = 0x0004;

// Timers
pub const HEARTBEAT_INTERVAL_MS: u64 = 5_000;
pub const HEARTBEAT_STALE_MS: u64 = 30_000;
pub const HELLO_INTERVAL_MS: u64 = 10_000;
pub const DEAD_INTERVAL_MS: u64 = 40_000;

// Contexto
pub const CTX_WINDOW_SIZE: usize = 20;          // Turns en ctx_window
pub const CTX_WINDOW_FETCH_TIMEOUT_MS: u64 = 50; // Timeout para fetch de window

// Cognición (jsr-memory)
pub const MEMORY_MAGIC: u32 = 0x4A534D45;       // "JSME"
pub const MEMORY_VERSION: u32 = 1;
pub const MAX_TENANTS: usize = 256;
pub const MAX_TAGS_PER_TENANT: usize = 16384;
pub const MAX_EVENTS_PER_TENANT: usize = 65536;
pub const MEMORY_FETCH_TIMEOUT_MS: u64 = 50;
pub const MEMORY_PACKAGE_MAX_BYTES: usize = 32 * 1024;  // 32KB

// NATS
pub const NATS_DEFAULT_PORT: u16 = 4222;
pub const NATS_STREAM_MAX_AGE_DAYS: u64 = 7;
pub const WAN_BATCH_SIZE: usize = 100;
pub const WAN_BATCH_TIMEOUT_MS: u64 = 100;
```

---

## F. Índice de Documentos

| # | Documento | Descripción |
|---|-----------|-------------|
| 01 | `01-arquitectura.md` | Fundamentos, islas, naming @isla, VPN overview |
| 02 | `02-protocolo.md` | Mensajes, framing, HELLO, LSA, librería de nodos |
| 03 | `03-shm.md` | Regiones SHM, estructuras Rust |
| 04 | `04-routing.md` | FIB, VPN, algoritmos, OPA, persistencia de contexto |
| 05 | `05-conectividad.md` | IRP, WAN, LSA entre gateways |
| 06 | `06-regiones.md` | Detalle de config y LSA regions |
| 07 | `07-operaciones.md` | Arranque, YAML, systemd, PostgreSQL |
| 08 | `08-apendices.md` | Glosario, decisiones, límites |
| 09 | `09-router-status.md` | Estado de implementación del router |
| 10 | `10-identity-layer3.md` | **ILK, SY.identity, routing L3** |
| 11 | `11-context.md` | **ICH, CTX, conversaciones, ctx_window** |
| 12 | `12-cognition.md` | **SY.cognition, episodios, jsr-memory, LanceDB** |
| 13 | `13-storage.md` | **NATS embebido, SY.storage, persistencia** |
| -- | `SY_nodes_spec.md` | Nodos SY (config.routes, opa.rules, identity, cognition, storage) |

---

## G. Referencias Externas

- [OPA WASM](https://www.openpolicyagent.org/docs/latest/wasm/)
- [Unix domain sockets](https://man7.org/linux/man-pages/man7/unix.7.html)
- [POSIX shared memory](https://man7.org/linux/man-pages/man7/shm_overview.7.html)
- [Seqlock](https://en.wikipedia.org/wiki/Seqlock)
- [YAML 1.2 Spec](https://yaml.org/spec/1.2.2/)
- [UUID RFC 4122](https://datatracker.ietf.org/doc/html/rfc4122)
- [PostgreSQL](https://www.postgresql.org/docs/)
- [sqlx (Rust)](https://github.com/launchbadge/sqlx)
- [tokio-postgres](https://docs.rs/tokio-postgres/)
- [LanceDB](https://lancedb.github.io/lancedb/)
- [NATS JetStream](https://docs.nats.io/nats-concepts/jetstream)
- [async-nats (Rust)](https://docs.rs/async-nats/)
