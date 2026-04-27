# Fluxbee — Reingeniería completa para cerrar el gap entre necesidad, solución, artefactos y ejecución

**Status:** propuesta canónica de reingeniería
**Fecha:** 2026-04-23
**Audiencia:** `SY.architect`, `SY.admin`, `SY.orchestrator`, runtime/package developers, AI-agent designers, operator UX
**Base:** `solution-manifest-spec_1.md`, `executor_manifest_pilot_spec.md`, `executor_manifest_pilot_tasks.md`, `programmer_agent_tasks.md`, `runtime_package_publish_tasks.md`, `handbook_fluxbee.md`, `sy_architect.rs`, y la revisión integrada previa del puente `solution_manifest -> executor_plan`

---

## 1. Objetivo

Definir la reingeniería completa del sistema para que Fluxbee pueda evolucionar desde el piloto actual hacia una arquitectura consistente, donde una necesidad humana pueda recorrer estas capas de forma trazable y corregible:

1. necesidad / intención del operador
2. diseño de solución
3. producción de artefactos técnicos
4. compilación a plan operativo
5. ejecución exacta sobre Fluxbee admin
6. verificación y aprendizaje reutilizable

La meta no es “mejorar el piloto” de manera incremental sin criterio.
La meta es **fijar una arquitectura correcta**, conservar únicamente las fronteras que ya demostraron valor real y reemplazar sin piedad las piezas que hoy mezclan responsabilidades.

---

## 2. Diagnóstico de la situación actual

## 2.1 Lo que ya probó valor real

Hay una frontera que sí cerró y que debe preservarse como núcleo firme:

- `executor_plan` como artefacto operativo secuencial
- `SY.admin` como hogar del executor AI
- catálogo de functions 1:1 contra acciones reales de admin
- schemas estrictos por acción
- stop-on-error
- ejecución visible paso a paso en chat

Eso ya resuelve un problema real: la última milla de ejecución exacta.

## 2.2 Lo que todavía está conceptualmente mezclado

Hoy siguen mezcladas o mal nombradas estas responsabilidades:

- diseño de solución
- compilación a plan operativo
- programación real de software/artefactos
- reparación de errores
- aprendizaje reusable (cookbook)

El nombre `programmer` se usa de forma ambigua para varias funciones distintas. Eso vuelve borroso el loop de errores y hace débil la atribución causal.

## 2.3 Decisión estratégica

No se debe preservar la implementación por continuidad histórica.
Se debe preservar solo la frontera correcta que ya funciona.

Regla madre de esta reingeniería:

> **Preservar fronteras correctas; reemplazar capas ambiguas.**

---

## 3. Principios canónicos

### 3.1 Una capa no debe hacer el trabajo de otra

- el manifest no ejecuta
- el executor no diseña
- el coder no opera admin
- el auditor no implementa
- el host no improvisa contratos

### 3.2 Los artefactos deben ser explícitos

Cada salto entre capas debe producir un artefacto versionable, auditable y comparable.
No se permiten “saltos mágicos” sostenidos solo por prompt.

### 3.3 Las iteraciones deben estar guiadas por clasificación de fallas

No se reintenta “porque falló”.
Se reintenta según una clase de falla y con un `repair_packet` explícito.

### 3.4 El aprendizaje debe ser estratificado

No existe “un cookbook general”.
Debe haber memorias distintas por capa:

- patrones de diseño
- patrones de artefactos
- patrones de compilación a plan
- patrones de reparación

### 3.5 La operación real pertenece al control plane real

La ejecución sigue anclada en la superficie exacta de admin, con nombres 1:1 y contracts estrictos.
No se introduce DSL, macro layer ni aliases operativos nuevos.

---

## 4. Arquitectura objetivo

```text
human need
  -> host (SY.architect)
  -> designer
  -> solution_manifest
  -> design auditor
  -> manifest compiler / reconciliation planner
      -> build_task_packets (si faltan artefactos)
      -> real_programmer
      -> artifact auditor
      -> build_artifact_bundles
      -> compiler re-check
      -> executor_plan
  -> execution auditor (preflight opcional)
  -> executor (SY.admin)
  -> execution_report
  -> verifier / auditor final
  -> cookbook write (solo casos verificados)
```

