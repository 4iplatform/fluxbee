# SY.identity - Auditoria tecnica detallada

Fecha de revision: 2026-05-07  
Alcance principal: `src/bin/sy_identity.rs`  
Contexto relacionado: `README.md`, `docs/onworking COA/node_secret_tasks.md`, `scripts/install.sh`, `scripts/fluxbee_db_bootstrap.sh`

## 1. Resumen ejecutivo

`SY.identity` es el nodo de identidad de Fluxbee. Mantiene el registro de tenants (`TNT`), interlocutores (`ILK`), canales (`ICH`), aliases temporales y definiciones cognitivas de agentes. En `motherbee` actua como primario con PostgreSQL; en workers actua como replica de lectura sincronizada desde el primario.

La arquitectura actual esta razonablemente alineada con el modelo de Fluxbee:

- `SY.identity` es duenio del dominio identity.
- La persistencia primaria vive en PostgreSQL (`fluxbee_identity`).
- La lectura rapida para otros componentes vive en SHM (`/jsr-identity-<hive>`).
- Los secrets de DB no viven en `hive.yaml`; se cargan via `CONFIG_SET` y se guardan en `secrets.json`.
- Las mutaciones principales requieren primario.
- Los workers no escriben DB y sincronizan desde `motherbee`.

Riesgos principales detectados:

- El canal TCP de sync (`identity.sync.port`, default `9100`) no tiene autenticacion propia en `SY.identity`; depende del despliegue/red/firewall.
- El mecanismo de migracion de schema es parcial; para alpha/testing se acepta recrear DB, pero para produccion futura faltaria migracion formal.
- `FLUXBEE_DATABASE_URL` / `JSR_DATABASE_URL` siguen siendo fallback de compatibilidad y pueden saltarse el flujo de secrets por nodo si se usan.
- Algunas respuestas de error siguen exponiendo mensajes crudos de Postgres o parsing. Es util para alpha, pero habria que clasificar/redactar mejor para entornos compartidos.
- El modelo de autorizacion es por nombre L2/prefix; suficiente para la etapa actual, pero no criptograficamente fuerte si un nodo malicioso logra registrarse con nombre permitido.

## 2. Responsabilidades reales

`SY.identity` implementa estas responsabilidades:

- Crear y administrar tenants (`TNT_CREATE`, `TNT_UPDATE`, `TNT_SET_SPONSOR`, `TNT_APPROVE`, `TNT_LIST`, `TNT_GET`).
- Crear, registrar y actualizar interlocutores (`ILK_PROVISION`, `ILK_REGISTER`, `ILK_UPDATE`, `ILK_SET_DEFINITION`, `ILK_LIST`, `ILK_GET`).
- Asociar canales a interlocutores (`ILK_ADD_CHANNEL`, `ICH_SET_ENABLED`).
- Mantener aliases temporales por merge de ILKs.
- Persistir estado primario en PostgreSQL.
- Publicar snapshots y deltas a SHM.
- Sincronizar replicas con full sync inicial y delta stream.
- Exponer `CONFIG_GET` / `CONFIG_SET` para el secreto de DB del primario.
- Reportar `PING`, `STATUS` e `IDENTITY_METRICS`.

No deberia ser responsabilidad de `SY.identity`:

- Crear roles de PostgreSQL.
- Instalar PostgreSQL.
- Resetear DBs de testing.
- Gestionar secrets globales.
- Reemplazar a `SY.admin` como plano de operacion.
- Implementar routing de negocio.

Esto esta alineado con el cambio reciente: la creacion/reset de DBs corresponde a `scripts/fluxbee_db_bootstrap.sh` y al futuro paquete Linux, no al runtime del nodo.

## 3. Topologia y modos

### Primario

El primario es `SY.identity@motherbee`.

Condiciones impuestas por codigo:

- `role=motherbee` requiere `hive_id=motherbee`.
- Si el hive es primario, intenta cargar `postgres_url`.
- Si DB esta lista, asegura DB `fluxbee_identity`, asegura schema y carga el store desde DB.
- Si DB no esta lista, arranca degradado y rechaza acciones mutantes con `DB_NOT_READY`.

