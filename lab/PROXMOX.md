# Proxmox lab — how to use it (daily operation)

Operating guide for the VM lab. It complements (does not duplicate):

- [`docs/packaging-and-build.md`](../docs/packaging-and-build.md) — §3 **adding a node to the install**
  (one line in `base-nodes.json`), §4 build box / `.deb` / apt repo, §4.4 layout of the 3 servers.
- [`docs/history/14-runtime-rollout-motherbee.md`](../docs/history/14-runtime-rollout-motherbee.md) — **THE runbook**
  to publish/update/spawn a runtime on an ALREADY installed backend (publish → manifest →
  `/update` → `/nodes`). Do not invent alternative routes: it is this one.
- [`lab/logbook/METHOD.md`](logbook/METHOD.md) — **the working method for infra + PROD**
  (agent capabilities, rules, change classification, logbook protocol).
  **If the work touches production, that doc rules**; the day's logbook goes in `lab/logbook/YYYY-MM-DD.md`.

## Map (who is who)

| VM | Proxmox host | Name | Role |
|----|--------------|--------|-----|
| 210 | PC-004-165 | fb-build | **The machine for COMPILING** + apt repo `:8900`. Repo in `/opt/fluxbee`. |
| 240 | PC-004-157 | fbi-mb | Motherbee of the integrated hive (admin API `:8080` local). |
| 241 | PC-004-157 | fbi-worker | Worker (clean box, bootstrapped by the MB — no .deb, no DB). |
| 242 | PC-004-157 | fbi-ingress | Ingress (DMZ). |
| 243 | PC-004-157 | fbi-egress | Egress. |
| 250/9000 | — | templates | Do not touch (clone, do not start). |
| 271 | PC-004-156 | fbdev-edge | Clean Linux box (template 250) for the dev EDGE: `ingress1` via `add_hive`, `192.168.4.85`. **Join pending** (see logbook 2026-09-25). |
| 270 | PC-004-156 | fbdev-mb | Motherbee **installed with `apt install`** from the PROD repo (`192.168.8.180:8900`), `192.168.4.84`. Snapshot `pre-apt-install-20260925` = clean base to repeat the test. |

The 3 hosts (`PC-004-165/156/157`) form a cluster: any `PVE_HOST` answers for all of them,
but **`PVE_NODE` is mandatory** to operate VMs (it says which host the VM lives on).

## Access: `lab/pve.py` (guest-agent, no SSH)

Everything is operated through the Proxmox API + qemu-guest-agent (exec/push/pull **as root** inside the
VM). There is no SSH to the VMs for lab operation.

```bash
export PVE_HOST=192.168.4.165          # any host in the cluster
export PVE_TOKEN='<user>@pve!<tokenid>=<secret>'   # API token (ask the operator for it; NEVER commit it)

# VMs of a host
PVE_NODE=PC-004-157 python3 lab/pve.py list

# Run inside a VM (root)
PVE_NODE=PC-004-157 python3 lab/pve.py exec 240 -- 'systemctl is-active sy-orchestrator'

# Copy files local <-> VM
PVE_NODE=PC-004-165 python3 lab/pve.py pull 210 /path/in/vm ./local
PVE_NODE=PC-004-157 python3 lab/pve.py push 240 ./local /path/in/vm

# Snapshot / rollback (before something destructive)
PVE_NODE=PC-004-157 python3 lab/pve.py snapshot 240 before-X
PVE_NODE=PC-004-157 python3 lab/pve.py rollback 240 before-X
```

More commands: the docstring of [`lab/pve.py`](pve.py). Known gotcha: a character above
U+00FF in an `exec` (`—`, `→`, `✓`) **wedges the guest-agent** until it is restarted. It is not the
load, as was believed ([FINDINGS B-11](logbook/FINDINGS.md)). `pve.py` already encodes the argv.

## Compiling: ALWAYS on fb-build (VM210)

Here **nothing is compiled on the Mac or on the MB**: the documented build box is fb-build
(full toolchain, `source /etc/profile.d/fluxbee-toolchain.sh`, repo in `/opt/fluxbee`).

```bash
# Update the build box's repo to the pushed branch
PVE_NODE=PC-004-165 python3 lab/pve.py exec 210 -- \
  'cd /opt/fluxbee && git fetch origin <branch> && git checkout <branch> && git reset --hard origin/<branch>'

# A single binary (e.g. an IO runtime; the io workspace is separate)
PVE_NODE=PC-004-165 python3 lab/pve.py exec 210 -- \
  'source /etc/profile.d/fluxbee-toolchain.sh; cd /opt/fluxbee && cargo build --release --manifest-path nodes/io/Cargo.toml -p <crate>'

# The full .deb (reads base-nodes.json; see docs/packaging-and-build.md §4.2-4.3)
PVE_NODE=PC-004-165 python3 lab/pve.py exec 210 -- 'bash /root/build2.sh'   # or scripts/make-deb.sh
```

## Bringing a new runtime to hive 240 (WITHOUT a new .deb)

Canonical path = [`docs/history/14-runtime-rollout-motherbee.md`](../docs/history/14-runtime-rollout-motherbee.md).
Summary for the case where the target is the motherbee itself (VM240):

```bash
# 1. binary: fb-build -> VM240 (pull to local, push to the VM)
PVE_NODE=PC-004-165 python3 lab/pve.py pull 210 /opt/fluxbee/nodes/io/target/release/<bin> /tmp/<bin>
PVE_NODE=PC-004-157 python3 lab/pve.py push 240 /tmp/<bin> /tmp/<bin>

# 2. publish + manifest on the MB (runbook §2-3) — publish-runtime.sh is generic (--runtime/--binary)
# 3. POST /hives/motherbee/update category=runtime (runbook §6)
# 4. POST /hives/motherbee/nodes  {"node_name":"...","runtime":"...","runtime_version":"current","tenant_id":...}  (runbook §7)
# 5. verify: active unit + CONFIG_GET (UNCONFIGURED until the secret is loaded in SY.vault)
```

A node in the **base set** (`packaging/base-nodes.json`) goes into the `.deb` automatically
(`build-deb.sh` reads the manifest — **the build script is not touched**) and `fluxbee-firstboot`
spawns it on an install from scratch. The runtimes channel above is for adding it to a backend
that is already running.

## Rules

- **Look for the doc before operating** (this file + the two runbooks above). The path to
  integrate/publish nodes ALREADY EXISTS; do not create side scripts/routes per node.
- Secrets (PVE tokens, keys) go through env/vault — never in the repo or in logs.
- Only the MB (240) has the .deb/DB; workers/ingress/egress are clean boxes bootstrapped by the
  MB (`add_hive`) — installing postgres or the .deb on a worker is a bug, not a fix.