Esta arquitectura separa claramente tres mundos:

1. **Diseño declarativo**
2. **Construcción/materialización técnica**
3. **Ejecución operativa**

---

## 5. Componentes canónicos y responsabilidades

## 5.1 Host — `SY.architect`

### Responsabilidades

- conversar con el operador
- mantener contexto de sesión, historial y versiones
- invocar los actores correctos
- persistir artefactos intermedios
- decidir el próximo actor según el estado del pipeline
- renderizar progreso y resultados en chat
- exponer UI/API para manifest, ejecución, trazas y reportes

### No responsabilidades

- no ejecutar el plan directamente si existe executor en `SY.admin`
- no concentrar toda la lógica del catálogo admin en el prompt
- no reemplazar al auditor con reglas ad hoc en el host
- no escribir artefactos complejos por cuenta propia

## 5.2 Designer

### Responsabilidades

- producir `solution_manifest`
- revisar y extender manifest existentes
- completar topología, nodos, routing, policy, identity, ownership y constraints
- responder feedback del auditor de diseño

### No responsabilidades

- no generar `executor_plan`
- no generar packages concretos
- no operar publish/spawn/route

## 5.3 Real Programmer (Coder / Runtime Builder)

### Responsabilidades

- generar paquetes `inline_package` o bundles
- crear o modificar archivos de runtime
- generar definitions de workflow
- generar source Rego / assets / prompts / config compleja
- producir artefactos verificables y estables
- corregir fallas de build/materialization/contract si el auditor lo clasifica como reparable por código

### No responsabilidades

- no elegir la arquitectura general
- no decidir topología
- no ejecutar admin
- no compilar el plan final
- no resolver manualmente rutas o deploy order salvo como hints del artifact

## 5.4 Manifest Compiler / Reconciliation Planner

### Responsabilidades

- leer `solution_manifest`
- leer estado real actual
- determinar el delta deseado
- detectar qué artefactos faltan, cuáles existen y cuáles pueden reutilizarse
- emitir `build_task_packet` para el programmer real cuando haga falta materializar software o config compleja
- compilar a `executor_plan` cuando ya están disponibles los artefactos necesarios
- preservar el orden correcto publish -> sync/update -> run_node -> config -> route
- mantener alineación exacta con el surface admin

### No responsabilidades

- no escribir software
- no improvisar campos si faltan definiciones de diseño
- no ejecutar admin “para ver qué pasa”

## 5.5 Auditor

El auditor deja de ser una idea futura y pasa a ser componente obligatorio.

### Subroles de auditoría

#### a. Design Auditor
Valida:
- coherencia del `solution_manifest`
- consistencia entre topology/runtimes/nodes/routing/policy/identity
- gaps arquitectónicos
- ownership ambiguo
- decisiones peligrosas o incompletas

#### b. Artifact Auditor
Valida:
- shape de paquetes
- consistencia runtime_base/runtime_type
- nomenclatura
- archivos requeridos
- constraints técnicas declaradas por el compiler

#### c. Failure Auditor
Clasifica:
- si la falla es de diseño
- de compilación a plan
- de artefacto
- de contrato admin
- de entorno
- transitoria
- no reintentable

#### d. Verification Auditor
Decide:
- si el caso fue realmente exitoso
- si merece ingresar a cookbook
- si la solución quedó operativa según la aceptación esperada

## 5.6 Executor — `SY.admin`

### Responsabilidades

- ejecutar `executor_plan`
- usar functions 1:1 con admin
- usar help cuando corresponda
- frenar ante error o ambigüedad
- emitir eventos estructurados
- producir `execution_report`

### No responsabilidades

- no rediseñar solución
- no crear nuevos pasos
- no corregir artefactos
- no reemplazar al compiler

---

## 6. Reasignación de lo que existe hoy

## 6.1 El `fluxbee_programmer` actual no debe seguir llamándose así

La implementación actual llamada `fluxbee_programmer` cumple en realidad el rol de:

- sintetizar `executor_plan`
- consultar estado vivo (`query_hive`)
- consultar help de acciones (`get_admin_action_help`)
- devolver plan + summary
- hacer una pre-validación inicial

