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

### Topologías multi-hive: cuándo usar worker-hive

| Situación | Usar |
| --- | --- |
| Nodo de uso general, sirve a todos los tenants | `motherbee` |
| Nodo con alta carga de cómputo o hardware especial | `worker-*` dedicado |
| Nodo con aislamiento de tenant requerido | `worker-*` por tenant |

Cuando el nodo destino es un `worker-*` que no existe aún, incluir en `desired_state.topology` la definición del hive y su VPN. Si el worker ya existe, omitir `topology`.

### Naming convention para nodos IO con tenant

Nodos IO de tenant único: `IO.slack@motherbee` (sin sufijo de tenant).

Nodos IO multi-tenant (un nodo por tenant): `IO.slack.T126@motherbee` donde `T126` es el identificador corto del tenant. Usar el mismo identificador corto en todos los nodos del mismo tenant.

No combinar nodos AI y IO del mismo tenant bajo identificadores diferentes.

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

### Args completos de `run_node`

| Campo | Tipo | Req | Descripción |
| --- | --- | --- | --- |
| `node_name` | string | **sí** | Nombre completo: `AI.coa@motherbee` |
| `runtime` | string | no | Nombre del runtime sin `@hive`. Omitible si derivable del nombre del nodo. |
| `runtime_version` | string | no | Versión del runtime. Default: `"current"`. |
| `tenant_id` | string | condicional | ID de tenant para primer spawn con identidad. Ver regla abajo. |
| `unit` | string | no | Override del sufijo de unit systemd. Usar raramente. |
| `config` | object | no | Config de runtime pasada durante el spawn. |

**Regla de `tenant_id`:** Requerido cuando el runtime necesita identidad de tenant en la creación inicial de la instancia. Esto es el caso para runtimes AI multi-tenant. El valor tiene formato `"tnt:<uuid>"` (ej. `"tnt:43d576a3-d712-4d91-9245-5d5463dd693e"`). Llamar `get_admin_action_help run_node` en tiempo de compilación para confirmar si el runtime específico lo requiere.

Si el manifest incluye `tenant_id` en la config del nodo, extraerlo y pasarlo como arg de nivel raíz en `run_node`, no dentro de `config`.

```json
{
  "hive": "motherbee",
  "node_name": "AI.coa@motherbee",
  "runtime": "AI.common",
  "runtime_version": "current",
  "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
}
```

### `executor_fill` — cuándo y cómo usarlo

`executor_fill.allowed` es una lista de campos de `step.args` que el ejecutor AI puede rellenar con valores derivados del estado de infraestructura en tiempo de ejecución.

Usar `executor_fill` solo cuando un arg no puede conocerse en tiempo de compilación del plan y solo puede derivarse consultando el estado live del sistema.

```json
{
  "id": "s1",
  "action": "run_node",
  "args": { "hive": "motherbee", "node_name": "AI.coa@motherbee", "runtime": "AI.common" },
  "executor_fill": { "allowed": ["runtime_version"], "notes": ["use latest stable version from list_runtimes"] }
}
```

**Regla general:** La mayoría de los args deben estar hardcodeados en tiempo de compilación. Usar `executor_fill` solo si el valor es genuinamente imposible de obtener en ese momento.

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

---

## Sección 4: Para Archi (coordinador del pipeline)

**Rol:** Recibir tareas del operador, arrancar el pipeline, confirmar los checkpoints, y reportar el resultado. Archi NUNCA muta el sistema directamente.

### Regla fundamental: toda mutación va por `fluxbee_plan_compiler`

Archi no tiene tools de escritura. Cualquier cambio al sistema — crear nodo, publicar runtime, modificar config, desplegar workflow — debe ir a través del pipeline iniciado con `fluxbee_plan_compiler`.

**Si el operador pide crear un nodo:** Archi arranca el pipeline con una descripción del objetivo. El pipeline genera el manifest, compila el plan, y ejecuta los steps.

**Archi nunca llama:** `run_node`, `publish_runtime_package`, `node_control_config_set`, `wf_rules_compile_apply`, ni ninguna otra acción de mutación.

### Árbol de decisión para operaciones de ciclo de vida

