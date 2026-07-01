# Auditoria sy.identity - operatividad y seguridad

Fecha: 2026-06-30  
Repositorio: `fluxbee`  
Branch auditada: `daily_onworking_coa`  
Commit auditado: `25e883e`  
Foco: `src/bin/sy_identity.rs`, contrato SDK de identidad, documentacion operativa y puntos de integracion con SHM, router, vault y replicacion.

## Resumen ejecutivo

`sy.identity` tiene una base funcional amplia y tests unitarios locales pasando, pero la superficie nueva de replicacion y algunos flujos administrativos tienen riesgos importantes antes de considerarlos aptos para operacion segura.

Los hallazgos mas relevantes son:

- El puerto TCP de sync de identidad queda autenticacion opcional por defecto, escucha en `0.0.0.0:9100` y permite full-sync, subscribe y publish sin HMAC si no se configura explicitamente `identity.sync.auth = required`.
- El manejo de conexiones de sync bloquea el loop principal durante el handshake y especialmente durante full-sync, lo que expone a `sy.identity` a DoS por clientes lentos o colgados.
- `VAULT_SECRET_CHANGED` se procesa antes del control de autorizacion normal y puede terminar el proceso primario con `exit(0)`.
- `TNT_CREATE` puede borrar de memoria un tenant existente si el request era idempotente pero falla la escritura a DB.
- Una replica puede quedar viva con estado incompleto, y ante gaps de delta solo reconecta sin forzar full-sync.
- Los ILKs publicados por replicas al primario parecen ser estado live, no durable: se aplican a memoria/SHM pero no se persisten en PostgreSQL.
- La autorizacion por prefijos L2 es demasiado amplia para acciones sensibles, y `ILK_REGISTER` permite crear `ilk_type = system` desde registradores que no deberian tener ese poder.

## Alcance y metodologia

Revision estatica de:

- `src/bin/sy_identity.rs`
- `crates/fluxbee_sdk/src/identity.rs`
- `crates/fluxbee_sdk/src/protocol.rs`
- `src/shm/mod.rs`
- integraciones relevantes en `src/bin/sy_vault.rs` y `nodes/io/common/src/inbound.rs`
- documentacion existente en `docs/`, incluyendo auditoria previa y planes `onworking COA`

Checks ejecutados:

```bash
cargo test -p fluxbee-sdk identity
cargo test --bin sy_identity
```

Resultado:

- `cargo test -p fluxbee-sdk identity`: OK, 6 tests pasados.
- `cargo test --bin sy_identity`: OK, 33 tests pasados.

Limitaciones:

- No se corrio E2E real con PostgreSQL, router, multiples hives y sockets TCP expuestos.
- No se hizo fuzzing de payloads ni prueba de carga sobre sync `9100`.
- No se auditaron exhaustivamente todos los consumidores de SHM; solo los puntos necesarios para evaluar impacto.

## Hallazgos

### F-01 - Critico - Sync TCP queda sin autenticacion por defecto y permite exfiltracion/poisoning

Evidencia:

- `identity_sync_auth_required` solo exige HMAC si la config es exactamente `required`; cualquier valor ausente o typo deja auth desactivada: `src/bin/sy_identity.rs:3176`.
- El primario escucha en todas las interfaces: `0.0.0.0:{sync_port}` en `src/bin/sy_identity.rs:2896`.
- `handle_sync_connection` solo hace challenge HMAC cuando `auth_required` es true: `src/bin/sy_identity.rs:3939`.
- Sin auth, un peer puede pedir `IDENTITY_FULL_SYNC_REQUEST` y recibir chunks completos: `src/bin/sy_identity.rs:3951`.
- Sin auth, un peer puede subscribirse a deltas: `src/bin/sy_identity.rs:3961`.
- Sin auth, un peer puede publicar `IDENTITY_DELTA_PUBLISH` con `hive_id` arbitrario; el chequeo `authed_hive` solo aplica si hubo auth: `src/bin/sy_identity.rs:4020`.
- La autoridad de delta es principalmente por contenido: `delta_authorized_for_hive` permite `IlkUpsert` si el ILK parece pertenecer al `publisher_hive`: `src/bin/sy_identity.rs:1613`.

Impacto:

- Cualquier peer que llegue al puerto de sync puede exfiltrar el estado de identidad completo si auth no esta configurada.
- Puede inyectar ILKs live para hives no primarios eligiendo un `node_name` con sufijo `@publisher_hive`.
- Aunque ese poisoning no se persista en DB, si llega a SHM puede afectar routing, autorizacion y consumidores de identidad hasta reinicio o reconciliacion.
- Un typo en config desactiva auth silenciosamente.

Recomendacion:

- Hacer auth requerida por defecto en primario.
- Fallar startup ante valores desconocidos de `identity.sync.auth`.
- Si se necesita modo local sin auth, bindear solo a loopback y exigir opt-in explicito.
- En publish, exigir siempre que `publisher_hive` este autenticado y, preferentemente, en allowlist de replicas esperadas.
- Agregar tests negativos para full-sync, subscribe y publish sin auth.

### F-02 - Alto - Una conexion de sync puede bloquear el loop principal

Evidencia:

- El branch `listener.accept()` del `tokio::select!` espera inline a `handle_sync_connection(...)`: `src/bin/sy_identity.rs:3101`.
- En full-sync se hacen `write_all` y `flush` de todos los chunks sin spawn ni timeout de escritura: `src/bin/sy_identity.rs:3951`.
- Subscribe y publish terminan spawneando tareas, pero recien despues de que el loop principal hizo handshake y parseo inicial.

Impacto:

- Un cliente lento o que deja de leer durante full-sync puede dejar ocupado el loop principal.
- Mientras tanto se demoran heartbeats, GC de aliases, mensajes de sistema, ingest de replicas, aplicacion de deltas, status y config.
- Combinado con F-01, es un DoS remoto sobre `9100`.

Recomendacion:

- Spawnear una task por conexion aceptada inmediatamente, o usar un pool acotado.
- Agregar timeouts de lectura y escritura a todas las operaciones de socket.
- Mantener el loop principal solo para mutaciones de estado y eventos internos.

### F-03 - Alto - `VAULT_SECRET_CHANGED` puede reiniciar el primario antes de autorizacion

Evidencia:

- `process_system_message` procesa `MSG_VAULT_SECRET_CHANGED` antes de llamar a `is_authorized`: `src/bin/sy_identity.rs:1788`.
- El handler del primario termina el proceso con `std::process::exit(0)` si el payload matchea el interes local: `src/bin/sy_identity.rs:6084`.
- El interes local es amplio: `resource_type = postgres`, root tenant, self ILK y `system_caller`: `src/bin/sy_identity.rs:6106`.
- `matches_interest` valida tipo de recurso y tenant/ilk, pero no autentica origen ni `source`: `crates/fluxbee_sdk/src/protocol.rs:351`.

Impacto:

- Cualquier nodo que pueda rutear un system message con payload compatible hacia `SY.identity@motherbee` puede forzar restart del primario.
- Si el evento se repite, puede producir loop de disponibilidad.
- El diseno documentado de broadcasts abiertos para vault asume bajo impacto porque solo dispara re-resolve; aca el efecto real es salida del proceso.

Recomendacion:

- Aceptar este mensaje solo desde `SY.vault@<hive-local>` o desde una ruta autenticada equivalente.
- Validar `payload.hive_id == hive_id` y origen antes de salir.
- Considerar re-resolver primero y reiniciar solo si cambio efectivamente el secreto/version.
- Agregar rate limit y test de origen no autorizado.

### F-04 - Alto - `TNT_CREATE` puede borrar un tenant existente de memoria ante falla de DB

Evidencia:

- `create_tenant` devuelve un tenant existente cuando el nombre o domain ya existe, con `created = false`, sin mutar el store: `src/bin/sy_identity.rs:1162`.
- El handler, ante error de DB, ejecuta siempre `self.store.tenants.remove(&tenant_id)`: `src/bin/sy_identity.rs:2351`.

Impacto:

- Si se envia un `TNT_CREATE` idempotente para un tenant existente y falla la persistencia, se borra de memoria ese tenant.
- El caso puede afectar incluso al root tenant si el request matchea su nombre/domain.
- Como la respuesta es error, no necesariamente se reconstruye SHM ni se emite delta, dejando memoria, SHM y DB divergentes hasta restart.

Recomendacion:

- Hacer rollback por snapshot como en otros caminos mutantes, o remover solo cuando `created == true`.
- Agregar test: tenant existente + falla DB inyectada conserva el store intacto.