Ese componente no es un “programmer real”.
Es un **plan compiler operativo**.

### Decisión

Renombrar conceptualmente y luego en código:

- de `fluxbee_programmer`
- a `fluxbee_plan_compiler`

Durante la migración puede mantenerse el nombre viejo por compatibilidad, pero el rol nuevo debe quedar explicitado en spec, prompt y docs.

## 6.2 El infrastructure specialist actual

Debe evolucionar hacia un rol más claro de:

- materializador de infraestructura / assembler
- útil para convertir una intención técnica en `publish_request`, bundles o payloads reutilizables

Pero no debe absorber diseño ni operación final.

## 6.3 El executor actual

Se preserva como base canónica.
No se rediseña su frontera.
Solo se consolida como capa final estable.

---

## 7. Artefactos canónicos

## 7.1 `solution_manifest`

### Propósito
Fuente declarativa versionable del estado deseado completo de la solución.

### Debe contener

- `solution`
- `tenant`
- `topology`
- `runtimes`
- `nodes`
- `routing`
- `policy`
- `identity`
- ownership explícito (`system`, `solution`, `external`) donde aplique
- metadata de versión e historial

### No debe contener

- pasos operativos
- SCMD
- nombres de functions OpenAI
- orden de ejecución detallado
- defaults mecánicos de admin

### Recomendación de recorte

Separar explícitamente:

- `desired_state`
- `advisory`

Para evitar que sizing notes y recomendaciones terminen contaminando reconciliación.

## 7.2 `build_task_packet`

### Propósito
Instrucción cerrada enviada al real programmer para construir o corregir un artefacto técnico.

### Shape conceptual

```json
{
  "task_id": "...",
  "source_manifest_ref": "...",
  "artifact_kind": "runtime_package_source | workflow_definition | opa_bundle | node_config_bundle | other",
  "goal": "...",
  "target_hive": "...",
  "constraints": {
    "runtime_type": "...",
    "runtime_base": "...",
    "naming_rules": true,
    "forbidden_actions": [],
    "required_files": []
  },
  "acceptance": {
    "must_publish": true,
    "must_spawn": false,
    "must_compile": true,
    "must_verify": true
  },
  "known_context": {
    "published_runtimes": [],
    "running_nodes": [],
    "existing_routes": []
  },
  "attempt": 1,
  "max_attempts": 3,
  "cookbook_context": []
}
```

## 7.3 `build_artifact_bundle`

### Propósito
Salida estructurada del real programmer.

### Debe contener

- tipo de artefacto
- summary corta
- payload técnico principal
- assumptions explícitas
- verification hints
- content digest
- version/ref si aplica

### Shape conceptual

```json
{
  "status": "artifact_ready",
  "artifact_kind": "runtime_package_source",
  "summary": "...",
  "artifact": {
    "publish_request": { ... }
  },
  "assumptions": ["..."],
  "verification_hints": ["..."],
  "artifact_digest": "sha256:..."
}
```

## 7.4 `audit_verdict`

### Propósito
Resultado mínimo y utilizable del auditor.

### Debe contener

- stage auditado
- status (`pass`, `revise`, `reject`)
- score opcional donde aplique
- findings
- blocking issues
- retry recommendation
- cookbook eligibility

## 7.5 `repair_packet`

### Propósito
Feedback compacto y accionable para la siguiente iteración.

### Regla
No se reinyectan logs crudos al actor AI.
Se resume el problema en forma mínima y estructurada.

### Shape conceptual

```json
{
  "task_id": "...",
  "attempt": 2,
  "previous_artifact_digest": "sha256:...",
  "failure_class": "STATIC_INVALID",
  "failing_stage": "publish_runtime_package",
  "error_summary": "package missing package.json",
  "error_details": ["..."],
  "do_not_repeat": ["..."],
  "must_preserve": ["runtime_name", "runtime_base"]
}
```

## 7.6 `executor_plan`

Se mantiene como artefacto operativo canónico de la última milla.
No se cambia su filosofía central.

### Debe seguir siendo

- secuencial
- estricto
- con `steps[].action` usando nombres exactos de admin
- con `steps[].args` tipados por schema real
- con `allow_help_lookup`
- con visibilidad en chat

