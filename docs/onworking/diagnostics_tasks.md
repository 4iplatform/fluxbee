# Diagnósticos - Variables y métricas transversales

Fecha base: 2026-02-22

Objetivo:
- centralizar qué diagnósticos existen hoy en el sistema,
- qué variables de entorno controlan cada flujo,
- y qué se está midiendo realmente (para usarlo como base de spec de observabilidad/diagnóstico).

Alcance:
- herramientas de diagnóstico/E2E (no runtime productivo directo),
- foco actual en router, NATS, OPA, orchestrator.

## 1) Inventario actual de diagnósticos

### 1.1 NATS RTT y estabilidad (WF)
- Script: `scripts/wf_nats_diag.sh`
- Binario: `src/bin/wf_nats_diag.rs`
- Propósito:
  - medir ida/vuelta request/reply por NATS (`wf.diag.echo`),
  - detectar timeouts/reconnects,
  - producir timeline compacto + resumen de latencias.

### 1.2 Orchestrator system flow (control plane)
- Script: `scripts/orchestrator_runtime_update_spawn_e2e.sh`
- Binario principal: `src/bin/orch_system_diag.rs`
- Propósito:
  - validar flujo `RUNTIME_UPDATE -> SPAWN_NODE -> KILL_NODE`,
  - validar respuesta explícita en errores de routing (`UNREACHABLE`, `TTL_EXCEEDED`),
  - validar modo `Resolve` (router + OPA) vs `Unicast`.

### 1.3 Salud OPA en SHM (pre-check de resolve)
- Binario: `src/bin/opa_shm_diag.rs`
- Integración:
  - usado por `scripts/orchestrator_runtime_update_spawn_e2e.sh` cuando `ORCH_ROUTE_MODE=resolve`.
- Propósito:
  - verificar estado OPA leído desde `/jsr-opa-<hive>` antes de correr pruebas de resolve.

### 1.4 Suite NATS integral
- Script: `scripts/nats_full_suite.sh`
- Propósito:
  - ejecutar smoke lifecycle NATS embebido,
  - E2E transporte admin-storage,
  - E2E funcional admin (nodos/routers/storage),
  - con cronometraje por etapa y total.

### 1.5 JetStream envelope E2E (contract-light)
- Script: `scripts/jetstream_envelope_e2e.sh`
- Binario: `src/bin/jetstream_envelope_diag.rs`
- Propósito:
  - validar infraestructura JetStream con payload JSON opaco (sin contrato de negocio),
  - verificar `publish -> consume -> ack`,
  - forzar redelivery por no-ack intencional en servidor de diagnóstico,
  - producir resumen simple de conteos (`published/received/acked/no-ack`).

### 1.6 Blob Sync E2E + invariancia de contrato
- Script: `scripts/blob_sync_e2e.sh`
- Binario: `src/bin/blob_sync_diag.rs`
- Propósito:
  - validar `resolve_with_retry` en escenario cross-root (modo sync),
  - validar invariancia del contrato (`BlobRef` + `text/v1`) entre modo sin sync y modo sync,
  - producir resumen compacto (`contract_invariant`, `consumer_retry_elapsed_ms`, roots y modo de sync).

## 2) Variables de diagnóstico (catálogo operativo)

## 2.1 `wf_nats_diag.sh` + `wf_nats_diag.rs`
- `NATS_URL`
- `WF_DIAG_SUBJECT`
- `WF_DIAG_TIMEOUT_SECS`
- `WF_DIAG_LOOPS`
- `WF_DIAG_INTERVAL_MS`
- `WF_DIAG_SID`
- `WF_DIAG_TRACE_PREFIX` (binario)
- `WF_DIAG_MODE=server|client` (binario)
- `JSR_LOG_LEVEL`
- `BUILD_BIN`
- `SKIP_NATS_PREFLIGHT`
- `INCLUDE_ROUTER_JOURNAL`
- `DIAG_ROUTER_LOG_LINES`
- `STREAM_CLIENT_LOG`
- `STREAM_SERVER_LOG`
- `SHOW_TIMING_SUMMARY`
- `SHOW_FULL_LOGS`
- `WF_DIAG_BIN_PATH`

