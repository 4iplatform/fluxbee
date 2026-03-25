# IO/IA Runtime Distribution on Orchestrator v2 (Current Plan)

Estado: vigente sobre plataforma v2  
Fecha: 2026-03-10

## 1. Alcance

Este documento cubre únicamente lo pendiente para nodos `IO.*` y `AI.*` en el modelo ya canónico de plataforma:
- distribución vía `dist/`;
- rollout por `SYSTEM_UPDATE`;
- ejecución por `SPAWN_NODE`.

No redefine orchestrator v2 ni control-plane general.

## 2. Supuesto de base (ya cerrado)

Se asume resuelto a nivel plataforma:
- contrato remoto de update: `SYSTEM_UPDATE` / `SYSTEM_UPDATE_RESPONSE`;
- API admin canónica: `POST /hives/{id}/update`;
- source of truth de software: `/var/lib/fluxbee/dist/...`.

Referencias:
- `docs/02-protocolo.md`
- `docs/07-operaciones.md`
- `docs/14-runtime-rollout-motherbee.md`
- `docs/onworking/sy_orchestrator_v2_tasks.md`

## 3. Qué falta para IO/IA

## 3.1 Publicación canónica de runtimes IO/IA

Hoy `scripts/install-io.sh` y `scripts/install-ia.sh` instalan localmente (`/usr/bin` + systemd), pero no publican runtimes versionados en `dist/runtimes`.

Falta:
- publicar binarios IO/IA en:
  - `/var/lib/fluxbee/dist/runtimes/<runtime>/<version>/bin/...`
- garantizar `bin/start.sh` por runtime-version;
- actualizar `dist/runtimes/manifest.json` de forma atómica.

## 3.2 Convención de runtime names para IO/IA

Definir naming estable para manifest/runtime key (ejemplos):
- `AI.chat`
- `IO.slack`
- `IO.sim`

Regla:
- el `runtime` usado en `SPAWN_NODE` debe mapear 1:1 con la key en `manifest.runtimes`.

## 3.3 Estrategia de configuración de runtime

Cerrar una sola estrategia (recomendado: externa, operator-managed):
- runtime trae binario + `start.sh`;
- configuración por instancia queda fuera del runtime (env/config gestionada por orchestrator/admin).

## 4. Contrato de runtime IO/IA en dist

## 4.1 Layout mínimo

```text
/var/lib/fluxbee/dist/runtimes/
  manifest.json
  AI.chat/
    0.1.0/
      bin/
        ai-node-runner
        start.sh
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

## 4.2 Manifest mínimo

```json
{
  "schema_version": 1,
  "version": 1741640000000,
  "updated_at": "2026-03-10T12:00:00Z",
  "runtimes": {
    "AI.chat": { "current": "0.1.0", "available": ["0.1.0"] },
    "IO.slack": { "current": "0.1.0", "available": ["0.1.0"] },
    "IO.sim": { "current": "0.1.0", "available": ["0.1.0"] }
  },
  "hash": "sha256:..."
}
```

## 4.3 Start script

Cada runtime-version debe incluir `bin/start.sh` ejecutable.

Debe:
- validar precondiciones mínimas;
- ejecutar el binario correspondiente del runtime;
- retornar `exit != 0` en error de arranque.

## 5. Pipeline operativo IO/IA (canónico)

1. Publicar runtime en motherbee (`dist/runtimes`).
2. Actualizar `dist/runtimes/manifest.json`.
3. Esperar/verificar convergencia `dist` al hive destino.
4. Ejecutar `POST /hives/{id}/update` (`category=runtime`, `manifest_version`, `manifest_hash`).
5. Ejecutar `POST /hives/{id}/nodes` (`SPAWN_NODE`) para levantar instancia.
6. Verificar estado con `GET /versions` y `GET /deployments`.

## 6. Implementación recomendada en este repo

## Fase A - Script de publish runtime

Implementado en este repo:
- `scripts/publish-runtime.sh`
- `scripts/publish-ia-runtime.sh`
- `scripts/publish-io-runtime.sh`

Responsabilidades:
- entradas: `runtime`, `version`, `binary_path`, `--set-current`;
- staging + validación;
- copia a `dist/runtimes/<runtime>/<version>/bin/`;
- generación/validación de `start.sh`;
- merge atómico de `dist/runtimes/manifest.json`.

## Fase B - Wrappers por dominio

Opcionalmente agregar:
- `scripts/publish-ia-runtime.sh`
- `scripts/publish-io-runtime.sh`

para encapsular convenciones de nombre/runtime y binarios por crate.

## Fase C - Validación E2E

Automatizar E2E de IO/IA sobre pipeline canónico:
- publish -> sync -> `SYSTEM_UPDATE` -> `SPAWN_NODE` -> `KILL_NODE`.

## 7. Checklist

- [ ] Script de publish canónico para runtimes IO/IA.
  - Estado: implementado (`scripts/publish-runtime.sh`, `scripts/publish-ia-runtime.sh`, `scripts/publish-io-runtime.sh`).
- [ ] Publicación real de `AI.chat`, `IO.slack`, `IO.sim` en `dist/runtimes`.
- [ ] `start.sh` presente y válido por runtime-version.
- [ ] Manifest runtime actualizado de forma atómica.
- [ ] Rollout por `POST /hives/{id}/update` validado.
- [ ] Spawn/Kill de IO/IA validado por API en worker.

## 8. Respuesta directa de dependencia

Para deploy de IO/IA, con el estado actual de plataforma:
- no dependemos de cerrar más cambios estructurales en orchestrator v2 para el flujo canónico;
- sí dependemos de completar cambios internos de IO/IA (publicación de runtime, manifest y start scripts).
