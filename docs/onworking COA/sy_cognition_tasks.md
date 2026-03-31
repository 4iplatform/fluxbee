# SY.cognition v2 - Backlog de implementación y migración grande

Fecha base: 2026-03-31  
Spec fuente: [`docs/12-cognition-v2.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/12-cognition-v2.md)

## 0) Estado real de partida

Hoy `SY.cognition` **no existe** como binario implementado en el repo.

Estado observado del sistema:
- no existe `src/bin/sy_cognition.rs`
- no existe `jsr-memory` implementado en código
- no existe `memory_package` v2 implementado en router
- no existe `compute_thread_id(...)` en SDK
- el protocolo activo sigue anclado en `ctx`, `ctx_seq`, `ctx_window`
- los AI nodes hoy manejan memoria inmediata y `thread_state` con key canónico `src_ilk`
- `storage.events`, `storage.items`, `storage.reactivation` responden al modelo cognitivo viejo, no al modelo v2 consolidado

Conclusión:
- esto no es una iteración menor
- es una **migración fundacional** de protocolo + SDK + router + IO + storage + AI + nuevo servicio `SY.cognition`

---

## 1) Decisiones ya cerradas

Estas decisiones se toman como base y no se vuelven a discutir durante la implementación salvo bug grave de diseño:

- [x] `thread` pasa a ser la unidad física/canónica de continuidad conversacional en cognition.
- [x] `ctx`, `ctx_seq`, `ctx_window` responden a una versión inicial y deben migrarse fuera del núcleo cognitivo v2.
- [x] `thread_id` se calcula en SDK y lo usan los IO nodes o el productor que conozca el canal/medium.
- [x] `thread_seq` forma parte del carrier canónico de cognition y lo asigna el router por `thread_id`.
- [x] la memoria inmediata de AI y `thread_state` **no** cambian a `thread_id`; siguen keyed por `src_ilk`
- [x] `thread_id` queda como metadata auxiliar para AI, no como key principal de su estado runtime
- [x] `SY.cognition` sigue separado de OPA y de `SY.policy`
- [x] si hay que extender ambos SDKs, se hace
- [x] conviene un cambio inicial grande y coherente antes que iteraciones chicas que congelen una arquitectura híbrida mala

Regla arquitectónica:
- `thread` pertenece a cognition y al modelo de conversación física
- `src_ilk` sigue perteneciendo al runtime operativo inmediato de AI
- no forzar una unificación artificial entre ambos

---

## 2) Principios de migración

- no reciclar conceptos viejos con nombres nuevos
- no mezclar `ctx` viejo con `thread/scope/context` v2 en la lógica nueva
- compatibilidad solo donde haga falta para no romper el sistema vivo durante la transición
- una vez que un producer canónico emite v2, el código nuevo debe dejar de producir shapes v1 salvo carrier explícito de compat
- `SY.cognition` v2 no debe nacer acoplado a OPA, claims ni policy
- `SY.cognition` debe usar los patrones de los otros nodos SY:
  - config
  - state dir
  - secrets
  - socket/router
  - NATS
  - SDK helpers
  - status/health/admin

---

## 3) Fricciones concretas a resolver

### 3.1 Protocolo

- [`docs/02-protocolo.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/02-protocolo.md) sigue modelando:
  - `ctx`
  - `ctx_seq`
  - `ctx_window`
  - `memory_package` viejo
- v2 necesita:
  - `thread_id` canónico
  - `thread_seq` canónico
  - nuevo `memory_package`
  - eventualmente `reason`/`context` cognitivos, no `ctx` legacy

### 3.2 Router

- hoy no existe reader de `jsr-memory`
- hoy no existe ensamblado real de `memory_package`
- hay que decidir qué queda del carrier viejo (`ctx_window`) y qué deja de producir router

### 3.3 SDKs

- falta `compute_thread_id(...)`
- falta modelado canónico de `thread_id`
- falta modelado canónico de `thread_seq`
- falta contrato de `memory_package` v2
- falta contrato canónico para turn/event payloads que cognition va a consumir/producir

### 3.4 IO nodes