## 2.2 `orchestrator_runtime_update_spawn_e2e.sh` + `orch_system_diag.rs`
- `TARGET_HIVE`
- `ORCH_TARGET_HIVE` (binario)
- `ORCH_RUNTIME`
- `ORCH_VERSION`
- `ORCH_TIMEOUT_SECS`
- `ORCH_SEND_KILL`
- `ORCH_SEND_RUNTIME_UPDATE`
- `ORCH_UNIT`
- `ORCH_ROUTE_MODE=unicast|resolve`
- `ORCH_EXPECT_SPAWN_UNREACHABLE_REASON`
- `ORCH_EXPECT_SPAWN_ERROR_CODE` (mutuamente excluyente con `ORCH_EXPECT_SPAWN_UNREACHABLE_REASON`)
- `ORCH_DIAG_NODE_NAME` (default: `WF.orch.diag`; útil para simular origen no autorizado)
- `JSR_LOG_LEVEL`
- `BUILD_BIN`

## 2.3 `opa_shm_diag.rs` (pre-check OPA)
- `OPA_HIVE_ID`
- `OPA_EXPECT_STATUS`
- `OPA_MIN_VERSION`
- `OPA_MAX_HEARTBEAT_AGE_MS`

## 2.4 `nats_full_suite.sh`
- `BASE`
- `HIVE_ID`
- `FULL_SUITE_PROFILE=resilience|perf`
- `FULL_SUITE_INCLUDE_LIFECYCLE`
- `FULL_SUITE_INCLUDE_ADMIN`
- `FULL_SUITE_INCLUDE_JETSTREAM_ENVELOPE`
- `FULL_SUITE_SMOKE_CHECK_STOP_START`
- `METRICS_TIMEOUT_SECS`
- pass-through a scripts internos:
  - `RUN_ROUTER_RESTART`
  - `RUN_ROUTER_STOP_START`
  - `RUN_STORAGE_RESTART`
  - `RUN_ROUTER_CYCLE`
  - `RUN_NODE_CYCLE`
  - `RUN_STORAGE_CONFIG_CYCLE`
  - `RUN_STORAGE_METRICS_CHECK`

## 2.5 `jetstream_envelope_e2e.sh` + `jetstream_envelope_diag.rs`
- `NATS_URL`
- `JETSTREAM_DIAG_MODE=server|client` (binario)
- `JETSTREAM_DIAG_STACK=router_nats|fluxbee_sdk`
- `JETSTREAM_DIAG_SUBJECT`
- `JETSTREAM_DIAG_QUEUE`
- `JETSTREAM_DIAG_SID`
- `JETSTREAM_DIAG_LOOPS`
- `JETSTREAM_DIAG_INTERVAL_MS`
- `JETSTREAM_DIAG_FAIL_FIRST_N`
- `JETSTREAM_DIAG_WAIT_SECS`
- `JETSTREAM_DIAG_TRACE_PREFIX` (binario)
- `JSR_LOG_LEVEL`
- `BUILD_BIN`
- `SHOW_FULL_LOGS`

## 2.6 `blob_sync_e2e.sh` + `blob_sync_diag.rs`
- `BUILD_BIN`
- `BLOB_DIAG_BIN_PATH`
- `BLOB_DIAG_SOURCE_ROOT`
- `BLOB_DIAG_TARGET_ROOT`
- `BLOB_DIAG_FILENAME`
- `BLOB_DIAG_CONTENT`
- `BLOB_DIAG_MIME`
- `BLOB_DIAG_PAYLOAD_TEXT`
- `BLOB_DIAG_SYNC_MODE=copy|external`
- `BLOB_DIAG_SYNC_DELAY_MS`
- `BLOB_DIAG_RETRY_MAX_WAIT_MS`
- `BLOB_DIAG_RETRY_INITIAL_MS`
- `BLOB_DIAG_RETRY_BACKOFF`
- `BLOB_DIAG_REQUIRE_SYNCTHING_ACTIVE`
- `BLOB_DIAG_SYNCTHING_SERVICE`
- `JSR_LOG_LEVEL`
- `SHOW_FULL_LOGS`

