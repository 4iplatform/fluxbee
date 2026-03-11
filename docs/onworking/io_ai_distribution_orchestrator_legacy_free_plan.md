# Legacy-Free para IO/IA (condiciones y plan de implementación)

Estado: propuesta operativa  
Fecha: 2026-03-05

## 1. Objetivo

Definir qué tiene que cumplirse para declarar el flujo de nodos `IO.*` y `AI.*` como **legacy-free** en distribución/orquestación, y cómo implementarlo sin ambigüedad.

"Legacy-free" aquí significa:
- sin dependencia operativa de `RUNTIME_UPDATE`;
- sin dependencia operativa de rutas legacy (`/var/lib/fluxbee/runtimes`, `/var/lib/fluxbee/core`, `/var/lib/fluxbee/vendor`);
- control-plane canónico por `SYSTEM_UPDATE` + `POST /hives/{id}/update`.

## 2. Criterios de salida (Definition of Done)

Se considera legacy-free cuando se cumplan todos:

1. Protocolo canónico único
- `SYSTEM_UPDATE` / `SYSTEM_UPDATE_RESPONSE` es el único contrato activo de update.
- `RUNTIME_UPDATE` queda removido del runtime de orchestrator (no solo "obsoleto en docs").

2. Path canónico único
- Fuente de verdad de software: `/var/lib/fluxbee/dist/...`.
- Sin fallback de lectura/escritura a `/var/lib/fluxbee/runtimes`, `/core`, `/vendor`.

3. Admin API canónica
- `POST /hives/{id}/update` usado para rollout remoto.
- Código y tests sin rutas de update alternas legacy.

4. Tooling sin legacy
- Scripts E2E/diagnóstico no usan `ORCH_SEND_RUNTIME_UPDATE` ni esperan `RUNTIME_UPDATE_RESPONSE`.
- Install/publish scripts de IO/IA publican en `dist/runtimes` + `manifest` canónico.

5. Ejecución de nodos IO/IA por runtime canónico
- `SPAWN_NODE` resuelve runtime/version contra `dist/runtimes/manifest.json`.
- Start script requerido en `dist/runtimes/<runtime>/<version>/bin/start.sh`.

6. Evidencia E2E
- Update runtime/core/vendor por `SYSTEM_UPDATE` validado en al menos 1 worker.
- Spawn/kill IO/IA funcionando sin paths legacy.
- Rollback verificado sin fallback legacy.

## 3. Alcance específico IO/IA

Incluye:
- empaquetado/publicación de runtime de `io-slack`, `io-sim`, `ai-node-runner`;
- actualización de manifest de runtimes;
- rollout por hive con `SYSTEM_UPDATE`;
- ejecución remota vía orchestrator local del worker.

No incluye:
- cambios funcionales de negocio de IO/IA (mensajería, prompts, etc.);
- rediseño de payloads/protocolo de aplicación.

## 4. Contrato canónico de runtime para IO/IA

## 4.1 Layout esperado en dist

```text
/var/lib/fluxbee/dist/runtimes/
  manifest.json
  AI.chat/
    0.1.0/
      bin/
        ai-node-runner
        start.sh
      config/
        ai_chat.yaml (opcional, según estrategia)
  IO.slack/
    0.1.0/
      bin/
        io-slack
        start.sh
  IO.sim/
    0.1.0/
      bin/
        io-sim
        start.sh
```

## 4.2 `manifest.json` mínimo

```json
{
  "schema_version": 1,
  "version": 1741180000000,
  "updated_at": "2026-03-05T15:00:00Z",
  "runtimes": {
    "AI.chat": {
      "current": "0.1.0",
      "available": ["0.1.0"]
    },
    "IO.slack": {
      "current": "0.1.0",
      "available": ["0.1.0"]
    },
    "IO.sim": {
      "current": "0.1.0",
      "available": ["0.1.0"]
    }
  },
  "hash": "sha256:..."
}
```

## 4.3 Start script

Cada runtime-version debe tener `bin/start.sh` ejecutable, con contrato estable:
- lee variables de entorno estándar de Fluxbee;
- ejecuta el binario correcto con su config;
- `exit != 0` en error de precondiciones.

## 5. Estado actual vs target (resumen)

