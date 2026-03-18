# Fluxbee SDK / Integración - Tareas operativas

Estado:
- documento vivo para registrar fricciones, mejoras y bloqueos detectados durante pruebas reales con SDK, router, identity, orchestrator y tooling
- regla operativa: si algo bloquea una prueba o uso básico, se corrige en el momento y queda marcado como hecho; si no bloquea, se deja acá como backlog explícito

## Abiertos

- [ ] Identity SHM: revisar modelo de permisos para lectura por nodos no privilegiados.
  - Hallazgo: el SHM se crea con `0600`.
  - Pregunta abierta: eso es policy deseada o sólo un default demasiado estricto para integraciones SDK?
  - Esta tarea es de diseño; no está tratada todavía como bug obligatorio.

- [ ] Publish por API: evaluar endpoint HTTP de upload/staging para que `fluxbee-publish` pueda ser reemplazable por llamada API completa.
  - No bloquea operación actual.
  - Queda como mejora de producto/herramientas.

## Hechos

- [x] Identity SHM: reemplazar el sync global en hot path por escritura incremental orientada a deltas.
  - Problema observado: `ILK_PROVISION` disparaba un rewrite completo del snapshot de identity SHM y el router fallaba leyendo `/jsr-identity-<hive>` durante la ventana de seqlock.
  - Cambio aplicado:
    - `IdentityRegionWriter` ahora soporta upserts/remociones incrementales para tenants, ILKs, ICH entries, mappings y aliases.
    - `sy_identity` aplica `IdentityDelta` directamente al SHM en vez de ejecutar `sync_identity_shm_mappings(...)` por cada acción.
    - `ILK_PROVISION` quedó con fast path atómico de una sola ventana de seqlock para escribir ILK temporal + ICH + mappings, evitando varias escrituras encadenadas sobre el mismo snapshot.
    - el rebuild completo queda como fallback de reparación si la aplicación incremental falla.
  - Resultado esperado: `ILK_PROVISION` y otras acciones puntuales dejan de reescribir regiones completas del SHM.

- [x] Identity SHM / Router: distinguir timeout de lectura (`SeqLockTimeout`) de `InvalidHeader`.
  - El router ahora puede diagnosticar mejor cuándo el snapshot no está corrupto sino simplemente no disponible dentro de la ventana de lectura.

- [x] Instrumentación temporal fuerte para diagnóstico de `ILK_PROVISION -> SHM -> router pre-resolve`.
  - `sy_identity` ahora loguea recepción de `ILK_PROVISION`, duración del store update, duración del apply al SHM y momento de respuesta usando `trace_id`.
  - el writer del SHM loguea timeout de lectura con contadores de reintento/seqlock.
  - el router loguea apertura/lectura del snapshot identity y tiempos del `identity-aware resolve`.
  - Objetivo: reconstruir orden exacto entre procesos y distinguir contención real de desincronismo entre actor/respuesta/lectura.

- [x] Identity SHM: corregida salida temprana que dejaba el seqlock abierto en `ILK_PROVISION`.
  - Hallazgo con logs reales: `odd_seq_spins > 0`, `seq_retry_count = 0` y `last_seq` impar mostraban que el writer entraba en la región crítica y no salía.
  - Causa: `provision_temporary_ilk(...)` hacía `?` sobre `upsert_ich_mapping_entry(...)` dentro de la sección protegida, y un error dejaba `seq` impar hasta reinicio.
  - Corrección: cerrar siempre la sección crítica antes de propagar error y dejar `warn` explícito si falla la carga de mappings.

- [x] Identity SHM: migración del writer a guard/RAII para seqlock.
  - Se agregó `SeqlockWriteGuard` en la capa compartida de SHM.
  - `RouterRegionWriter`, `ConfigRegionWriter`, `LsaRegionWriter` e `IdentityRegionWriter` ya no dependen de `seqlock_begin_write/seqlock_end_write` manual en sus operaciones de escritura.
  - Resultado: el hot path de identity queda protegido estructuralmente contra salidas tempranas que dejen el lock abierto.

- [x] SHM core: recovery automático de `seq` impar heredado al abrir writers.
  - Hallazgo: aun después de corregir el hot path, una SHM persistida podía seguir arrancando con `seq` impar de una corrida previa.
  - Corrección aplicada: `RouterRegionWriter`, `ConfigRegionWriter`, `LsaRegionWriter` e `IdentityRegionWriter` normalizan `seq` impar al abrir y lo dejan logueado.
  - Alcance: la corrección vive en la capa compartida `src/shm/mod.rs`, así que cubre a los binarios del sistema que usan esos writers.

- [x] SHM core: cobertura explícita de recovery para `seq` impar heredado.
  - Se agregaron tests de reapertura/recovery para `router`, `config`, `lsa` e `identity`.
  - Objetivo cubierto: validar que una SHM persistida con `seq` impar se normaliza al reabrirse antes de volver a operar.

- [x] Revisión de arquitectura SHM: writers centralizados, reader del SDK todavía separado.
  - Los writers/headers de SHM del sistema viven en `src/shm/mod.rs`.
  - `rt-gateway`, `sy_identity` y `sy_config_routes` consumen esa capa central.
  - El SDK sólo duplica lectura de identity SHM en `crates/fluxbee_sdk/src/identity.rs`; ese es el punto pendiente de alineación.

