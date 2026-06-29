# Ilk sync bidireccional — registro global aditivo (diseño)

_2026-06-27. Cierra el bug diagnosticado en `lab/STATUS.md` ("Vista de ilks por
hive"): la replicación de `SY.identity` es one-way (primary→replica), así que los
ilks locales de un replica nunca llegan a motherbee y **no hay vista global**._

## Objetivo
La **SHM de identity es el "quién existe" de todo el sistema** (routing/auth la
leen). Sin ilk no existís. Por eso cada hive tiene que converger a la **unión
aditiva** de todos los ilks de la malla. Mecanismo: **bidireccional, centralizado
en motherbee** — cada hive empuja sus ilks a motherbee; motherbee mergea additivo
y re-broadcastea a todos.

## Estado actual (one-way)
- **Primary** (motherbee): bindea `:9100`; `handle_sync_connection` atiende
  `FULL_SYNC_REQUEST` (manda chunks) y `DELTA_SUBSCRIBE` (registra subscriber y le
  **pushea** deltas con ACK/retry). **Nunca lee ilks de un replica.**
- **Replica** (worker): al boot `fetch_full_sync_from_primary` + spawnea
  `run_delta_subscription_loop`→`stream_deltas_from_primary` (consume deltas).
- Cada hive siembra sus propios ilks (`ensure_system_ilks_from_hive`, tag
  `@hive`). En un replica, los deltas locales (command handler, `alias_gc`,
  boot-seed) se **descartan** en el gate `if is_primary` (línea ~2858 y ~2797).

## Diseño

### Nuevo op de sync: `DELTA_PUBLISH` (replica → primary, upstream)
Conexión dedicada del replica a `:9100`. Primera línea: `{operation:
"IDENTITY_DELTA_PUBLISH", hive_id: "<self>"}`. Luego el replica **escribe** un
stream de `IdentityDeltaEnvelope` (sus deltas locales); el primary **lee** cada
uno, lo valida, y responde `IDENTITY_DELTA_ACK{seq}` (mismo framing que el
downstream, invertido).

### Replica
- Canal `upstream_tx`/`upstream_rx`. Donde hoy se dropean los deltas locales
  (`if is_primary` en command handler + alias_gc), en un replica se **mandan a
  `upstream_tx`**.
- Tarea `run_delta_publish_loop(upstream, hive_id, upstream_rx)`: conecta, manda
  `DELTA_PUBLISH`, drena `upstream_rx` → write+ACK; reconecta on error.
- **Beat de reconciliación**: al boot (tras seed) y cada ~30s, el loop manda al
  `upstream_tx` un snapshot de **todos sus ilks `@self`** (los que `node_name`
  termina en `@<self_hive>`). Apply idempotente en el primary ⇒ convergencia y
  recovery sin lógica de replay por reconexión.

### Primary (motherbee)
- `handle_sync_connection` gana el arm `DELTA_PUBLISH`: lee `hive_id`, luego lee
  deltas; por cada uno manda `(publisher_hive, delta)` a `ingest_tx` y ACKea.
- Canal `ingest_tx`/`ingest_rx`. Nuevo arm en el `select!` del loop principal:
  drena `ingest_rx` →
  1. **Authority check** (`delta_authorized_for_hive`): un replica solo puede
     upsert/delete ilks cuyo `node_name` termina en `@<publisher_hive>`. Para
     `IlkDelete` (solo trae `ilk_id`), se resuelve el ilk existente y se chequea
     su `@hive`; si no existe o no matchea, se rechaza (no se puede forjar
     `@motherbee` ni `@otro-hive`).
  2. `store.apply_delta` (additivo: `ilk_id = node@hive` namespaced ⇒ unión pura).
  3. `sync_identity_shm_mappings` (la SHM de motherbee refleja la unión).
  4. `assign_delta_seqs` + `broadcast_deltas` a **todos** los subscribers ⇒ los
     demás replicas convergen.

### Invariantes / decisiones
- **Loop-avoidance**: el replica empuja solo deltas de **origen local** (los que
  recibe downstream van por `delta_event_rx`, que solo aplica, no re-pushea). El
  re-broadcast de motherbee vuelve al replica y se aplica idempotente (apply no
  re-emite). Sin loop.
- **Autoridad por-hive**: cada hive es dueño de sus `@hive` ilks; el check impide
  forjar ilks de otro hive. Motherbee nunca recibe `@motherbee` de un replica
  (su `hive_id` nunca es "motherbee").
- **Additivo**: unión por `ilk_id`. Deletes por tombstone (`deleted_at_ms`, ya
  existe) vía `IlkDelete`.
- **Tipos pusheados**: `IlkUpsert`/`IlkDelete` (+ `AliasUpsert`/`AliasDelete`,
  ligados a renames de ilk). **Tenants NO** se pushean desde replica
  (`action_requires_primary` ya los gatea al primary).
- **Persistencia**: motherbee **no** persiste en su DB los ilks recibidos (el
  worker es la fuente; el beat los re-establece tras un restart). Se puede
  agregar después si se quiere DB durable de la unión.

## Seguridad (post revisión adversarial)
La revisión (5 lentes + verificación) confirmó hallazgos; los fixes aplicados:
- **Impersonación del primary (era blocker):** el puerto de sync `:9100` es TCP
  crudo en la malla interna y **no está peer-autenticado** (ya lo estaba para
  READ: `FULL_SYNC`/`SUBSCRIBE` exponen todo el store a cualquiera que conecte).
  El path de WRITE agrega riesgo. Mitigación aplicada: el ingest **rechaza** todo
  publish cuyo `publisher_hive == hive_id propio` (cierra impersonar a motherbee,
  el caso catastrófico). **Validado en lab**: el ataque (handshake `hive_id:
  "motherbee"` + snapshot vacío que reconciliaría-borraría todo) se rechaza
  (`rejected ... impersonating the primary hive`) y los ilks quedan intactos.
- **Protección de system ilks (era major DOS):** ni `reconcile_hive_ilks` (stale)
  ni `apply_delta(IlkDelete)` borran un `is_well_known_system_ilk` (`source =
  hive.system_nodes`) — espeja el guard `SYSTEM_ILK_PROTECTED` de `delete_ilk`. Un
  snapshot forjado que omita `SY.identity@<hive>` no puede borrarlo.
- **Colisión de `ilk_id` (era major):** `delta_authorized_for_hive` + el upsert de
  `reconcile_hive_ilks` rechazan escribir un `ilk_id` cuyo dueño existente sea otro
  hive (no se puede secuestrar un id ajeno con un `node_name` forjado).
- **DoS de memoria (era major):** el canal de ingest es **bounded** (4096) con
  backpressure a la conexión del replica.
- **Pérdida de delta (era major):** `publish_to_primary` reintenta write+ACK
  (idempotente: el primary deduplica por `seq`).

**Residual (follow-up, NO en este cambio):** autenticación mutua real del canal
`:9100` (mTLS / HMAC, o binding al peer address con el mapa hive→IP). Es un gap
**pre-existente** (READ ya estaba abierto) y excede este feature; el guard de
impersonación + la protección de system ilks acotan el blast radius mientras
tanto. Un nodo de la malla comprometido aún podría forjar ilks NO-system de OTRO
worker (reclamando su hive_id) — lo cierra el peer-auth.

## Validación
Lab (Docker, motherbee + worker1, 21 nodos) — el repro del bug. **Logrado**:
`GET /hives/motherbee/identity/ilks` pasó de **12 → 19** (12 propios + 7 de
worker1, etiquetados `@worker1`). Recuperación por beat validada: restart del
identity de motherbee → cae a 12 → re-converge a 19 (~46s, ≤1.5 beats) sin
persistir en DB. Ataque de impersonación rechazado (arriba). Tests unitarios (33):
authority + colisión + protección system-ilk + additividad/hard-delete +
self_owned/tombstones + loop-avoidance.
