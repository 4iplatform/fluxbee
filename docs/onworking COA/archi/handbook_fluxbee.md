# Fluxbee Pipeline Handbook

**Audiencia:** Agentes internos del pipeline — Designer, RealProgrammer, PlanCompiler  
**Estado:** v2 — rearchitecture spec v4  
**Fecha:** 2026-04-24

---

## Modelo mental de Fluxbee

Fluxbee es un sistema de nodos distribuidos que se comunican por mensajes. Cada **hive** es un nodo host (una máquina o instancia) que corre uno o más **nodos de aplicación**.

```text
humano / cliente
      │
      ▼
  [nodo IO]  ←→  [nodo AI]  ←→  [nodo WF]
                                      │
                                  [nodo SY]
```

### Runtimes y nodos — la relación clave

| Concepto | Qué es | Analogía |
| --- | --- | --- |
| **Runtime** | Plantilla publicada: binario + config base | Imagen Docker |
| **Nodo** | Instancia en ejecución de un runtime | Container corriendo |

Un runtime se **publica** una vez. Un nodo se **inicia** a partir de un runtime publicado.

### Naming convention — obligatorio

Patrón: `TIPO.nombre@hive`

| Prefijo | Tipo | Comportamiento config |
| --- | --- | --- |
| `AI.*` | Nodos de lenguaje | hot apply (sin restart) |
| `WF.*` | Nodos de workflow | restart requerido |
| `IO.*` | Integración I/O | hot apply (sin restart) |
| `SY.*` | Sistema / infra | restart requerido |

**Reglas:** Nunca omitir el prefijo ni el `@hive`. El `@hive` debe coincidir con el hive donde vive el nodo.

### Runtimes base disponibles

> Siempre verificar con `query_hive(list_runtimes)` — esta tabla puede estar desactualizada.

| Runtime | Tipo | Uso |
| --- | --- | --- |
| `AI.common` | `full_runtime` | Base genérica para nodos AI nuevos |
| `AI.chat` | `full_runtime` | Nodo AI con chat — no usar como base para nuevos nodos |
| `WF.engine` / `wf.engine` | `full_runtime` | Motor base para nodos workflow |
| `IO.api` | `full_runtime` | Integración HTTP API |
| `IO.slack` | `full_runtime` | Integración Slack |

---

## Sección 1: Para el Designer

**Rol:** Producir un `solution_manifest` v2 que declare el estado deseado del sistema.

### Estructura del manifest

```json
{
  "manifest_version": "2.0",
  "solution": { "name": "...", "description": "..." },
  "desired_state": { ... },
  "advisory": { ... }
}
```

### Secciones válidas de `desired_state`

| Sección | Cuándo usar |
| --- | --- |
| `topology` | Nuevos hives o VPNs entre hives |
| `runtimes` | Nuevos packages a publicar (inline_package, bundle_upload) |
| `nodes` | Nodos a crear, modificar, o eliminar |
| `routing` | Rutas de mensajes entre hives o prefijos |
| `wf_deployments` | Definiciones de workflows a desplegar |
| `opa_deployments` | Políticas OPA a aplicar |

**Secciones NO soportadas en esta versión:** `policy`, `identity`

### Reglas de propiedad (ownership)

- `"ownership": "solution"` → el pipeline puede crear, modificar y eliminar este recurso
- `"ownership": "external"` → el reconciler nunca emite DELETE para este recurso
- Cuando se omite → default `"external"` (conservador, nunca borra)

### Patrones comunes de design

**Nodo AI nuevo en hive existente:**
```json
{
  "desired_state": {
    "runtimes": [{ "name": "AI.example", "version": "0.1.0", "package_source": "inline_package", "ownership": "solution" }],
    "nodes": [{ "node_name": "AI.example@motherbee", "hive": "motherbee", "runtime": "AI.example", "config": {}, "ownership": "solution" }]
  }
}
```

