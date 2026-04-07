# Especificación técnica de cambios — Frontdesk + AI Nodes (mode gov) (v1)

> Nota 2026-03-31:
> Esta especificación refleja la etapa donde el gating era `mode=default|gov`.
> La dirección vigente evolucionó a separación por runtime/ownership:
> - `AI.common` y `SY.frontdesk.gov` como runtimes diferenciados,
> - `SY.frontdesk.gov` apoyado en `nodes/gov/common` + SDKs,
> - `mode=gov` queda como mecanismo transicional.

> Objetivo: definir **qué hay que cambiar** (no el orden) para:
> 1) habilitar `SY.frontdesk.gov` como **AI Node** con capacidad de completar identidad vía Identity,
> 2) introducir **mode** (default/gov) en el runtime de AI Nodes para evitar creep de tools especiales,
> 3) ajustar extracción/uso de `src_ilk` y el keying de LanceDB (thread-state) según la decisión actual.

> Scope: branch actual “estado de hoy” .  
> Fuera de scope: `NODE_STATUS_GET` (ya documentado aparte).

---

## A. Cambios en AI Nodes Runtime (comunes a todos los AI Nodes)

### A1) Introducir `--mode=default|gov` en `ai_node_runner`

**Requerimiento**
- El bin `fluxbee-ai-nodes --bin ai_node_runner` debe aceptar un flag:
  - `--mode=default` (por default)
  - `--mode=gov`

**Semántica**
- `mode` define el **set de tools** que se registran en el runtime:
  - `default`: tools comunes (web, blobs, thread-state, etc.)
  - `gov`: tools comunes + tools “gov” (por ahora: `ilk_register`)

**No permitido**
- `mode` NO puede ser activado por config dinámica/mensajes. Solo por argv/env/launcher (orchestrator/systemd).

**Artefactos esperados**
- Parsing de argv actualizado.
- `mode` visible en logs de arranque.
- `STATUS`/debug puede incluir `mode` en extensions (opcional).

---

### A2) Centralizar registro de tools en un único punto (ToolRegistry)

**Requerimiento**
- El runtime debe tener una función única responsable de registrar tools, e.g.:
  - `build_tool_registry(mode, config) -> ToolRegistry`

**Semántica**
- `mode=default` **no registra** tools gov (aunque el YAML lo “pida”).
- `mode=gov` registra tools gov.

**Motivo**
- Evitar ifs dispersos / gating por nombre.

---

### A3) Extracción canónica de `src_ilk` (carrier actual)

**Contexto**
- En el SDK/envelope del branch, `Meta` no tiene `src_ilk` como campo estructural.
- Por docs del core, el override de router y flows dependen de `meta.context.src_ilk`.

**Requerimiento**
- Implementar un extractor único:
  - `extract_src_ilk(message) -> Option<String>`
- Prioridad:
  1) `meta.context.src_ilk` (carrier actual)
  2) (compat futura) si en el futuro aparece top-level/l3, soportarlo sin romper

**Semántica**
- Si falta `src_ilk`, el nodo sigue operando, pero:
  - se loggea warning (nivel `warn`)
  - y las tools que lo requieran deben devolver error claro (`missing_src_ilk`).

---

### A4) LanceDB / thread-state: cambiar keying a `src_ilk` (según decisión actual)

**Decisión actual** (según conversación)
- El equipo quiere cambiar de `thread_id` a `src_ilk` como key principal del store.

**Requerimiento**
- Actualizar las tools internas `thread_state_get/put/delete` (o equivalente) para que:
  - key principal = `src_ilk` (extraí­do por A3)
  - el JSON guardado sea libre por prompting

**Compat y alias/canonical**
- Por Identity flows: pueden existir alias con TTL y canonicalización.
- El store por `src_ilk` debe contemplar continuidad.
  - mínimo requerido en MVP: intentar lookup en el orden:
    1) `src_ilk` recibido
    2) (si Identity expone canonical) `canonical_ilk` (🧩)
  - Alternativa MVP aceptable: almacenar en el JSON un campo `known_ilks: []` y migrar (rename) cuando cambie (si se detecta).

