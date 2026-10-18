# Fluxbee / Archi — Cambio por capas empezando por prompts

**Objetivo inmediato:** ordenar la información que reciben los agentes, empezando por prompts, sin mover todavía la arquitectura ni reescribir el pipeline completo.

**Alcance de esta iteración:** prompts e inyección de contexto. No tocar el executor de `SY.admin` salvo que aparezca una falla concreta. No agregar herramientas nuevas a agentes si el problema se puede resolver moviendo información a la capa correcta.

---

## 1. Diagnóstico resumido

El problema actual no parece ser que la AI no pueda programar Fluxbee. El problema es que parte de la información necesaria para programar está mezclada entre:

- prompt de workflow;
- handbook de plataforma;
- admin action help;
- cookbooks;
- task packets;
- schemas implícitos en tools.

Eso produce una zona ambigua: el agente a veces tiene una acción disponible, pero no tiene el modelo compacto para elegirla bien; o tiene un ejemplo, pero no sabe si es contrato, patrón o caso particular.

Regla de separación propuesta:

```text
Prompt        = cómo trabaja el agente.
Handbook      = qué es Fluxbee y cómo está modelada la plataforma.
Action help   = qué acción admin existe, qué parámetros acepta, precondiciones, riesgos y respuesta.
Cookbook      = qué patrón ya funcionó antes.
Task packet   = qué tarea concreta debe resolver ahora.
Schema/tool    = forma exacta de input/output.
```

---

## 2. Inventario actual de información por agente

Estado reportado por dev:

| Agente | Handbook | Cookbook | Admin actions catalog | query_hive | get_admin_action_help | get_manifest_current |
|---|---:|---:|---:|---:|---:|---:|
| Archi chat | Sí | No | No | vía `fluxbee_system_get` | vía `fluxbee_system_get` | No |
| Designer | Sí | `design_cookbook` | No | Sí | No | Sí |
| DesignAuditor | Sí | No | No | No | No | Sí |
| RealProgrammer | Sí | `artifact_cookbook` | No | No | No | No |
| PlanCompiler | No | `plan_compile_cookbook` | Sí, compacto | Sí | Sí | No |

Lectura crítica:

- `Archi` está razonablemente ubicado como host/coordinador.
- `Designer` está razonablemente ubicado como diseñador de desired state.
- `DesignAuditor` está bien como auditor sin acceso live.
- `RealProgrammer` está bien sin `query_hive` si el `build_task_packet` viene completo.
- `PlanCompiler` es el punto más débil: tiene acciones y herramientas, pero no tiene el modelo compacto de plataforma que necesita para no confundir runtime, nodo, instancia, proceso, L2, config live, config persistida, etc.

---

## 3. Capas de la cebolla

Usar esta nomenclatura para evitar discusiones ambiguas:

| Capa | Nombre | Responsabilidad | Grado de rigidez |
|---|---|---|---|
| L0 | Admin Executor | Ejecuta exactamente un `executor_plan` validado | Máxima rigidez |
| L1 | Plan Compiler | Convierte task/delta claro en `executor_plan` | Rígido con algo de lectura live |
| L2 | Artifact Programmer | Genera runtime packages, configs, prompts, workflows, OPA | Generativo acotado |
| L3 | Reconciler | `desired_state + actual_state -> delta_report` | Determinístico |
| L4 | Designer | Convierte necesidad técnica en `solution_manifest` | Probabilístico informado |
| L5 | Archi Host | Coordina conversación, tools, confirmaciones y pipeline | Router / coordinador |
| L6 | Process Translator | Detecta proceso real, ownership, fricción, fuente de verdad | Futuro, no tocar ahora |

Esta iteración trabaja principalmente en **L1: PlanCompiler** y deja preparadas reglas para los otros prompts.

---

## 4. Principio rector para prompts

Cada prompt debe quedar como **workflow del agente**, no como manual completo de Fluxbee.

Un prompt sí puede contener:

```text
- rol del agente;
- entrada esperada;
- salida obligatoria;
- procedimiento paso a paso;
- herramientas permitidas;
- prohibiciones;
- condiciones de bloqueo;
- cuándo debe llamar exactamente una tool final.
```

Un prompt no debería contener:

```text
- catálogo largo de endpoints;
- schema largo de workflows;
- payloads completos de acciones admin;
- detalles constructivos generales de plataforma;
- ejemplos extensos que parezcan contrato;
- reglas específicas de una acción que deberían vivir en get_admin_action_help;
- patrones exitosos que deberían vivir en cookbook.
```

---

# 5. Cambio principal: PlanCompiler

## 5.1 Problema actual

`PlanCompiler` hoy recibe:

- catálogo compacto de admin actions;
- `query_hive`;
- `get_admin_action_help`;
- `plan_compile_cookbook`;
- prompt propio.

Pero no recibe handbook ni un extracto compacto del modelo de plataforma.

No conviene inyectarle el handbook completo. Eso movería el problema de “falta info” a “demasiada info”.

La solución propuesta es inyectar un bloque chico y explícito:

```text
PLATFORM_PLANNING_FACTS
```

Ese bloque no reemplaza el handbook. Es una destilación mínima para planificación.

---

## 5.2 Nuevo bloque: `PLATFORM_PLANNING_FACTS`

Agregar un bloque estático inyectado al prompt del PlanCompiler, idealmente desde un archivo o constante separada.

Nombre sugerido:

```text
PLAN_COMPILER_PLATFORM_FACTS
```

Contenido sugerido:

```markdown
## PLATFORM_PLANNING_FACTS

Use these facts only to select and validate the right planning intent. Do not treat them as admin action schemas. For exact parameters and request shape, call `get_admin_action_help` for each action.

### Core concepts

- A Fluxbee **runtime** is a published package/template. It is similar to an image.
- A Fluxbee **node** is a managed instance created from a runtime. It is similar to a running or persisted container instance.
- A published runtime does not imply that a node exists.
- A managed node instance may exist even when the process is stopped.
- A live process is not the same thing as a persisted managed instance.

### Naming

- A full L2 node identity is composed as `<node_name>@<hive>`.
- `node_name` must include a functional prefix such as `AI.*`, `IO.*`, `WF.*`, `SY.*`, or supported `RT.*`.
- The hive in the node identity suffix must match the target hive argument when creating or operating that node.
- Runtime names do not include `@hive`. Node instances do.
- Example: `AI.support@motherbee` is a node instance. `ai.common` or `AI.common` is a runtime/package name.

### Lifecycle selection

- Use `run_node` only to create/spawn a new managed node instance.
- Use `start_node` only when the managed instance already exists and is stopped.
- Use `restart_node` only when the managed instance already exists and should be restarted.
- Use `kill_node` to stop a node process. It does not necessarily delete the persisted instance.
- Use `remove_node_instance` only to delete the persisted managed instance.
- Use `publish_runtime_package` to publish a runtime package. It does not spawn nodes.

### Config selection

- `get_node_config` reads persisted node config from disk/snapshot.
- `node_control_config_get` asks a running node for its live config contract.
- `node_control_config_set` updates live node-defined config for an existing node. It does not create nodes.
- For config updates, read current live config first when config_version is required.

### Restart semantics

- `AI.*` and `IO.*` config changes are generally hot-apply unless the live contract says otherwise.
- `WF.*` and `SY.*` config changes are conservative and generally require restart unless the live contract says otherwise.
- If restart requirement is ambiguous, surface it as risk or block rather than guessing.

### Tenant and identity

- Some first-spawn operations for AI/IO tenant-scoped nodes require `tenant_id` at root action args.
- Do not invent `tenant_id`.
- If a required `tenant_id` is missing and cannot be read from reliable context, block with a clear missing-field reason.

### Planning discipline

- Use `query_hive` for pre-read state needed to produce static executor plan args.
- Do not include read-only actions as executor plan steps unless the operator explicitly requested a read-only executor plan.
- Do not choose a runtime by vague name similarity. If the runtime is missing or ambiguous, block or require upstream clarification.
```

---

## 5.3 Refactor del prompt base de PlanCompiler

El prompt base debería quedar dividido en secciones claras:

1. Role
2. Inputs
3. Workflow
4. Tool discipline
5. Output contract
6. Hard prohibitions
7. Delta mode
8. Legacy direct-task mode
9. Cookbook rule

### Prompt sugerido