### Replica

En workers:

- No usa DB local.
- Arranca con estado in-memory local minimo.
- Si `identity.sync.upstream` esta configurado, pide full sync al primario.
- Luego mantiene delta subscription.
- Expone SHM local con los datos sincronizados.
- Rechaza acciones mutantes con `NOT_PRIMARY`.

### SHM

Nombre por hive:

```text
/jsr-identity-<hive_id>
```

Uso esperado:

- OPA/routing/IO pueden leer identidad, tenants, ILKs, ICHs, aliases y hashes cognitivos sin llamar a PostgreSQL.
- `SY.identity` es writer del segmento.

## 4. Modelo de datos

### Tenant

Campos principales:

- `tenant_id`: `tnt:<uuid>`
- `name`
- `domain`
- `status`: `pending`, `active`, `suspended`
- `settings`
- `sponsor_tenant_id`: opcional, referencia a otro tenant
- `created_at`, `updated_at`

Semantica:

- Un tenant sin sponsor es root.
- Un tenant puede ser sponsor si tiene children.
- `TNT_SET_SPONSOR` acepta asignar o limpiar sponsor.
- Hay validacion contra self-sponsorship y ciclos.

### ILK

Campos principales:

- `ilk_id`: `ilk:<uuid>`
- `ilk_type`: `human`, `agent`, `system`
- `registration_status`: `temporary`, `partial`, `complete`
- `tenant_id`
- `identification`: JSON libre normalizado por caminos de registro
- `association`: derivado para persistencia DB
- `definition`: JSON de definicion cognitiva para agentes
- `channels`: lista de ICHs embebida en memoria
- `deleted_at_ms`: soft delete para temporales/aliases

Notas:

- `ILK_PROVISION` solo permite `human` o `agent`; `system` queda reservado para caminos SY.
- `ILK_SET_DEFINITION` solo aplica a `ilk_type=agent`.
- La definicion cognitiva acepta solo:
  - `role_hash`
  - `skill_hashes`
  - `handbook_hashes`

### ICH

Campos principales:

- `ich_id`: `ich:<uuid>`
- `ilk_id`
- `tenant_id`
- `channel_type`
- `address`
- `owner_l2_name`
- `is_primary`
- `enabled`

Semantica:

- Lookup canonico: `(channel_type_lower, address_lower, tenant_id_lower)`.
- La constraint DB evita duplicar el mismo canal dentro del tenant.
- `enabled` permite controlar disponibilidad del canal sin borrar identidad.

### Alias

Campos:

- `old_ilk_id`
- `canonical_ilk_id`
- `expires_at_ms`

Uso:

- Permite merges temporales de identidad.
- GC corre cada `30s`.
- Cuando expira, borra alias y puede soft-deletear ILKs temporales.

### Vocabulary

Existe tabla y espacio en SHM, pero en el codigo actual aparece como capacidad preparada y no como flujo funcional principal.

## 5. Acciones soportadas

### Lectura

| Accion | Respuesta | Fuente autorizada | Notas |
|---|---|---|---|
| `ILK_LIST` | `ILK_LIST_RESPONSE` | `SY.admin@*` | Lista ILKs conocidos. |
| `ILK_GET` | `ILK_GET_RESPONSE` | `SY.admin@*` | Resuelve aliases y embebe tenant. |
| `TNT_LIST` | `TNT_LIST_RESPONSE` | `SY.admin@*` | Lista tenants con summary. |
| `TNT_GET` | `TNT_GET_RESPONSE` | `SY.admin@*` | Devuelve tenant, sponsor, children y counts. |
| `IDENTITY_METRICS` | `IDENTITY_METRICS_RESPONSE` | abierto | Devuelve metricas internas. |
| `PING` | `PONG` | abierto | Health basico. |
| `STATUS` | `STATUS_RESPONSE` | abierto | Estado y DB readiness. |
| `CONFIG_GET` | `CONFIG_RESPONSE` | `SY.admin@*` | Descubre contrato de DB secret. |