**Nodo en worker-hive con routing:**
```json
{
  "desired_state": {
    "topology": {
      "hives": [{ "hive_id": "worker-220", "role": "worker", "ownership": "solution" }],
      "vpns": [{ "vpn_id": "vpn-main-w220", "hive_id": "worker-220", "ownership": "solution" }]
    },
    "nodes": [{ "node_name": "AI.specialist@worker-220", "hive": "worker-220", "runtime": "AI.common", "config": {}, "ownership": "solution" }],
    "routing": [{ "hive": "motherbee", "prefix": "AI.specialist.", "action": "FORWARD", "next_hop_hive": "worker-220", "ownership": "solution" }]
  }
}
```

### Lo que NO hace el Designer

- No genera executor plans ni admin steps
- No valida si los runtimes existen en el hive — eso lo hace el reconciler
- No toma decisiones de negocio sobre si la solución es correcta

---

## Sección 2: Para el RealProgrammer

**Rol:** Producir un `artifact_bundle` que materialice exactamente lo que describe un `build_task_packet`. Un bundle por tarea. Exactamente una llamada a `submit_artifact_bundle`.

### Artifact kinds y sus requisitos

#### `runtime_package` (inline_package)

Archivos requeridos en `artifact.files`:
- `package.json` — JSON con campos: `name`, `version`, `type`, `runtime_base`
- `config/default-config.json` — JSON con configuración base del nodo

Archivos opcionales:
- `prompts/system.txt` — System prompt para nodos AI
- cualquier otro archivo de configuración

```json
{
  "artifact_kind": "runtime_package",
  "artifact": {
    "files": {
      "package.json": "{\"name\":\"ai.support\",\"version\":\"0.1.0\",\"type\":\"config_only\",\"runtime_base\":\"AI.common\"}",
      "config/default-config.json": "{\"tenant_id\":\"tnt:example\"}",
      "prompts/system.txt": "Eres un asistente especializado en..."
    }
  }
}
```

**Reglas de `package.json`:**
- `name`: lowercase, sin espacios, puede tener puntos y guiones
- `runtime_base`: debe ser un runtime que existe en `known_context.available_runtimes`
- `type`: `"config_only"` para packages sin binario propio

#### `workflow_definition`

```json
{
  "artifact_kind": "workflow_definition",
  "artifact": {
    "workflow_name": "invoice_approval",
    "version": "0.1.0",
    "steps": [
      { "id": "s1", "type": "ai_call", "node": "AI.validator@motherbee" }
    ]
  }
}
```

- `workflow_name` es requerido
- `steps` es requerido y no puede estar vacío
- Cada step necesita un `id` único

#### `opa_bundle`

```json
{
  "artifact_kind": "opa_bundle",
  "artifact": {
    "rego_source": "package fluxbee.access\n\ndefault allow = false\n"
  }
}
```

- `rego_source` es requerido
- Debe contener una declaración `package` al inicio

#### `config_bundle`

Genera los archivos listados en `packet.requirements.required_files`. El auditor verifica que todos estén presentes.

### Con repair_packet

Si recibís un `repair_packet`, significa que el intento anterior falló la auditoría. Debes:
1. Aplicar cada item de `required_corrections` sin excepción
2. No repetir nada listado en `do_not_repeat`
3. Preservar lo que está en `must_preserve`

### Lo que NO hace el RealProgrammer

- No toma decisiones de diseño — eso ya está en el manifest
- No genera admin steps ni executor plans
- No evalúa si el artifact es semánticamente correcto para el negocio

---

## Sección 3: Para el PlanCompiler

**Rol:** Traducir un `delta_report` a un `executor_plan` ejecutable por `SY.admin`. Cada `DeltaOperation` del delta report tiene un `compiler_class` que indica exactamente qué admin steps emitir.

### El plan es estático

Los args de cada step son fijos — el output de un step no puede alimentar el siguiente.

**Correcto:** Usar `query_hive` durante la generación del plan para obtener los valores. Luego escribirlos directamente en los args.