**Errores**
- Si falta `src_ilk`, `thread_state_*` debe fallar con error explí­cito (no silent).

---

## B. Tool gov: `ilk_register` (frontdesk → identity)

### B1) Diseñar tool: `ilk_register` (expuesta solo en mode=gov)

**Requerimiento**
- Agregar tool al runtime AI Nodes disponible solo si `--mode=gov`.

**Nombre sugerido**
- `ilk_register` (o `identity_ilk_register` si se quiere namespacing)

**Inputs mínimos**
- `src_ilk: string` (target a completar)
- `identity_candidate`:
  - `name`
  - `email`
  - `phone` (opcional)
  - `tenant_hint` (dominio/código/solicitud)
- `thread_id` (opcional para audit; carrier futuro)

**Output**
- `{ status: "ok", registered: true, canonical_ilk?: string }` o
- `{ status: "error", error_code, message, retryable }`

**Semántica**
- Ejecuta `ILK_REGISTER` hacia Identity (ver B2).
- No persiste secretos.
- Log audit (sin PII en claro, salvo debug controlado).

---

### B2) Implementar llamada a Identity usando helpers del SDK

**Contexto**
- En el branch existe `crates/fluxbee_sdk/src/identity.rs` con helpers `identity_system_call_ok(...)`.

**Requerimiento**
- `ilk_register` debe usar `identity_system_call_ok(...)` o equivalente para invocar:
  - `action = "ILK_REGISTER"`
  - `payload` según spec de Identity (documento “Identity — flujos de ILK”).

**Targets**
- `receiver` configurable por env/config:
  - `GOV_IDENTITY_TARGET` (default `"SY.identity"`)
- `fallback_target` configurable para primary (motherbee), para manejar `NOT_PRIMARY`.

**Errores**
- Manejar `NOT_PRIMARY` usando fallback (si el helper ya lo hace, basta con setear fallback_target).
- Exponer `INVALID_*` como no-retryable.
- Exponer `TIMEOUT/UNAVAILABLE` como retryable.

---

### B3) Seguridad: tool gov no debe ser invocable en mode=default

**Requerimiento**
- A nivel registry, `ilk_register` no existe en mode=default.
- Si un prompt intenta llamarla en mode=default:
  - el runtime debe responder “tool not found” (o error explí­cito) y no ejecutar nada.

---

## C. Implementación del nodo `SY.frontdesk.gov` (como config/prompt sobre AI Nodes)

### C1) Config/prompt: “frontdesk flow” usando thread-state + confirmación

**Requerimiento**
- Mantener frontdesk como AI node estándar (`ai_node_runner`) con config propia.
- El prompt debe:
  1) mantener estado local usando `thread_state_get/put/delete` (ahora keyeado por `src_ilk`)
  2) pedir datos mínimos: `name`, `email`, tenant association (según docs core)
  3) pedir confirmación explí­cita
  4) llamar `ilk_register` (tool gov) al confirmar
  5) borrar estado (`thread_state_delete`) al éxito

**Nota**
- El nodo frontdesk corre con `--mode=gov`.

---

### C2) Wiring de deploy/spawn

**Requerimiento**
- Orchestrator/systemd debe spawnear `SY.frontdesk.gov` con:
  - `ai_node_runner --mode=gov --config <config_path>`
- El resto de AI Nodes debe spawnearse con `--mode=default`.

🧩 **A ESPECIFICAR (core)**:
- Dónde vive la regla de “este nodo es gov” (hardcode, hive.yaml, policy).

---

## D. Documentación a actualizar (después de implementar)

> Nota: esta sección es para que el equipo sepa qué docs tocar cuando el código quede, no bloquea el desarrollo.

1) `AI_nodes_spec.md`:
- agregar `mode` y semántica de tool registry
- documentar tool `ilk_register` (solo mode=gov)
- actualizar thread-state keying por `src_ilk`

