# Fluxbee Programmer Handbook

**Audiencia:** `fluxbee_programmer` agent (y cualquier agente que genere executor plans)  
**Estado:** borrador — se va completando a medida que se prueba  
**Fecha:** 2026-04-21

---

## 1. Modelo mental de Fluxbee

Fluxbee es un sistema de nodos distribuidos que se comunican por mensajes. Cada **hive** es un nodo host (una máquina o instancia) que corre uno o más **nodos de aplicación**. Los nodos reciben y envían mensajes usando nombres de destino, no IPs.

```
humano / cliente
      │
      ▼
  [nodo IO]  ←→  [nodo AI]  ←→  [nodo WF]
                                      │
                                  [nodo IO]
```

Un nodo no es un proceso cualquiera — es una instancia de un **runtime** publicado en el hive.

---

## 2. Runtimes y nodos — la relación clave

| Concepto | Qué es | Analogía |
|---|---|---|
| **Runtime** | Plantilla publicada: binario + config base | Imagen Docker |
| **Nodo** | Instancia en ejecución de un runtime | Container corriendo |

- Un runtime se **publica** una vez (o cuando se actualiza).
- Un nodo se **crea e inicia** a partir de un runtime publicado.
- Múltiples nodos pueden correr desde el mismo runtime con diferentes nombres e instancias.
- Si el runtime no está publicado y materializado en el hive, `run_node` falla.

**Para crear un nodo nuevo:**
1. Verificar que el runtime base existe y está materializado (`current_materialized: true`)
2. Llamar `run_node` con el `runtime` correcto explícito

---

## 3. Naming convention — obligatorio

Todos los nombres de nodo siguen el patrón: `TIPO.nombre@hive`

| Prefijo | Tipo de nodo | Ejemplos |
|---|---|---|
| `AI.` | Nodos de lenguaje / inteligencia | `AI.coa@motherbee`, `AI.chat@motherbee` |
| `WF.` | Nodos de workflow / orquestación | `WF.router@motherbee`, `WF.invoice@motherbee` |
| `IO.` | Nodos de integración I/O | `IO.slack.main@motherbee`, `IO.api.support@motherbee` |
| `SY.` | Nodos de sistema (infra interna) | `SY.admin@motherbee`, `SY.orchestrator@motherbee` |

**Reglas:**
- Nunca crear un `node_name` sin el prefijo de tipo. `coa@motherbee` es inválido.
- El nombre después del punto es libre pero debe ser descriptivo y en minúsculas con puntos o guiones.
- El `@hive` al final es obligatorio y debe coincidir con el hive destino.
- El prefijo del nodo debe coincidir con el prefijo del runtime base. Un nodo `AI.*` usa un runtime `AI.*`.

---

## 4. Runtimes disponibles por tipo de nodo

> **Siempre verificar con `query_hive(list_runtimes)`** — esta tabla puede estar desactualizada.

| Runtime | Tipo | Uso |
|---|---|---|
| `AI.common` | `full_runtime` | Base genérica para nodos AI nuevos sin lógica específica |
| `AI.chat` | `full_runtime` | Nodo AI con interfaz de chat — ya tiene instancia propia, no usar como base para otros nodos |
| `WF.engine` o `wf.engine` | `full_runtime` | Motor base para nodos workflow |
| `IO.api` | `full_runtime` | Nodo de integración HTTP API |
| `IO.slack` | `full_runtime` | Integración con Slack |

**Criterio de selección:**
- Para un nodo AI genérico nuevo → `AI.common`
- Para un nodo WF nuevo → `wf.engine` o un runtime `workflow` type que tenga `runtime_base`
- Para un nodo IO nuevo → el runtime que corresponda al sistema externo
- Si el usuario especifica un runtime → usarlo tal cual, verificar que existe

---

## 5. Ciclo de vida de un nodo — orden de operaciones

```
1. publish_runtime_package   ← solo si el runtime NO está materializado
2. run_node                  ← crea e inicia la instancia
3. (opcional) add_route      ← si hay que routear mensajes al/desde el nodo
```

- Si el runtime ya está publicado y `current_materialized: true`, saltar paso 1.
- Si el nodo ya está corriendo, saltar paso 2.
- Las rutas se agregan después de que el nodo existe.
- **Nunca** invertir este orden en el executor plan.

---

## 6. Args requeridos para `run_node`

Siempre incluir todos estos campos. Si falta uno, el executor lo rechaza.

```json
{
  "hive": "motherbee",
  "node_name": "AI.coa@motherbee",
  "runtime": "AI.common",
  "runtime_version": "current"
}
```