Reemplazar el cuerpo actual de `PLAN_COMPILER_SYSTEM_PROMPT_BASE` por algo cercano a esto:

```markdown
You are the `plan_compiler` agent inside SY.architect.

Your only job is to produce a valid `executor_plan` JSON for SY.admin.
You do not design solutions. You do not generate runtime artifacts. You do not execute admin actions. You do not interpret broad user intent.

## Inputs

You receive one of two input modes:

1. `delta_report` mode — preferred pipeline mode.
   - Translate each deterministic delta operation into executor plan steps.
   - Use the canonical `compiler_class` translation table from the provided context.
   - Do not add steps that are not justified by a delta operation.

2. `legacy_direct_task` mode — backward-compatible path for clear direct mutations.
   - The task must already be clear and operational.
   - If required target values are missing or ambiguous, block instead of guessing.

## Required workflow

1. Determine the required admin action sequence.
2. Use `query_hive` only when live state is needed to produce static plan args.
3. Before using any admin action in a plan step, call `get_admin_action_help(action)`.
4. Build a static `executor_plan` with concrete args.
5. Call `submit_executor_plan` exactly once.

## Tool discipline

- `query_hive` is for read-only pre-planning context only.
- `get_admin_action_help` is mandatory for every admin action used in the final plan.
- `submit_executor_plan` is the final tool call.
- Never call mutating admin actions directly.

## Executor plan format

Return exactly this shape through `submit_executor_plan`:

```json
{
  "plan_version": "0.1",
  "kind": "executor_plan",
  "metadata": {
    "name": "<short_snake_case_name>",
    "target_hive": "<hive_id>"
  },
  "execution": {
    "strict": true,
    "stop_on_error": true,
    "allow_help_lookup": true,
    "steps": [
      {
        "id": "s1",
        "action": "<admin_action_name>",
        "args": {}
      }
    ]
  }
}
```

## Hard rules

- Use only admin actions available in the injected catalog or returned by admin help.
- Never invent action names.
- Never invent arg fields.
- Never leave unresolved placeholders such as `{hive}` or `{node_name}`.
- Never include broad design reasoning in the plan.
- Never choose a runtime by guess or vague similarity.
- Never use `run_node` to start an existing stopped instance.
- Never use `start_node` to create a missing instance.
- Never use `node_control_config_set` to create a node.
- Never include read-only discovery actions as executor steps unless the requested plan itself is read-only.
- If a required value is missing, submit a blocked/invalid plan response if supported by the tool contract, or produce a concise validation failure summary through `submit_executor_plan` according to the existing schema.

## Delta-report mode

When `delta_report` is present:

- Translate only `delta_report.operations`.
- Use the canonical compiler_class table from context.
- Respect operation order and dependencies.
- Use approved artifact `publish_source` exactly when provided.
- Do not reinterpret the desired state.
- Do not add cleanup, restart, publish, route, or config steps unless they are required by the compiler_class translation.

## Legacy direct-task mode

When no `delta_report` is present:

- Handle only clear operational mutation requests.
- Use live reads to determine whether the correct action is `run_node`, `start_node`, `restart_node`, config set, route add, runtime publish, etc.
- If the request is broad design work, do not compile; it should have gone through the pipeline.
- If required values are missing, block rather than guess.

## Cookbook use

Use `plan_compile_cookbook` only as examples of previously successful shapes.
The cookbook does not override admin action help, platform facts, delta_report, or validation errors.

## Human summary

The human summary must be one short paragraph explaining what the plan will do and what risks exist.
```

Notas para dev:

- Si `submit_executor_plan` no soporta un estado bloqueado, no inventar uno sin revisar schema. En ese caso, la salida debe fallar validación de manera clara o usar el mecanismo existente de error del tool runner.
- La frase “block instead of guessing” debe estar respaldada por el validador. Si el agente intenta inventar, el validador debe rechazar.

---

## 5.4 Qué sacar del prompt de PlanCompiler

Remover o mover fuera del prompt base:

| Contenido actual | Destino correcto |
|---|---|
| Naming convention Fluxbee largo | `PLATFORM_PLANNING_FACTS` / handbook |
| Diferencia runtime vs node | `PLATFORM_PLANNING_FACTS` |
| Reglas completas de WF definition | `workflow_definition_contract_v1` o admin help de `wf_rules_compile_apply` |
| Ejemplos largos de payloads admin | `get_admin_action_help` |
| Runtime selection heuristics | eliminar o pasar a contract/policy determinística |
| Shape completo de cookbook | `cookbook_entry_v2.schema.json` |

Regla: si el texto responde “qué es Fluxbee” o “cómo se llama un campo de una acción”, probablemente no pertenece al workflow prompt.

---

## 5.5 Cambio de inyección en `build_plan_compiler_prompt(...)`

Actualmente el prompt se arma con actions + cookbook. Cambiarlo para incluir también `PLATFORM_PLANNING_FACTS`.

Orden sugerido:

```text
PLAN_COMPILER_SYSTEM_PROMPT_BASE

PLATFORM_PLANNING_FACTS

AVAILABLE ACTIONS compact catalog

PLAN_COMPILE_COOKBOOK examples

TASK / DELTA CONTEXT
```

Motivo del orden:

- Primero se establece cómo debe trabajar.
- Después se le da modelo mental mínimo.
- Después se le da superficie de acciones.
- Después patrones observados.
- Al final la tarea concreta.

No inyectar el handbook completo al PlanCompiler en esta iteración.

---

# 6. Cambios por agente

## 6.1 Archi chat

### Estado deseado

`Archi` debe ser host/coordinador, no programador ni ejecutor.

Debe decidir entre:

```text
read/status                         -> fluxbee_system_get
mutación clara operacional           -> fluxbee_plan_compiler
solución amplia / multi-recurso       -> fluxbee_start_pipeline
pipeline existente                   -> fluxbee_pipeline_action
proceso humano ambiguo               -> futuro process_translator, no ahora
```

### Cambios recomendados ahora

Cambio mínimo de prompt:

```markdown
Archi must not build admin payloads itself.
For clear mutations, call `fluxbee_plan_compiler` with the operator task, target hive if known, and relevant context.
For broad solution design or multi-resource desired-state work, call `fluxbee_start_pipeline`.
For reads, use `fluxbee_system_get`.
Do not answer mutation requests with hand-written SCMD payloads unless the operator explicitly asked for manual SCMD.
```

### Qué NO hacer

- No inyectar admin action catalog a Archi.
- No darle cookbooks todavía.
- No permitir que Archi arme payloads grandes de admin como comportamiento normal.
- No duplicar en Archi reglas que pertenecen al PlanCompiler.

---

## 6.2 Designer

### Estado deseado

`Designer` convierte intención técnica en `solution_manifest`. No ejecuta ni planea acciones admin.

### Cambios recomendados ahora

No tocar funcionalmente en esta iteración, salvo limpiar el prompt si hay frases que sugieran steps/admin operations.

Agregar/reforzar:

```markdown
Designer produces desired state only.
Designer must not produce executor plans, admin action sequences, SCMD commands, or deployment procedures.
If an implementation detail belongs to admin execution, represent the desired final state instead of the action sequence.
```

### Futuro cercano

Externalizar schema de manifest hacia:

```text
contracts/solution_manifest_v2.schema.json
```

El prompt puede resumirlo, pero no debería ser la fuente canónica del schema.

---

## 6.3 DesignAuditor

### Estado deseado

`DesignAuditor` valida consistencia del manifest. No debe leer estado live ni planear ejecución.

### Cambios recomendados ahora

No tocar en esta iteración.

Solo agregar, si no existe:

```markdown
DesignAuditor checks the submitted manifest against the injected manifest contract and handbook.
DesignAuditor must not propose admin actions or executor steps.
```

### Futuro cercano

Crear:

```text
contracts/design_audit_checklist_v1.json
```

El prompt debería decir “aplicar checklist”, no contener toda la checklist como prosa larga.

---

## 6.4 RealProgrammer

### Estado deseado

`RealProgrammer` genera artifacts a partir de un `build_task_packet` completo. No consulta live state y no decide arquitectura.

### Cambios recomendados ahora

No agregar `query_hive`.

Si falla por falta de info, corregir el `build_task_packet`, no el acceso del agente.

Refuerzo de prompt:

```markdown
RealProgrammer works only from the build_task_packet, artifact contract, repair_packet if present, handbook excerpt, and artifact cookbook.
RealProgrammer must not design topology, choose admin actions, publish runtimes, or infer missing live state.
If the packet lacks a required value, return a blocked/underspecified artifact result according to the tool contract instead of guessing.
```

### Qué mover fuera del prompt

| Contenido | Destino correcto |
|---|---|
| Layout completo de runtime package | `runtime_package_contract_v1` |
| Reglas de files requeridos | schema/tool de `submit_artifact_bundle` + artifact contract |
| Ejemplos de package.json | cookbook o contract examples |
| Reglas de workflow artifact | `workflow_definition_contract_v1` |

---

## 6.5 PlanCompiler

Este es el cambio principal de la iteración.

### Estado deseado

`PlanCompiler` debe tener:

```text
- workflow prompt limpio;
- PLATFORM_PLANNING_FACTS compacto;
- admin actions catalog compacto;
- get_admin_action_help obligatorio;
- query_hive para pre-read;
- plan_compile_cookbook como patrones, no contrato.
```

### Cambios concretos

1. Agregar bloque `PLAN_COMPILER_PLATFORM_FACTS`.
2. Inyectarlo en `build_plan_compiler_prompt(...)`.
3. Reducir `PLAN_COMPILER_SYSTEM_PROMPT_BASE` a workflow.
4. Remover schema largo de WF del prompt base.
5. Remover heurísticas de elección de runtime si no son determinísticas.
6. Reforzar “block instead of guessing”.
7. Asegurar que el validador rechace acciones usadas sin help lookup, si esa telemetría ya está disponible.

---

## 6.6 Admin Executor

### Estado deseado

El executor debe seguir rígido.

### Cambios recomendados ahora

No tocar prompt ni herramientas salvo bug comprobado.

Opcional, no bloqueante:

```text
Agregar telemetría en execution events:
- help_lookup_required
- help_lookup_done
- action_help_used
```

No ampliar inteligencia del executor. Si el executor necesita decidir arquitectura, algo falló en capas superiores.

---

# 7. Archivos sugeridos

Para esta iteración, se puede implementar como constantes en `sy_architect.rs`, pero la dirección final debería separar archivos.

## Mínimo para esta iteración

```text
src/bin/sy_architect.rs
  - PLAN_COMPILER_SYSTEM_PROMPT_BASE actualizado
  - PLAN_COMPILER_PLATFORM_FACTS nuevo
  - build_plan_compiler_prompt(...) inyecta ambos
```

## Dirección recomendada posterior

```text
assets/prompts/archi_host_workflow.md
assets/prompts/designer_workflow.md
assets/prompts/design_auditor_workflow.md
assets/prompts/real_programmer_workflow.md
assets/prompts/plan_compiler_workflow.md
assets/prompts/admin_executor_workflow.md

assets/knowledge/fluxbee_platform_model.md
assets/knowledge/plan_compiler_platform_facts.md
assets/knowledge/runtime_package_model.md
assets/knowledge/workflow_model.md

assets/contracts/solution_manifest_v2.schema.json
assets/contracts/artifact_bundle_v1.schema.json
assets/contracts/runtime_package_contract_v1.json
assets/contracts/workflow_definition_contract_v1.json
assets/contracts/cookbook_entry_v2.schema.json
```

No hace falta crear todo ahora. Para la primera iteración alcanza con `PLAN_COMPILER_PLATFORM_FACTS` y prompt refactor.

---

# 8. Pruebas mínimas de esta iteración

No ejecutar una suite enorme. Probar que el cambio de prompt mejora selección de capa y de acción.

## Caso 1 — read-only

Input:

```text
Listá los nodos de motherbee.
```

Esperado:

```text
Archi usa fluxbee_system_get.
No llama PlanCompiler.
No genera executor_plan.
```

## Caso 2 — crear nodo AI con tenant

Input:

```text
Creá AI.test@motherbee usando runtime ai.common para tenant tnt:00000000-0000-0000-0000-000000000001.
```

Esperado:

```text
Archi llama fluxbee_plan_compiler.
PlanCompiler usa query_hive si necesita verificar runtime/nodo.
PlanCompiler llama get_admin_action_help(run_node).
PlanCompiler genera step run_node.
No usa start_node.
No mete tenant_id solo dentro de config si el action help lo requiere a nivel raíz.
```

## Caso 3 — arrancar instancia existente detenida

Input:

```text
Arrancá AI.test@motherbee que ya existe pero está detenida.
```

Esperado:

```text
PlanCompiler genera start_node.
No genera run_node.
```

## Caso 4 — reiniciar nodo existente

Input:

```text
Reiniciá AI.test@motherbee.
```

Esperado:

```text
PlanCompiler genera restart_node.
No genera kill_node + run_node.
```

## Caso 5 — publicar runtime y correr nodo

Input:

```text
Publicá el runtime ai.demo y corré AI.demo@motherbee con ese runtime.
```

Esperado si artifact/source está disponible:

```text
publish_runtime_package -> run_node
```

Esperado si artifact/source no está disponible:

```text
Bloquea o deriva a pipeline/artifact flow.
No inventa inline_package.
```

## Caso 6 — falta tenant requerido

Input:

```text
Creá AI.test@motherbee usando runtime ai.common.
```

Esperado si el runtime/action contract requiere tenant:

```text
Bloquea por missing tenant_id o pide ese dato una sola vez aguas arriba.
No inventa tenant_id.
```

---

# 9. Acceptance criteria

La iteración se considera correcta si:

1. `PlanCompiler` recibe `PLATFORM_PLANNING_FACTS`.
2. `PlanCompiler` ya no depende del prompt base para saber payloads específicos de acciones.
3. `PlanCompiler` diferencia correctamente:
   - runtime vs node;
   - `run_node` vs `start_node` vs `restart_node`;
   - publish runtime vs run node;
   - config persistida vs config live.
4. Las pruebas mínimas producen la acción correcta o bloquean claramente.
5. No se agregaron herramientas nuevas a agentes sin necesidad.
6. No se inyectó el handbook completo al PlanCompiler.
7. No se debilitó el executor de `SY.admin`.

---

# 10. Anti-patterns a evitar

No hacer esto:

```text
- Meter todo el handbook en PlanCompiler.
- Dar query_hive al RealProgrammer para compensar packets incompletos.
- Agregar admin action catalog a Archi.
- Resolver errores agregando frases sueltas al prompt cada vez.
- Poner schemas largos de WF dentro del prompt base.
- Dejar que PlanCompiler elija runtime por parecido de nombre.
- Permitir que el executor haga decisiones de arquitectura.
- Duplicar reglas de acciones admin en prompt y en get_admin_action_help.
```

Hacer esto:

```text
- Si falta conocimiento de plataforma, ponerlo en handbook/facts.
- Si falta contrato de acción, ponerlo en admin action help.
- Si falta patrón observado, ponerlo en cookbook.
- Si falta dato concreto de la tarea, ponerlo en task packet/context.
- Si falta validación, ponerla en validator, no en prompt.
```

---

# 11. Orden de cambio sugerido

Mantenerlo chico:

1. Agregar `PLAN_COMPILER_PLATFORM_FACTS`.
2. Inyectarlo en `build_plan_compiler_prompt(...)`.
3. Limpiar `PLAN_COMPILER_SYSTEM_PROMPT_BASE` para que sea workflow puro.
4. Correr los 6 casos mínimos.
5. Registrar por cada caso:
   - decisión de Archi;
   - action sequence propuesta;
   - help lookups hechos;
   - plan validation result;
   - motivo de bloqueo si bloqueó.
6. Recién después decidir si hace falta tocar Designer o RealProgrammer.

---

# 12. Nota de diseño

La dirección correcta no es hacer todos los agentes más inteligentes. Es hacer que cada capa tenga la clase de información que necesita.

En esta primera iteración, el objetivo no es que Fluxbee resuelva procesos humanos complejos. El objetivo es más básico y medible:

```text
Archi debe poder transformar una mutación técnica clara en un executor_plan válido, sin confundir conceptos básicos de Fluxbee y sin inventar parámetros.
```

Cuando eso funcione de forma repetible, recién tiene sentido subir a artifacts, manifests, reconciler y después proceso real / mejora continua.
