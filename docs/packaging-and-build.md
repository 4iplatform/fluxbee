# Fluxbee — Packaging, Build & Install

**Status:** v1 (2026-07-22) · **Audience:** dev/ops who build the `.deb`, set up a backend or add nodes.

This document explains how Fluxbee is packaged, how any dev builds the `.deb` from
GitHub without thinking, what is left running after the install, and how to add a new IO/AI node to the
base set.

---

## 1. Packaging model (summary)

Fluxbee ships as **ONE single integrated Debian package** (`fluxbee_<ver>_amd64.deb`,
motherbee only — see [07-operaciones.md](history/07-operaciones.md) for the deploy model). The package carries:

- The **core** (`SY.*`): rt-gateway, sy-admin, sy-config-routes, sy-architect, sy-vault,
  sy-orchestrator, sy-storage, sy-identity, sy-cognition, sy-edge (Rust) +
  sy-opa-rules, sy-timer, sy-wf-rules, wf-generic (Go) + sy-frontdesk-gov. Each core
  binary travels in `/usr/bin` **and** in `/var/lib/fluxbee/dist/core/bin` (this copy is the one the
  dist-sync replicates to the spokes; its hashes are baked into `dist/core/manifest.json`).
- The **base IO/AI nodes**, defined declaratively in
  [`packaging/base-nodes.json`](../packaging/base-nodes.json) (the single source of truth).

There are **two layers**:

- **Layer 1 — installer baseline:** the `.deb` bakes in the core + the base set of nodes. A
  from-scratch install is **self-sufficient** (it needs no external fetch for the base nodes).
- **Layer 2 — growth:** new nodes or version bumps are published and deployed through the
  runtimes channel (`publish` + `POST /hives/{id}/update category=runtime` over the dist-sync),
  **with no new `.deb`** for whoever is already installed.

**There are no separate apt packages (core vs nodes)**: the runtimes channel already provides the
separate, versioned and hash-verified lifecycle; a second package would be redundant.

### Node classes

Each entry of `base-nodes.json` declares its class:

| Class | What it is | How it starts |
|-------|--------|--------------|
| **singleton** | (empty class today) motherbee-only infra node with a baked systemd unit — IO.blob/IO.cloud moved to managed runtime | — |
| **runtime** | instanced node, spawnable per tenant via `run_node` from `dist/runtimes/<runtime>/<ver>` | if `boot: true`, `fluxbee-firstboot` auto-spawns a default instance in the root tenant (only io.cloud and io.blob, see §2); if `false`, it stays baked in and spawnable on demand |

---

## 2. Current base set

Defined in `packaging/base-nodes.json`:

| Node | Class | At boot |
|------|-------|---------|
| IO.blob | runtime (boot=true) | running |
| IO.cloud | runtime (boot=true, role: motherbee) | running (degraded if there is no Fluxbee Cloud — the Cloud is another repo) |
| io.api | runtime | baked in, NOT at boot: each API (`IO.api.<label>`) is launched by the tenant that needs it, with its tenant |
| io.slack | runtime | baked in, NOT at boot: each Slack binding is launched by the tenant that needs it, with its tenant |
| io.wapp | runtime | baked in, NOT at boot: each WhatsApp number (`IO.wapp.<label>`) is launched by its tenant |
| io.web | runtime (core IO) | baked in, NOT at boot: the operator starts `IO.web@motherbee` once, in the root tenant ([io-web.md](io/io-web.md)) |
| ai.generic | runtime | baked in, NOT at boot: the `AI.*` instances are created with `run_node` when they are needed |
| wf.engine | runtime | baked in, NOT at boot — the WF.* nodes are spawned from a **workflow package** that runs on this runtime, not by `run_node` on the bare runtime (that gives `WF_RUNTIME_PACKAGE_REQUIRED`) |
| io.linkedhelper | runtime | baked in, NOT at boot (a tenant launches it on demand) |