## 3) Qué se mide hoy (métricas efectivas)

### 3.1 Métricas de latencia NATS (WF diag)
- Cliente:
  - `elapsed_ms` por request (`wf nats diag client response received`).
  - `elapsed_ms` de fallo (`wf nats diag client request failed`).
  - contadores NATS: `nats_timeouts`, `nats_reconnects`, `nats_in_flight`, `nats_last_error`.
- Servidor:
  - `total_elapsed_ms` por handler (`wf nats diag server response published`).
- Resumen agregado del script:
  - `count`, `min`, `p50`, `p95`, `avg`, `max`,
  - `client_timeout_events`.

### 3.2 Métricas de flujo system/orchestrator
- Trazabilidad por `trace_id`.
- Resultado de cada etapa:
  - `SPAWN_NODE_RESPONSE.status`,
  - `KILL_NODE_RESPONSE.status`.
- Errores explícitos de routing:
  - `UNREACHABLE` con `reason` + `original_dst`,
  - `TTL_EXCEEDED` con `original_dst` + `last_hop`.
- Modo de routing validado en salida:
  - `route_mode` (`unicast` o `resolve`).

### 3.3 Métricas de salud OPA por SHM
- `opa_status` (`ok|loading|error|unknown`).
- `policy_version`.
- `heartbeat_age_ms` (frescura de estado).
- `wasm_size`, `entrypoint`, `owner_uuid`, `owner_pid`.

### 3.4 Métricas de suite NATS integral
- duración por etapa (`... completed in Xs`),
- duración total (`NATS full suite passed in Xs`),
- resultados funcionales de cada bloque.

### 3.5 Métricas JetStream envelope (contract-light, transporte/stats)
- Conteo base:
  - `client_published` (`sent`),
  - `server_received`,
  - `server_acked`,
  - `server_intentional_noack`.
- Transporte/comunicación:
  - `redelivered_detected` (reentrega observada por `recv_idx > 0` o `trace/seq` repetido),
  - `replayed_after_restart` (entrega durable tras restart del consumidor/router),
  - `client_timeout_events`,
  - `nats_reconnects`,
  - `nats_in_flight_peak`,
  - `nats_last_error` (si existe).
- Latencia compacta (cuando aplique):
  - `latency_ms_p50`,
  - `latency_ms_p95`,
  - `latency_ms_max`.
- Timeline compacto por evento:
  - `client published`,
  - `server received`,
  - `server intentionally not acking`,
  - `server acked`,
  - `client timeout` (si ocurre),
  - `nats reconnect` (si ocurre).

### 3.6 Métricas Blob (diag actual)
- Suite `blob_sync_e2e.sh`:
  - `contract_invariant=true|false`,
  - `consumer_retry_elapsed_ms`,
  - `sync_mode`,
  - `source_root`, `target_root`,
  - `syncthing_service_checked`.
- Binario `blob_sync_diag.rs`:
  - `ELAPSED_MS` en consumidor (`resolve_with_retry`),
  - `RESOLVED_BYTES`,
  - `CONTRACT_SIGNATURE` (productor).

### 3.7 Blob SDK (estado actual implementado)
- Cobertura funcional actual en `fluxbee_sdk`:
  - `put`, `put_bytes`, `promote` (`staging -> active`),
  - `resolve`, `exists`, `stat`, `resolve_with_retry`,
  - contrato `text/v1` + `content_ref` automático por límite de tamaño.
