# Adapter self-update loop — validation runbook

Valida el **loop de update del adapter LinkedHelper** (incremento #1+#2): el cloud
computa `update_required`/`available` en cada `alive` y el adapter lo consume
—descarga, **verifica sha256**, hace swap atómico del binario, re-exec, y
finaliza o hace rollback—. Dos harness cubren dos niveles de riesgo:

| Harness | Qué valida | Requiere Proxmox |
|---|---|---|
| `scripts/test-adapter-update-loop.sh` | **Mecánica** end-to-end con el binario real (download→verify→swap→re-exec→finalize/rollback) | No |
| `scripts/test-adapter-update-loop-vm.sh` | **Clean-slate** desde un OS prístino + adapter como **servicio systemd** (residuo de install/update, servicio vivo a través del re-exec) | Sí |

Ambos usan `scripts/adapter-update-mock-cloud.py` (mock mínimo de los endpoints
del cloud) para aislar la mecánica del adapter del stack completo. La lógica real
del cloud (`resolveAdapterUpdate`, endpoint de artefacto) está cubierta por los
unit tests en `fluxbee_cloud` y el E2E vivo previo.

## 1. Harness local (mecánica) — validado

```bash
ADAPTER_CRATE=~/repos/fluxbee_cloud/adapters/linked-helper/adapter-rs \
  bash scripts/test-adapter-update-loop.sh
```

Construye v1 (versión actual) y v2 (`0.2.0`), y corre 3 escenarios:

- **GOOD** (`required` + sha correcta): el adapter sube v1→v2, `lastUpdate.result=success`, el binario `.prev` retenido se limpia tras finalizar.
- **BAD** (`required` + sha inválida): el adapter **rechaza** antes del swap, se queda en v1, `lastUpdate.result=failed`.
- **AVAIL** (oferta no obligatoria): el adapter **loguea** `update_available` y **no** aplica.

Estado: **7/7 asserts PASS** en macOS (2026-07-02). Como macOS es Unix, valida el
path real de swap + re-exec.

## 2. Harness de VM (clean-slate + systemd)

Prerrequisitos (ver memoria `proxmox-test-env`):

- `PVE_HOST` + `PVE_TOKEN` exportados.
- `VMID` = clon limpio de Ubuntu 24.04 en el pool `dev` con qemu-guest-agent.
- La VM con salida a internet (rustup + deps de crate; edition 2024 necesita Rust ≥ 1.85, más nuevo que el `cargo` de apt).

```bash
VMID=201 ADAPTER_CRATE=~/repos/fluxbee_cloud/adapters/linked-helper/adapter-rs \
  bash scripts/test-adapter-update-loop-vm.sh
```

Flujo: sube el fuente del crate + el mock → instala toolchain y compila v1/v2/**v2crash**
(v2 + `--features test-crash-on-boot`) en la VM → instala el adapter como servicio
systemd + el mock como unidad → **snapshot `update-loop-baseline`** → corre GOOD,
BAD y **CRASH-LOOP** desde ese baseline (rollback de snapshot entre escenarios):

- **GOOD**: sube v1→v2, `lastUpdate.result=success`, servicio `active` a través del restart.
- **BAD**: sha inválida rechazada pre-swap, se queda en v1, `result=failed`.
- **CRASH-LOOP**: v2 con sha válida que paniquea al arrancar → verifica + swap → el
  **boot-gate** (máx 3 arranques) restaura v1 → `result=rolled_back`, servicio `active` en v1.

**Estado: validado en VM (2026-07-02) — 8/8 PASS** sobre un clon limpio de Ubuntu
24.04 (Proxmox VMID 201, pool `dev`). GOOD (3), BAD (2), CRASH-LOOP (3) — incluye
el **rollback supervisado real** (systemd reinicia el binario nuevo, el boot-gate
cuenta 3 crashes y revierte a v1, servicio vuelve `active`).

Notas operativas del harness (aprendidas al correrlo):
- `pve.py push` solo acepta texto → el tarball del crate y el mock `.py` se
  transportan **base64** y se decodifican en la VM.
- El template mínimo no trae linker C y el exec del guest-agent no setea `$HOME` →
  el harness hace `apt install build-essential` y exporta `HOME`/`PATH` para cargo.
- Un snapshot de disco deja la VM **detenida** tras el rollback → el harness hace
  `start` + `wait-agent`.
- `/tmp` es tmpfs (se vacía al rebootear) → los binarios v1/v2/v2crash + el mock
  viven en **`/opt/lh-test`** (en disco) para sobrevivir el snapshot/rollback.
- Requiere un clon **fresco** (sin snapshot `update-loop-baseline` previo, que
  colisionaría al re-crearlo). Los `/.cargo/env: No such file` en el log son ruido
  inofensivo del `.profile`.

## 2b. Instalación como servicio (Linux) — packaging

En `fluxbee_cloud/adapters/linked-helper/packaging/`:
- `install-linkedhelper-adapter.sh --cloud <url> --token <tok> [--binary … --partitions-root … --interval …]` — usuario de sistema `fluxbee-lh`, binario en `/opt/fluxbee/lh-adapter` (**escribible por el servicio** para el self-update), estado en `/var/lib/fluxbee/lh-adapter`, unidad systemd (`Restart=always`), enroll idempotente, `enable --now`.
- `uninstall-linkedhelper-adapter.sh [--purge]` — quita servicio+binario; `--purge` borra estado+usuario (clean slate).
- `fluxbee-lh-adapter.service` — unidad de referencia (la efectiva la genera el installer).

La mecánica OS-específica (swap + restart) vive detrás del **seam de plataforma**
(`adapter-rs/src/platform/`): `unix.rs` (rename dance + `exec()`, o exit-bajo-supervisor)
y `windows.rs` (stub → fase 2). El spike que valida Windows: `packaging/windows-spike.md`.

## 2c. Onboarding real (download-from-UI) + test

Flujo real de alta (solo con el **token**, el tenant queda pegado al token del lado
servidor): la UI emite el token ("Generate installation token") y muestra un
one-liner `curl -fsSL <cloud>/api/adapters/linkedhelper/install.sh | sudo bash -s -- --cloud <cloud> --token <tok>`.
El instalador baja el binario de `GET /api/adapters/linkedhelper/download` (gateado
por el **token de enrollment**, sin consumirlo) y hace `enroll`. Rutas cloud nuevas:
`/download` (token-gated, sirve el `latest` del manifest por plataforma) y
`/install.sh` (sirve el script de packaging, vía env `FLUXBEE_LH_ADAPTER_INSTALL_SCRIPT`).

**Test contra el cloud REAL** (`fluxbee/scripts/test-adapter-onboarding-docker.sh`):
compila un binario linux-x64 en un contenedor `rust`, lo stagea + manifest en el
release dir del cloud, y corre un contenedor **Ubuntu limpio** que baja el binario
con el token y hace enroll contra el cloud local (`host.docker.internal:3002`).
Asserts: `adapterId` emitido por el cloud + `tenantId` resuelto del token + un ciclo
`alive` OK. Usa `--no-service` (systemd no corre en un contenedor plano; el service
está cubierto por el harness de VM). Prerrequisitos: cloud arrancado con
`FLUXBEE_LH_ADAPTER_RELEASES_PATH` + `FLUXBEE_LH_ADAPTER_INSTALL_SCRIPT`, y un token
emitido desde la UI. La VM de Proxmox **no** sirve para esto (no rutea al cloud de
la Mac); por eso el test real usa Docker local.

## 3. Formato del manifest de releases (cloud real)

`FLUXBEE_LH_ADAPTER_RELEASES_PATH` apunta a un JSON. Los artefactos referenciados
por `artifact` viven **junto** al manifest y se sirven autenticados en
`/api/adapters/:adapterId/artifacts/:releaseId`.

```json
{
  "channels": {
    "stable": {
      "linux-x64": {
        "latestVersion": "0.2.0",
        "minSupportedVersion": "0.1.5",
        "releaseId": "lh-adapter-0.2.0-linux-x64",
        "artifact": "lh-adapter-0.2.0-linux-x64",
        "sha256": "<hex sha256 del artefacto>",
        "size": 12345678,
        "sig": null
      }
    }
  }
}
```

Semántica: `available` si la versión reportada < `latestVersion`; `required` si <
`minSupportedVersion`. `artifact` debe ser un nombre de archivo plano (guard
anti path-traversal). Generar el sha con `shasum -a 256 <archivo>` (o `sha256sum`).

## 4. Alcance y límites conocidos

- **Firma de artefactos:** la verificación de firma (`target.sig`) es un *seam* cableado; hoy `sha256` es **obligatorio** y la firma se loguea pero no se enforce. La clave/CI de firma es fast-follow — no shippear self-update sin sha256.
- **Rollback de crash-on-launch (fase 1, hecho):** el rechazo *pre-swap* (sha/size inválidos) siempre estuvo cubierto. Ahora, **bajo un supervisor** (systemd; detectado por `INVOCATION_ID`), un binario nuevo que arranca-pero-crashea se resuelve con el **boot-gate**: cada arranque con `pendingUpdate` incrementa `bootAttempts`; tras 3 → restaura `.prev` (`lastUpdate=rolled_back`) y reinicia en el binario viejo. Backoff persistente evita re-intentar el mismo `releaseId`. **Límite residual:** un binario que crashea *antes* de que corra el boot-gate (p.ej. segfault al cargar) sigue sin auto-rollback — sin supervisor no hay reintento. En modo no-supervisado (`exec()` in-place) tampoco hay boot-gate (no hay quién reinicie).
- **OS:** swap + re-exec son **Unix-only** por ahora (systemd/launchd). Windows (servicio) y macOS (Mac real) son follow-on del track de instalación.
- **Versión autoritativa:** el adapter reporta `env!("CARGO_PKG_VERSION")` (no el campo de state), así la versión sigue siendo verdadera después de un self-update.

## 5. Contrato wire (alive → update)

```
adapter --alive--> cloud
  { adapterVersion, os, arch, ... }
cloud --resp--> adapter
  { ..., update: { available, required, reason?, target?: {
      releaseId, version, url, sha256, size, sig? } } }
adapter: si required && target.version != versión_actual && !fallado_antes:
  GET <cloud_base><url>  (Bearer adapter_secret)
  verify(size, sha256)   ->  swap atómico  ->  re-exec  ->  finalize
```
