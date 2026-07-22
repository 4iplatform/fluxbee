# Fluxbee — Packaging, Build & Install

**Estado:** v1 (2026-07-22) · **Audiencia:** dev/ops que arman el `.deb`, montan un backend o agregan nodos.

Este documento explica cómo se empaqueta Fluxbee, cómo cualquier dev arma el `.deb` desde
GitHub sin pensar, qué queda andando al instalar, y cómo agregar un nodo IO/AI nuevo al
set base.

---

## 1. Modelo de packaging (resumen)

Fluxbee se distribuye como **UN solo paquete Debian integrado** (`fluxbee_<ver>_amd64.deb`,
solo motherbee — ver [07-operaciones.md](07-operaciones.md) para el modelo de deploy). El paquete trae:

- El **core** (`SY.*`): rt-gateway, sy-admin, sy-config-routes, sy-architect, sy-vault,
  sy-orchestrator, sy-storage, sy-identity, sy-cognition, sy-policy, sy-edge (Rust) +
  sy-opa-rules, sy-timer, sy-wf-rules, wf-generic (Go) + sy-frontdesk-gov. Cada binario del
  core viaja en `/usr/bin` **y** en `/var/lib/fluxbee/dist/core/bin` (esta copia es la que el
  dist-sync replica a los spokes; sus hashes se hornean en `dist/core/manifest.json`).
- Los **nodos base IO/AI**, definidos declarativamente en
  [`packaging/base-nodes.json`](../packaging/base-nodes.json) (la fuente única de verdad).

Hay **dos capas**:

- **Capa 1 — baseline del instalador:** el `.deb` hornea el core + el set base de nodos. Un
  install de cero es **autosuficiente** (no necesita fetch externo para los nodos base).
- **Capa 2 — crecimiento:** nodos nuevos o bumps de versión se publican y se despliegan por el
  canal de runtimes (`publish` + `POST /hives/{id}/update category=runtime` sobre el dist-sync),
  **sin `.deb` nuevo** para quien ya está instalado.

**No hay paquetes apt separados (core vs nodos)**: el canal de runtimes ya provee el ciclo de
vida separado, versionado y hash-verificado; un segundo paquete sería redundante.

### Clases de nodo

Cada entrada de `base-nodes.json` declara su clase:

| Clase | Qué es | Cómo arranca |
|-------|--------|--------------|
| **singleton** | nodo de infra motherbee-only (1 por hive), ej. `IO.blob`, `IO.cloud` | unit systemd horneada; corre al boot (`role_gate` restringe al rol) |
| **runtime** | nodo instanciado, spawneable por-tenant vía `run_node` desde `dist/runtimes/<runtime>/<ver>` | si `boot: true`, `fluxbee-firstboot` auto-spawnea una instancia default al boot; si `false`, queda horneado y spawneable a demanda |

---

## 2. Set base actual

Definido en `packaging/base-nodes.json`:

| Nodo | Clase | Al boot |
|------|-------|---------|
| IO.blob | singleton | corriendo |
| IO.cloud | singleton (role: motherbee) | corriendo (degradado si no hay Fluxbee Cloud — la Cloud es otro repo) |
| io.api | runtime | instancia default `IO.api@motherbee` corriendo + spawnable |
| io.slack | runtime | instancia default `IO.slack@motherbee` corriendo + spawnable |
| ai.generic | runtime | instancia default `AI.chat@motherbee` corriendo + spawnable |
| wf.engine | runtime | horneado, NO al boot — los nodos WF.* se spawnean desde un **workflow package** que corre sobre este runtime, no por `run_node` sobre el runtime pelado (da `WF_RUNTIME_PACKAGE_REQUIRED`) |
| io.linkedhelper | runtime | horneado, NO al boot (spawnable a demanda) |

Los nodos base arrancan **corriendo pero degradados** hasta que el operador cargue su
secreto/config en `SY.vault` — es deliberado: un backend recién instalado tiene los nodos
básicos vivos "out of the box".

---

## 3. Agregar un nodo IO/AI nuevo al install

**Es una edición de una línea** en `packaging/base-nodes.json`:

```json
{ "runtime": "io.foo", "crate": "io-foo", "bin": "io-foo", "workspace": "nodes/io", "boot": true, "instance": "IO.foo@motherbee" }
```