## 7.7 `execution_report`

### Propósito
Resumen verificable de lo que ocurrió al ejecutar.

### Debe contener

- execution id
- plan ref/digest
- steps ejecutados
- resultados por paso
- primer fallo si existió
- output relevante
- redactions aplicadas
- resumen final

## 7.8 `cookbook_entry_v2`

### Propósito
Memoria reutilizable de casos realmente exitosos.

### Debe incluir

- layer (`design`, `artifact`, `plan_compile`, `repair`)
- task pattern
- trigger
- constraints relevantes
- steps/actions pattern o artifact pattern
- failure classes avoided
- acceptance signature
- environment fingerprint
- evidence refs
- recorded_at

---

## 8. Cookbooks separados

## 8.1 Cookbook de diseño

Patrones de solución/manifest.
Ejemplos:

- tenant support system with dedicated worker
- AI + WF + IO topology patterns
- policy/routing composition patterns

## 8.2 Cookbook de artefactos

Patrones de package/config/workflow source.
Ejemplos:

- `config_only` package over `AI.common`
- `workflow` runtime over `wf.engine`
- OPA bundle pattern

## 8.3 Cookbook de compilación a plan

Patrones para convertir cierto delta del manifest en secuencias `executor_plan`.
Ejemplos:

- publish -> sync/update -> run_node -> add_route
- config-set after config-get version read

## 8.4 Cookbook de reparación

Patrones de falla -> corrección.
Ejemplos:

- missing package.json in inline_package
- runtime_base mismatch
- wrong node naming
- missing materialized runtime before run_node

## 8.5 Regla de escritura

Un cookbook solo se escribe si:

1. el flujo llegó a éxito real
2. el auditor final lo marcó como reusable
3. la evidencia es suficiente
4. no hubo objeciones abiertas

---

## 9. Loops canónicos

## 9.1 Loop de diseño

```text
host
  -> designer
  -> solution_manifest
  -> design auditor
  -> if revise: back to designer
  -> if pass: forward to manifest compiler
```

### Stop conditions

- score suficiente
- sin blocking issues
- ownership y topology consistentes

## 9.2 Loop de construcción de artefactos

```text
manifest compiler
  -> build_task_packet
  -> real_programmer
  -> build_artifact_bundle
  -> artifact auditor
  -> if revise: repair_packet -> real_programmer
  -> if pass: artifact registered / available to compiler
```

### Stop conditions

- max attempts alcanzado
- failure class no reintentable
- mismo digest + misma clase de falla repetidos

## 9.3 Loop de compilación a plan

```text
manifest compiler
  + actual state
  + available artifacts
  -> executor_plan
  -> optional preflight audit
  -> executor
```

### Regla
El compiler no debe generar plan mientras falten artefactos requeridos.

## 9.4 Loop de ejecución

```text
host
  -> executor (SY.admin)
  -> execution_report
  -> failure auditor / verifier
  -> if retryable by code: repair_packet -> real_programmer
  -> if retryable by plan: back to manifest compiler / plan compiler
  -> if retryable by design: back to designer
  -> if success: cookbook gate
```

---

## 10. Taxonomía obligatoria de fallas

El auditor debe clasificar toda falla en una de estas clases mínimas:

- `DESIGN_INVALID`
- `MANIFEST_INCOMPLETE`
- `ARTIFACT_STATIC_INVALID`
- `ARTIFACT_CONTRACT_INVALID`
- `PLAN_INVALID`
- `EXECUTION_ADMIN_CONTRACT_INVALID`
- `EXECUTION_ENVIRONMENT_MISSING`
- `EXECUTION_TRANSIENT`
- `LOGIC_BUG`
- `NON_RETRYABLE`

## 10.1 Regla de enrutamiento según clase

### `DESIGN_INVALID` / `MANIFEST_INCOMPLETE`
Vuelve al designer.

### `ARTIFACT_STATIC_INVALID` / `ARTIFACT_CONTRACT_INVALID` / `LOGIC_BUG`
Vuelve al real programmer vía `repair_packet`.

### `PLAN_INVALID`
Vuelve al plan compiler.

