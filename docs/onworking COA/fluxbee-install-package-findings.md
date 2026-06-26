# Fluxbee — requisitos de instalación de cero (hallazgos)

_Anotado 2026-06-26 al provisionar motherbee en una VM Ubuntu 24.04 limpia (no
Docker). El lab Docker los enmascara porque la imagen **hornea** la salida de
`scripts/install.sh`. Estos son los gaps que un **paquete de instalación real**
debe cubrir para un "primer install desde cero"._

## 1. Target de glibc (binarios)
Los binarios compilados en Ubuntu 24.04 (glibc 2.39) **NO corren** en 20.04
(glibc 2.31): `sy-orchestrator: GLIBC_2.39 not found → exit 1`, crash-loop.
→ El paquete debe: declarar OS/glibc mínimo soportado, **o** compilar por-target,
**o** linkear estático (musl). Hoy no hay declaración ni binario estático.

## 2. `dist/core/{bin,manifest.json}` es REQUERIDO en boot (gap más grande)
El orchestrator aborta en boot si falta `/var/lib/fluxbee/dist/core/manifest.json`:
`Error: "core manifest missing at '.../dist/core/manifest.json' (run scripts/install.sh)"`.
Tener los binarios en `/usr/bin` **no alcanza**. El paquete debe producir:
- `/var/lib/fluxbee/dist/core/bin/` con los 14 binarios (`rt-gateway`, `sy-*`, `wf-generic`).
- `dist/core/manifest.json` = `{schema_version, components:{<svc>:{service,version,build_id,sha256,size}}}`
  con el sha256/size de **cada** binario (lo genera `scripts/install.sh`).

## 3. PostgreSQL + DB bootstrap
El orchestrator crash-loopea hasta que el secreto `storage_postgres_url` está en
el vault, y storage/identity necesitan postgres. El install debe: instalar
postgres, correr `scripts/fluxbee_db_bootstrap.sh` (crea DBs `fluxbee`,
`fluxbee_identity`, `fluxbee_storage`, rol `fluxbee`).

## 4. Orden de primer-boot: vault_put del secreto postgres (chicken-egg)
El orchestrator no estabiliza hasta tener el secreto postgres en el vault, **pero**
el vault necesita el admin/orchestrator arriba para recibirlo. El install debe:
arrancar el orchestrator (crash-loopea), **esperar** que el admin (8080) responda,
hacer `POST /hives/<hive>/vault/secrets` con el `postgres_url` (resource_type
`postgres`, tenant root), y recién ahí el stack (identity/storage) estabiliza.
→ Es un orden de arranque real que el paquete debe orquestar (no es un install
estático puro).

## 5. Seed de vendor syncthing + manifest
`blob/dist sync` (syncthing) necesita el binario en
`/var/lib/fluxbee/dist/vendor/syncthing/` + un `manifest.json` con su hash. Sin
eso, warnings de sync. (El lab lo siembra en worker-install.)

## 6. Config + secretos
`/etc/fluxbee/`: `hive.yaml` (con `wan.listen`/`admin.listen` publicables — el
template trae admin en `127.0.0.1`, hay que abrirlo + `uplinks: []`),
`sy-config-routes.yaml`, y **`vault.master.key`** (la master key del vault).

## 7. systemd: rt-gateway lo arranca el orchestrator
`rt-gateway` queda **disabled** en systemd (no arranca en boot); el **orchestrator**
lo levanta. El paquete debe instalar las 15 units pero solo `enable` el
orchestrator, y asegurar que éste pueda `systemctl start rt-gateway`.

## 8. Otros
- Requiere **root** (instalar a `/usr/bin`, units, postgres, dirs en `/var/lib/fluxbee`).
- Warning `syncthing service user missing; using root fallback` → el paquete
  debería crear el service user `fluxbee` o documentar el fallback a root.
- Dirs base: `/var/lib/fluxbee/{nodes,state,ssh,storage,dist/core/bin,dist/vendor}`.

## 9. Target EGRESS (provisión del host que motherbee bootstrapea)
_Anotado 2026-06-26 validando F5/F6/F16/F17 en VM con `add_hive role=egress`._
- **PostgreSQL NO es necesario en el host egress.** El rol `egress` corrió
  `add_hive` y quedó `connected` con postgres **inactive** (el egress corre
  orchestrator + rt-gateway + `SY.config-routes`; no `SY.storage`/`identity`/`vault`).
  El template genérico de provisión (`et-provision.sh`) instala+bootstrapea postgres
  por inercia, pero el **paquete egress puede omitirlo** → imagen/box más liviana.
- **Tras reboot/revert, postgresql quedó `inactive`** (no auto-enabled en el
  snapshot). Para roles que SÍ lo necesitan (motherbee/worker con storage), el
  install debe `systemctl enable` postgresql (no solo `start`).
- **`sudo` del usuario de bootstrap no es NOPASSWD de fábrica**: el `add_hive`
  lo configura en su bootstrap (sudoers con NOPASSWD para la lista de binarios,
  incl. `/bin/bash`). El host egress solo necesita: sshd con password auth + el
  usuario en `sudo`/`wheel` (con password). Lo demás lo hace `add_hive`.
- **Unit de boot del NAT** (`fluxbee-egress-nft.service`, F17): lo **auto-instala
  el orchestrator** en el host egress durante el reconcile (no es un artefacto del
  paquete motherbee). El paquete no necesita enviarlo; sí debe garantizar que el
  host egress tenga `nftables`/`nft` en PATH (el reconcile falla loud si falta).

## 10. Deploy de un binario actualizado a la malla (no es primer-install, pero relacionado)
- Para que un `add_hive` propague un binario nuevo hay que actualizar **a la vez**:
  `dist/core/bin/<svc>` **y** la entrada de ese `<svc>` en `dist/core/manifest.json`
  (sha256 + size + build_id). `add_hive` sincroniza desde `dist/core/bin` y verifica
  contra el manifest; actualizar `/usr/bin` solo (o el bin sin el manifest) hace que
  el remoto reciba un binario que no matchea el hash esperado.
- **glibc**: el binario debe compilarse para el target de las VMs (Ubuntu 24.04 =
  glibc 2.39). Cross-compilar desde macOS no sirve; se compiló en un container
  `ubuntu:24.04` (la imagen builder del lab) y se verificó `objdump -T | GLIBC_2.39`.

## Resumen para el paquete
Un instalador real = binarios **glibc-correctos** + `dist/core` (bin+manifest) +
config + units + postgres+DB + **secuencia de primer-boot** (arrancar → esperar
admin → vault_put postgres → estabiliza). Los puntos 2 y 4 son los que más
fácilmente se omiten y rompen el primer install.