- el IO no genera hoy el `thread_id` v2 canónico
- el router no genera hoy `thread_seq` por `thread_id`
- hay que bajar reglas por tipo de canal:
  - DM / 1:1
  - group
  - medium-native thread

### 3.5 AI runtime

- hoy immediate memory y `thread_state` viven por `src_ilk`
- eso queda así
- pero AI debe empezar a recibir `thread_id` y eventualmente `memory_package` v2 en metadata sin romper el store vigente

### 3.6 Storage

- el modelo viejo `events/items/reactivation` no coincide con:
  - threads
  - contexts
  - reasons
  - scopes
  - memories
  - episodes
- hay que decidir si se:
  - reutilizan los subjects viejos con nuevo significado, o
  - se introduce contrato durable nuevo y limpio para cognition v2

Recomendación:
- **no reutilizar** `storage.events/items/reactivation` como base conceptual del modelo v2
- mantener `storage.turns` como input inmutable
- crear persistencia cognitiva v2 explícita

### 3.7 SHM

- `jsr-memory` v2 es completamente nuevo
- no hay base de código real para reciclar
- se debe diseñar desde el contrato, no desde restos de v1

---

## 4) Decisiones de implementación recomendadas

Estas decisiones no son solo tasks; son la forma recomendada de ejecutar el cambio.

### 4.1 Hacer v2 por capas, pero con contrato grande cerrado desde el día 1

La implementación puede ir por fases, pero el **contrato final** debe quedar cerrado al principio:
- thread
- context
- reason
- scope
- memory
- episode
- memory_package
- SHM
- persistencia durable

No construir una v2 “chiquita” con contrato incompleto, porque después cuesta más migrarla que hacer bien el corte ahora.

### 4.2 Mantener `src_ilk` y `thread_id` en planos distintos

- `src_ilk`: runtime/AI/inmediate memory/thread_state
- `thread_id`: cognition / continuidad física / cross-message

### 4.3 Arrancar Reason Evaluator determinístico

Para v1 de ejecución:
- tagger AI para:
  - `tags`
  - `reason_signals_canonical`
  - `reason_signals_extra`
- reason evaluator determinístico sobre las 8 commandments

Esto reduce costo y estabiliza comportamiento.

### 4.4 Introducir compat corta, no indefinida

Compatibilidad recomendada:
- leer `thread_id` donde exista
- tolerar `ctx` viejo mientras se migra la producción
- pero dejar de **producir** `ctx` como carrier canónico cuando entre la ola principal

---

## 5) Ola principal de migración

Esta es la propuesta de ejecución grande/coherente.

### Fase COG-M0 - Freeze de contratos canónicos

- [ ] COG-M0-T1. Congelar contrato v2 de `thread_id` en protocolo y SDK.
- [ ] COG-M0-T1b. Congelar contrato v2 de `thread_seq` en protocolo/core:
  - campo `meta.thread_seq`
  - lo asigna el router
  - orden canónico dentro del thread
- [ ] COG-M0-T2. Congelar shape de `memory_package` v2.
- [ ] COG-M0-T3. Congelar entidades durables:
  - `thread`
  - `context`
  - `reason`
  - `scope`
  - `scope_instance`
  - `memory`
  - `episode`
- [ ] COG-M0-T4. Congelar subjects/acciones de storage para cognition v2.
- [ ] COG-M0-T5. Congelar layout lógico de `jsr-memory`.
- [ ] COG-M0-T6. Congelar estrategia de compat con `ctx*`:
  - qué se deja de producir
  - qué se sigue leyendo temporalmente
  - fecha/criterio de remoción

Contrato propuesto a congelar en esta fase:
- carrier canónico del turn:
  - `meta.thread_id`
  - `meta.thread_seq`
  - `meta.ich`
  - `meta.src_ilk`
  - `meta.dst_ilk` opcional
- regla de ownership:
  - SDK/IO calculan `thread_id`
  - router asigna `thread_seq`
- política de compat:
  - `meta.ctx`, `meta.ctx_seq`, `meta.ctx_window` quedan legacy
  - se pueden seguir leyendo durante la migración
  - no deben seguir siendo el carrier canónico de nuevos paths