### `EXECUTION_ADMIN_CONTRACT_INVALID`
Puede volver al plan compiler o exponer drift de contract que exige fix de plataforma.

### `EXECUTION_ENVIRONMENT_MISSING`
No debe mandar al coder a reescribir artefactos si el problema es de entorno.
Debe escalar a compiler/host para resolver prerequisitos.

### `EXECUTION_TRANSIENT`
Retry del executor sin tocar al coder.

### `NON_RETRYABLE`
Stop inmediato.

---

## 11. Qué debe cambiar en los módulos actuales

## 11.1 `fluxbee_programmer` actual

### Cambio canónico

Reinterpretarlo como `plan_compiler`.

### Qué conservar

- uso de catálogo admin vivo
- `query_hive`
- `get_admin_action_help`
- `submit_executor_plan`
- validación previa
- summary para el host

### Qué sacar de su identidad

- dejar de presentarlo como “programmer real”
- no usarlo para escribir packages o software
- no mezclar su cookbook con cookbook de artefactos

## 11.2 Nuevo `real_programmer`

### Crear componente nuevo

Debe ser un agente/tool separado con:

- prompt propio
- packet de entrada propio
- salida estructurada propia
- retry policy propia
- cookbook técnico propio

### Inputs típicos

- `build_task_packet`
- `repair_packet`
- cookbook técnico filtrado

### Outputs

- `build_artifact_bundle`

## 11.3 Auditor

### Crear componente nuevo

Primero puede ser único con varios modos internos:

- `design_audit`
- `artifact_audit`
- `failure_classification`
- `verification_gate`

Más adelante puede separarse físicamente.

## 11.4 Manifest Compiler

### Crear componente nuevo o reusar el actual programmer en rol nuevo

En el corto plazo, el componente actual puede migrar hacia aquí.
En el mediano plazo debe quedar claro que este rol no escribe software.

## 11.5 Host orchestration

`SY.architect` debe pasar a una state machine más explícita.

Estados sugeridos:

- `designing`
- `design_auditing`
- `artifact_building`
- `artifact_auditing`
- `planning`
- `plan_auditing`
- `executing`
- `verifying`
- `completed`
- `failed`
- `blocked`

---

## 12. Reingeniería del cookbook actual

## 12.1 Qué está mal hoy

El cookbook actual del programmer está orientado a “planes que salieron bien”.
Eso es insuficiente porque mezcla:

- conocimiento de steps
- conocimiento de entorno
- conocimiento de artefactos
- conocimiento de reparación

## 12.2 Nueva política

Separar almacenamiento por layer.

### Ejemplo de layout

```text
cookbook/
  design-v1.json
  artifact-v1.json
  plan-compile-v1.json
  repair-v1.json
```

## 12.3 Regla de retrieval

Cada actor recibe solo el cookbook de su capa y, como máximo, algunos ejemplos relevantes filtrados por:

- artifact kind
- runtime type
- failure class
- topology pattern

---

## 13. Reingeniería de prompts

## 13.1 Prompt del Designer

Debe pensar en:

- solución
- topología
- ownership
- constraints
- policy
- nodos y runtimes como estado deseado

No debe pensar en:

- calls admin exactas
- payload JSON exacto de publish/run_node
- help lookup

## 13.2 Prompt del Real Programmer

Debe pensar en:

- files, packages, definitions, configs
- compliance con constraints del packet
- artefacto mínimo suficiente
- no prose, salida estructurada

No debe pensar en:

- topología general
- orden de deploy final
- admin surface completo

## 13.3 Prompt del Plan Compiler

Debe pensar en:

- live state
- help lookup
- compile to `executor_plan`
- ordering rules
- no invention rule
- names exactos de admin

## 13.4 Prompt del Auditor

Debe pensar en:

- clasificación mínima
- findings concretos
- feedback corto y accionable
- stop criteria
- reusable/not reusable

## 13.5 Prompt del Executor

Se preserva estrecho y operacional:

- ejecutar solo steps declarados
- function exacta por acción admin
- sin reordenar
- sin inventar
- help como apoyo
- reportar eventos

---

## 14. Modelo de migración

## 14.1 Fase 0 — Congelación conceptual

Congelar oficialmente:

- `executor_plan`
- executor en `SY.admin`
- función 1:1 con admin
- visibilidad en chat

Esto pasa a ser base estable.

## 14.2 Fase 1 — Renombre y separación semántica

- documentar que el `fluxbee_programmer` actual pasa a rol `plan_compiler`
- sacar de docs/prompt toda idea de que ese componente “programa software”
- separar cookbook actual en `plan-compile`

## 14.3 Fase 2 — Crear `real_programmer`

Implementar:

- tool nuevo
- `build_task_packet`
- `build_artifact_bundle`
- `repair_packet`
- cookbook técnico

## 14.4 Fase 3 — Crear auditor

Implementar primero una versión compacta capaz de:

- auditar manifest
- auditar artifact bundles
- clasificar fallas
- decidir cookbook eligibility

## 14.5 Fase 4 — Crear manifest compiler explícito

El compiler debe:

- leer manifest
- detectar faltantes
- pedir artefactos
- compilar a plan
- dejar de depender de texto libre del host

## 14.6 Fase 5 — Integración end-to-end

Pipeline completo:

- manifest design
- design audit
- artifact build
- artifact audit
- plan compile
- execute
- verify
- cookbook write

## 14.7 Fase 6 — Deprecaciones

Retirar:

- narrativa ambigua de “programmer” para el plan compiler
- cookbook único
- retries implícitos sin classification

---

## 15. Cambios concretos recomendados en código

## 15.1 En `SY.architect`

### Agregar

- `real_programmer_pending`
- `audit_pending`
- storage para `build_task_packets`, `artifact_bundles`, `audit_verdicts`, `repair_packets`
- state machine explícita por task
- cookbook por layer

### Refactorizar

- `programmer_pending` -> `plan_compiler_pending`
- traces diferenciadas por actor
- rendering de loops multi-actor

## 15.2 En `SY.admin`

### Mantener

- executor mode
- validate/execute plan
- function catalog 1:1

### Agregar opcionalmente

- mejores códigos de error para failure auditor
- redactions más finas para `execution_report`

## 15.3 En persistencia

Crear storage explícito para:

- `solution_manifest` versions
- `build_task_packet`
- `build_artifact_bundle`
- `audit_verdict`
- `repair_packet`
- `executor_plan`
- `execution_report`
- cookbook entries por layer

---

## 16. Criterios de éxito

La reingeniería está bien implementada si se cumplen estas condiciones:

1. el `executor_plan` sigue funcionando y no se degrada
2. el viejo `programmer` deja de ser una caja semántica ambigua
3. existe un `real_programmer` separado con inputs/outputs propios
4. el auditor clasifica fallas y corta loops improductivos
5. el `solution_manifest` deja de saltar directo al executor sin capa intermedia
6. existe un compiler/planner explícito entre manifest y plan
7. cada cookbook aprende solo dentro de su capa
8. el operador puede ver en chat qué actor está trabajando, con qué artifact y por qué se reintenta

---

## 17. Orden recomendado de implementación

1. congelar y declarar canónica la frontera `executor_plan -> executor`
2. renombrar conceptualmente el programmer actual a `plan_compiler`
3. separar el cookbook actual en `plan-compile`
4. crear `real_programmer` con packet/output propios
5. crear auditor mínimo
6. crear `repair_packet` y taxonomía de fallas
7. crear `manifest compiler` explícito
8. conectar pipeline completo con trazas visibles
9. solo después, endurecer reconciler/versionado/rollback del `solution_manifest`

---

## 18. Bottom line

La mejor evolución de Fluxbee no es “seguir agregando capacidades al programmer actual”.
Tampoco es “tirar el piloto”.

La dirección correcta es:

- **preservar** el executor como núcleo firme,
- **reubicar** el programmer actual como compiler/planner,
- **crear** un real programmer para artefactos,
- **hacer obligatorio** al auditor,
- y **formalizar** el puente `solution_manifest -> build tasks -> artifacts -> executor_plan`.

Esa arquitectura evita quedarse a medias porque:

- no congela errores semánticos del piloto,
- no rompe la única frontera ya validada,
- y transforma lo aprendido en una plataforma más general y más rigurosa.