### F-05 - Alto - Replicas pueden quedar vivas con estado incompleto y gaps de delta no fuerzan full-sync

Evidencia:

- En worker, si falla el full-sync inicial se loguea warning y el nodo sigue con estado local: `src/bin/sy_identity.rs:2904`.
- `run_delta_subscription_loop` reconecta ante errores del stream: `src/bin/sy_identity.rs:4233`.
- `stream_deltas_from_primary` detecta gaps/out-of-order y retorna error: `src/bin/sy_identity.rs:4296`.
- El caller no fuerza full-sync despues del gap; vuelve a subscribirse.
- `STATUS` reporta role/state/database, pero no salud de upstream, secuencia de delta, ultimo full-sync ni lag: `src/bin/sy_identity.rs:1831`.

Impacto:

- Una replica puede publicar SHM incompleto y parecer operativa.
- Si se pierden deltas, la reconexion puede continuar desde un estado inconsistente sin recuperar el faltante.
- Otros nodos pueden tomar decisiones de identidad con datos parciales.

Recomendacion:

- Si falla full-sync inicial, marcar la replica como degraded/not-ready y exponerlo en `STATUS`.
- Ante gap de delta, limpiar o marcar estado y ejecutar full-sync antes de resubscribir.
- Agregar campos de salud: `upstream_connected`, `last_full_sync_at`, `last_delta_seq`, `delta_lag`, `reconnects`, `last_sync_error`.

### F-06 - Medio - ILKs publicados por replicas no se persisten en PostgreSQL

Evidencia:

- El primario persiste sus ILKs de sistema propios durante bootstrap: `src/bin/sy_identity.rs:2928`.
- Las replicas envian periodicamente `self_owned_ilks`: `src/bin/sy_identity.rs:2991`.
- El primario ingesta deltas de replicas, actualiza memoria/SHM y rebroadcast, pero no persiste esos ILKs en DB en ese camino: `src/bin/sy_identity.rs:3020`.

Impacto:

- Los ILKs de hives worker quedan como estado live/cache en el primario.
- Tras restart del primario, el estado global pierde ILKs remotos hasta que cada worker este online y republique.
- Si un worker esta caido durante el restart, sus ILKs pueden desaparecer de la vista global.

Recomendacion:

- Definir explicitamente si los ILKs de replica son fuente durable o cache live.
- Si son durables, persistir `IlkUpsert`/delete aceptados por delta en la DB primaria.
- Si son cache live, documentarlo y exponer estado/edad de cada hive remoto.

### F-07 - Medio/Alto - Autorizacion por prefijos L2 es demasiado amplia

Evidencia:

- Varias acciones sensibles aceptan prefijos como `SY.admin@`, `SY.architect@`, `SY.frontdesk.gov@`, `SY.orchestrator@` e `IO.`: `src/bin/sy_identity.rs:1675`.
- `is_authorized` acepta exact matches o `starts_with(prefix)`: `src/bin/sy_identity.rs:2666`.
- Hay tests para fuente faltante y frontdesk configurado, pero no para rechazo cross-hive de un prefijo privilegiado.

Impacto:

- Un L2 privilegiado de otro hive podria ejecutar acciones administrativas si el router permite llegar.
- Un registro con nombre que empieza por un prefijo sensible recibe permisos de esa familia.
- Acciones como `CONFIG_SET`, tenant create/update/sponsor, `ILK_DELETE` y set de definicion quedan demasiado expuestas.

Recomendacion:

- Para acciones de alto riesgo, usar allowlist exacta y local: `SY.admin@<hive>`, `SY.architect@<hive>`, frontdesk exacto configurado y orchestrator exacto donde aplique.
- Hacer excepciones cross-hive explicitas por accion.
- Agregar tests negativos para `SY.admin@foreign`, frontdesk no configurado y nombres con prefijo reservado.

### F-08 - Medio/Alto - `ILK_REGISTER` permite crear ILKs `system` desde registradores amplios

Evidencia:

- `validate_ilk_type` acepta `human`, `agent` y `system`: `src/bin/sy_identity.rs:4829`.
- `register_ilk` escribe directamente el `ilk_type` solicitado: `src/bin/sy_identity.rs:885`.
- `ILK_REGISTER` esta autorizado para `SY.frontdesk.gov@` y `SY.orchestrator@`: `src/bin/sy_identity.rs:1681`.
- `ILK_PROVISION` si tiene un validador separado que rechaza `system`: `src/bin/sy_identity.rs:4836`.
- En vault, un caller con ILK tipo `system` tiene fallback de lectura de pool root: `src/bin/sy_vault.rs:1392`.

