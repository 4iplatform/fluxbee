# Fluxbee SDK / Integración - Tareas operativas

Estado:
- documento vivo para registrar fricciones, mejoras y bloqueos detectados durante pruebas reales con SDK, router, identity, orchestrator y tooling
- regla operativa: si algo bloquea una prueba o uso básico, se corrige en el momento y queda marcado como hecho; si no bloquea, se deja acá como backlog explícito

## Abiertos

- [ ] SDK: degradar `IdentityShmError::Nix(EACCES|EPERM)` a "lookup unavailable" en helpers de alto nivel, no sólo en ejemplos.
  - Contexto: `IO.test` falló al leer el SHM de identity desde un proceso sin permisos de lectura.
  - Hoy el workaround quedó absorbido en `nodes/test/io-test/src/main.rs`.
  - Objetivo: que el fallback a `ILK_PROVISION` sea natural para integradores del SDK.

- [ ] Router/SDK: alinear contrato de `src_ilk`.
  - Hallazgo: el router hoy hace pre-resolve leyendo `meta.context.src_ilk`.
  - Riesgo: integradores pueden asumir otro shape del mensaje si leen docs viejas o ejemplos incompletos.
  - Objetivo: definir y documentar un contrato único, y si hace falta aceptar ambos formatos temporalmente.

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
    - el rebuild completo queda como fallback de reparación si la aplicación incremental falla.
  - Resultado esperado: `ILK_PROVISION` y otras acciones puntuales dejan de reescribir regiones completas del SHM.

- [x] Identity SHM / Router: distinguir timeout de lectura (`SeqLockTimeout`) de `InvalidHeader`.
  - El router ahora puede diagnosticar mejor cuándo el snapshot no está corrupto sino simplemente no disponible dentro de la ventana de lectura.

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
  - si vuelve a aparecer contención de lectura, revisar primero:
    - duración de ventana de seqlock
    - costo de reindex de ICH
    - frecuencia de fallback a rebuild completo

## Criterio para nuevas entradas

- Si aparece una fricción real durante pruebas o integración externa:
  - agregar item acá
  - marcar si es `bloqueante` o `no bloqueante`
  - si se corrige en el momento, dejarla en `Hechos` con una línea de contexto