### Mutacion

| Accion | Respuesta | Fuente autorizada | Requiere primario |
|---|---|---|---|
| `ILK_PROVISION` | `ILK_PROVISION_RESPONSE` | `IO.*` | Si |
| `ILK_REGISTER` | `ILK_REGISTER_RESPONSE` | `SY.frontdesk.gov@*`, `SY.orchestrator@*`, bootstrap exacto | Si |
| `ILK_ADD_CHANNEL` | `ILK_ADD_CHANNEL_RESPONSE` | `SY.frontdesk.gov@*`, frontdesk configurado | Si |
| `ILK_UPDATE` | `ILK_UPDATE_RESPONSE` | `SY.orchestrator@*`, bootstrap exacto | Si |
| `ILK_SET_DEFINITION` | `ILK_SET_DEFINITION_RESPONSE` | `SY.admin@*`, `SY.architect@*`, bootstrap exacto | Si |
| `ICH_SET_ENABLED` | `ICH_SET_ENABLED_RESPONSE` | `IO.*`, `SY.admin@*`, `SY.architect@*`, `SY.frontdesk.gov@*` | Si |
| `TNT_CREATE` | `TNT_CREATE_RESPONSE` | `SY.admin@*`, `SY.architect@*`, `SY.frontdesk.gov@*`, frontdesk configurado | Si |
| `TNT_UPDATE` | `TNT_UPDATE_RESPONSE` | `SY.admin@*`, `SY.architect@*`, `SY.frontdesk.gov@*` | Si |
| `TNT_SET_SPONSOR` | `TNT_SET_SPONSOR_RESPONSE` | `SY.admin@*`, `SY.architect@*`, `SY.frontdesk.gov@*` | Si |
| `TNT_APPROVE` | `TNT_APPROVE_RESPONSE` | `SY.admin@*` | Si |
| `CONFIG_SET` | `CONFIG_RESPONSE` | `SY.admin@*` | Solo primario |

## 6. Autorizacion

El control de autorizacion se basa en `src_l2_name`.

Mecanismos:

- Lista de prefixes por accion.
- Lista de nombres exactos para bootstrap.
- Variante especial: `SY.orchestrator.relay.<...>@hive` tambien puede mapear a `SY.orchestrator@hive`.
- Algunas acciones (`PING`, `STATUS`, `IDENTITY_METRICS`) son abiertas.

Evaluacion:

- Correcto para etapa actual y para control dentro de un router confiable.
- No es suficiente como frontera de seguridad fuerte si un proceso puede registrarse con nombre L2 privilegiado.
- La seguridad real depende de:
  - control de quien puede correr nodos
  - control del router socket
  - permisos del host
  - integridad de `SY.orchestrator`

Recomendacion futura:

- Asociar autorizacion a ILK/system identity verificada, no solo a nombre L2.
- Firmar o validar identidad de nodos SY privilegiados.
- Reducir acciones abiertas o responder solo a fuentes SY conocidas cuando sea posible.

## 7. Persistencia PostgreSQL

DB fija:

```text
fluxbee_identity
```

Creacion:

- `SY.identity` parsea el `postgres_url` base.
- Cambia DB a `postgres` para crear `fluxbee_identity` si falta.
- Luego cambia DB a `fluxbee_identity`.
- Asegura schema propio.

Tablas:

- `identity_tenants`
- `identity_ilks`
- `identity_ilk_aliases`
- `identity_ichs`
- `identity_vocabulary`

Indices/constraints principales:

- `identity_tenants(tenant_id)` PK
- `identity_tenants.sponsor_tenant_id` FK self-reference
- `identity_ilks(ilk_id)` PK
- unique parcial por `(email, tenant_id)` cuando email existe y no deleted
- unique parcial por `node_name` cuando existe y no deleted
- unique en ICH por `(channel_type, address, tenant_id)`

Evaluacion:

- El modelo es claro y tiene constraints importantes.
- El uso de transaccion en persistencia ILK es correcto.
- `upsert_tenant_in_db` es simple y adecuado.
- Hay manejo de rollback in-memory si falla persistencia en varias mutaciones.

Fricciones:

- No hay sistema formal de migraciones versionadas.
- `CREATE TABLE IF NOT EXISTS` + algunos `ALTER TABLE` sirve para alpha, pero no asegura upgrades completos.
- Los mensajes de error DB no siempre estan clasificados; algunas fallas quedan como `DB_WRITE_FAILED`.
- `ensure_database_exists` requiere que el rol tenga permiso de crear DB si la DB no existe. El nuevo `fluxbee_db_bootstrap.sh` reduce esta friccion en instalacion limpia.

Decision operativa recomendada para alpha:

- Para testing limpio, usar `FLUXBEE_DB_RESET=1 scripts/install.sh`.
- No meter compatibilidad de DB vieja dentro de `SY.identity` salvo migraciones necesarias y deliberadas.
- Mantener `SY.identity` como duenio de schema, no de rol/cluster PostgreSQL.

## 8. Secrets y CONFIG_SET

Campo canonico:

```text
config.database.postgres_url
```

Persistencia:

```text
/var/lib/fluxbee/nodes/SY/SY.identity@motherbee/secrets.json
```

Precedencia de lectura:

1. `secrets.json` local del nodo.
2. `FLUXBEE_DATABASE_URL`.
3. `JSR_DATABASE_URL`.
4. missing.

Contrato:

- `CONFIG_GET` devuelve metadata y redaction.
- `CONFIG_SET` requiere:
  - `node_name`
  - `schema_version`
  - `config_version`
  - `apply_mode=replace`
  - `config.database.postgres_url`
- El valor real no vuelve en respuestas.
- Aplicar nuevo secret requiere restart de `sy-identity`.

Evaluacion:

- Alineado con `node_secret_tasks.md`.
- Buen uso de helper SDK para `secrets.json`.
- Metadata de auditoria usa `updated_by_ilk`, `updated_by_label` y `trace_id`.

Riesgos:

- Fallback env permite operar sin secret por nodo. Es util para bootstrap/testing, pero puede confundir diagnostico.
- Si `CONFIG_SET` se manda por un cliente que loguea payload completo, el secret puede quedar fuera de `SY.identity`; esto debe controlarse en `SY.admin`/Archi/SCMD.

## 9. SHM

`SY.identity` vuelca a SHM:

- tenants
- ILKs
- ICHs
- aliases
- mappings de lookup ICH -> ILK
- definition hashes para agentes

Capacidades por defecto:

- `max_ilks`: `8192`
- `max_tenants`: `1024`
- `max_vocabulary`: `4096`
- `max_ilk_aliases`: default igual a `max_ilks`

Configuracion:

```yaml
identity:
  max_ilks: ...
  max_tenants: ...
  max_vocabulary: ...
  max_ilk_aliases: ...
```

Comportamiento:

- Full sync inicial al crear writer.
- Fast path para `ILK_PROVISION`.
- Deltas incrementales para mutaciones.
- Si falla apply incremental, reconstruye snapshot completo.
- Heartbeat cada 5 segundos.

Evaluacion:

- Disenio correcto para lecturas de baja latencia.
- Buen fallback de snapshot completo.
- Inclusion de `sponsor_tenant_id`, `enabled`, `owner_l2_name` y hashes cognitivos habilita OPA/routing sin llamadas DB.

Riesgos:

- Si se exceden limites de SHM, puede degradar escritura/lectura. Debe haber monitoreo.
- La fuente de verdad sigue siendo DB en primario; SHM debe considerarse cache/snapshot operativo.

## 10. Sincronizacion entre hives

Primario:

- Escucha en `0.0.0.0:<identity.sync.port>`.
- Default: `9100`.
- Atiende:
  - `IDENTITY_FULL_SYNC_REQUEST`
  - `IDENTITY_DELTA_SUBSCRIBE`

Replica:

- Lee `identity.sync.upstream`.
- Hace full sync inicial.
- Se suscribe a deltas.
- Valida secuencia creciente.
- Acknowledge por delta.
- Reconecta cada 1 segundo si se cae.

Evaluacion:

- El protocolo es simple y suficiente para alpha.
- Full sync chunked evita payload unico gigante.
- Delta ACK con retries reduce perdida simple.

Riesgos:

- No hay autenticacion ni autorizacion en el socket TCP de sync.
- No hay TLS.
- No hay compresion ni backpressure avanzado.
- Si hay gap de secuencia, la replica corta stream y reconecta, pero no queda claro si siempre hace full resync antes de seguir.

Recomendaciones:

- En testing, limitar por firewall/red.
- Para produccion, agregar autenticacion de sync o encapsularlo en canal seguro.
- Ante gap de delta, forzar full sync antes de reabrir delta stream.
- Exponer metricas de sync: ultimo full sync, ultimo delta seq, reconnections, lag.

## 11. Definicion cognitiva de agentes

El cambio actual consolida el concepto de "alma" de agentes en `definition`.

Solo agentes (`ilk_type=agent`) pueden tener:

```json
{
  "role_hash": "<sha256>",
  "skill_hashes": ["<sha256>"],
  "handbook_hashes": ["<sha256>"]
}
```

Validaciones:

- JSON debe ser objeto.
- Solo se aceptan esas tres claves.
- Hashes deben ser SHA-256 hex de 64 caracteres.
- Arrays respetan limites de SHM (`IDENTITY_DEFINITION_MAX_SKILLS`, `IDENTITY_DEFINITION_MAX_HANDBOOKS`).
- Definicion vacia limpia la configuracion cognitiva.

Flujo:

- `ILK_SET_DEFINITION` modifica el ILK.
- Se persiste en DB como `identity_ilks.definition`.
- Se publica a SHM como bytes de hash.
- AI nodes pueden leer hashes desde SHM y luego resolver blobs/capacidades por fuera.

Evaluacion:

- El modelo es correcto porque identity guarda referencias inmutables por hash, no prompt text ni blobs grandes.
- Permite que OPA/routing usen capacidades por hash.
- Mantiene separation of concerns: identity conoce la identidad y definicion declarativa; storage/blob/cognition resuelven contenido.

Riesgo:

- Falta un contrato operacional visible que una "definicion" con su blob real y su lifecycle.
- Si un hash apunta a blob inexistente, identity lo acepta igual; la validacion de existencia queda en otra capa.

## 12. Configuracion relevante en hive.yaml

Campos usados:

```yaml
hive_id: motherbee
role: motherbee|worker

government:
  identity_frontdesk: SY.frontdesk.gov@motherbee

identity:
  default_tenant: fluxbee
  merge_alias_ttl_secs: 3600
  max_ilks: 8192
  max_tenants: 1024
  max_vocabulary: 4096
  max_ilk_aliases: 8192
  sync:
    port: 9100
    upstream: 127.0.0.1:9100
```

Campo legacy presente pero no usado para DB efectiva:

```yaml
database:
  url: ...
```

`SY.identity` define `DatabaseSection`, pero el flujo efectivo usa secret local o env. El doc operativo debe seguir marcando que DB secrets no van en `hive.yaml`.

## 13. Observabilidad

Eventos/logs relevantes:

- Arranque sin DB: `sy.identity started without active DB backend`.
- DB schema ensured.
- Store cargado desde DB.
- Full sync bootstrap applied/failed.
- SHM snapshot/delta apply.
- Alias GC local/DB.
- Sync request failed.
- Action response con elapsed.

Endpoints/acciones de diagnostico:

- `STATUS`
- `PING`
- `IDENTITY_METRICS`
- `CONFIG_GET`
- `TNT_LIST`
- `ILK_LIST`

Gap:

- No hay health endpoint HTTP propio; se opera via L2/admin.
- No hay metricas persistentes de replication lag.
- `STATUS` podria exponer mas informacion de sync en replicas.

## 14. Hallazgos

### H1 - Sync TCP sin autenticacion