Impacto:

- Un frontdesk comprometido o bugueado puede registrar ILKs tipo `system` en tenants activos.
- Dado que otros servicios confian en SHM para resolver tipo de ILK, esto puede ampliar privilegios downstream.

Recomendacion:

- Separar permisos por tipo: frontdesk solo deberia registrar `human`/`agent`.
- Reservar `system` para bootstrap, definiciones deterministicas de `hive.yaml` o un flujo administrativo mas restringido.
- Agregar test: `SY.frontdesk.gov` + `ilk_type = system` debe rechazarse.

### F-09 - Medio - Permisos SHM `0600` y fallback silencioso a miss degradan operacion

Evidencia:

- La region SHM se crea con modo `0o600`: `src/shm/mod.rs:2553`.
- El SDK convierte `EACCES`/`EPERM` en `Ok(None)` o `false`: `crates/fluxbee_sdk/src/identity.rs:452`, `crates/fluxbee_sdk/src/identity.rs:505`, `crates/fluxbee_sdk/src/identity.rs:936`.
- IO common loguea el error y trata el resultado como miss/provisioning path: `nodes/io/common/src/inbound.rs:185`.
- La documentacion de IO ya menciona el sintoma `EACCES` y que los runtimes deben tener read access.

Impacto:

- Si consumidores y `sy.identity` no corren con el mismo usuario/grupo, el sistema degrada a RPC/provisioning sin fallar fuerte.
- Se ocultan problemas de instalacion y se incrementa carga sobre `sy.identity`/frontdesk.

Recomendacion:

- Estandarizar usuario/grupo/ACL para SHM en instalacion.
- Exponer `EACCES` como metrica/health issue aunque el fallback siga funcionando.
- Agregar guard operativo que valide acceso de lectura desde cada runtime.

### F-10 - Bajo/Medio - Migraciones de schema siguen siendo ad hoc

Evidencia:

- `ensure_primary_schema` usa `CREATE TABLE IF NOT EXISTS` y `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, sin tabla de versionado ni migraciones ordenadas: `src/bin/sy_identity.rs:5058`.

Impacto:

- Puede ser aceptable para alpha/testing, pero es fragil para upgrades de produccion.
- No hay forma clara de auditar partial migrations, rollback o drift entre ambientes.

Recomendacion:

- Agregar tabla `identity_schema_migrations`.
- Convertir cambios de schema en migraciones versionadas e idempotentes.
- Registrar version esperada en `STATUS`.

## Orden sugerido de remediacion

1. Cerrar F-01 y F-02 juntos: auth requerida por defecto, fail-closed de config y manejo concurrente/timeouts de sync.
2. Corregir F-03 para evitar restart remoto no autorizado.
3. Corregir F-04 porque puede corromper estado en memoria con un flujo comun/idempotente.
4. Endurecer replicacion: F-05 y decision de durabilidad F-06.
5. Reducir privilegios: F-07 y F-08.
6. Mejorar operabilidad: F-09 y F-10.

## Gaps de cobertura recomendados

- Tests de sync con auth ausente, requerida, typo de config y publish con hive no autenticado.
- Test de cliente full-sync lento para verificar que no bloquea heartbeats/status.
- Test de `VAULT_SECRET_CHANGED` desde origen no vault.
- Test de rollback `TNT_CREATE` contra tenant existente con error de persistencia.
- Test de replica con gap de delta que debe forzar full-sync.
- Tests cross-hive para autorizacion por `src_l2_name`.
- Tests de `ILK_REGISTER` rechazando `system` desde frontdesk.

## Conclusion

`sy.identity` esta avanzado funcionalmente, pero hoy la combinacion de sync abierto por defecto, DoS por loop bloqueable y algunos bypasses/privilegios amplios lo deja en una posicion riesgosa para despliegues donde el puerto de sync o el router no esten estrictamente aislados.

La recomendacion es tratar F-01, F-02, F-03 y F-04 como bloqueantes antes de exponerlo fuera de un entorno de desarrollo controlado.