```text
Nodo no existe en el sistema → pipeline con compiler_class NODE_RUN_MISSING
Nodo existe, detenido       → pipeline con compiler_class NODE_RESTART_ONLY
Nodo existe, config cambió  → pipeline con compiler_class NODE_CONFIG_APPLY_HOT o NODE_CONFIG_APPLY_RESTART
Nodo existe, recrear limpio → pipeline con compiler_class NODE_RECREATE (kill + run_node)
```

Nunca asumir cuál es el caso sin consultar el estado actual con `fluxbee_system_get` primero.

### El hive es input crítico para todo comando de nodo

El sufijo `@hive` en el nombre del nodo no es decoración: identifica la máquina física donde corre el nodo. Cualquier acción de admin que toca un nodo o la topología necesita el `hive` como input — en el path (`POST /hives/{hive}/nodes/...`) o como arg del step. Sin el hive correcto, el comando va a la máquina equivocada o falla.

Acciones que requieren `hive` explícito:

- Lifecycle: `run_node`, `start_node`, `restart_node`, `kill_node`, `remove_node_instance`
- Config: `node_control_config_set`, `node_control_config_get`, `set_node_config`, `get_node_config`
- Routing: `add_route`, `delete_route`, `list_routes`
- VPN: `add_vpn`, `delete_vpn`, `list_vpns`
- Workflows: `wf_rules_compile_apply`, `wf_rules_apply`, `wf_rules_get_status`, etc.

Antes de invocar `fluxbee_plan_compiler` para una mutación de nodo, resolvé el hive en este orden y parate apenas tengas la respuesta:

1. **¿El operador nombró un hive explícitamente?** ("en motherbee", "en worker-220") → usá ese.
2. **¿El operador nombró un nodo existente con `@hive` en su nombre?** (ej. "modificá `AI.support@motherbee`") → el hive es el sufijo después del `@`.
3. **¿Existe un solo hive en el cluster?** → usá ese, no preguntes. Verificarlo es barato: `fluxbee_system_get` con path `/inventory/summary` o `/hives`.
4. **¿Hay varios hives y la intención es genuinamente ambigua?** → pregunta única al operador: "¿En qué hive lo querés crear, motherbee o worker-220?".

Anti-patrones a evitar:

- ❌ Llamar `fluxbee_plan_compiler` sin saber el hive y "esperar que el plan_compiler decida". El plan_compiler no inventa hives.
- ❌ Asumir `motherbee` por default. Funciona en clusters de un solo hive pero produce despliegues equivocados en clusters multi-hive.
- ❌ Preguntar al operador el hive cuando solo hay uno en el sistema.
- ❌ Preguntar el hive cuando el operador ya lo dejó implícito en el nombre del nodo (`@motherbee`).

### `tenant_id` en el contexto de Archi

Cuando el operador pide crear un nodo para un tenant específico:

1. Identificar el `tenant_id` del contexto de la conversación o preguntarlo al operador (UNA sola vez).
2. Incluir el `tenant_id` en la descripción del pipeline task para que el Designer lo incorpore en el manifest.
3. El PlanCompiler lo extraerá del manifest y lo pasará como arg de `run_node`.

El `tenant_id` tiene formato `"tnt:<uuid>"`. Si el operador provee un ID corto como `"acme"`, formatear como `"tnt:acme"`.

### Nodos `SY.*` — no crear desde pipeline

Los nodos `SY.*` (SY.admin, SY.storage, SY.identity, SY.cognition, SY.config.routes) son infraestructura del sistema. No deben crearse ni modificarse desde el pipeline. Son gestionados por el operador o por bootstrap del hive.

Si el operador pide modificar un `SY.*`, escalar y pedir confirmación explícita antes de proceder.

### Cuándo NO arrancar el pipeline

| Situación | Acción correcta |
| --- | --- |
| El operador solo quiere consultar el estado del sistema | Usar `fluxbee_system_get` directamente |
| El operador quiere ver logs o métricas | Usar `fluxbee_system_get` directamente |
| El operador quiere listar nodos / runtimes | Usar `fluxbee_system_get` directamente |
| El operador quiere un cambio de configuración | Arrancar pipeline |
| El operador quiere crear, modificar o eliminar un recurso | Arrancar pipeline |

### Regla anti-pregunta

Hacer como máximo UNA pregunta de aclaración por tarea. Si la tarea es razonablemente clara (topología inferible, runtime obvio), arrancar el pipeline sin preguntar. Si faltan datos críticos imposibles de inferir (ej. qué tenant), preguntar una vez con una sola pregunta concreta.
