# Blob: reconciliación de spec y plan operativo (v1.16)

Fecha: 2026-02-23
Estado: Acordado para ejecución

## 1) Decisiones cerradas

1. `SY.storage` no participa en blobs.
- Ownership de blobs: toolkit transversal + procesos productores/consumidores.
- `SY.storage` sigue limitado a NATS + PostgreSQL.

2. El blob es agnóstico del medio de almacenamiento.
- Para la capa blob, `NFS` y `Syncthing` son equivalentes: ambos terminan en un path local.
- El contrato sigue siendo `blob_ref` + filesystem local.

3. No habrá API de blobs por red como camino de producto.
- Se elimina el track de `POST/GET /blob` como fallback operativo.
- El transporte de mensajes sigue en router/NATS; el archivo se resuelve por filesystem sincronizado/compartido.

4. Syncthing queda como herramienta de sincronización externa.
- El orchestrator la instala, inicia, monitorea y reinicia si corresponde.
- El orchestrator no participa en transferencia de blobs ni en lógica de negocio de blobs.

5. Se debe expresar explícitamente en config:
- `blob.path`
- si Syncthing está activo o no.

## 2) Modelo de config acordado

```yaml
# hive.yaml
blob:
  enabled: true
  path: "/var/lib/fluxbee/blob"
  sync:
    enabled: false        # true => Syncthing activo
    tool: "syncthing"    # reservado para extensibilidad
    api_port: 8384
    data_dir: "/var/lib/fluxbee/syncthing"
```

Reglas:
- `sync.enabled=false`: operación sobre disco local/compartido (NFS/volumen infra).
- `sync.enabled=true`: Syncthing activo como capa de sync de archivos.

## 3) Contrato funcional (sin cambios conceptuales)

- Inline `<64KB` y `blob_ref` para payload grande (`docs/02-protocolo.md`).
- El router no inspecciona el payload.
- Flujo:
  1. Productor guarda blob en `blob.path`.
  2. Productor publica envelope con `blob_ref`.
  3. Consumidor recibe envelope y resuelve archivo local.
  4. Si aún no llegó la réplica (caso sync), aplica retry/backoff y termina en error canónico si expira.

Errores canónicos propuestos:
- `BLOB_NOT_FOUND`
- `BLOB_INVALID_ID`
- `BLOB_IO_ERROR`
- `BLOB_ACCESS_DENIED`
- `BLOB_TOO_LARGE`

## 4) Correcciones de documentación a aplicar

## 4.1 Blob y operación

1. `docs/onworking/blob_tasks.md`
- Reordenar: toolkit blob primero (agnóstico del medio).
- Eliminar track de API por red.
- Dejar sync como etapa posterior opcional (`sync.enabled=true`).

2. `docs/07-operaciones.md`
- Agregar bloque de `blob` en `hive.yaml` con `path` + `sync.enabled`.
- Documentar `data_dir` de Syncthing cuando está activo.

3. `docs/01-arquitectura.md`
- Agregar Syncthing como herramienta auxiliar opcional (no proceso SY).

## 4.2 Inconsistencias generales detectadas (estado 2026-02-23)

1. Usuario bootstrap SSH (`root` vs `administrator`):
- Estado: **resuelto** en documentación operativa (`docs/07-operaciones.md`).

2. SHM versión LSA (`LSA_VERSION=1` vs `2`):
- Estado: **resuelto** en `docs/03-shm.md`, alineado con `src/shm/mod.rs`.

3. Drift epoch/seqlock en SHM:
- Estado: **resuelto** en `docs/03-shm.md`; se documenta seqlock (`seq`) según implementación actual.

4. Paths legacy `json-router` vs `fluxbee`:
- Estado: **resuelto** en documentos de spec principales (paths migrados a `/etc/fluxbee`, `/var/lib/fluxbee`, `/var/run/fluxbee`).

Nota: quedan pendientes otras mejoras documentales de detalle (componentes, tablas y ejemplos), pero estas 4 inconsistencias ya no bloquean el plan blob.

## 5) Plan de trabajo acordado

## Fase A - Toolkit blob base (prioridad máxima)

Objetivo:
- implementar capacidad transversal de blobs sin depender de NFS ni Syncthing.

Tareas:
1. Definir `BlobRef` y validadores.
2. Implementar toolkit fs:
- `put` (atómico: `tmp + rename`)
- `get`
- `stat`
- `delete` (según política)
3. Dedupe por hash (`sha256`) + verificación de integridad.
4. Layout estable en `blob.path`.
5. Tests unitarios de integridad y concurrencia.

Salida:
- toolkit reutilizable por cualquier proceso.

## Fase B - Contrato operativo y resiliencia

Tareas:
1. Definir política inline vs `blob_ref`.
2. Definir retry/backoff de resolución en consumidor.
3. Definir errores canónicos y mapeo de fallas.
4. E2E productor/consumidor usando filesystem local.

Salida:
- comportamiento end-to-end determinista sin sync externo.

## Fase C - Diagnóstico y métricas

Tareas:
1. Métricas base: `blob_put_total`, `blob_get_total`, bytes, errores.
2. Diagnóstico E2E blob similar a suites NATS.
3. Documentación en `docs/onworking/diagnostics_tasks.md`.

Salida:
- visibilidad operativa completa para blob.

## Fase D - Sync opcional con Syncthing (última)

Tareas:
1. Integrar `sync.enabled` en config.
2. Orchestrator: install/start/health/restart de Syncthing.
3. Documentar prerequisitos de red y permisos.
4. E2E con sync activo, sin cambiar contrato blob.

Salida:
- mismo toolkit blob funcionando con sync activo o no.

## 6) Criterio de éxito final

- El sistema usa blobs grandes con `blob_ref` de forma estable.
- El código de blobs no cambia al alternar `NFS`/`Syncthing`.
- `SY.storage` no queda acoplado a blobs.
- No existe dependencia de API blob por red.