- `memory_package` v2:
  - lookup principal por `thread_id`
  - shape nuevo con `package_version=2`
  - `dominant_context` + `dominant_reason` obligatorios cuando el paquete no está vacío
- durable model v2:
  - `storage.turns` sigue como input inmutable
  - cognition v2 usa subjects explícitos por dominio, no reutiliza conceptualmente `storage.events/items/reactivation`
  - entidades durables recomendadas:
    - `cognition_threads`
    - `cognition_contexts`
    - `cognition_reasons`
    - `cognition_scopes`
    - `cognition_scope_instances`
    - `cognition_memories`
    - `cognition_episodes`
- `jsr-memory` v2:
  - lookup principal por `thread_id`
  - contiene resúmenes compactos, no evidencia completa
  - expone dominant/top refs para:
    - contexts
    - reasons
    - memories
    - episodes

Salida:
- un contrato único para implementación, sin ambigüedad v1/v2.

### Fase COG-M1 - Protocolo + SDK + carrier de mensajes

Nota práctica:
- el cambio en `Meta` del SDK/core tiene fan-out alto porque hoy hay muchas inicializaciones estructurales manuales de `Meta { ... }`
- conviene hacer esa actualización como una sola ola coherente dentro de `M1`, no como patch aislado

- [x] COG-M1-T1. Extender estructuras de mensaje en SDK/core para `meta.thread_id`.
- [x] COG-M1-T1b. Extender estructuras de mensaje en SDK/core para `meta.thread_seq`.
- [x] COG-M1-T2. Marcar `meta.ctx`, `meta.ctx_seq`, `meta.ctx_window` como legacy/deprecated en docs y types.
- [x] COG-M1-T3. Definir si `ctx_window`:
  - se elimina del carrier canónico, o
  - se mantiene como compat temporal solo para paths viejos
- [x] COG-M1-T4. Definir carrier canónico de `memory_package` v2 en `meta`.
- [x] COG-M1-T5. Actualizar `docs/02-protocolo.md` a la semántica nueva.
- [x] COG-M1-T6. Actualizar ejemplos/docs de AI/IO que todavía describen `ctx` como unidad central.

Estado actual de cierre de `M1`:
- `Meta` del SDK/core ya modela `thread_id`, `thread_seq`, `dst_ilk`, `ich`, `ctx*` legacy y `memory_package`.
- `docs/12-cognition-v2.md` y `docs/02-protocolo.md` ya reflejan el carrier v2.
- los paths IO nuevos ya pueden emitir `meta.thread_id` canónico y los AI nodes leen `meta.thread_id` con fallback legacy a `context.thread_id`.
- la asignación real de `thread_seq` ya quedó en el router por `thread_id`; la validación completa del root workspace sigue bloqueada externamente por `protoc` en `lance`.

Salida:
- protocolo y SDK ya hablan el idioma v2.

### Fase COG-M2 - Thread en SDK e IO

- [x] COG-M2-T1. Implementar `compute_thread_id(channel_type, params)` en SDK.
- [x] COG-M2-T2. Elegir función hash canónica y formato estable de input.
- [x] COG-M2-T3. Definir API de SDK para los tres casos:
  - DM / 1:1
  - group / persistent channel
  - medium-native thread
- [x] COG-M2-T4. Migrar IO nodes/productores para emitir `thread_id`.
- [x] COG-M2-T5. Implementar asignación de `thread_seq` en router por `thread_id`.
- [x] COG-M2-T6. Definir fallback cuando el medium no provee `native_thread_id`.
- [x] COG-M2-T7. Agregar tests de estabilidad/compat del hash y del sequencing por thread.

Estado actual de `M2`:
- `fluxbee_sdk` ya expone `compute_thread_id(...)`.
- el formato actual usa material canónico versionado + `sha256`, con salida `thread:sha256:<hex>`.
- la API ya separa `DirectPair`, `PersistentChannel` y `NativeThread`.
- `io-common` ya adopta el cálculo canónico para Slack:
  - `NativeThread` cuando existe `thread_ts`
  - `PersistentChannel` cuando no existe `thread_ts`