2) `ai-frontdesk-gov-spec.md`:
- reemplazar “upgrade TBD” por `ILK_REGISTER` (tool `ilk_register`)
- aclarar que corre como AI node con `--mode=gov`

3) `ai-nodes-examples-annex.md`:
- examples de `ilk_register` tool call (gov)
- examples de `thread_state_*` keyeado por `src_ilk`

---

## E. Criterios de validación (alto nivel)

- `SY.frontdesk.gov` puede completar identidad:
  - recibe conversación con `meta.context.src_ilk` temporal
  - recolecta mínimos, confirma
  - ejecuta `ILK_REGISTER`
  - limpia state y el flujo continúa

- En `--mode=default`, tool `ilk_register` no existe y no puede ser invocada.

- Thread-state funciona keyeado por `src_ilk` (y falla explí­cito si falta).


---

## F. Lista de tareas sugerida (implementacion)

> Orden recomendado para minimizar riesgo y mantener el nodo util en cada paso.

### F0) Baseline tecnico y guardrails

- [ ] Verificar baseline local:
  - `cargo check -p fluxbee-ai-sdk`
  - `cargo check -p fluxbee-ai-nodes`
- [ ] Dejar congelado el alcance: cambios solo en AI SDK/AI runner/docs (sin tocar core salvo reporte de inconsistencias).

### F1) Modo de runtime (`default|gov`)

- [x] Agregar parsing de `--mode=default|gov` en `ai_node_runner` (`default` por defecto).
- [x] Loggear `mode` al arranque de cada instancia.
- [x] Rechazar valores invalidos con error explicito.

### F2) Registro de tools centralizado por mode

- [x] Crear punto unico de armado de tools:
  - `build_tool_registry(mode, behavior_ctx, runtime_cfg)`.
- [x] `mode=default`: registrar solo tools comunes.
- [x] `mode=gov`: registrar comunes + gov.
- [x] Validar que el prompt no pueda habilitar tools fuera del mode.

### F3) Extraccion canonica de `src_ilk`

- [x] Implementar `extract_src_ilk(message) -> Option<String>` en un unico lugar del runtime.
- [x] Priorizar `meta.context.src_ilk` (carrier actual).
- [x] Si falta `src_ilk`, emitir `warn` y devolver error claro en tools dependientes (`missing_src_ilk`).

### F4) Thread-state keying por `src_ilk`

- [x] Migrar tools `thread_state_get/put/delete` para usar key principal `src_ilk`.
- [x] Quitar dependencia funcional de `thread_id` para thread-state.
- [x] Definir compat MVP:
  - fallback de lectura por key previa (si aplica), o
  - migracion explicita en primer acceso.
- [x] Agregar tests de:
  - persistencia por `src_ilk`,
  - error cuando falta `src_ilk`.

### F5) Tool gov `ilk_register`

- [x] Implementar tool `ilk_register` (solo mode=gov).
- [x] Integrar llamada a Identity via `identity_system_call_ok(...)`.
- [x] Soportar targets configurables:
  - `GOV_IDENTITY_TARGET` (default `SY.identity`)
  - `GOV_IDENTITY_FALLBACK_TARGET` (primary/motherbee).
- [x] Mapear errores:
  - `NOT_PRIMARY` -> fallback
  - `INVALID_*` -> no retry
  - `TIMEOUT/UNAVAILABLE` -> retryable.
- [x] Asegurar redaccion de PII en logs (excepto debug controlado).

### F6) Aislamiento de seguridad por mode

- [x] Confirmar que `ilk_register` no exista en `mode=default`.
- [x] Test negativo: invocacion de tool gov en `mode=default` -> `tool not found` (sin side effects).

### F7) Wiring frontdesk runtime

- [x] Ajustar config/prompt de `SY.frontdesk.gov` al flujo:
  - recolectar minimos,
  - confirmar,
  - llamar `ilk_register`,
  - limpiar estado.
- [x] Dejar ejemplo operativo para spawn con:
  - `--mode=gov` para frontdesk,
  - `--mode=default` para el resto.