- Pruebas existentes:
  - suite `blob::tests` (E2E local + errores),
  - suite `payload::tests` (contrato `text/v1`).
- Resultado esperado actual:
  - `blob::tests`: `10 passed`,
  - `payload::tests`: `3 passed`.

## 4) Reglas de interpretación (importante)
- Estos diagnósticos son herramientas E2E/operativas; no forman parte del runtime normal de `sy-orchestrator` ni `sy-admin`.
- Variables `ORCH_*`, `WF_DIAG_*`, `OPA_*` aplican solo al proceso de diagnóstico invocado.
- Para análisis de performance:
  - usar mínimo 20-30 iteraciones en `wf_nats_diag`,
  - separar perfil `resilience` vs `perf` en `nats_full_suite`.

## 5) Comandos de referencia

WF NATS diag:
```bash
BUILD_BIN=0 WF_DIAG_LOOPS=30 WF_DIAG_TIMEOUT_SECS=3 WF_DIAG_INTERVAL_MS=100 \
SHOW_TIMING_SUMMARY=1 SHOW_FULL_LOGS=0 JSR_LOG_LEVEL=info \
bash scripts/wf_nats_diag.sh
```

Orchestrator diag positivo (resolve):
```bash
TARGET_HIVE="worker-220" ORCH_ROUTE_MODE="resolve" BUILD_BIN=0 \
bash scripts/orchestrator_runtime_update_spawn_e2e.sh
```

Orchestrator diag negativo (espera `UNREACHABLE`):
```bash
TARGET_HIVE="worker-220" ORCH_ROUTE_MODE="resolve" ORCH_SEND_RUNTIME_UPDATE=0 ORCH_SEND_KILL=0 \
ORCH_EXPECT_SPAWN_UNREACHABLE_REASON="OPA_NO_TARGET" BUILD_BIN=0 \
bash scripts/orchestrator_runtime_update_spawn_e2e.sh
```

Orchestrator diag (simular origen no autorizado en hardening de `SY.orchestrator`):
```bash
TARGET_HIVE="worker-220" ORCH_ROUTE_MODE="unicast" ORCH_DIAG_NODE_NAME="WF.unauthorized" \
ORCH_SEND_RUNTIME_UPDATE=0 ORCH_SEND_KILL=0 ORCH_EXPECT_SPAWN_ERROR_CODE="FORBIDDEN" BUILD_BIN=0 \
bash scripts/orchestrator_runtime_update_spawn_e2e.sh
```

JetStream envelope E2E (payload opaco + redelivery):
```bash
BUILD_BIN=0 \
JETSTREAM_DIAG_STACK=fluxbee_sdk \
JETSTREAM_DIAG_LOOPS=20 \
JETSTREAM_DIAG_FAIL_FIRST_N=1 \
JETSTREAM_DIAG_INTERVAL_MS=50 \
bash scripts/jetstream_envelope_e2e.sh
```

Blob SDK local (E2E + errores):
```bash
cargo test -p fluxbee-sdk blob::tests -- --nocapture
cargo test -p fluxbee-sdk payload::tests -- --nocapture
```

Blob sync E2E (`resolve_with_retry` + invariancia de contrato):
```bash
BUILD_BIN=0 \
BLOB_DIAG_REQUIRE_SYNCTHING_ACTIVE=1 \
BLOB_DIAG_SYNC_MODE=copy \
bash scripts/blob_sync_e2e.sh
```

Blob sync E2E en modo externo (espera propagación real por Syncthing/NFS):
```bash
BUILD_BIN=0 \
BLOB_DIAG_REQUIRE_SYNCTHING_ACTIVE=1 \
BLOB_DIAG_SYNC_MODE=external \
BLOB_DIAG_RETRY_MAX_WAIT_MS=30000 \
bash scripts/blob_sync_e2e.sh
```