Requisitos:
1. El crate existe (ej. `nodes/io/io-foo`, miembro del workspace `nodes/io`).
2. Agregar la entrada al manifest (arriba). `boot: true` = arranca una instancia default al
   boot; `false` = solo horneado/spawnable.
3. Para un **singleton** nuevo (raro; solo infra 1-por-hive): agregarlo a `singletons`, crear su
   unit systemd, y sumarlo al allowlist `MOTHERBEE_PACKAGED_NON_SYSTEM_NODES` en
   `src/bin/sy_orchestrator.rs` + a `system_nodes` de `packaging/hive.yaml.example`.

`build-deb.sh` compila el crate, lo hornea/publica según su clase, e instala el manifest al
target; `fluxbee-firstboot` lo arranca/spawnea. **No hay que tocar el script de build.**

Un nodo que NO esté en el set base igual se puede sumar a un backend ya instalado por el canal
de runtimes (`scripts/publish-runtime.sh` + `POST /hives/{id}/update category=runtime`), sin
`.deb` nuevo.

---

## 4. Build box — armar el `.deb` (cualquier dev, sin pensar)

Fluxbee separa **BUILD** (en una máquina con toolchain) de **INSTALL** (el `.deb` en el target).

### 4.1 Prerequisitos del build box (una vez)

Ubuntu 24.04 con:

- `git`
- Rust toolchain (`rustup` estable) — `cargo`
- Go (para sy-opa-rules/sy-timer/sy-wf-rules/wf-generic)
- `protobuf-compiler` (`protoc`)
- `python3`, `dpkg-deb` (vienen con Ubuntu)

```bash
sudo apt-get update
sudo apt-get install -y git build-essential protobuf-compiler golang python3 dpkg-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
```

### 4.2 Armar el `.deb`

```bash
git clone git@github.com:4iplatform/fluxbee.git ~/fluxbee   # (o https)
~/fluxbee/scripts/make-deb.sh --branch main --version 0.1.0
```

`scripts/make-deb.sh` clona-o-actualiza el repo, verifica el toolchain, corre
`packaging/build-deb.sh`, y deja el paquete en `dist/fluxbee_<ver>_amd64.deb`. Un dev nuevo solo
necesita acceso a GitHub y el toolchain — nada más.

> Caja de referencia en el lab: la VM **fb-build** (Proxmox `PC-004-165`, VM 210) ya tiene el
> toolchain y `/opt/fluxbee`. `scripts/make-deb.sh` reproduce ese setup en cualquier máquina.

### 4.3 Repo apt interno (instalar por red, sin copiar el `.deb`) — recomendado

En vez de copiar el `.deb` a cada box, dejá la máquina de build como **repo apt interno**: sirve el
`.deb` por HTTP y cualquier box de la red hace `apt install fluxbee`. apt resuelve `postgresql` (y
demás Depends) del archive de Ubuntu automáticamente — los clones quedan como Ubuntu pelado.

En el build+repo box (tras `make-deb.sh`):

```bash
scripts/apt-repo-publish.sh --serve          # publica el .deb en un repo flat + lo sirve en :8900
```

`apt-repo-publish.sh` arma un repo flat (`dpkg-scanpackages` + `apt-ftparchive release`) y lo sirve.
Es **sin firmar** + `[trusted=yes]` (uso interno). Para un repo **público/internet**, firmá el
`Release` con GPG (`InRelease`) y sacá `[trusted=yes]` — el `.deb` en sí no cambia. Volvé a correr
el script tras cada build nuevo para regenerar el índice.

> **Fijá la IP del repo box — FUERA del pool DHCP.** La URL del repo (`http://<host>:8900`) queda
> escrita en cada cliente; si el build box está por DHCP y su IP drifta, todos los `apt update`
> fallan con *no route to host*. Dale IP estática (netplan `dhcp4:false` + `addresses`, y
> `network:{config:disabled}` en `/etc/cloud/cloud.cfg.d/` si es cloud-init) **por encima del rango
> DHCP del router** (si no, el router puede reasignar esa IP y colisiona). En el lab: pool DHCP
> `192.168.4.20–.150`, repo fijado en `192.168.4.200`.

En cualquier cliente (Ubuntu limpio):

