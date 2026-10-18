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

- **CONFIG_CHANGED**: mensaje SYSTEM protegido con `subsystem`, `version` y `config`. Hoy lo usa solo SY.admin, para la policy OPA de usuario (ver `opa-distribution.md`); rutas/VPN/taps ya no (desde 0.1.44) y el `node_config` de los orquestadores se eliminó en 0.1.45.

- **CONFIG_RESPONSE**: Unicast de respuesta a CONFIG_CHANGED. Confirma aplicación (`status: ok`) o reporta error (`status: error`). Obligatorio para todos los nodos SY que reciben configuración.

### Islas y Conectividad

- **Isla**: Dominio local de routing donde los routers comparten `/dev/shm`. Un host físico = una isla.

- **Motherbee**: Isla madre. Contiene PostgreSQL, SY.storage, SY.orchestrator, SY.admin. Cerebro del cluster.

- **Worker**: Isla hija. Stateless, reconstruible. Contiene router (con su stream), SY.cognition, nodos AI/IO/WF. Músculo del cluster.

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

- **jsr-memory-<hive>**: Región SHM cognitiva. Contiene snapshot por comunicación (`thread_id` + agente) con `memory_package` v2 truncado. Writer: SY.cognition.

- **Seqlock**: Mecanismo de sincronización para un writer y múltiples readers. El writer incrementa un contador de secuencia antes y después de escribir (impar = escritura en curso); el reader lee el contador, lee el dato in-place, y reintenta si el contador cambió o quedó impar. Readers nunca copian ni bloquean.

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

- **thread_id**: Identificador físico/canónico de la conversación. Lo calcula SDK/IO y viaja en `meta.thread_id`.

- **Turno de una comunicación**: el orden de una comunicación es el número de turno que lleva SY.cognition; el mensaje no lleva secuencia (`meta.thread_seq` salió del protocolo en 0.1.78).

- **ctx / ctx_seq / ctx_window**: Campos legacy/históricos del protocolo. Ya no forman parte del carrier conversacional activo del repo.

- **Persistencia de turns**: El router donde entra cada mensaje lo copia a su stream (`storage.turns`); `SY.storage` lo escribe en `turns`, el log de mensajes.

### Cognición y Memoria

- **Event (Episodio)**: Unidad de experiencia consolidada. Rango de turns con boundaries claros.

- **Highlight**: Turns verbatim seleccionados que capturan la esencia de un episodio.

- **Memory Item**: Conclusión accionable derivada de un episodio (fact, procedure, constraint, preference).

- **Cue (Tag)**: Etiqueta normalizada para indexación y recuperación. Ej: `intent:billing.issue`, `kw:doble_cobro`.

- **activation_strength**: Probabilidad de que un evento/item sea recuperado. Sube con uso exitoso.

- **context_inhibition**: Supresión contextual de un evento/item. Permite reemplazo funcional sin borrado.

- **jsr-memory-<hive>**: Región SHM con snapshot (`thread_id`, agente) → `memory_package` v2. Writer: SY.cognition.

- **LanceDB**: Base de datos embebida para episodios y items. Cache reconstruible desde PostgreSQL.

- **MemoryPackage**: Paquete de antecedentes relevantes que el router agrega al mensaje que entrega a un nodo `AI.*` (el de esa comunicación; uno que puso el emisor se descarta).

- **SY.cognition**: Proceso que consolida evidencia en memoria episódica. Single-writer de `jsr-memory` y durable cognition storage; LanceDB sigue diferido.

- **Cold Start**: Reconstrucción de memoria desde PostgreSQL cuando LanceDB está vacío/corrupto.

### Persistencia y stream del router

- **Stream del router**: Log corto en disco de cada router, que es también un nodo: `RT.<router>.stream@<hive>`. Entrega sus registros en lotes con ack (`STREAM_BATCH` / `STREAM_ACK`) a sus sinks por el socket de fluxbee, por el routing normal (local, IRP, WAN). Reemplazó al broker NATS embebido, que se borró en 0.1.78.

- **Ventana**: 120 s y 32 MB por router. Mejor esfuerzo: lo que un sink no toma a tiempo se pierde y se cuenta (una línea de alarma cada 30 s; `streamlost` en `ops.py health`).

- **fire & forget**: El router deja la copia en una cola acotada del stream y nunca espera al log ni a PostgreSQL.

- **STREAM_APPEND**: Mensaje de sistema con el que un nodo agrega un registro al stream de su router (`fluxbee_sdk::stream::append`); el router solo acepta los subjects permitidos a ese productor.

- **SY.storage**: Proceso que toma los lotes de los streams en `storage_inbox` y escribe a PostgreSQL. Solo corre en isla madre.

- **Idempotencia**: Propiedad de las escrituras: `ON CONFLICT DO NOTHING` permite re-envíos sin duplicados.

- **Batch**: Lote del stream (`STREAM_BATCH`): como mucho 256 registros y 96 KB, uno en vuelo por sink.

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
| Stream por router (`RT.<router>.stream@<hive>`) | Sin servidor externo, mismo binario; el socket de fluxbee es el caño (NATS se borró en 0.1.78) |
| Router no espera a DB | La copia va a una cola acotada del stream, SY.storage persiste async |
| SY.storage solo en madre | Único punto de escritura a PostgreSQL, simplicidad |
| Lotes del stream por el routing normal | Sin bridge aparte: un lote viaja como cualquier mensaje (local, IRP, WAN) |
| Un stream por router OK | SY.storage deduplica por `dedupe_key` (hash de subject y payload), idempotencia |
| Orchestrator solo en Motherbee | Workers son stateless, systemd local basta |
| Workers reconstruibles | LanceDB/jsr-memory se regeneran desde PostgreSQL |

### Distribución Motherbee / Worker

| Componente | Motherbee | Worker |
|------------|:---------:|:------:|
| PostgreSQL | ✓ | - |
| SY.storage | ✓ | - |
| SY.orchestrator | ✓ | - |
| SY.admin | ✓ | - |
| Router + stream | ✓ | ✓ |
| Syncthing (opcional, blobs) | ✓ | ✓ |
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

// Cognición (jsr-memory)
pub const MEMORY_MAGIC: u32 = 0x4A534D4D;       // "JSMM"
pub const MEMORY_VERSION: u32 = 1;
pub const MAX_TENANTS: usize = 256;
pub const MAX_TAGS_PER_TENANT: usize = 16384;
pub const MAX_EVENTS_PER_TENANT: usize = 65536;
pub const MEMORY_FETCH_TIMEOUT_MS: u64 = 50;
pub const MEMORY_PACKAGE_MAX_BYTES: usize = 32 * 1024;  // 32KB

// Stream del router (src/router/stream.rs)
pub const STREAM_MAX_AGE_SECS: u64 = 120;
pub const STREAM_MAX_BYTES: u64 = 32 * 1024 * 1024;
pub const STREAM_QUEUE_MAX_BYTES: u64 = 16 * 1024 * 1024;
pub const STREAM_BATCH_MAX_RECORDS: usize = 256;
pub const STREAM_BATCH_MAX_BYTES: usize = 96 * 1024;
pub const STREAM_ALARM_INTERVAL_SECS: u64 = 30;
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
| 11 | `11-context.md` | **Documento histórico del modelo CTX/ctx_window** |
| 12 | `12-cognition.md` | **Documento histórico v1 de cognition** |
| 13 | `13-storage.md` | **Stream del router, SY.storage, persistencia** |
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