**Incorrecto:** Intentar derivar args de otros steps en tiempo de ejecución.

### Tabla de compiler_class → admin steps

| compiler_class | Steps requeridos (en orden) |
| --- | --- |
| `NODE_RUN_MISSING` | `run_node` |
| `NODE_CONFIG_APPLY_HOT` | `node_control_config_set` |
| `NODE_CONFIG_APPLY_RESTART` | `node_control_config_set` → `restart_node` |
| `NODE_RESTART_ONLY` | `restart_node` |
| `NODE_KILL` | `kill_node` |
| `NODE_RECREATE` | `kill_node` → `run_node` |
| `RUNTIME_PUBLISH_ONLY` | `publish_runtime_package` |
| `RUNTIME_PUBLISH_AND_DISTRIBUTE` | `publish_runtime_package` (con sync_to + update_to) |
| `RUNTIME_DELETE_VERSION` | `remove_runtime_version` |
| `HIVE_CREATE` | `add_hive` |
| `VPN_CREATE` | `add_vpn` |
| `VPN_DELETE` | `delete_vpn` |
| `ROUTE_ADD` | `add_route` |
| `ROUTE_DELETE` | `delete_route` |
| `ROUTE_REPLACE` | `delete_route` → `add_route` |
| `WF_DEPLOY_APPLY` | `wf_rules_compile_apply` |
| `WF_DEPLOY_RESTART` | `wf_rules_compile_apply` → `restart_node` |
| `OPA_APPLY` | `opa_compile_apply` |

**Clases bloqueadas (no emitir steps):** `NOOP`, `WF_REMOVE`, `OPA_REMOVE`, `BLOCKED_*`

### Args requeridos para `run_node`

```json
{
  "hive": "motherbee",
  "node_name": "AI.coa@motherbee",
  "runtime": "AI.common",
  "runtime_version": "current"
}
```

- `runtime` es el nombre del runtime sin `@hive`
- `runtime_version`: usar `"current"` salvo versión específica requerida

### Clasificación de riesgo (para CONFIRM 2)

| Riesgo | Clases | Implicación |
| --- | --- | --- |
| `non_destructive` | HIVE_CREATE, VPN_CREATE, RUNTIME_PUBLISH_*, NODE_RUN_MISSING, ROUTE_ADD, WF_DEPLOY_APPLY, OPA_APPLY | Seguro |
| `restarting` | NODE_CONFIG_APPLY_RESTART, NODE_RESTART_ONLY, WF_DEPLOY_RESTART | Interrumpe servicio brevemente |
| `destructive` | VPN_DELETE, RUNTIME_DELETE_VERSION, NODE_KILL, NODE_RECREATE, ROUTE_DELETE, ROUTE_REPLACE, WF_REMOVE, OPA_REMOVE | Irreversible |

### Tools disponibles durante la generación

| Tool | Cuándo usar |
| --- | --- |
| `query_hive` | Consultar estado real: runtimes disponibles, nodos corriendo, rutas existentes |
| `get_admin_action_help` | Schema exacto de una acción (args requeridos, tipos, ejemplos) |
| `submit_executor_plan` | Entregar el plan final — exactamente una vez |

### Lo que NO hace el PlanCompiler

- No toma decisiones sobre qué cambios hacer — eso está en el delta_report
- No verifica si los cambios son seguros para el negocio — eso es del operador (CONFIRM 2)
- No inventa admin steps adicionales más allá de los definidos para cada compiler_class

---

## Gaps conocidos / pendientes

- [ ] Campos opcionales de `run_node` (e.g. `tenant_id` para nodos multi-tenant)
- [ ] Cómo funciona `executor_fill` en los steps
- [ ] Topologías multi-hive: cuándo un nodo vive en `worker-*` vs `motherbee`
- [ ] Convenciones de naming para nodos IO con tenant (ej. `IO.slack.T126@motherbee`)
- [ ] Nodos `SY.*` — cuándo se crean, si alguna vez se crean desde pipeline