- `io-sim` ya genera `thread_id` canónico por defecto y mantiene `SIM_THREAD_ID` solo como override explícito.
- `SY.architect` ya emite `meta.thread_id` y `meta.ich` en el path de impersonation chat, en vez de depender solo de `meta.context`.
- el router ya asigna `thread_seq` cuando el mensaje entra sin secuencia usando `meta.thread_id` o fallback legacy a `context.thread_id`.
- hay tests de hash/compat en `fluxbee_sdk` y tests de sequencing en router; correrlos en el root workspace sigue bloqueado externamente por `protoc` en `lance`.

Compatibilidad legacy todavía abierta y a cerrar:
- el router todavía tiene fallback a `meta.context.thread_id` cuando falta `meta.thread_id`.
- ese fallback existe solo para transición/migración.
- debe removerse cuando los productores vivos ya no dependan del carrier legacy.
- ese cierre queda explícitamente diferido a `COG-M9`.

Salida:
- todo turn nuevo relevante ya nace con `thread_id`.

### Fase COG-M3 - Nuevo contrato durable de cognition

- [x] COG-M3-T1. Definir si cognition v2 persiste por NATS subjects nuevos o por socket/admin hacia `SY.storage`.
- [ ] COG-M3-T1b. Congelar contrato JetStream/durable consumer para cognition v2:
  - nombre canónico de consumer/queue para `SY.cognition` sobre `storage.turns`
  - nombres canónicos para consumidores futuros de entidades `storage.cognition.*`
- [ ] COG-M3-T1c. Congelar semántica de replay/restart:
  - desde qué offset/seq retoma `SY.cognition`
  - cómo reconstruye después de caída/redeploy
- [ ] COG-M3-T1d. Congelar política de ack/redelivery/poison handling:
  - cuándo se ackea
  - qué pasa si un turn falla repetidamente
  - cómo se evita bloquear el stream completo
- [ ] COG-M3-T2. No mezclar semánticamente `storage.events/items/reactivation` viejos con entidades nuevas sin una capa de compat explícita.
- [ ] COG-M3-T3. Definir tablas/contratos para:
  - `cognition_threads`
  - `cognition_contexts`
  - `cognition_reasons`
  - `cognition_scopes`
  - `cognition_scope_instances`
  - `cognition_memories`
  - `cognition_episodes`
  - tablas auxiliares de signals/tags si hacen falta
- [ ] COG-M3-T4. Definir modelo de persistencia de turns como input inmutable:
  - `storage.turns` sigue siendo la fuente base
- [ ] COG-M3-T5. Definir qué artefactos se reconstruyen y cuáles son source of truth.

Decisión recomendada:
- persistencia por NATS subjects explícitos de cognition v2
- no reciclar subjects viejos como carrier conceptual del modelo nuevo

Estado actual de `M3`:
- el contrato de transporte ya quedó congelado del lado core como familia NATS explícita `storage.cognition.*`.
- el embedded broker ya trata esos subjects como storage/durable stream subjects.
- `fluxbee_sdk` ya expone tipos compartidos para:
  - entidades durables cognition v2
  - operaciones (`upsert` / `close` / `delete`)
  - envelopes tipados
  - mapping entidad -> subject
- el boundary del router quedó más claro:
  - router publica input inmutable a `storage.turns`
  - router **no** publica entidades derivadas `storage.cognition.*`
  - las entidades derivadas las escribirá `SY.cognition` hacia `SY.storage`
- lo que sigue abierto en NATS/JetStream no es el publisher del router sino el contrato de consumidores durables:
  - queue names
  - replay/start position
  - ack/redelivery
  - recuperación tras restart
- falta todavía el contrato payload por entidad y el lado consumidor/escritor en `SY.storage` / `SY.cognition`.

Subjects recomendados:
- `storage.cognition.threads`
- `storage.cognition.contexts`
- `storage.cognition.reasons`
- `storage.cognition.scopes`
- `storage.cognition.scope_instances`
- `storage.cognition.memories`
- `storage.cognition.episodes`

Recomendación de source of truth:
- durable real:
  - turns
  - contexts
  - reasons
  - scopes
  - memories
  - episodes
