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
| **singleton** | (clase vacia hoy) nodo de infra motherbee-only con unit systemd horneada — IO.blob/IO.cloud pasaron a runtime managed | — |
| **runtime** | nodo instanciado, spawneable por-tenant vía `run_node` desde `dist/runtimes/<runtime>/<ver>` | si `boot: true`, `fluxbee-firstboot` auto-spawnea una instancia default en el tenant raíz (solo io.cloud e io.blob, ver §2); si `false`, queda horneado y spawneable a demanda |

---

## 2. Set base actual

Definido en `packaging/base-nodes.json`:

| Nodo | Clase | Al boot |
|------|-------|---------|
| IO.blob | runtime (boot=true) | corriendo |
| IO.cloud | runtime (boot=true, role: motherbee) | corriendo (degradado si no hay Fluxbee Cloud — la Cloud es otro repo) |
| io.api | runtime | horneado, NO al boot: cada API (`IO.api.<label>`) la lanza el tenant que la necesita, con su tenant |
| io.slack | runtime | horneado, NO al boot: cada binding de Slack lo lanza el tenant que lo necesita, con su tenant |
| io.wapp | runtime | horneado, NO al boot: cada número de WhatsApp (`IO.wapp.<label>`) lo lanza su tenant |
| ai.generic | runtime | horneado, NO al boot: las instancias `AI.*` se crean con `run_node` cuando hacen falta |
| wf.engine | runtime | horneado, NO al boot — los nodos WF.* se spawnean desde un **workflow package** que corre sobre este runtime, no por `run_node` sobre el runtime pelado (da `WF_RUNTIME_PACKAGE_REQUIRED`) |
| io.linkedhelper | runtime | horneado, NO al boot (lo lanza un tenant a demanda) |

Solo IO.cloud e IO.blob arrancan con la instalación, y son los únicos nodos IO que corren en el
tenant raíz `tnt:00000000-0000-0000-0000-000000000001` (decisión del operador 2026-10-02, A-43).
Arrancan **corriendo pero degradados** hasta que el operador los configure. Los demás runtimes IO
quedan horneados y spawneables: los lanza un tenant (o un operador, para ese tenant) con `run_node`.
El orchestrator rechaza cualquier otro nodo IO en el tenant raíz con `TENANT_ROOT_NOT_ALLOWED`.

---

## 3. Agregar un nodo IO/AI nuevo al install

**Es una edición de una línea** en `packaging/base-nodes.json`:

```json
{ "runtime": "io.foo", "crate": "io-foo", "bin": "io-foo", "workspace": "nodes/io", "boot": false }
```

Requisitos:
1. El crate existe (ej. `nodes/io/io-foo`, miembro del workspace `nodes/io`).
2. Agregar la entrada al manifest (arriba). `boot: false` = horneado y spawneable: lo lanza un
   tenant con `run_node`. `boot: true` (con `instance`) hace que `fluxbee-firstboot` arranque una
   instancia default en el tenant raíz; solo lo usan io.cloud e io.blob, porque el orchestrator
   rechaza cualquier otro nodo IO en el tenant raíz (`TENANT_ROOT_NOT_ALLOWED`, A-43).
3. Para un **singleton** nuevo (raro; solo infra 1-por-hive): agregarlo a `singletons`, crear su
   unit systemd, y sumarlo al allowlist `HIVE_YAML_NON_SY_LIFECYCLE_NODES` (vacío hoy) en
   `crates/fluxbee_sdk/src/managed_node.rs` + a `system_nodes` de `packaging/hive.yaml.example`.

`build-deb.sh` compila el crate y lo hornea/publica según su clase; `fluxbee-firstboot` spawnea los
que tienen `boot: true`. **No hay que tocar el script de build.**

> **Invariante (U-3): `dist/runtimes/manifest.json` es ESTADO DEL OPERADOR, nunca payload del
> paquete.** El `.deb` envía solo el árbol de artefactos (`dist/runtimes/<runtime>/<versión>/`) y
> lo **registra en tiempo de instalación** con `fluxbee-seed-runtimes`, que **mergea** contra el
> manifest vivo llamando al único merger (`scripts/publish-runtime.sh`) — el mismo camino que
> `scripts/install.sh` ya usaba. Empaquetarlo lo **reemplazaba** en cada upgrade y dejaba
> huérfano todo runtime publicado en caliente. `build-deb.sh` tiene un guard post-build que
> rechaza el paquete si el manifest vuelve a colarse en el payload.
>
> `packaging/deb-preinst` saca una copia del manifest vivo **antes** del unpack. No es opcional:
> al dejar de ser payload, dpkg lo trata como *obsolete file* y lo **borra** en el primer upgrade
> al paquete arreglado; y solo el `preinst` del paquete NUEVO corre a tiempo, porque los scripts
> del que ya está instalado no se pueden cambiar retroactivamente.

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
sudo apt-get install -y git build-essential protobuf-compiler golang python3 apt-utils
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

> Caja de referencia: la VM **fb-build** (VM 110 en el Proxmox de PROD; el cluster de dev tiene
> otra, la VM 210 en `PC-004-165`, §4.4) ya tiene el
> toolchain y `/opt/fluxbee`. `scripts/make-deb.sh` reproduce ese setup en cualquier máquina.

