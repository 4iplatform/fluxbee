# JSON Router - Estado de Implementación (Router)

Checklist de cobertura **según spec v1.13**.  
Objetivo: revisar punto por punto y marcar pendientes.

---

## 1. Routing local (intra‑isla)

- [x] FIB (LPM + prioridad/metric)
- [x] Rutas estáticas (`ACTION_DROP` / `ACTION_FORWARD`)
- [x] Resolución por UUID (L1)
- [x] Resolución directa por nombre L2 en `routing.dst` (sin OPA)
- [x] Resolución por nombre (L2)
- [x] Rebuild FIB en cambios de config/LSA

## 2. VPN

- [x] Asignación por reglas (SHM config)
- [x] Filtro de routing por VPN
- [x] Excepción `SY.*`
- [x] Broadcast transversal (system)

## 3. Broadcast / Multicast

- [x] Broadcast local
- [x] Broadcast inter‑router (IRP)
- [x] Broadcast inter‑isla (WAN)
- [x] TTL en broadcast
- [x] Cache anti‑loops

## 4. IRP (Intra‑router peering)

- [x] IRP sockets por UUID
- [x] Forward al router dueño del nodo destino
- [x] Transparente para nodos

## 5. WAN (Inter‑isla)

- [x] WAN hello/accept/reject básico
- [x] Conexión gateway ↔ gateway
- [x] Forward inter‑isla vía gateway
- [x] LSA local + remoto (propagación)

## 6. OPA (Resolver)

### 6.1 Lifecycle OPA (router)
- [x] `OPA_RELOAD` (system broadcast)
- [x] Carga WASM desde SHM `/dev/shm/jsr-opa-<hive>`
- [x] SHM: `opa_policy_version` + `opa_load_status`

### 6.2 Resolver real
- [x] Evaluación OPA en‑process (Wasmtime)
- [x] `OPA_NO_TARGET` si OPA devuelve null
- [x] `OPA_ERROR` si falla evaluación

### 6.3 Pendientes OPA
- [x] Builtins OPA completos
- [x] Resolver entrypoint (usa `entrypoints` + `opa_eval_ctx_set_entrypoint` si existe)
- [x] Carga de `data` bundle (si policy usa data)
  - Router carga opcionalmente `data.json` desde `/var/lib/fluxbee/opa/current/data.json`.
  - Si el archivo no existe, usa `data` embebido en WASM (comportamiento anterior).
  - Si `data.json` existe pero es inválido, el reload falla en forma explícita (`OPA_ERROR`).
- [x] Validación de `hash` en `OPA_RELOAD`

## 7. Casos especiales

- [x] `@*` wildcard de isla
- [x] `UNREACHABLE` + `TTL_EXCEEDED`

## 8. Estado / Observabilidad

- [x] Logs básicos de routing y config
- [ ] Métricas/telemetría (no especificado, pendiente)

---

## Notas

- SY.opa.rules es **control plane** (Go), no router. Compila Rego → WASM y escribe en SHM.
- Routers (Rust) leen WASM de `/dev/shm/jsr-opa-<hive>` y ejecutan con Wasmtime.
- Router usa `data bundle` externo opcional (`/var/lib/fluxbee/opa/current/data.json`) para poblar `opa_eval_ctx_set_data`.
- Si la policy usa builtins no implementados → `OPA_ERROR`.