- derivado/reconstruible:
  - `jsr-memory`
  - índices de enriquecimiento
  - caches locales

### Fase COG-M4 - Skeleton de `SY.cognition`

- [ ] COG-M4-T1. Crear `src/bin/sy_cognition.rs`.
- [ ] COG-M4-T2. Definir config del nodo:
  - NATS endpoint / subjects
  - storage endpoint
  - AI provider config/secret
  - paths locales
  - tuning de thresholds
- [ ] COG-M4-T3. Definir state dir:
  - `/var/lib/fluxbee/nodes/SY/SY.cognition@<hive>/...`
- [ ] COG-M4-T4. Conectar al router como nodo SY regular.
- [ ] COG-M4-T5. Exponer `PING`, `STATUS`, `CONFIG_GET`, `CONFIG_SET` si aplica
- [ ] COG-M4-T6. Definir arranque degradado:
  - sin AI provider
  - sin storage
  - sin SHM
  - sin DB cache
- [ ] COG-M4-T7. Integrar logs, status y unit systemd como el resto de SY.*

Salida:
- `SY.cognition` existe como proceso vivo y observable aunque todavía no procese todo el pipeline.

### Fase COG-M5 - Pipeline mínimo canónico: thread + tagger + contexts + reasons

- [ ] COG-M5-T1. Consumir turns desde el carrier decidido.
- [ ] COG-M5-T2. Implementar tagger extendido:
  - `tags`
  - `reason_signals_canonical`
  - `reason_signals_extra`
- [ ] COG-M5-T3. Implementar evaluator de contextos.
- [ ] COG-M5-T4. Implementar reason evaluator determinístico v1.
- [ ] COG-M5-T5. Implementar update determinístico de contextos.
- [ ] COG-M5-T6. Implementar update determinístico de razones.
- [ ] COG-M5-T7. Persistir entidades básicas v2.
- [ ] COG-M5-T8. Definir y persistir co-ocurrencias contexto/razón si se mantienen como entidad explícita.

Salida:
- cognition ya produce conocimiento estructurado básico sin esperar scope/memory/episode.

### Fase COG-M6 - Scope + binding + periodización

- [ ] COG-M6-T1. Definir algoritmo de asociación thread → scope actual.
- [ ] COG-M6-T2. Implementar binding energy con:
  - similitud de contexto
  - similitud de reason canonical
  - similitud ILK
- [ ] COG-M6-T3. Implementar EMA y threshold de unbind.
- [ ] COG-M6-T4. Implementar `scope_instance`.
- [ ] COG-M6-T5. Emitir eventos de transición de scope.

Nota:
- si esta fase demora demasiado, puede diferirse la materialización completa de `scope_instance`, pero **no** redefinir el contrato. El contrato queda congelado desde `COG-M0`.

### Fase COG-M7 - Memory fusion + episodes

- [ ] COG-M7-T1. Implementar period detection con:
  - main context
  - dominant reason
- [ ] COG-M7-T2. Implementar summarizer de scope
- [ ] COG-M7-T3. Crear/reforzar/decayer memories
- [ ] COG-M7-T4. Persistir `dominant_context_id` + `dominant_reason_id`
- [ ] COG-M7-T5. Implementar gate de episodes
- [ ] COG-M7-T6. Persistir `evidence_reason_ids`

### Fase COG-M8 - SHM nueva + enrichment del router

- [ ] COG-M8-T1. Diseñar layout físico de `jsr-memory-<hive>`.
- [ ] COG-M8-T2. Implementar writer seqlock en `SY.cognition`.
- [ ] COG-M8-T3. Implementar reader en router.
- [ ] COG-M8-T4. Implementar fetch/build de `memory_package` v2.
- [ ] COG-M8-T5. Aplicar límites y truncación.
- [ ] COG-M8-T6. Garantizar que enrichment ocurre después del routing.
- [ ] COG-M8-T7. Garantizar que OPA no lee `memory_package`.

### Fase COG-M9 - Alineación con AI runtime y compat controlada

- [ ] COG-M9-T1. Alinear docs y runtime AI:
  - `thread_id` = metadata
  - `src_ilk` = key canónico de state/immediate memory