Only IO.cloud and IO.blob start with the install, in the root tenant
`tnt:00000000-0000-0000-0000-000000000001` (operator decision 2026-10-02, A-43).
They start **running but degraded** until the operator configures them. IO.web, the third IO node
of the root tenant, starts only when the operator launches it there with `run_node`
([io-web-v2](io-web-v2.md) §11.2). The other IO runtimes
stay baked in and spawnable: a tenant launches them (or an operator, for that tenant) with `run_node`.
In the root tenant (the genesis one) only the core is born: the SY.\*, the RT.\* and the core IO nodes (io.cloud,
io.blob, io.web). The orchestrator defines this and rejects any other node there (IO, AI or WF) with
`TENANT_ROOT_NOT_ALLOWED` (operator decision 2026-10-14, `docs/ilk-tenant-model-v1.md` D10).

---

## 3. Add a new IO/AI node to the install

**It is a one-line edit** in `packaging/base-nodes.json`:

```json
{ "runtime": "io.foo", "crate": "io-foo", "bin": "io-foo", "workspace": "nodes/io", "boot": false }
```

Requirements:
1. The crate exists (e.g. `nodes/io/io-foo`, a member of the `nodes/io` workspace).
2. Add the entry to the manifest (above). `boot: false` = baked in and spawnable: a
   tenant launches it with `run_node`. `boot: true` (with `instance`) makes `fluxbee-firstboot` start a
   default instance in the root tenant; only io.cloud and io.blob use it, because the orchestrator
   rejects in the root tenant everything that is not core (`TENANT_ROOT_NOT_ALLOWED`, A-43 and D10).
3. For a new **singleton** (rare; only 1-per-hive infra): add it to `singletons`, create its
   systemd unit, and add it to the allowlist `HIVE_YAML_NON_SY_LIFECYCLE_NODES` (empty today) in
   `crates/fluxbee_sdk/src/managed_node.rs` + to `system_nodes` of `packaging/hive.yaml.example`.

`build-deb.sh` compiles the crate and bakes/publishes it according to its class; `fluxbee-firstboot` spawns the
ones that have `boot: true`. **There is no need to touch the build script.**

> **Invariant (U-3): `dist/runtimes/manifest.json` is OPERATOR STATE, never package
> payload.** The `.deb` ships only the artifact tree (`dist/runtimes/<runtime>/<version>/`) and
> **registers it at install time** with `fluxbee-seed-runtimes`, which **merges** against the
> live manifest by calling the single merger (`scripts/publish-runtime.sh`), the one every
> runtime publish uses. Packaging it **replaced** it on every upgrade and left
> every hot-published runtime orphaned. `build-deb.sh` has a post-build guard that
> rejects the package if the manifest sneaks back into the payload.
>
> `packaging/deb-preinst` takes a copy of the live manifest **before** the unpack. It is not optional:
> once it is no longer payload, dpkg treats it as an *obsolete file* and **deletes** it on the first upgrade
> to the fixed package; and only the `preinst` of the NEW package runs in time, because the scripts
> of the one already installed cannot be changed retroactively.

A node that is NOT in the base set can still be added to an already installed backend through the
runtimes channel (`scripts/publish-runtime.sh` + `POST /hives/{id}/update category=runtime`), with no
new `.deb`.

---

## 4. Build box — build the `.deb` (any dev, without thinking)

Fluxbee separates **BUILD** (on a machine with a toolchain) from **INSTALL** (the `.deb` on the target).

### 4.1 Build box prerequisites (once)

Ubuntu 24.04 with:

- `git`
- Rust toolchain (stable `rustup`) — `cargo`
- Go (for sy-opa-rules/sy-timer/sy-wf-rules/wf-generic)
- `protobuf-compiler` (`protoc`)
- `python3`, `dpkg-deb` (they come with Ubuntu)

```bash
sudo apt-get update
sudo apt-get install -y git build-essential protobuf-compiler golang python3 apt-utils
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
```

### 4.2 Build the `.deb`

```bash
git clone git@github.com:4iplatform/fluxbee.git ~/fluxbee   # (or https)
~/fluxbee/scripts/make-deb.sh --branch main --version 0.1.0
```