- [x] SDK/SHM: reader de identity alineado con la semántica central.
  - Se mantuvo la implementación separada en `crates/fluxbee_sdk/src/identity.rs`, pero quedó alineada con `json_router::shm` en:
    - validación de región antes de abrir el header
    - auto-discovery de límites desde el header válido
    - loop de lectura bajo seqlock
    - timeout explícito `SeqLockTimeout`
    - logging de diagnóstico con contadores equivalentes cuando vence la lectura
  - Resultado: SDK y sistema ya no divergen en el comportamiento observable del reader de identity SHM, aunque el código todavía no esté extraído a una librería compartida.

- [x] Identity: eliminar carrera entre `ILK_PROVISION_RESPONSE` y visibilidad del ILK en el SHM consumido por el router.
  - Hallazgo en prueba real: `IO.test` provisionaba un ILK temporal y enviaba el probe inmediatamente, pero el router todavía no lo veía en SHM y caía a OPA.
  - Ajuste aplicado: `sy_identity` ahora sincroniza el SHM antes de responder acciones del sistema que mutan el store.

- [x] Packaging E2E núcleo cerrado.
  - `PUB-T23`: `config_only` con asserts de `_system.runtime_base` y `_system.package_path`.
  - `PUB-T24`: `workflow` reescrito desde cero como flujo autocontenido.
  - `PUB-T25`: matriz negativa compacta.
  - `PUB-T26`: template layering.

- [x] Orchestrator relay: agregar `NodeUuidMode::{Persistent, Ephemeral}` y usar `Ephemeral` para relays temporales.
  - Resultado: los relays siguen teniendo UUID L1 y nombre L2, pero ya no ensucian `state/nodes` con `*.uuid` efímeros.

- [x] Documentación SDK/protocolo actualizada para `NodeUuidMode`.
  - README principal.
  - `docs/02-protocolo.md`.

- [x] README principal actualizado con guía operativa para publicar runtimes custom con `fluxbee-publish`.
  - build
  - package
  - deploy
  - readiness
  - spawn
  - verificación de `_system`

- [x] Ejemplos reales agregados en `nodes/test/`.
  - `io-test`
  - `ai-test-gov`
  - Objetivo: mostrar uso real del SDK y probar routing de ILK temporal hacia frontdesk configurado.

- [x] `io-test`: tratar `EACCES|EPERM` del SHM de identity como lookup no disponible y hacer fallback a provision.
  - Esto desbloquea la prueba real del flujo aunque todavía falte endurecer el helper del SDK.

- [x] SDK: degradar `IdentityShmError::Nix(EACCES|EPERM)` a "lookup unavailable" en helpers de alto nivel.
  - `resolve_ilk_from_shm_name`, `resolve_ilk_from_hive_id` y `resolve_ilk_from_hive_config` ahora devuelven `Ok(None)` cuando el lookup falla por permiso denegado.
  - Resultado: el fallback a `ILK_PROVISION` ya no depende de workarounds específicos en cada integrador.

- [x] Router/SDK: contrato de `src_ilk` alineado.
  - El contrato canónico queda en `meta.src_ilk` y el `Meta` tipado del SDK ya lo expone.
  - El router ahora consume `meta.src_ilk` como única fuente para el pre-resolve/canonicalización.
  - Los ejemplos `IO.test` y `AI.test.gov` ya usan `meta.src_ilk` como forma principal.

- [x] Nodos de referencia de identity con ciclo completo de software por CLI.
  - Script: `scripts/identity_test_nodes_publish_e2e.sh`.
  - Valida build -> package -> `fluxbee-publish` -> `--deploy` -> readiness -> spawn -> reply real entre `IO.test` y `AI.test.gov`.
  - Objetivo: reproducir problemas de install/rollout en entorno de prueba usando runtimes controlados, sin tocar nodos productivos.

## Notas de arquitectura

- El `fluxbee-publish` actual entra por HTTP a `SY.admin`; no usa el socket/router como cliente.
- Un runtime publicado queda en `dist` y en `/versions`, pero no se vuelve nodo gestionado hasta el `spawn`.
- Los nodos custom spawneados pasan a ser workloads gestionados por la infra, no componentes core tipo `SY.*` o `RT.*`.
- El routing automático de ILKs temporales hacia frontdesk existe, pero depende de:
  - `government.identity_frontdesk` configurado en el `hive.yaml` instalado
  - `src_ilk` presente en el shape que hoy consume el router
  - `registration_status = temporary` en identity
- La convención `.gov` hoy es convención operativa, no enforcement del sistema.
- Para identity SHM:
  - `full snapshot sync` queda reservado para bootstrap, rebuild y fallback.
  - el hot path de acciones del sistema debe operar con deltas incrementales.
  - `ILK_PROVISION` no debe abrir varias ventanas de seqlock seguidas; si vuelve a aparecer contención, revisar primero que siga entrando al fast path atómico antes de mirar OPA o routing.
  - las escrituras seguras deberían tender a una abstracción con guard/RAII, no a `begin/end` manual repetido.
  - si vuelve a aparecer contención de lectura, revisar primero:
    - duración de ventana de seqlock
    - costo de reindex de ICH
    - frecuencia de fallback a rebuild completo

## Criterio para nuevas entradas

- Si aparece una fricción real durante pruebas o integración externa:
  - agregar item acá
  - marcar si es `bloqueante` o `no bloqueante`
  - si se corrige en el momento, dejarla en `Hechos` con una línea de contexto