- `hive`: el hive destino, exactamente igual al `@hive` en `node_name`
- `node_name`: nombre completo con prefijo y `@hive`
- `runtime`: nombre del runtime base, **sin** versión ni `@hive` (e.g. `AI.common`, no `AI.common@motherbee`)
- `runtime_version`: usar `"current"` salvo que se requiera una versión específica

> Consultar `get_admin_action_help(run_node)` para ver la lista completa de campos opcionales.

---

## 7. Los args del executor plan son estáticos

El output de un step **no puede alimentar** los args del siguiente step en tiempo de ejecución. El plan es un JSON fijo generado de antemano.

**Incorrecto** — no hacer esto:
```
s1: list_runtimes   ← intento de "ver qué hay" en ejecución
s2: run_node con runtime derivado de s1  ← imposible, los args son fijos
```

**Correcto** — usar `query_hive` durante la generación del plan:
```
[generación] query_hive(list_runtimes) → determinar que AI.common existe
[plan]        s1: run_node(runtime="AI.common", ...)
```

Solo incluir en el executor plan acciones que **mutan estado**. Las lecturas van antes, durante la generación.

---

## 8. Cómo usar las tools disponibles durante la generación

El programmer tiene tres tools:

| Tool | Cuándo usar |
|---|---|
| `query_hive` | Consultar estado real del hive: runtimes disponibles, nodos corriendo, rutas existentes, etc. Siempre antes de generar steps que dependan de ese estado. |
| `get_admin_action_help` | Consultar el schema exacto de una acción antes de generar un step. Args requeridos, opcionales, tipos, ejemplos. Siempre antes de usar una acción que no se conoce de memoria. |
| `submit_executor_plan` | Entregar el plan final. Llamar exactamente una vez cuando el plan está completo y verificado. |

**Flujo recomendado para cualquier tarea:**
1. `query_hive(inventory)` — ver qué existe (nodos corriendo, para no recrear)
2. `query_hive(list_runtimes)` — ver qué runtimes están disponibles
3. `get_admin_action_help(accion)` — para cada acción que vayas a usar
4. `submit_executor_plan(...)` — con el plan completo

---

## 9. Casos comunes

### Crear un nodo AI nuevo

**Pre-check:** `query_hive(list_runtimes)` → confirmar `AI.common` existe y `current_materialized: true`

```json
{
  "steps": [
    {
      "id": "s1",
      "action": "run_node",
      "args": {
        "hive": "motherbee",
        "node_name": "AI.coa@motherbee",
        "runtime": "AI.common",
        "runtime_version": "current"
      }
    }
  ]
}
```

### Crear un nodo WF con runtime nuevo (no publicado)

**Pre-check:** `query_hive(list_runtimes)` → runtime no aparece o `current_materialized: false`

```json
{
  "steps": [
    { "id": "s1", "action": "publish_runtime_package", "args": { ... } },
    { "id": "s2", "action": "run_node", "args": { "hive": "...", "node_name": "WF.router@motherbee", "runtime": "wf.router", "runtime_version": "current" } }
  ]
}
```

### Agregar una ruta

Solo después de que el nodo destino existe y corre:

```json
{
  "id": "s3",
  "action": "add_route",
  "args": {
    "prefix": "AI.support.",
    "action": "FORWARD",
    "next_hop_hive": "worker-220"
  }
}
```

> Consultar `get_admin_action_help(add_route)` para campos exactos.

---

## 10. Lo que NO es responsabilidad del programmer

- Interpretar la intención del usuario — eso lo hace Archi antes de llamar al programmer.
- Verificar si el plan tiene sentido de negocio — eso es del operador humano (CONFIRM).
- Hacer rollback si algo falla — el executor maneja eso con `stop_on_error: true`.
- Decidir si publicar o no el runtime cuando el usuario no lo mencionó — si no está materializado, publicar; si está, saltar ese paso.

---

## Pendientes / gaps conocidos

- [ ] Campos opcionales de `run_node` (e.g. `tenant_id` para nodos multi-tenant)
- [ ] Cómo funciona `executor_fill` en los steps (campos que el executor puede completar automáticamente)
- [ ] Topologías multi-hive: cuándo un nodo vive en `worker-*` vs `motherbee`
- [ ] Convenciones de naming para nodos IO con tenant (ej. `IO.slack.T126@motherbee`)
- [ ] Cómo verificar que un nodo quedó realmente corriendo después de `run_node`
- [ ] Casos de nodos `SY.*` — cuándo se crean, si alguna vez