Severidad: alta para produccion, aceptable para alpha controlada.

El listener de sync acepta conexiones TCP y procesa JSON line protocol. No hay autenticacion ni TLS.

Impacto:

- Un actor con acceso de red al puerto podria pedir full sync.
- Podria abrir subscriptions y consumir recursos.

Mitigacion actual:

- Firewall/orchestrator.
- Exposicion controlada del host.

Recomendacion:

- Autenticacion por token/clave del hive.
- Restriccion de origen por config.
- TLS o canal privado.

### H2 - Autorizacion basada en nombre L2

Severidad: media/alta.

Las acciones sensibles se autorizan por `src_l2_name` y prefixes. Si un nodo malicioso logra registrarse con un nombre privilegiado, podria mutar identidad.

Recomendacion:

- Atar permisos a ILK/system identity validada.
- Endurecer router para impedir collision/spoofing de nombres SY.
- Auditar en `SY.admin`/orchestrator la creacion de nodos con nombres SY.

### H3 - Migraciones DB no versionadas

Severidad: media.

`ensure_primary_schema` usa `CREATE TABLE IF NOT EXISTS` y algunos `ALTER TABLE`, pero no hay versionado formal.

Impacto:

- En alpha, borrar/recrear DB es aceptable.
- En produccion, cambios de schema pueden quedar a medias.

Recomendacion:

- Crear tabla `identity_schema_migrations`.
- Versionar migraciones SQL.
- Mantener `install/reset` como flujo de testing, no como upgrade.

### H4 - Env fallback puede confundir el modelo de secrets

Severidad: media.

`FLUXBEE_DATABASE_URL` y `JSR_DATABASE_URL` siguen activos como fallback.

Impacto:

- Un nodo puede arrancar configurado sin que `CONFIG_GET` muestre `local_file`.
- Operadores pueden creer que el secret esta persistido cuando viene de env.

Recomendacion:

- Mantener por compatibilidad en alpha.
- Documentar claramente `source=env_compat`.
- En futura version, degradar env fallback a bootstrap-only o eliminarlo.

### H5 - Errores DB parcialmente crudos

Severidad: baja/media.

Algunos errores incluyen string crudo de Postgres/parsing.

Impacto:

- Ayuda mucho en desarrollo.
- Puede exponer detalles internos en entornos compartidos.

Recomendacion:

- Mantener en alpha.
- Antes de produccion, mapear a codigos y mover detalle a logs internos.

### H6 - Gap de delta stream no fuerza full resync explicitamente

Severidad: media.

La replica detecta gap/out-of-order y reconecta. El loop de delta puede volver a subscribirse, pero no se observa full sync obligatorio antes de continuar.

Impacto:

- Una replica podria quedar con estado incompleto si pierde deltas y no hace full refresh.

Recomendacion:

- Ante gap, ejecutar full sync y recien despues resubscribirse.

### H7 - Vocabulary preparado pero no integrado

Severidad: baja.

La tabla/SHM para vocabulary existe, pero no hay flujo visible de mutacion/lectura equivalente a ILK/TNT.

Recomendacion:

- Si no se usa en sprint actual, documentarlo como reserved.
- Si se usa, agregar acciones y tests.

## 15. Pruebas existentes observadas

En `sy_identity.rs` hay tests unitarios sobre:

- Autorizacion de bootstrap/frontdesk.
- Rechazo de fuentes no autorizadas.
- Resolucion de ILK con alias y tenant embebido.
- `TNT_GET` con sponsor/counts.
- `TNT_LIST` ordenado y con summaries.
- Idempotencia de tenant create por name/domain.
- Default tenant root.
- Set/clear sponsor.
- Rechazo de ciclos/self-sponsor.
- Validacion de definicion cognitiva.
- Rechazo de definicion para no-agent.
- Lookup de ICH por tenant.

Faltantes sugeridos:

- E2E de `CONFIG_SET` + restart + DB ready.
- E2E de `TNT_CREATE` + `TNT_SET_SPONSOR` + `TNT_LIST` via admin.
- E2E de `ILK_SET_DEFINITION` + SHM readback.
- E2E de replica worker con full sync y delta.
- Test de gap de delta que verifique full resync.
- Test de `fluxbee_db_bootstrap.sh` en Postgres local limpio.

## 16. Runbook minimo

### Server limpio con PostgreSQL instalado

```bash
sudo PATH="$PATH" FLUXBEE_DB_USER=sa FLUXBEE_DB_PASSWORD='***' ./scripts/install.sh
```

### Testing desde cero

```bash
./scripts/fluxbee_stop.sh
sudo PATH="$PATH" FLUXBEE_DB_RESET=1 FLUXBEE_DB_USER=sa FLUXBEE_DB_PASSWORD='***' ./scripts/install.sh
```

### Cargar secret de DB en SY.identity

Usar `CONFIG_SET` via `SY.admin`/Archi con:

```json
{
  "node_name": "SY.identity@motherbee",
  "schema_version": 1,
  "config_version": 1,
  "apply_mode": "replace",
  "config": {
    "database": {
      "postgres_url": "postgresql://sa:***@127.0.0.1:5432/fluxbee"
    }
  }
}
```

Despues:

```bash
sudo systemctl restart sy-identity
```

### Verificar estado

```bash
curl -sS -X POST "http://127.0.0.1:8080/hives/motherbee/nodes/SY.identity@motherbee/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"operator"}' | jq .
```

Esperado:

- `ok=true`
- `state=configured`
- `config.database.postgres_url="***REDACTED***"`
- `config.database.source="local_file"`
- `config.database.db_name="fluxbee_identity"`

## 17. Recomendaciones priorizadas

### Corto plazo

- Mantener DB bootstrap fuera de `SY.identity`.
- Usar reset total de DB en testing, no migraciones ad hoc.
- Agregar E2E para `TNT_LIST`/`TNT_GET`/`TNT_SET_SPONSOR`.
- Agregar E2E para `ILK_SET_DEFINITION` y lectura SHM por hashes.
- Asegurar que docs/handbook de Archi expliquen `TNT_LIST`, `TNT_GET`, `ILK_SET_DEFINITION` y diferencia runtime vs instancia.

### Mediano plazo

- Agregar full resync ante gap de delta stream.
- Exponer metricas de sync de replica.
- Clasificar errores DB en codigos estables.
- Formalizar migraciones DB.

### Produccion futura

- Autenticacion/TLS para identity sync.
- Autorizacion por identidad criptografica, no solo L2 name.
- Endurecer alta de nombres `SY.*`.
- Evaluar cifrado at-rest para `secrets.json` como hardening, no como frontera contra root.

## 18. Criterio de salud esperado

Un `SY.identity@motherbee` sano deberia cumplir:

- `STATUS` reporta `state=configured`.
- `CONFIG_GET` reporta `ok=true`.
- `TNT_LIST` devuelve al menos el tenant root `fluxbee`.
- PostgreSQL tiene DB `fluxbee_identity`.
- `identity_tenants`, `identity_ilks`, `identity_ichs`, `identity_ilk_aliases` existen.
- SHM `/jsr-identity-motherbee` existe y tiene heartbeat activo.
- No hay warnings repetidos de `DB_NOT_READY`.
- No hay gaps de delta en workers.

## 19. Conclusion

`SY.identity` esta bien ubicado como autoridad de identidad de Fluxbee y ya concentra piezas importantes: tenants, sponsors, ILK/ICH, aliases, definicion cognitiva y publicacion a SHM. El mayor riesgo no esta en el modelo de datos, sino en las fronteras operativas: bootstrap de PostgreSQL, sync TCP sin autenticacion y autorizacion por nombre L2.

Para la etapa alpha, la decision correcta es mantener `SY.identity` simple y limpiar DBs desde infraestructura de instalacion. Para pasar a un entorno mas estable, las prioridades deberian ser migraciones DB versionadas, hardening de sync y autorizacion mas fuerte para mutaciones de identidad.