```bash
echo 'deb [trusted=yes] http://<build-host>:8900 ./' | sudo tee /etc/apt/sources.list.d/fluxbee.list
sudo apt-get update && sudo apt-get install -y fluxbee
sudo nano /etc/fluxbee/hive.yaml && sudo fluxbee-firstboot
```

### 4.4 Layout de 3 servers (lab Proxmox `PC-004-165/157/156`)

| Server | Rol | Qué corre |
|--------|-----|-----------|
| PC-004-165 | build+repo + dev | fb-build (toolchain + `make-deb` + repo apt `:8900`) + VMs de prueba destruibles |
| PC-004-156 | stable | backend fluxbee instalado por el repo, mantenido entre majors |
| PC-004-157 | dev/spare | VMs destruibles |

> Un build box **dedicado en 157** solo requiere una **deploy key de GitHub** para clonar el repo
> privado (el cloud image de Ubuntu no trae `qemu-guest-agent`, así que las VMs del lab se crean
> clonando el template base ya provisto y migrándolo entre nodos). Follow-up cuando haya key.

---

## 5. Instalar el backend (motherbee)

En la motherbee (caja Linux limpia; ver [07-operaciones.md](07-operaciones.md) §2):

```bash
sudo apt-get install ./fluxbee_0.1.0_amd64.deb   # trae PostgreSQL (Depends duro)
sudo nano /etc/fluxbee/hive.yaml                 # copiado de hive.yaml.example; editar hive_id/wan
sudo fluxbee-firstboot
```

`fluxbee-firstboot` (idempotente): bootea PostgreSQL + crea rol/DBs, arranca el orchestrator,
hace el `vault_put` del secreto de postgres (la **conexión a la DB queda resuelta sola en el
vault**), reconecta los consumidores, arranca los singletons (IO.blob/IO.cloud), y **auto-spawnea
las instancias default de los boot-runtimes** (io.api/io.slack/ai.generic). Al terminar
imprime los **próximos pasos**.

Después del firstboot quedan **corriendo**: el core `SY.*` + IO.blob + IO.cloud +
`AI.chat@motherbee` + `IO.api@motherbee` + `IO.slack@motherbee` — varios
**degradados** hasta cargar sus secretos. (`wf.engine` queda **horneado pero NO al boot** —
`boot:false` en `base-nodes.json`; se spawnea a demanda desde un workflow package.)

### 5.1 Lo que pone el usuario (secretos en el vault)

Postgres ya está resuelto por el firstboot. Lo demás es del operador:

```bash
# Key de proveedor AI (para que AI.chat / architect / admin / cognition funcionen):
curl -sS -X POST http://127.0.0.1:8080/hives/motherbee/vault/secrets \
  -H 'content-type: application/json' \
  -d '{"key":"openai_root_pool","value":{"api_key":"sk-..."},"metadata":{"tenant_id":"tnt:00000000-0000-0000-0000-000000000001","resource_type":"openai"}}'
# (resource_type "anthropic" para una key de Anthropic. OJO: el default_provider es "openai";
#  si cargás una key de Anthropic, poné además `ai.default_provider: anthropic` en
#  /etc/fluxbee/hive.yaml y reiniciá los nodos AI, o siguen resolviendo el pool de openai.)

# Tokens de Slack para IO.slack: resource_type "slack", value {app_token, bot_token}.
```

**Architect (Archi):** `http://<motherbee>:3000` · **Admin API:** `http://127.0.0.1:8080`.

`IO.cloud` corre aunque no haya Fluxbee Cloud configurada (la Cloud vive en otro repo); es
problema de quien conecte una Cloud, no del backend.

---

## 6. Referencias

- [`packaging/base-nodes.json`](../packaging/base-nodes.json) — el set base declarativo.
- [`packaging/build-deb.sh`](../packaging/build-deb.sh) — build del `.deb` (lee el manifest).
- [`packaging/fluxbee-firstboot`](../packaging/fluxbee-firstboot) — bootstrap + auto-spawn.
- [`scripts/make-deb.sh`](../scripts/make-deb.sh) — entrypoint de build para devs.
- [07-operaciones.md](07-operaciones.md) — deploy y ciclo de vida (add_hive, update, roles).
- [14-runtime-rollout-motherbee.md](14-runtime-rollout-motherbee.md) — canal de update de runtimes.