`scripts/make-deb.sh` clones or updates the repo, checks the toolchain, runs
`packaging/build-deb.sh`, and leaves the package in `dist/fluxbee_<ver>_amd64.deb`. A new dev only
needs access to GitHub and the toolchain — nothing else.

> Reference box: the **fb-build** VM (VM 110 on the PROD Proxmox; the dev cluster has
> another one, VM 210 on `PC-004-165`, §4.4) already has the
> toolchain and `/opt/fluxbee`. `scripts/make-deb.sh` reproduces that setup on any machine.

### 4.3 Internal apt repo (install over the network, without copying the `.deb`) — recommended

Instead of copying the `.deb` to each box, make the build machine an **internal apt repo**: it serves the
`.deb` over HTTP and any box on the network runs `apt install fluxbee`. apt resolves `postgresql` (and
the other Depends) from the Ubuntu archive automatically — the clones stay as bare Ubuntu.

On the build+repo box (after `make-deb.sh`):

```bash
scripts/apt-repo-publish.sh --serve          # publishes the .deb in a flat repo + serves it on :8900
```

`apt-repo-publish.sh` builds a flat repo (`apt-ftparchive packages` + `apt-ftparchive release`) and serves it.
It is **unsigned** + `[trusted=yes]` (internal use). For a **public/internet** repo, sign the
`Release` with GPG (`InRelease`) and remove `[trusted=yes]` — the `.deb` itself does not change. Run
the script again after each new build to regenerate the index.

> **Pin the repo box IP — OUTSIDE the DHCP pool.** The repo URL (`http://<host>:8900`) is
> written into every client; if the build box is on DHCP and its IP drifts, every `apt update`
> fails with *no route to host*. Give it a static IP (netplan `dhcp4:false` + `addresses`, and
> `network:{config:disabled}` in `/etc/cloud/cloud.cfg.d/` if it is cloud-init) **above the router's DHCP
> range** (otherwise the router can reassign that IP and it collides). In the lab: DHCP pool
> `192.168.4.20–.150`, repo pinned at `192.168.4.200`.

On any client (clean Ubuntu):

```bash
echo 'deb [trusted=yes] http://<build-host>:8900 ./' | sudo tee /etc/apt/sources.list.d/fluxbee.list
sudo apt-get update && sudo apt-get install -y fluxbee
sudo nano /etc/fluxbee/hive.yaml && sudo fluxbee-firstboot
```

### 4.4 Dev cluster layout (Proxmox lab `PC-004-165/157/156`)

| Server | Role | What runs |
|--------|-----|-----------|
| PC-004-165 | build+repo + dev | fb-build (toolchain + `make-deb` + apt repo `:8900`) + disposable test VMs |
| PC-004-156 | stable | fluxbee backend installed from the repo, kept between majors |
| PC-004-157 | dev/spare | disposable VMs |

> A **dedicated build box on 157** only needs a **GitHub deploy key** to clone the
> private repo (the Ubuntu cloud image does not ship `qemu-guest-agent`, so the lab VMs are created by
> cloning the base template already provided and migrating it between nodes). Follow-up when there is a key.

---

## 5. Install the backend (motherbee)

On the motherbee (a clean Linux box; see [07-operaciones.md](history/07-operaciones.md) §2):

```bash
sudo apt-get install ./fluxbee_0.1.0_amd64.deb   # brings PostgreSQL (hard Depends)
sudo nano /etc/fluxbee/hive.yaml                 # copied from hive.yaml.example; edit hive_id/wan
sudo fluxbee-firstboot
```

On a development host, `scripts/install.sh [<version>]` replaces the first line: it builds the
package from the local checkout with `packaging/build-deb.sh` and installs it with
`apt-get install --reinstall`, so a rebuild at the installed version goes in too.

`fluxbee-firstboot` (idempotent): boots PostgreSQL + creates the role/DBs, starts the orchestrator,
does the `vault_put` of the postgres secret (the **DB connection is resolved on its own in the
vault**), reconnects the consumers and auto-spawns in the root tenant the boot managed runtimes
(IO.blob/IO.cloud). When it finishes it prints the **next steps**.