### 4.3 Repo apt interno (instalar por red, sin copiar el `.deb`) — recomendado

En vez de copiar el `.deb` a cada box, dejá la máquina de build como **repo apt interno**: sirve el
`.deb` por HTTP y cualquier box de la red hace `apt install fluxbee`. apt resuelve `postgresql` (y
demás Depends) del archive de Ubuntu automáticamente — los clones quedan como Ubuntu pelado.

En el build+repo box (tras `make-deb.sh`):

```bash
scripts/apt-repo-publish.sh --serve          # publica el .deb en un repo flat + lo sirve en :8900
```

`apt-repo-publish.sh` arma un repo flat (`apt-ftparchive packages` + `apt-ftparchive release`) y lo sirve.
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

### 4.4 Layout del cluster de dev (lab Proxmox `PC-004-165/157/156`)

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
vault**), reconecta los consumidores y auto-spawnea en el tenant raíz los runtimes managed de boot
(IO.blob/IO.cloud). Al terminar imprime los **próximos pasos**.

Después del firstboot quedan **corriendo**: el core `SY.*` + IO.blob + IO.cloud, **degradados**
hasta configurarlos. (io.api, io.wapp, io.slack, io.linkedhelper, `ai.generic` y `wf.engine` quedan
**horneados pero NO al boot** — `boot:false` en `base-nodes.json`; sus instancias se crean a
demanda, y las IO desde su tenant.)

Un hive instalado antes de 0.1.56 puede tener `IO.api@motherbee` e `IO.wapp.default@motherbee` en
el tenant raíz. El orchestrator ya no los arranca, reinicia ni relanza al boot
(`TENANT_ROOT_NOT_ALLOWED`); si siguen corriendo, no los mata. Borralos
(`DELETE /hives/motherbee/nodes/<nodo>` con `{"purge_instance":true}`) y lanzá cada nodo desde su
tenant.

### 5.1 Lo que pone el usuario (secretos en el vault)

Postgres ya está resuelto por el firstboot. Lo demás es del operador:

```bash
# Key de proveedor AI (la usan architect, admin, cognition, frontdesk y todo nodo AI.*):
curl -sS -X POST http://127.0.0.1:8080/hives/motherbee/vault/secrets \
  -H 'content-type: application/json' \
  -d '{"key":"openai_root_pool","value":{"api_key":"sk-..."},"metadata":{"tenant_id":"tnt:00000000-0000-0000-0000-000000000001","resource_type":"openai"}}'
# (resource_type "anthropic" para una key de Anthropic. OJO: el default_provider es "openai";
#  si cargás una key de Anthropic, poné además `ai.default_provider: anthropic` en
#  /etc/fluxbee/hive.yaml y reiniciá los nodos AI, o siguen resolviendo el pool de openai.)

# Tokens de Slack para un binding IO.slack (lo lanza su tenant): resource_type "slack", value {app_token, bot_token}.
```

**Architect (Archi):** `http://127.0.0.1:3000` en el motherbee · **Admin API:** `http://127.0.0.1:8080`.
Los dos escuchan solo en loopback (no tienen autenticación, FINDINGS A-46). Desde otra máquina,
con un túnel SSH que lleve los dos puertos (la UI de Archi indica llamar al admin en el 8080):
`ssh -L 3000:127.0.0.1:3000 -L 8080:127.0.0.1:8080 <usuario>@<motherbee>` y abrir `http://127.0.0.1:3000`.

`IO.cloud` corre aunque no haya Fluxbee Cloud configurada (la Cloud vive en otro repo); es
problema de quien conecte una Cloud, no del backend.

---

## 6. Referencias

- [`packaging/base-nodes.json`](../packaging/base-nodes.json) — el set base declarativo.
- [`packaging/build-deb.sh`](../packaging/build-deb.sh) — build del `.deb` (lee el manifest).
- [`packaging/fluxbee-firstboot`](../packaging/fluxbee-firstboot) — bootstrap + auto-spawn.
- [`packaging/fluxbee-seed-runtimes`](../packaging/fluxbee-seed-runtimes) — registra los runtimes
  base en el manifest **vivo** en tiempo de instalación (merge, no reemplazo). También es la
  superficie de reparación del operador: `sudo fluxbee-seed-runtimes`.
- [`packaging/deb-preinst`](../packaging/deb-preinst) — copia el manifest vivo antes del unpack.
- [`packaging/deb-prerm`](../packaging/deb-prerm) / [`packaging/deb-postinst`](../packaging/deb-postinst)
  — en un upgrade el `prerm` para solo `sy-orchestrator`, `sy-admin` y `sy-wf-rules`; el resto del
  core sigue sirviendo con los binarios viejos hasta que el boot del orchestrator nuevo lo reinicia
  en orden (A-45). Detalle en [07-operaciones.md](07-operaciones.md) §6.1.
- [`scripts/make-deb.sh`](../scripts/make-deb.sh) — entrypoint de build para devs.
- [07-operaciones.md](07-operaciones.md) — deploy y ciclo de vida (add_hive, update, roles).
- [14-runtime-rollout-motherbee.md](14-runtime-rollout-motherbee.md) — canal de update de runtimes.