Estado actual (observado):
- existe `SYSTEM_UPDATE`, pero también compat de `RUNTIME_UPDATE`;
- existe prioridad de `dist`, pero con fallback legacy;
- scripts IO/IA (`install-io.sh`, `install-ia.sh`) instalan localmente en `/usr/bin` y units, no publican runtime canónico en `dist/runtimes`.

Target legacy-free:
- quitar compat/fallback;
- introducir publicación canónica IO/IA a `dist/runtimes`;
- usar `SYSTEM_UPDATE` para rollout por hive.

## 6. Plan de implementación

## Fase A - Publicación canónica de runtimes IO/IA

1. Crear script de publicación (nuevo)
- `scripts/publish-runtime.sh` (genérico) o `publish-ia.sh` + `publish-io.sh`.

2. Responsabilidades del script
- construir/copiar binarios a staging;
- escribir `bin/start.sh`;
- mover a `/var/lib/fluxbee/dist/runtimes/<runtime>/<version>/`;
- actualizar merge de `/var/lib/fluxbee/dist/runtimes/manifest.json`;
- recalcular `hash` y `version` monotónica;
- validar estructura final.

3. Política de activación
- opción `--set-current` para promover versión al `current`;
- sin `--set-current`, solo agrega a `available`.

## Fase B - Rollout por orchestrator

1. Trigger desde admin
- `POST /hives/{id}/update` con:
  - `category=runtime`
  - `manifest_version`
  - `manifest_hash`

2. Orchestrator worker
- valida manifest local dist;
- aplica update local;
- responde `SYSTEM_UPDATE_RESPONSE` con detalle.

3. Verificación
- `GET /versions?hive=<id>`
- `GET /deployments?hive=<id>`

## Fase C - Migración de ejecución de nodos

1. Mantener instalación local actual solo como modo dev/local
- `install-io.sh` / `install-ia.sh` quedan para entorno de desarrollo o nodo único.

2. Producción
- ejecución por `SPAWN_NODE` + runtime en dist.

3. Config de nodos
- definir estrategia única:
  - configs embebidas por versión de runtime, o
  - configs externas gestionadas por orchestrator (recomendado).

## Fase D - Corte de legacy

1. Código
- remover handlers/paths de `RUNTIME_UPDATE`;
- remover fallback a `/var/lib/fluxbee/runtimes|core|vendor`.

2. Scripts/tests
- eliminar variables y pruebas `ORCH_SEND_RUNTIME_UPDATE`;
- convertir E2E a `SYSTEM_UPDATE`.

3. Documentación
- actualizar docs v1.16 remanentes;
- declarar deprecación efectiva (fecha de corte).

## 7. Checklist operativo (ejecutable)

- [ ] Script de publish runtime implementado para IO/IA.
- [ ] Publicación de `AI.chat`, `IO.slack`, `IO.sim` en `dist/runtimes`.
- [ ] Manifest runtime canónico actualizado sin errores.
- [ ] Rollout de runtime por `POST /hives/{id}/update` validado.
- [ ] Spawn remoto de IO/IA usando start script en dist validado.
- [ ] Remoción de compat `RUNTIME_UPDATE` completada.
- [ ] Remoción de fallback legacy paths completada.
- [ ] E2E final (runtime/core/vendor + spawn/kill IO/IA) en verde.

## 8. Riesgos y mitigaciones

1. Riesgo: romper workers no migrados
- Mitigación: ventana de transición con feature flag de compat y fecha de corte.

2. Riesgo: publish inconsistente de manifest
- Mitigación: operación atómica (escribir `.next`, validar, renombrar).

3. Riesgo: runtime publicado sin start script válido
- Mitigación: validación estricta pre-publish.

4. Riesgo: divergencia docs/código
- Mitigación: PRs acoplados (código + doc + E2E obligatorio).

## 9. Recomendación práctica para este repo

Secuencia mínima recomendada:
1. Implementar script de publish IO/IA a `dist/runtimes`.
2. Usarlo en un entorno de prueba con `SYSTEM_UPDATE` runtime.
3. Migrar scripts E2E que hoy usan `RUNTIME_UPDATE`.
4. Remover compat/fallback legacy recién cuando los E2E estén estables.