- [ ] COG-M9-T2. Garantizar que `memory_package` no rompe prompts/configs vigentes.
- [ ] COG-M9-T3. Revisar `AI.frontdesk.gov` y otros nodos que hoy dependen de `thread_state`.
- [ ] COG-M9-T4. Definir carrier legacy de compat para paths que todavía lean `ctx`.
- [ ] COG-M9-T5. Remover gradualmente producción canónica de `ctx*` en nuevos paths.

### Fase COG-M10 - Cold start, rebuild y cierre de migración

- [ ] COG-M10-T1. Implementar rebuild desde storage durable.
- [ ] COG-M10-T2. Regenerar `jsr-memory` desde durable.
- [ ] COG-M10-T3. Definir criterio de corrupción/rebuild local.
- [ ] COG-M10-T4. E2E completo:
  - turn con `thread_id`
  - tagger
  - contexts + reasons
  - scope
  - memory
  - episode
  - SHM
  - router enrichment
- [ ] COG-M10-T5. Remoción formal de mecanismos viejos:
  - `ctx` como unidad cognitiva canónica
  - shapes/documentación v1 incompatibles

---

## 6) Sistemas a tocar

Este cambio no es solo un nuevo binario. Toca como mínimo:

- [ ] `docs/12-cognition-v2.md`
- [ ] `docs/02-protocolo.md`
- [ ] `docs/03-shm.md`
- [ ] `docs/13-storage.md`
- [ ] `docs/AI_nodes_spec.md`
- [ ] IO docs/runtime specs
- [ ] `crates/fluxbee_sdk`
- [ ] SDK/runtime helpers de AI si corresponde
- [ ] router
- [ ] `SY.storage`
- [ ] `SY.cognition` nuevo
- [ ] AI nodes que consumen metadata conversacional

---

## 7) Riesgos fuertes

- Riesgo 1: construir `SY.cognition` nuevo sobre carriers `ctx` viejos y terminar con una pseudo-v2 híbrida.
- Riesgo 2: intentar usar `thread_id` también como key principal de AI state y romper flujos vivos que hoy dependen de `src_ilk`.
- Riesgo 3: reciclar `storage.events/items/reactivation` con semántica ambigua y contaminar el durable model.
- Riesgo 4: meter `memory_package` en router antes de congelar SHM/layout y truncation strategy.
- Riesgo 5: mezclar cognición con policy/OPA antes de estabilizar el pipeline base.

---

## 8) Orden recomendado de ejecución

Orden recomendado realista, sin traicionar el cambio grande:

1. `COG-M0` freeze de contratos
2. `COG-M1` protocolo + carrier
3. `COG-M2` thread en SDK/IO
4. `COG-M3` durable model
5. `COG-M4` skeleton de `SY.cognition`
6. `COG-M5` contexts + reasons
7. `COG-M6` scope/binding
8. `COG-M7` memories/episodes
9. `COG-M8` SHM + router enrichment
10. `COG-M9/M10` compat, rebuild y cierre

---

## 9) Recomendación práctica para la próxima iteración

La próxima iteración no debería empezar por código del binario.

Debería empezar por cerrar estas piezas de diseño/contrato:
- [ ] forma exacta de `meta.thread_id`
- [ ] qué pasa con `ctx`, `ctx_seq`, `ctx_window`
- [ ] contrato durable de cognition v2
- [ ] layout lógico de `jsr-memory`
- [ ] shape exacto de `memory_package`

Una vez cerrado eso, recién ahí conviene crear `SY.cognition`.

---

## 10) Criterio de cierre de esta ola

Esta ola queda cerrada cuando:
- `thread` reemplaza a `ctx` como unidad cognitiva canónica
- IO/sdk emiten `thread_id`
- `SY.cognition` existe y procesa pipeline v2
- router enriquece con `memory_package` v2
- AI mantiene `src_ilk` como key de state
- `jsr-memory` existe como SHM nueva real
- el durable model cognitivo v2 está estable
- no quedan mecanismos centrales mezclados entre v1 y v2

Estado:
- [ ] No iniciado en código
- [x] Dirección arquitectónica y backlog base definidos