Nota:
- en la policy actual de `sandbox` (observación 2026-02-22), `Resolve` para `SPAWN_NODE` devuelve `OPA_NO_TARGET`.
- usar `OPA_ERROR` solo para validar fallas técnicas del resolver OPA.

Pre-check standalone de OPA:
```bash
OPA_EXPECT_STATUS=ok OPA_MIN_VERSION=1 OPA_MAX_HEARTBEAT_AGE_MS=30000 \
./target/release/opa_shm_diag
```

## 6) TODO para futura spec de diagnósticos
- [ ] Definir contrato JSON unificado de salida para todos los scripts de diagnóstico.
- [ ] Estandarizar naming de variables (`DIAG_*`) y dejar alias de compatibilidad.
- [ ] Publicar un endpoint en `sy-admin` para exponer resumen agregado de diagnósticos.
- [ ] Definir presupuesto de latencia objetivo por flujo (NATS, system routing, OPA resolve).
- [ ] Agregar versionado de “perfil de diagnóstico” (resilience/perf/wan/opa).
- [ ] Implementar resumen compacto de métricas de transporte en suite JetStream envelope (`sent/acked/redelivered/replayed/timeouts/reconnects/p50-p95-max`).
- [x] Implementar suite Blob E2E (`blob_ref` + verificación end-to-end por contenido) con salida compacta y resumen.
- [ ] Extender suite Blob E2E con hash end-to-end (`sha256`) en salida estándar.
- [ ] Incorporar resumen de Blob Stats en formato agregable (`count/p50/p95/max`, errores y volumen).

## 7) Router Stats (propuesta para spec, sin implementación aún)

Objetivo:
- dejar definidos nombres de telemetría relevantes para router,
- como base para implementación posterior (`/metrics` o export equivalente).

### 7.1 Routing core
- `routing_total{result}`:
  - resultados sugeridos: `forward_local`, `forward_peer`, `forward_wan`, `drop`, `unreachable`, `ttl_exceeded`.
- `routing_drop_reason_total{reason}`:
  - razones sugeridas: `OPA_NO_TARGET`, `OPA_ERROR`, `no_route`, `vpn_block`, `policy_deny`, etc.
- `routing_decision_latency_ms` (histograma):
  - medir costo de decisión de ruta (lookup + validaciones + resolver cuando aplique).

### 7.2 Conectividad y sesiones
- `nodes_connected` (gauge).
- `peer_routers_connected` (gauge).
- `wan_peers_connected` (gauge).
- `node_connect_total`.
- `node_disconnect_total`.
- `socket_read_error_total`.
- `socket_write_error_total`.

### 7.3 OPA / Resolve
- `opa_eval_total`.
- `opa_eval_error_total`.
- `opa_eval_latency_ms` (histograma).
- `opa_reload_total`.
- `opa_reload_error_total`.
- `opa_policy_version` (gauge).
- `opa_data_bundle_load_total`.
- `opa_data_bundle_error_total`.

### 7.4 LSA/WAN control-plane
- `lsa_received_total`.
- `lsa_applied_total`.
- `lsa_rejected_total{reason}`:
  - razones sugeridas: `hive_mismatch`, `stale_seq`, `parse_error`, `unauthorized`.
- `lsa_remote_hives` (gauge).
- `lsa_stale_hives` (gauge).

### 7.5 SHM salud operativa
- `shm_read_fail_total`.
- `shm_seqlock_retry_total`.
- `shm_heartbeat_age_ms{region}` (gauge):
  - regiones sugeridas: `router`, `config`, `lsa`, `opa`.

### 7.6 NATS en router (si path activo)
- `nats_publish_total`.
- `nats_publish_error_total`.

### 7.7 Notas de rollout sugeridas
- Fase 1: counters/gauges críticos (errores y estado).
- Fase 2: histogramas de latencia.
- Fase 3: endpoint/export de métricas + alertas base.
