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
ORCH_EXPECT_SPAWN_UNREACHABLE_REASON="OPA_ERROR" BUILD_BIN=0 \
bash scripts/orchestrator_runtime_update_spawn_e2e.sh
```

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
