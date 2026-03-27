# Gap de lifecycle: delete de nodo AI no limpia estado local persistido

## Resumen ejecutivo

Al eliminar/reinstalar `AI.chat@motherbee`, el runtime puede seguir arrancando con configuración dinámica persistida local antigua (`/var/lib/fluxbee/state/ai-nodes/AI.chat@motherbee.json`), aunque el spawn/config más reciente en core indique otros valores.

Esto causó que `runtime.immediate_memory` quedara desactivada en runtime efectivo (`null`/defaults), a pesar de que el config del API mostraba `enabled=true`.

## Contexto observado

- Nodo: `AI.chat@motherbee`
- Fecha de validación: `2026-03-26`
- Señales observadas:
  - `immediate memory store ready` en logs del runner.
  - No se creaban archivos en `.../immediate-memory/threads/*.json`.
  - El archivo local efectivo del runner mostraba:
    - `kind=openai_chat`
    - `runtime.immediate_memory=null`

Archivo clave:

- `/var/lib/fluxbee/state/ai-nodes/AI.chat@motherbee.json`

## Reproducción (caso típico)

1. Existe estado local previo del nodo en `state/ai-nodes`.
2. Se ejecuta delete/recreate desde API (incluyendo `force` en delete).
3. El runner arranca en bootstrap y carga config efectiva persistida local.
4. El estado nuevo esperado desde spawn/core no se refleja en runtime efectivo.

## Comportamiento actual vs esperado

Actual:

- `DELETE` de nodo no garantiza cleanup del estado local del runner.
- Reinstall puede heredar estado/config vieja.
- Cambios recientes de config pueden no aplicarse como esperado.

Esperado:

- Una operación de “delete + reinstall limpio” debe eliminar o invalidar el estado local asociado al nodo.
- El runtime efectivo tras reinstall debe reflejar el config actual de core/spawn.

## Impacto

- Config drift entre “lo que muestra API” y “lo que ejecuta el runner”.
- Falsos negativos en pruebas funcionales (ejemplo: immediate memory parecía no funcionar).
- Diagnóstico operativo más costoso por superposición de fuentes de verdad.

## Responsabilidad recomendada

Owner primario: Core/Orchestrator lifecycle.

Razonamiento:

- El core decide alta/baja/reinstall de instancias.
- El runner no debería inferir unilateralmente intención de “factory reset” de su estado.
- La semántica de delete forzado debe vivir en el plano de control.

## Propuesta para core

1. Definir semántica explícita de delete:
   - `DELETE /nodes/{name}` sin `force`: remove lógico/instancia, conservar estado local.
   - `DELETE /nodes/{name}` con `force:true`: cleanup completo de estado local asociado.
2. Cleanup mínimo requerido con `force:true`:
   - `${STATE_DIR}/ai-nodes/<node_name>.json`
   - `${STATE_DIR}/ai-nodes/<node_name_sanitized>/` (stores de runtime: `lancedb`, `immediate-memory`, etc.)
3. Exponer trazabilidad:
   - evento/log de cleanup realizado con paths afectados.
4. Alinear bootstrap precedence:
   - evitar priorización ciega de estado persistido obsoleto frente a configuración vigente del control plane.

## Mitigación operativa inmediata

Hasta que core implemente la semántica final:

1. Tras delete/reinstall de AI node, limpiar manualmente estado local:
   - `/var/lib/fluxbee/state/ai-nodes/<node>.json`
   - `/var/lib/fluxbee/state/ai-nodes/<node_sanitized>/`
2. Validar runtime efectivo local después de spawn:
   - `jq '.config.runtime.immediate_memory' /var/lib/fluxbee/state/ai-nodes/<node>.json`

## Criterios de aceptación para core

1. Con `force:true`, uninstall/reinstall no hereda config persistida vieja.
2. Tras reinstall, runtime efectivo local coincide con config vigente de core.
3. Caso de prueba: `runtime.immediate_memory.enabled=true` se refleja en archivo local efectivo y en comportamiento (hit/miss + persistencia).