### F8) Documentacion y cierre

- [x] Actualizar `AI_nodes_spec.md` (mode + tool gov + keying por `src_ilk`).
- [x] Actualizar `ai-frontdesk-gov-spec.md` (reemplazar TBD por `ilk_register`).
- [x] Actualizar `ai-nodes-examples-annex.md` (ejemplos gov/default).
- [x] Registrar comandos de validacion E2E minimos.

### F9) Validacion E2E minima (salida)

- [ ] Caso A: frontdesk en `mode=gov` completa registro de ILK temporal a completo.
- [ ] Caso B: nodo AI comun en `mode=default` no puede invocar `ilk_register`.
- [ ] Caso C: thread-state persiste y limpia por `src_ilk`.

## G. Comandos de validacion E2E minima (manual)

### G1) Frontdesk en mode=gov (manual run)

```bash
export OPENAI_API_KEY="sk-REPLACE_ME"
export GOV_IDENTITY_TARGET="SY.identity"
export GOV_IDENTITY_FALLBACK_TARGET="SY.identity@motherbee"
export GOV_IDENTITY_TIMEOUT_MS="10000"
RUST_LOG=info cargo run --release -p fluxbee-ai-nodes --bin ai_node_runner -- \
  --mode gov \
  --config /tmp/ai_frontdesk_gov.utf8.yaml
```

### G2) Frontdesk en mode=gov (instalado/systemd)

```bash
bash scripts/install-ia.sh
sudo install -m 0644 /tmp/ai_frontdesk_gov.utf8.yaml /etc/fluxbee/ai-nodes/ai-frontdesk-gov.yaml
sudo tee /etc/fluxbee/ai-nodes/sy-frontdesk-gov.env >/dev/null <<'EOF'
AI_NODE_MODE=gov
OPENAI_API_KEY=sk-REPLACE_ME
GOV_IDENTITY_TARGET=SY.identity
GOV_IDENTITY_FALLBACK_TARGET=SY.identity@motherbee
GOV_IDENTITY_TIMEOUT_MS=10000
RUST_LOG=info,fluxbee_ai_nodes=debug,fluxbee_ai_sdk=debug
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now fluxbee-ai-node@sy-frontdesk-gov
sudo journalctl -u fluxbee-ai-node@sy-frontdesk-gov -f
```

### G3) Nodo AI comun en mode=default (instalado/systemd)

```bash
sudo tee /etc/fluxbee/ai-nodes/ai-chat.env >/dev/null <<'EOF'
AI_NODE_MODE=default
OPENAI_API_KEY=sk-REPLACE_ME
RUST_LOG=info
EOF
sudo systemctl restart fluxbee-ai-node@ai-chat
```

### G4) Sim para enviar mensajes a frontdesk

```bash
SIM_THREAD_ID="sim-thread-1" \
SIM_SRC_ILK="ilk:11111111-1111-4111-8111-111111111111" \
SIM_DST_NODE="SY.frontdesk.gov@motherbee" \
ROUTER_SOCKET="/var/run/fluxbee/routers" \
CONFIG_DIR="/etc/fluxbee" \
UUID_PERSISTENCE_DIR="/var/lib/fluxbee/state/nodes" \
IDENTITY_TARGET="SY.identity" \
IDENTITY_FALLBACK_TARGET="SY.identity@motherbee" \
IDENTITY_TIMEOUT_MS="10000" \
RUST_LOG=info,io_sim=debug,fluxbee_sdk=info \
cargo run --release -p io-sim
```

### G5) Criterios de verificacion rapida

- Caso A (`mode=gov`): en logs de frontdesk debe aparecer ejecucion de `ilk_register` y respuesta final de cierre.
- Caso B (`mode=default`): una invocacion a `ilk_register` debe devolver `unknown_tool` (sin side effects).
- Caso C (thread-state): logs de `thread_state_get/put/delete` deben operar con key de contexto (`src_ilk`) y limpiar estado al cierre.