After the firstboot these are left **running**: the `SY.*` core + IO.blob + IO.cloud, **degraded**
until they are configured. (io.api, io.wapp, io.slack, io.linkedhelper, io.web, `ai.generic` and
`wf.engine` stay **baked in but NOT at boot** — `boot:false` in `base-nodes.json`; their instances
are created on demand: the IO channel ones from their tenant, IO.web by the operator in the root
tenant.)

A hive installed before 0.1.56 may have `IO.api@motherbee` and `IO.wapp.default@motherbee` in
the root tenant. The orchestrator no longer starts, restarts or relaunches them at boot
(`TENANT_ROOT_NOT_ALLOWED`); if they are still running, it does not kill them. Delete them
(`DELETE /hives/motherbee/nodes/<node>` with `{"purge_instance":true}`) and launch each node from its
tenant.

### 5.1 What the user provides (secrets in the vault)

Postgres is already resolved by the firstboot. The rest is up to the operator:

```bash
# AI provider key for the core (architect, admin, cognition and frontdesk use it). An AI.* node does not
# use it: it reads the general key of its own tenant (no fallback to the root since 0.1.66).
curl -sS -X POST http://127.0.0.1:8080/hives/motherbee/vault/secrets \
  -H 'content-type: application/json' \
  -d '{"key":"openai_root_pool","value":{"api_key":"sk-..."},"metadata":{"tenant_id":"tnt:00000000-0000-0000-0000-000000000001","resource_type":"openai"}}'
# (resource_type "anthropic" for an Anthropic key. CAREFUL: the default_provider is "openai";
#  if you load an Anthropic key, also set `ai.default_provider: anthropic` in
#  /etc/fluxbee/hive.yaml and restart the AI nodes, or they keep resolving the openai pool.)

# Slack tokens for an IO.slack binding (its tenant launches it): resource_type "slack", value {app_token, bot_token}.
```

**Architect (Archi):** `http://127.0.0.1:3000` on the motherbee · **Admin API:** `http://127.0.0.1:8080`.
Both listen only on loopback (they have no authentication, FINDINGS A-46). From another machine,
use an SSH tunnel that carries both ports (the Archi UI says to call the admin on 8080):
`ssh -L 3000:127.0.0.1:3000 -L 8080:127.0.0.1:8080 <user>@<motherbee>` and open `http://127.0.0.1:3000`.

`IO.cloud` runs even if no Fluxbee Cloud is configured (the Cloud lives in another repo); that is
the problem of whoever connects a Cloud, not of the backend.

---

## 6. References

- [`packaging/base-nodes.json`](../packaging/base-nodes.json) — the declarative base set.
- [`packaging/build-deb.sh`](../packaging/build-deb.sh) — build of the `.deb` (reads the manifest).
- [`packaging/fluxbee-firstboot`](../packaging/fluxbee-firstboot) — bootstrap + auto-spawn.
- [`packaging/fluxbee-seed-runtimes`](../packaging/fluxbee-seed-runtimes) — registers the base runtimes
  in the **live** manifest at install time (merge, not replacement). It is also the
  operator's repair surface: `sudo fluxbee-seed-runtimes`.
- [`packaging/deb-preinst`](../packaging/deb-preinst) — copies the live manifest before the unpack.
- [`packaging/deb-prerm`](../packaging/deb-prerm) / [`packaging/deb-postinst`](../packaging/deb-postinst)
  — on an upgrade the `prerm` stops only `sy-orchestrator`, `sy-admin` and `sy-wf-rules`; the rest of the
  core keeps serving with the old binaries until the boot of the new orchestrator restarts it
  in order (A-45). Details in [07-operaciones.md](history/07-operaciones.md) §6.1.
- [`scripts/make-deb.sh`](../scripts/make-deb.sh) — build entrypoint for devs.
- [07-operaciones.md](history/07-operaciones.md) — deploy and lifecycle (add_hive, update, roles).
- [14-runtime-rollout-motherbee.md](history/14-runtime-rollout-motherbee.md) — runtimes update channel.
