# HANDBOOK — deploying fluxbee from scratch

> **What this is.** The recipes that **worked end-to-end** in the first deployment of fluxbee to
> production (Proxmox `192.168.8.207`, 2026-07-28 → 07-30). Every step here was actually run and
> verified; anything that was not validated is marked as such.
>
> **Sibling documents:** [`METHOD.md`](METHOD.md) (how we work) · [`FINDINGS.md`](FINDINGS.md)
> (what was discovered, with evidence) · [`PENDING-BUGS.md`](PENDING-BUGS.md) (what needs fixing) ·
> the daily logbooks (`2026-07-*.md`) have the raw detail of every step.
>
> **Portability rule** (`METHOD.md` §3 rule 0c): the next prod may be bare-metal or another
> hypervisor. Each recipe marks what is **Proxmox-specific** and what belongs to **the product**.
> Sections §1–§3 are about the hypervisor; **from §4 on it is pure fluxbee** and works the same on
> a physical server.

---

## 0. The mental model, in one sentence

**Only motherbee is installed with the `.deb`.** The other nodes are **clean Linux machines** that
`add_hive` takes over through SSH, bootstraps, configures and **locks down**. You do not install
fluxbee on a spoke by hand: if you find yourself doing it, you are fighting the design.

```
   [ motherbee ]  <- .deb + fluxbee-firstboot        the only one with Postgres and the vault
        │
        │  add_hive  (SSH: goes in open, comes out locked)
        ├──────────► [ worker  ]   generic work
        ├──────────► [ ingress ]   public door, TLS 443            (2 NICs)
        └──────────► [ egress  ]   outbound NAT to the internet    (2 NICs)
```

---

## 1. Decisions to make BEFORE touching anything

They are not optional: half of them cannot be changed comfortably later.

| Decision | What we chose | Why |
|---|---|---|
| Internal mesh network | `10.10.10.0/24`, no gateway | Directly connected. Leaves room for `10.10.11.x` etc. |
| Who provides internet? | The **egress**, with NAT | That is its role. See §7 on the gateway trap. |
| Ingress IP | Public, its own | The DNS points there. It **cannot** sit behind the egress NAT. |
| Default route of each node | Through its **external leg**, if it has one | A VM has **only one** default gateway. The internal one gets no `gw`. |
| Public hostname | `hive-<hash>.domain` | See §8: opaque on purpose. |
| Bootstrap credential | A user with `NOPASSWD:ALL` | A hard requirement of `add_hive`. It is revoked automatically at the end. |

**The addressing we used** (a concrete reference):

| VM | RAM/cores/disk | internal | second NIC |
|---|---|---|---|
| `fb-mb` | 10G / 4 / 60G | `10.10.10.10` | — |
| `fb-worker1` | 4G / 2 / 30G | `10.10.10.20` | — |
| `fb-ingress` | 2G / 2 / 20G | `10.10.10.30` | public `x.x.x.80/24` |
| `fb-egress` | 2G / 2 / 20G | `10.10.10.40` | admin `192.168.8.240/24` |
| `fb-build` | 8G / 4 / 80G | `10.10.10.50` | admin `192.168.8.180/24` (DHCP, the repo for the devs) *(outside the cluster)* |

> **The build VM goes OUTSIDE the cluster, on purpose.** The plan for an integration failure is
> *delete the cluster and start over*. If the `.deb` and the apt repo live inside it, every wipe
> costs an hour of compiling. Outside, it survives.

> **Add up the RAM before, not after.** I planned 18 G for the cluster and then added the 8 G build
> VM without adding it all up again: 27.9 G allocated on 23 G physical. KVM allocates on demand and
> it did not hurt, but it is a latent risk. ~~Policy: the build VM stays off except while it
> compiles.~~
>
> **Policy since 2026-09-25 (operator's decision): fb-build stays ALWAYS ON** — it serves the apt
> repo to the devs (`192.168.8.180:8900`). Headroom measured that day: host 15.9/29.4 GB used.
>
> **Its 8.x IP is DHCP, tied to the MAC** (`net1 = BC:24:11:EF:96:80`): the router keeps the lease
> as long as the MAC does not change (operator's decision; it is neither reserved nor pinned).
> **Do not recreate `net1`** — a new NIC brings another MAC and another IP, and breaks every dev's
> `sources.list`.

---

## 2. Hypervisor network · *Proxmox-specific*

Three networks, and the internal one is the only one fluxbee needs:

```text
vmbr0   nic0   192.168.8.0/24     administration (shared; its router .1 DOES reach the internet)
fbint    —     10.10.10.0/24      fluxbee INTERNAL — SDN simple zone + SNAT
vmbr1   nic1   x.x.x.0/24         PUBLIC (the host has NO IP on it)
```

**The internal one is built with SDN**, not with a standalone bridge:

```
Datacenter > SDN > Zones  -> type "Simple", ID: fluxint
Datacenter > SDN > VNets  -> ID: fbint, zone: fluxint
  subnet: 10.10.10.0/24, gateway: 10.10.10.1, SNAT: YES
Datacenter > SDN         -> Apply
```

With `SNAT: yes`, the host routes and masquerades: **the internal VMs reach the internet through
the host's IP** without touching anything else. That is what makes it possible to build the
template and compile before the egress exists.

⚠️ **A "Simple" zone is local to the node.** It does not span the hosts of a Proxmox cluster.
Multi-host needs VXLAN or EVPN.

### 🔥 The most expensive trap of all — Proxmox nested on VMware

If Proxmox runs as an ESXi/vSphere VM, **the port group has to allow**:

```
Promiscuous mode  : Accept
MAC address changes: Accept
Forged transmits  : Accept      <-- this is the one that breaks everything
```

With `Forged transmits: Reject`, vSphere drops any frame whose source MAC is not the Proxmox VM's.
The nested VMs **cannot even do ARP**. The symptom is baffling because **the internal network
works perfectly** (it goes out through the host's SNAT, with the host's MAC) while every bridged
NIC is dead.

**And it has to be applied to EVERY port group**, one per physical NIC. We applied it to the one
for `nic0`, the egress started working, and the ingress stayed broken exactly the same — the one
for `nic1` was missing.

> **How to diagnose it** (works for any "the network half works"): look for the **asymmetry**. If
> routed outbound traffic works and bridged traffic does not, the problem is not the VM or the
> bridge: it is **MAC filtering upstream**.

---

## 3. Base template · *Proxmox-specific, but the concept is universal*

Goal: a clean Linux image, with guest-agent and SSH, that gets cloned into **independent**
machines.

### 3.1 The image

```bash
POST /nodes/pve/storage/local/download-url
     content=import
     filename=noble-server-cloudimg-amd64.qcow2      # <-- .qcow2, NOT .img
     url=https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img
```

⚠️ The Ubuntu image **is qcow2 even though it is published with a `.img` extension**. With `.img`
the endpoint rejects it (`invalid filename or wrong extension`). Rename it when you download it.

### 3.2 The cloud-init snippet — **a human has to write it**

The cloud image **does not include `qemu-guest-agent`**, and the guest-agent is the access
channel. It has to be injected on first boot via user-data… and **the Proxmox API does not allow
placing snippets**: `upload` and `download-url` accept `iso, vztmpl, import` — `snippets` is not
in the enumeration, even though the storage does accept the type.

The operator writes it **once** in the host shell, and it stays there for good:

```yaml
# /var/lib/vz/snippets/tpl-userdata.yaml
#cloud-config
packages: [qemu-guest-agent, openssh-server, sudo]
ssh_pwauth: true
runcmd:
  - systemctl enable --now qemu-guest-agent
```

> **Method note.** This is a real limit, and **the right answer is not to ask for SSH to the
> hypervisor**. A one-off file written by the operator is cheaper than a root shell outside the
> task log. See `METHOD.md` §2.

### 3.3 Create, generalize, convert

```bash
POST /nodes/pve/qemu
     vmid=9000 name=fb-template cores=2 memory=2048
     scsi0=local-lvm:0,import-from=local:import/noble-server-cloudimg-amd64.qcow2
     ide2=local-lvm:cloudinit  agent=enabled=1  cpu=host
     net0=virtio,bridge=fbint
     ipconfig0=ip=10.10.10.9/24,gw=10.10.10.1  nameserver=1.1.1.1
     ciuser=fluxops  cicustom=user=local:snippets/tpl-userdata.yaml
     serial0=socket vga=serial0
# wait for the task to FINISH, then:
PUT  .../resize  disk=scsi0 size=20G
POST .../status/start
```

⚠️ **Chain on task status, never on `sleep`.** The `create` imports 624 MB and holds the VM lock
the whole time; a `resize` and a `start` launched right after it fail with
`can't lock file '/var/lock/qemu-server/lock-9000.conf'`. Poll `/tasks/<UPID>/status` until
`stopped OK`.

**Generalize** (inside the VM, before converting) — **this is what makes the clones distinct
machines**:

```bash
cloud-init clean --logs --seed          # each clone re-runs cloud-init with ITS OWN config
truncate -s 0 /etc/machine-id           # otherwise every clone asks for the SAME DHCP lease
ln -sf /etc/machine-id /var/lib/dbus/machine-id
rm -f /etc/ssh/ssh_host_*               # otherwise they share the SSH identity
apt-get clean; rm -rf /var/lib/apt/lists/*
journalctl --rotate; journalctl --vacuum-time=1s
```

**Verify before converting:** `machine-id` at 0 bytes · 0 host keys · cloud-init `not started`.

```bash
POST .../config  delete=cicustom,ipconfig0     # <-- see the trap below
POST .../status/shutdown                        # clean, not stop
POST .../qemu/9000/template
```

### ⚠️ `cicustom` and `ciuser` do not mix

With `cicustom=user=...`, Proxmox uses **that file INSTEAD OF the user-data it generates** — and
that is exactly the one that creates the user. `ciuser`/`cipassword` have **no effect**, and the
user never exists.

**That is why `cicustom` is deleted from the template**: the snippet was only needed once, to bake
the agent into the image. With the agent already inside, the clones use the standard cloud-init
and each one creates its user normally. **The clones do not depend on the snippet.**

### 3.4 Validate the template with the first clone

Do not consider the template good without this:

| Check | Expected |
|---|---|
| `machine-id` | **different** from the template |
| SSH host keys | **3, regenerated** |
| `qemu-guest-agent` | `active` |
| bootstrap user | exists, with `NOPASSWD:ALL` |
| `PasswordAuthentication` | effectively **`yes`** |

About the last one: on Ubuntu 24.04, `50-cloud-init.conf` (`yes`) and `60-cloudimg-settings.conf`
(`no`) coexist. **The first one wins by lexicographic order.** Verify it for real, not by reading
the config:

```bash
ssh -o BatchMode=yes -o PreferredAuthentications=password -o PubkeyAuthentication=no \
    fluxops@127.0.0.1 true
# "Permission denied (publickey,password)" => the server OFFERS password. Correct.
```

### ⚠️ A freshly booted clone is NOT ready

The Ubuntu cloud image triggers `unattended-upgrades` on first boot: it downloads ~15 MB, writes
~2.5 GB, **reboots on its own**, and the guest-agent may not come back (it is updating itself).
Seen on every clone. A variant seen on 2026-09-25 (a clone of template 250 in DEV): cloud-init
updated packages, the kernel included, and left `reboot-required` **without rebooting**. The
recipe below covers both cases.

**Recipe:** after cloning, wait for the cycle to finish and **reboot the VM** before considering
it operational. Confirm with: cloud-init `done`, agent `active`, `dpkg --audit` clean, no
`/var/run/reboot-required`.

This matters **directly** for `add_hive`, which has a 180 s timeout.

### 3.5 Operating through the guest-agent — what you need to know before, not after

If you work against the VMs through the Proxmox API (`agent/exec`), this will happen to you:

**A "wide" character in the command wedges the agent** → [B-11](FINDINGS.md). A single character
above U+00FF (`—`, `→`, `✓`) in the arguments of `agent/exec`, and `qemu-guest-agent` is wedged:
the API answers `QEMU guest agent is not running` with the VM **perfectly alive**, and it does not
come back until the agent restarts. Latin-1 accents (`é`, `ñ`, `¿`) get through.

> **It is not the load.** During August this was diagnosed as "the agent dies under I/O load",
> and it cost reboots of prod VMs. It was A/B tested (2026-09-25): with the VM idle, `—` wedges it
> and `é` does not. The commands that wedged it had `—`/`→`/`✓` in their `echo`.

`lab/pve.py` already guards against it (non-ASCII argv travels in base64). **If you talk to the
API some other way (curl, another script): ASCII-only arguments, or encoded ones.**

If it happens to you anyway:

```bash
# 1. before you panic, confirm that the VM is alive FROM ANOTHER VM of yours
#    (same internal network; the agent is not needed)
ping -c2 10.10.10.10
for p in 9000 9100; do timeout 3 bash -c "echo > /dev/tcp/10.10.10.10/$p" && echo "$p ok"; done

# 2. recover the agent: ACPI reboot first (it does not use the agent)
POST /nodes/pve/qemu/<id>/status/reboot
# it comes back in ~70s. If the first exec you send has a wide character again, it wedges again.
# 3. if the ACPI reboot does not take: hard reset.
POST /nodes/pve/qemu/<id>/status/reset
```

If it wedges with an **ASCII-only** command, it is something else: do not cover it up with a
reset; record it as a finding.

> A motherbee reboot/reset **is not free, but it is not serious either**: the 4 hives come back on
> their own. It is, in fact, the hardest cold-start test. But decide on it on purpose, not out of
> panic.

**Anything that takes more than a few seconds runs under `setsid nohup`, into a log.** If the call
times out or is cut off midway, the command survives and you read the log afterwards. Without
this, you lose the work AND you do not know whether it ran:

```bash
setsid nohup /root/whatever.sh > /root/whatever.log 2>&1 < /dev/null &
```

A corollary I learned the hard way: if the agent dies during the call that **creates** the
script, the script never existed. Before assuming it ran and stopped halfway, check whether the
log is there.

**Secrets go through stdin, never through argv.** `agent/exec` accepts `input-data` (base64).
Everything passed through `command=` ends up in the **Proxmox task log**:

```bash
# lab/qga_stdin.sh — passes the payload through stdin, not on the command line
POST agent/exec  command=/bin/bash  command=-lc  command="<script that reads stdin>"  input-data=<b64>
```

### ⚠️ After generalizing, the first `apt` needs a FULL `update`

Generalizing deletes `/var/lib/apt/lists/*`. If you run an `apt update` filtered to a single repo
(for example the fluxbee one), the Ubuntu lists are not there and the install fails with
`Depends: postgresql but it is not installable`. **A full `apt-get update`** (≈39 MB) solves it.

---

## 4. Building the `.deb` — *pure fluxbee*

On a separate VM, with the toolchain that the repo pins:

```bash
# rustc/cargo (the EXACT version of the repo), go, protobuf-compiler,
# build-essential, pkg-config, libssl-dev, git, dpkg-dev, apt-utils
git clone git@github.com:<org>/fluxbee.git /opt/fluxbee
bash packaging/build-deb.sh 0.1.0
```

**Measured performance:** Rust release **~55 min** with 4 vCPU on a Xeon E5-2620 @2.0 GHz. A full
cycle with Rust cached: ~27 min. **Run it with `nohup`**: it survives your session dropping.

### ⚠️ Clone and compile as the SAME user

If you clone as one user and compile as `root`, git flags *dubious ownership* and the Go step dies
with `error obtaining VCS status: exit status 128` — **after Rust has already compiled for 55
minutes**.

```bash
# idempotent: running it N times leaves ONE single entry (and cleans up old duplicates).
# With --add it adds one line per run: fb-build ended up with 23 copies (2026-09-25).
git config --global --replace-all safe.directory /opt/fluxbee '^/opt/fluxbee$'
```

> The error itself suggests `-buildvcs=false`. **Do not use it**: it turns off version stamping
> in the binaries to cover up a permissions problem. Fix the cause.

### The toolchain lives in the `HOME` of whoever compiles

`rustup` looks for its metadata in `$HOME/.rustup`. If you run through the guest-agent with a
`HOME` that is not the toolchain owner's, it tells you **"no installed toolchains"** with the
toolchain directory right there, present and complete:

```
error: rustup could not choose a version of cargo to run, because one wasn't
       specified explicitly, and no default is configured
```

Rust is not missing: you are looking at the wrong `.rustup`. On `fb-build` the toolchain belongs
to `root`, so the build script starts with `export HOME=/root` — while the `git fetch` runs as
`fluxops`, the user that holds the deploy key. Both users are needed, each in its own step.

> And if you already ran git as `root` by mistake, give the objects back before going on, or the
> next fetch as `fluxops` dies with `insufficient permission`:
> `find /opt/fluxbee/.git -user root -exec chown fluxops:fluxops {} +`

### Verify the `.deb` before trusting it

```bash
ls -la dist/fluxbee_*.deb              # ~240 MB. If it weighs 2 KB, it came out truncated.
dpkg-deb -c dist/fluxbee_*.deb | wc -l # ~111 entries
dpkg-deb -c dist/fluxbee_*.deb | grep -E "ai_node_runner|wf-generic|io-"
```

### Publishing it as an apt repo

```bash
scripts/apt-repo-publish.sh --serve     # serves on :8900 (PERSISTENT fluxbee-apt unit: survives reboots)
```

**Always install through `apt`, never with `dpkg -i`** — the package depends on `postgresql`, and
`apt` is the documented path that resolves dependencies.

#### ⚠️ Use the script. Do not rebuild the index by hand.

The script rebuilds the three index files, and all three matter (it writes them to temporary
files that it then renames, so an `apt-get update` in progress never sees a half-written index):

```bash
apt-ftparchive --db "$CACHE" packages . > Packages  &&  gzip -c Packages > Packages.gz  &&  apt-ftparchive release . > Release
```

Copying a new `.deb` into the folder **is not enough**. If you do it by hand, there are two ways
for it to go wrong, and neither one warns you:

| omission | symptom | why it misleads |
|---|---|---|
| not regenerating `Release` | `apt` says **`fluxbee is already the newest version`** with the new package right there | `apt` sees `Hit:` on `Release` and does not even look at `Packages` |
| an index with **only one** version (for example `dpkg-scanpackages` without `-m`) | `apt` offers only the newest one | you are left **without rollback through apt**; `apt-ftparchive packages` lists them all |

The second one is the dangerous one: it breaks nothing today; it takes away your way back. You
can spot it by comparing the size of `Packages` — 13 versions are ~13 KB, a single one is ~1 KB.

> This is written down because I stepped on it: I built the index by hand, ran into both, and
> lost two cycles. The script existed from the start.

#### ⚠️ `dpkg-deb` looks hung, and it is not

The last step compresses with `xz`. For **several minutes** the `.deb` stays at ~3 KB, and if you
look at it at that moment, `dpkg-deb -c` tells you `unexpected end of file in archive member
header`.

It is not truncated: it is compressing. Confirm that before you get alarmed:

```bash
ps aux | grep dpkg-deb        # 300-400% CPU = it is working
```

The finished package weighs **~240 MB**. The line `=== BUILD END rc=0 ===` is the only sign that
it finished; the file size halfway through says nothing.

---

## 5. Motherbee

```bash
# on the motherbee VM
echo "deb [trusted=yes] http://10.10.10.50:8900 ./" > /etc/apt/sources.list.d/fluxbee.list
apt-get update            # FULL (see 3.4)
apt-get install -y fluxbee
fluxbee-firstboot
```

**Devs on the internal infra** (8.x directly, or 4.x through the internal VPN) — same recipe,
different IP:

```bash
echo "deb [trusted=yes] http://192.168.8.180:8900 ./" | sudo tee /etc/apt/sources.list.d/fluxbee.list
sudo apt-get update && sudo apt-get install -y fluxbee    # or fluxbee=<version>
```

- **Requirement:** Ubuntu **24.04+ amd64** — the `.deb` declares `libc6 (>= 2.39)`; on an older
  distro apt rejects it with a clear error. `postgresql` and the other Depends come from the
  Ubuntu archive.
- **`[trusted=yes]` is mandatory** while the repo is not signed (apt rejects unsigned repos).
- It installs a **full backend** (motherbee): next come `hive.yaml` + `fluxbee-firstboot`.
- **Validated 2026-09-25:** `apt-get install -s fluxbee` from a clean Ubuntu 24.04 on the 8.x
  (it resolves 13 packages), and a download from a machine over VPN (source `10.100.0.2`). The
  box's 8.x leg answers clients from **any** subnet through policy routing (table 108, see the
  logbook).
- **Validated end to end in DEV (2026-09-25):**
  - Starting point: a clean VM on the 4.x LAN (VM 270 `fbdev-mb`, PC-004-156). **It reaches the
    repo without VPN**, via 192.168.4.1.
  - `apt install` 0.1.33: the 245 MB `.deb` takes ~3 min over the 4.x ↔ 8.x link.
  - `fluxbee-firstboot`: rc=0 in 28 s, without editing `hive.yaml`.
  - Result: `motherbee alive`, 21 units, 0 failed, and the base nodes in `UNCONFIGURED`.
  - Archi, `http://<ip>:3000`, answered from the VPN. **Since 0.1.57 it listens only on loopback**
    (A-46): use it on the motherbee or through an SSH tunnel (`ssh -L 3000:127.0.0.1:3000 ...`).

`fluxbee-firstboot` is **idempotent-ish but irreversible in practice**: it bootstraps the hive.
Its log tells exactly what it does:

```text
[1/5] postgresql + DB bootstrap
[2/5] start orchestrator          (crash-loops until the secret lands — this is normal)
[3/5] wait admin + vault_put postgres secret
[4/5] reconnect DB consumers
[5/6] wait hive ready + start motherbee singleton nodes
[6/6] spawn base node instances   (they start degraded, unconfigured)
```

**Acceptance:**

```bash
curl -s http://127.0.0.1:8080/hives                      # motherbee: alive
systemctl list-units 'sy-*' 'fluxbee*' 'rt-*' --all      # 19 units active running
systemctl --failed                                        # empty
curl -s http://127.0.0.1:8080/hives/motherbee/commands   # the audit log is already recording
```

The base nodes (`IO.cloud`, `IO.blob`) start in `UNCONFIGURED`.
**That is correct**, not a failure: the degraded start is by design.

> **The hive registry decides who may connect over the WAN.** The motherbee admits a hive only
> while `/var/lib/fluxbee/hives/<hive_id>/` exists: `add_hive` creates it when it accepts the
> join, `remove_hive` deletes it after the cleanup, and the link of a removed hive closes within
> 10 s. There is no other allowlist. In `journalctl -u rt-gateway` on the motherbee:
> `wan peer rejected: hive not registered peer_hive=<hive_id> … reason=HIVE_NOT_REGISTERED` (a
> dial refused) and `wan peer session closed: hive no longer registered peer_hive=<hive_id> …
> reason=HIVE_NOT_REGISTERED` (a live link cut).

---

## 6. The spokes — `add_hive`

**One command against an empty Linux machine.** It goes in open and comes out locked.

```jsonc
POST /hives                          // action: add_hive  (NOT /hives/<hive>/hives)
{
  "hive_id": "worker1",
  "role": "worker",                  // worker | ingress | egress
  "address": "10.10.10.20",
  "ssh_user": "fluxops",
  "ssh_password": "<template credential>",   // or ssh_key
  "harden_ssh": true                 // <-- NOT THE DEFAULT. Always set it.
}
```

### ⚠️ `harden_ssh` comes as `false`

The hardening works perfectly when it is requested (we verified it node by node). But you have to
**remember it**. An insecure default on the happy path → [PB-6](PENDING-BUGS.md#pb-6).

### What to verify after each join — **do not settle for what it reports**

```bash
sshd -T | grep -i passwordauth        # no
wc -l < ~fluxops/.ssh/authorized_keys # 0  (the MB key was revoked)
ls /etc/sudoers.d/                    # no fluxbee-orchestrator
systemctl list-units 'fluxbee*' 'rt-*' --no-legend | wc -l   # 9 services
```

### egress — it needs more

```jsonc
{ "role": "egress", "lan_cidr": "10.10.10.0/24",
  "lan_iface": "eth0", "wan_iface": "eth1" }
```

**`nftables` has to be installed beforehand** — `add_hive` egress **fails closed** without it.

Verify the NAT **on the box**, not in the response:

```bash
nft list ruleset | grep -E "masquerade|10.10.10"
sysctl net.ipv4.ip_forward                       # 1
journalctl -u <egress unit> | grep nat_applied
```

> **`internet_reachable` does not reach motherbee.** It is checked only on the egress host and is
> not passed on (v1, T-VER-1): the `add_hive` response returns it as `null`. Look at the egress
> journal.

### ingress — the certificate goes BEFORE the join

**Seed the TLS into the vault first**, or the edge comes up without HTTPS:

```jsonc
POST /hives/motherbee/vault/secrets
{ "key": "edge_tls_<domain>",
  "value": { "cert": "<PEM: leaf + intermediates>", "key": "<private PEM>" },
  "metadata": { "resource_type": "tls", "owner_node": "SY.edge@ingress1" } }
```

Then:

```jsonc
{ "role": "ingress", "ingress": { "listen": "0.0.0.0:443" } }
```

`allow_plaintext` is **forbidden** on 443. The edge is *fail-closed*: if the TLS cannot be set
up, it does **not** fall back to plaintext — it does not bring up the public frontend.

### ⚠️ You configure the secondary NICs yourself

`add_hive` **has no commands to configure secondary network NICs**, but the `ingress` and `egress`
roles **require** a second NIC by definition. You have to leave them configured beforehand and
pass it the interface names. It breaks the promise of "an empty machine" exactly in the two
roles that need it most → [PB-5](PENDING-BUGS.md#pb-5).

### ⚠️ DNS: do not give every node the same nameserver

I set `nameserver=192.168.8.1` (the admin network) on all four VMs alike. The **ingress has no
route to that network** → it was left without DNS. **Nodes with a public leg need a public DNS**
(`1.1.1.1`).

---

## 7. The gateway trap — `egress.gateway_ip`

The egress does NAT, but **the nodes have to use it as their gateway** for it to be of any use.
That is declared in `hive.yaml`.

⚠️ **The field is, by design, the WORKERS' route.** Motherbee **is not part of the egress model**
— and that is not an oversight: Fluxbee never rewrites the default route of the control plane
itself (doing it from a yaml field, on every boot and every watchdog tick, without rollback, on
the box that provisions the egress, is a lockout). → [PB-8](PENDING-BUGS.md#pb-8).

But motherbee **is** where the nodes that call the internet run (`IO.slack.*`, `IO.wapp.*`,
`AI.*`), so the orchestrator **checks** its route against `gateway_ip` and logs
`EGRESS_MOTHERBEE_BYPASS` when they do not match. It is observation, not correction: if you want
motherbee behind the egress, set it up at the OS/network level.

⚠️ **And the more immediate trap: today it may be that NOBODY routes through the egress.** The
injection only reaches the workers provisioned **after** the field is declared — a worker that
joined before keeps its original gateway.

**Do not take for granted that your motherbee goes out through the egress** without checking it:

```bash
curl -s https://ifconfig.me      # from motherbee: does it go out through the egress IP?
ip route get 1.1.1.1
```

---

## 8. DNS and certificate

**A record**: `hive-<hash>.<domain>` → the public IP of the ingress.

We chose an **opaque single-label hash** (`hive-k3m9x7q2`), not `ingress1` or the customer's
name, for three reasons:

1. The `*.domain` wildcard **only covers one label** — `a.b.domain` is not covered.
2. That hostname **will end up written into other parties' configurations** (Meta webhooks,
   Slack…). The less it says, the better.
3. It leaks neither the customer nor how many backends there are.

It is **deliberately different from the internal `hive_id`**: the external door belongs to the
edge, not to the hive (decision D7).

### The certificate: send the full chain

**Verify the file before loading it** — ours came with the leaf's `-----END CERTIFICATE-----`
stuck to the intermediate's `-----BEGIN` on the same line, and that way **openssl reads zero
certificates**:

```bash
grep -c "BEGIN CERTIFICATE" chain.crt
openssl crl2pkcs7 -nocrl -certfile chain.crt | openssl pkcs7 -print_certs -noout
openssl verify -untrusted intermediates.pem leaf.pem
# does the key match?
diff <(openssl x509 -in leaf.pem -noout -modulus) <(openssl rsa -in private.key -noout -modulus)
```

Serve **leaf + intermediates**, without the self-signed root (it is already in the trust stores;
it only adds bytes per handshake).

> **Why sending the chain matters.** Without the intermediate, a browser or a `curl` with the CA
> cached **still says OK** (it fetches the intermediate through AIA). A strict client —Go, Python,
> the Meta and Slack webhooks— fails. **It looks fine exactly where it does not matter.**

**Test it with a client that does NOT do AIA fetching:**

```python
import ssl, socket
c = ssl.create_default_context()
with socket.create_connection(("hive-xxx.domain", 443), timeout=20) as s:
    with c.wrap_socket(s, server_hostname="hive-xxx.domain") as t:
        print(t.version(), t.getpeercert()['notAfter'])
```

### ⚠️ Rotating the cert requires restarting `sy-edge`

The TLS material is read **only once, at boot**. Loading it into the vault is **not** enough: you
have to restart the service. Mitigated in the code (the edge now exits with `exit(0)` and systemd
restarts it), but **check that your binary has it** → [PB-1](PENDING-BUGS.md#pb-1).

---

## 9. Final acceptance

```bash
# 1. full topology
curl -s http://127.0.0.1:8080/hives
#    motherbee alive · worker1/ingress1/egress1 connected

# 2. public HTTPS, from OUTSIDE the datacenter
curl -sS -o /dev/null -w "%{http_code} verify=%{ssl_verify_result}\n" https://hive-xxx.domain/
#    404 verify=0   <-- the 404 is CORRECT: SY.edge only serves /e/<ich>

# 3. full chain
echo | openssl s_client -connect hive-xxx.domain:443 -servername hive-xxx.domain -showcerts 2>/dev/null \
  | grep -c "BEGIN CERTIFICATE"      # >= 2

# 4. the egress really routes
#    (from a worker) curl -s https://ifconfig.me  -> egress IP
```

**Hypervisor reboot:** validated **the mesh re-forming**: with the 4 VMs cold-started at the same
time, **it re-formed on its own**, without intervention. But the VMs **do not start on their own**
unless they have `onboot=1`: on July 30 they had to be started by hand → [B-13](FINDINGS.md).
Warnings:

- **Wait for convergence before diagnosing.** While it boots you will see inactive services and
  `NODE_NOT_FOUND`. It is not a failure: it is the boot, and it is **slow** ([B-14](FINDINGS.md)).
  Measured on a motherbee reboot (2026-09-25): the agent comes back at ~2.5 min, the 9 runtimes at
  ~3 min 20 s, and the public endpoint can return **502** for a few seconds while `io.cloud` comes
  up. The spokes need nothing: they ride out the hub going down without restarting.
- `sy-edge` can reach `restart counter is at 9` before it stabilizes → [PB-4](PENDING-BUGS.md#pb-4).

---

## 10. Trap table — the summary worth reading before you start

| # | Trap | Symptom | Solution |
|---|---|---|---|
| 1 | VMware port security | The nested VMs **do not even do ARP**; the internal network works anyway | `Forged transmits: Accept` **on every** port group |
| 2 | VM lock in Proxmox | `can't lock file ... got timeout` | Chain on task status, not on `sleep` |
| 3 | Cloud image `.img` | `invalid filename or wrong extension` | Rename to `.qcow2` |
| 4 | API without snippets | `snippets` is not in the enumeration | The operator writes it, once |
| 5 | `cicustom` vs `ciuser` | The user is never created | One or the other; delete `cicustom` from the template |
| 6 | Freshly booted clone | It reboots on its own, the agent disappears | Wait for the cycle + **reboot** |
| 7 | Filtered `apt update` | `Depends: postgresql but it is not installable` | A **full** `apt-get update` |
| 8 | Cloning and compiling as different users | `VCS status: exit status 128` after 55 min | `git config --global --replace-all safe.directory /opt/fluxbee '^/opt/fluxbee$'` |
| 9 | Truncated `.deb` | `dpkg-deb` exits 0 with a 2 KB package | Check the size and the entries (there is a preflight now) |
| 10 | `harden_ssh` at `false` | The box stays open | Pass it `true` explicitly |
| 11 | `nftables` missing | `add_hive` egress fails closed | Install it beforehand |
| 12 | Uniform DNS | The ingress is left without name resolution | A public DNS on the nodes with a public leg |
| 13 | Incomplete TLS chain | It works in the browser, fails in the webhooks | Serve leaf + intermediates; test without AIA |
| 14 | Cert rotated in the vault | The edge keeps serving the old one | Restart `sy-edge` (see PB-1) |
| 15 | Secondary NICs | `add_hive` does not configure them | Have them ready before the join |
| 16 | apt repo made by hand | `apt` says *already the newest version* with the new `.deb` right there | Use `scripts/apt-repo-publish.sh`: it regenerates **`Release`**, not just `Packages` |
| 17 | Index rebuilt by hand with `dpkg-scanpackages` without `-m` | It ends up with a single version · **you are left without rollback** | Use the script (`apt-ftparchive packages` lists them all). Do not rebuild it by hand |
| 18 | `dpkg-deb` "hung" | The `.deb` stays at 3 KB and `dpkg-deb -c` says *unexpected end of file* | It is compressing with `xz`. Wait for the `BUILD END`; it ends at ~240 MB |
| 19 | Toolchain and `$HOME` | `rustup` says *no installed toolchains* with the toolchain right there | `rustup` reads `$HOME/.rustup`: on `fb-build` compile with `HOME=/root` |
| 20 | Character > U+00FF in an `exec` (`—` `→` `✓`) | `QEMU guest agent is not running` with the VM alive — **it is not the load** (B-11) | `lab/pve.py` (it encodes the argv); if it already happened: reboot, and failing that, `reset` |
| 21 | Long command through the agent | The call times out midway and you lose the work | `setsid nohup ... > log` and read the log |
| 22 | Wrong port/unit | "the edge / the router / the admin went down" | `SY.edge` is on **:443**; on a spoke there is no `sy-router`; the admin is **loopback** |
| 23 | The API `payload` | A `while` that spins on a successful operation | The top-level `status` is the envelope's; the real result is under `payload.` |
| 24 | `verify=19` on local TLS | "the chain is broken" | It is missing SNI. Test with `--resolve` and a host that matches the wildcard |
| 25 | Hot-published runtime | The `.deb` goes up and that node stays on the old version, **without warning** | The seed respects your `current` on purpose. Promote it with `publish-runtime.sh --set-current` |
| 26 | `systemctl stop` on a managed node (`fluxbee-node-*`) | The unit **disappears** (it is transient, from `systemd-run`); `systemctl start` finds nothing, and the node only comes back when the reconcile relaunches it (~60 s) | Use the orchestrator: `kill_node` **without** `purge_instance` to stop it (it keeps the config and the ILK) and `POST /hives/<h>/nodes/<n>/start`. Since 0.1.68 that stop **lasts**: it removes the relaunch mark (`relaunch_on_boot`) and the start puts it back (A-67); before that, the reconcile brought the node back up |

---

## 11. What is portable and what is not

**Product — works the same on bare-metal:** §4 (`.deb` build), §5 (motherbee), §6 (`add_hive`),
§7 (gateway), §8 (DNS/cert), §9 (acceptance). Traps 8–15.

**Proxmox/VMware-specific:** §2 (SDN, port groups), §3 (template, cloud-init, guest-agent).
Traps 1–7.

**On bare-metal, §2 and §3 are replaced by:** cabling/VLANing the three networks, and installing
Ubuntu Server on each machine with a `NOPASSWD:ALL` user and SSH password login enabled.
**`add_hive` does not change at all** — its contract is "a clean Linux machine reachable over
SSH", and it does not care where that machine came from.

---

## 12. Updating an existing deployment

**Validated on 2026-07-30** with five chained upgrades in production. The three blockers that the
previous audit had found are closed; details in [`PENDING-BUGS.md`](PENDING-BUGS.md).

### The cycle

```bash
# 1. on the build box
git pull && bash packaging/build-deb.sh 0.1.N
bash scripts/apt-repo-publish.sh --serve      # publishes and KEEPS the previous ones

# 2. on motherbee
apt-get update && apt-get install -y fluxbee  # never dpkg -i

# 3. to each spoke (optional; the .deb only updates motherbee)
POST /hives/{spoke}/update  {"category":"core","manifest_version":0,"manifest_hash":"<motherbee's>"}
#    the hash comes from: GET /hives/motherbee/versions  ->  payload.hive.core.manifest_hash
```

**Real time:** 6 to 23 min of build depending on the cache, ~9 s for the publish (since 0.1.51
the index hashes only the new `.deb`; before, it took 8.6 min), ~1 min for the `apt install`, and
the system settles on its own in ~25 s.

### The same cycle in one command: `lab/ops.py` (since 2026-10-02)

`lab/ops.py` runs the cycle above through the guest-agent, on top of `lab/pve.py`, with the PROD
inventory (motherbee=100, worker1=101, ingress1=102, egress1=103, build 110; change it with
`FLUXBEE_HIVES` and `FLUXBEE_BUILD_VM`):

```bash
ops() { python3 lab/ops.py --env scratchpad/pve.env --pve-host 192.168.8.207 "$@"; }
ops build 0.1.N --wait          # git pull + build-deb.sh on the build box (log /root/build-0.1.N.log)
ops publish 0.1.N               # apt-repo-publish.sh (log /root/publish-0.1.N.log)
ops deploy 0.1.N --snapshot pre-<something>-0-1-N --drop <the-oldest>
ops versions                    # core version and hash per hive: the installed one if there is one, otherwise dist's
ops opa-status                  # user OPA policy per hive
ops health [--since HH:MM]      # per VM: units down, gate denials, routing.src drops (UTC)
ops admin GET '/hives/motherbee/routes?x=1&y=2'   # no shell: the & and the JSON pass through intact
ops run 100 script.sh [args]    # a local script (bash or .py) on a VM
```

`deploy` does, in order:

1. A snapshot of every VM.
2. `apt install` on motherbee.
3. Waits for the admin to report the new version.
4. Waits for each spoke's orchestrator to answer: the motherbee's admin answers before its router
   has the spokes back, and an update sent at that moment fails with `TRANSPORT_ERROR` (0.1.56).
5. core-update of the spokes in parallel; it retries one that did not get through, up to 3 times.
   Since 0.1.55 each spoke answers `status=ok phase=restarting`.
6. Waits for each spoke to have the motherbee's hash **installed** (`core.installed`). On a spoke,
   `core.manifest_hash` and the component versions are what dist brought, which Syncthing
   updates before any update: in 0.1.56 they made spokes that were still on the previous version
   count as updated.
7. `health` since the start of the deploy.

The 3-snapshot rule is respected this way: if a VM already has 3, `deploy` does not start until
you name the oldest with `--drop`, and it deletes that one and no other. The ledger record is
still made by hand.

### Record the deploy — ALWAYS (audit) · operator's rule (2026-08-20)

**Every version that touches PROD is recorded in [`../DEPLOYMENTS.md`](../DEPLOYMENTS.md)** — the
audit ledger. It applies to the `apt install` on motherbee **and** to each core-update of a spoke.
**Without an entry, the deploy is not done.** Flow:

```bash
# BEFORE the apt install: snapshot motherbee (the name goes in the ledger row).
# Never more than 3 per VM (see below): if there are already 3, delete the oldest first.
python3 lab/pve.py snapshots 100                       # the oldest comes first
python3 lab/pve.py delsnapshot 100 <the-oldest>        # only if there are already 3
python3 lab/pve.py snapshot 100 pre-<something>-<version>
# ... build + publish + apt install ...
# AFTER: add to the ledger a row in the table + an entry with:
#   version · previous version · COMMIT · scope (motherbee/spokes) · what changed (impact) ·
#   verification (real evidence, not "status ok") · rollback (snapshot + apt install fluxbee=<previous>)
```

The audit entry does **not** replace the logbook (`YYYY-MM-DD.md`, the journey) or `FINDINGS.md`
(the bugs): the ledger is only "which version ended up in prod and why it can be trusted". There
is a ready-made template at the end of `DEPLOYMENTS.md`.

### Snapshots: never more than 3 per VM · operator's rule (2026-09-25)

**No VM has more than 3 snapshots, ever.** Before taking a new one, if there are already 3, the
oldest is deleted. `lab/pve.py snapshot` **refuses** to create the fourth and tells you which one
to delete. The deletion stays an explicit step (`delsnapshot`) because it is irreversible.

- **Why:** on LVM-thin each snapshot retains every block that the VM changed since it was taken,
  and it grows with use inside the pool that **all** the VMs share. If the pool fills up, they all
  freeze. On 2026-09-25 fb-mb had 14, one per release since 0.1.19, and the first one with RAM.
  Deleting 11 freed **19 GiB** (the pool went from 64% to 54%).
- **The snapshot is for undoing the immediate past,** not for archiving versions. To go down to an
  old version, the path is the apt repo, which keeps them all: `apt install fluxbee=<previous>`.
  That is why the "by snapshot" rollbacks in the old entries of `DEPLOYMENTS.md` no longer exist.
- **Deleting is irreversible:** you delete the oldest one, named explicitly, never in a blind
  batch.

```bash
python3 lab/pve.py snapshots <id>                  # by date, the oldest first
python3 lab/pve.py delsnapshot <id> <the-oldest>
```

### Commits and versions · operator's rule (2026-09-25)

To track the changes **by version**:

1. **Every commit references the CURRENT fluxbee version** in its subject:
   `[<version>] <type>(<scope>): <what changed>`. The current version is the latest one **in
   production** according to [`../DEPLOYMENTS.md`](../DEPLOYMENTS.md) (the newest ✅ live row) —
   not the one about to be built. Example:
   `[0.1.33] fix(apt-repo): persistent server unit + atomic index swap`.
2. **The body references the changes:** what changed and why, plus whatever references apply
   (finding `FINDINGS B-n`, logbook `YYYY-MM-DD`, ledger entry).
3. **Every install in PROD changes the version** — major or minor; the same number is never
   re-installed. In the alpha stage the last component is bumped (`0.1.N → 0.1.N+1`) with
   `build-deb.sh <new>`, and it is recorded in the ledger (previous section).

A useful consequence: everything committed since the last install carries the base version, so
the **delta that goes into the next version** comes out by itself:

```bash
git log --oneline --grep='^\[0\.1\.33\]'     # what will go into 0.1.34
```

The commit that records an install in the ledger already carries the **new** version (because at
that moment it is the one in production): `[0.1.34] docs(lab): ledger entry for 0.1.34 (…)`.

### How to tell whether the update was REAL and not a phantom

`status: ok` is not enough. The check that tells a genuine update from one that did nothing:

```bash
p=$(systemctl show -p MainPID --value sy-orchestrator)
readlink /proc/$p/exe        # if it says "(deleted)" -> it is running the OLD binary
systemctl show -p ActiveEnterTimestamp --value sy-orchestrator   # must be LATER than the update
sha256sum /usr/bin/sy-orchestrator /var/lib/fluxbee/dist/core/bin/sy-orchestrator   # must match
```

### ⚠️ A hot-published runtime does NOT move forward with the `.deb`

The package seed moves the `current` of each base runtime to the new version… **except for a
runtime that you hot-published and whose directory is still on disk**. There it respects yours,
and publishes the package's version only as *available*:

```text
NOTE: base runtime io.api     'current' moved 0.1.13 -> 0.1.15 (package authority)
NOTE: base runtime ai.generic 'current' moved 0.1.13 -> 0.1.15 (package authority)
   ...and io.slack does NOT appear in the list
```

It is deliberate and correct: if it overwrote your `current`, every upgrade would silently undo
your deployment. But the side effect is just as silent the other way round — **that node falls
behind and nothing warns you**. The package goes up, everything else moves forward, and that
runtime stays on the old version.

Always check after an upgrade:

```bash
python3 -c "
import json; m=json.load(open('/var/lib/fluxbee/dist/runtimes/manifest.json'))
for k,v in m['runtimes'].items(): print(k, v['current'], v['available'])"
```

To accept the package's version, promote it with the same tool that the seed uses, and restart
the node **through admin**:

```bash
sudo /usr/share/fluxbee/publish-runtime.sh --runtime io.slack --version 0.1.15 \
     --binary /var/lib/fluxbee/dist/runtimes/io.slack/0.1.15/bin/io-slack \
     --dist-root /var/lib/fluxbee/dist --set-current
curl -sS -X POST "$ADMIN/hives/$HIVE/nodes/$NODE_ENC/restart" -d '{"confirm":true}'
```

> The `restart` through admin **does** take you to `current` — but only since 0.1.15. Before that,
> it resolved the latest concrete version stored instead of the intent
> (`requested_runtime_version: "current"`), and on top of that it restarted the unit without
> regenerating it: it reported the new version **while running the old binary**.

### ⚠️ How NOT to read a failure where there is none

Three times in one working day I read a healthy system as broken. All three were probing
mistakes, not product problems — and all three are avoided by knowing where each thing looks:

| what I tried | what I saw | what was really going on |
|---|---|---|
| `curl https://127.0.0.1:8443/` on the ingress | connection refused, "the edge went down" | **`SY.edge` listens on `:443`.** The `8443` is from another era |
| `systemctl is-active sy-router` on a worker | `inactive`, "the worker lost the router" | **On a spoke that unit does not exist.** List them with `systemctl list-units "sy-*"` before asking about one |
| `curl :8080/hives` from the worker | no answer, "the admin died" | **The admin API listens on loopback.** You query it from motherbee, not from a spoke |

And a fourth one, about TLS: against `127.0.0.1` you will see `ssl_verify_result=19`
(*self-signed in chain*) even when the chain is perfect — the reason is that there is no SNI or
hostname to validate. Verify with a concrete name that matches the cert's wildcard:

```bash
# the CN is *.fluxbee.ai — a literal "*.fluxbee.ai" does NOT work as a host
H=hive-test.fluxbee.ai
curl -s -o /dev/null -w "http=%{http_code} verify=%{ssl_verify_result} certs=%{num_certs}\n" \
     "https://$H/" --resolve "$H:443:127.0.0.1"
# expected: http=404 verify=0 certs=4
```

### ⚠️ The API response lives under `payload`

Everything the admin API returns comes wrapped:

```json
{"action":"...", "status":"...", "error_code":null, "payload":{ ...what you care about... }}
```

The top-level `status` belongs to the **envelope**, and it says `ok` even if the operation inside
failed. Real paths that are often needed:

```text
GET /hives                     ->  payload.hives[]           (NOT payload.hives[].status for the join: it is payload.hives[].join.phase)
GET /hives/<h>/versions        ->  payload.hive.core.manifest_hash
GET /versions                  ->  payload.comparison.verdict   and   payload.comparison.hives[]
```

> This already caused a real bug: four e2e loops read `payload.status` —always `"ok"`— and would
> have spun for 600 s on a **successful** join. If you are about to write a `while` over a
> response from this API, first check which field you are looking at.

### `update category=core` answers before restarting (since 0.1.55)

**The response arrives when the target already has the new binaries and units**, with
`phase: "restarting"`. The restarts run afterwards, in the background: the health gate, and the
rollback if the gate fails.

- **How to know how it ended:** in `GET /hives/{hive}/versions`, `core.last_update` (`phase`
  `restarting|done`, `status` `ok|rollback`, `errors`).
- **Before 0.1.55** it answered TIMEOUT even when it worked (U-8b). The response went out through
  the router that the update itself had just restarted, and it got lost. Validated live with
  0.1.56: the three spokes answered `phase=restarting` and ended in `done ok`.
- **Do not retry blindly:** verify with the check above, or with `core.last_update`.

### ⚠️ Runtime nodes and the recovery order

An upgrade replaces `dist/runtimes/<rt>/<version>/` and deletes the previous one. The nodes that
follow `current` **rebind and restart on their own** when the new orchestrator starts (look for
`runtime 'current' pointer moved` in the journal). If one is left hanging, the manual recovery
has a **mandatory order**:

```bash
# ONE single call: stops the node AND deletes its persisted directory.
DELETE /hives/{h}/nodes/{n}@{h}   -d '{"purge_instance":true}'

POST   /hives/{h}/nodes                 # run_node — tenant_id is MANDATORY: the one the node had
  {"node_name":"IO.api.acme","runtime":"io.api","runtime_version":"current",
   "tenant_id":"tnt:<the node's tenant>"}
# The root tenant (tnt:00000000-0000-0000-0000-000000000001) is only valid for IO.cloud and IO.blob:
# any other IO node there gets TENANT_ROOT_NOT_ALLOWED (A-43).
```

⚠️ **Use `purge_instance`, not two calls.** Chaining `kill_node` and then `remove_node_instance`
is a **race**: the kill answers `ok` before systemd brings the unit down, and the remove fails
with `NODE_INSTANCE_RUNNING`. And if that happens without anyone looking at the response, the
config **stays** and the node **reappears on its own** at the next start, because the
reconciliation relaunches it from the persisted config. The name goes **qualified**
(`<node>@<hive>`).

⚠️ **This recovery loses the node's configuration.** With `UNCONFIGURED` nodes it costs nothing;
with tokens loaded it is a real loss.

### Workflows: the deploy goes through the admin, and a broken WF node stays up (since 0.1.70)

- **How it is deployed:**
  - A workflow is deployed with `POST /hives/<hive>/wf-rules` (or `…/compile`, `…/apply`,
    `…/rollback`, `…/delete`) or from Archi.
  - SY.wf-rules launches, restarts and deletes the `WF.<name>@<hive>` node **through SY.admin**,
    like any other node, and only for the `WF.*` nodes of its own hive.
  - The node is left with `launched_by` = whoever asked for the deploy (`operator` over HTTP,
    `archi` from Archi): `get_node_status` shows it.
  - Deploying from Archi works since 0.1.70. Before that, it got stuck in the admin's loop and
    answered `PACKAGE_PUBLISH_FAILED` (A-68).
  - On a worker, the deploy first has the motherbee announce the package: it asks the motherbee
    to have Syncthing scan right away (`sync_hint` on `fluxbee-dist-runtimes`). Then it waits up
    to 45 s for the whole package, and repeats the scan before each retry.
    - Without that scan, the package once took 62 s: the motherbee's watcher batches the changes
      and is slow to announce them. The worker pulls the package as soon as the announcement
      arrives; a scan on the worker does not make it pull anything.
- **A WF node with a broken config or definition does not go down:**
  - it keeps running in `FAILED_CONFIG`;
  - `get_node_status` shows `health_state: DEGRADED` and `last_error` (`invalid_config`, or
    `invalid_definition` if the package's `flow/definition.json` is missing or does not validate);
  - you fix it by redeploying the workflow, or by correcting the config (`set_node_config`) and
    restarting it (`restart_node`).
  - Before, it went down and systemd relaunched it every 5 s, and the reason was only left in
    journald.

### A new system node in a role, with hives already joined (0.1.41: SY.opa.rules on ingress/egress)

A spoke's `hive.yaml` is written only once, at the join, and it is not re-issued. The spoke starts the nodes of **its own** `system_nodes.<role>` and validates them
against the core manifest that the motherbee sends it. That is why the order matters:

1. **Motherbee, before the `apt install`:** add the node to `system_nodes.<role>.nodes` in
   `/etc/fluxbee/hive.yaml` (the `.deb` does not overwrite your changes: its payload ships only
   `hive.yaml.example`; the postinst creates `hive.yaml` only if it is missing, and only fixes an
   `architect.listen` set to `0.0.0.0`, A-46). On restart, the orchestrator builds
   `dist/core/<role>` with the new binary, and Syncthing carries it to the spoke.
2. **Wait for the binary to reach the spoke:** `dist/core/bin/<binary>` present, with the sha256
   that `dist/core/manifest.json` states. If the spoke's `hive.yaml` names the node before that,
   the spoke's orchestrator does not start: it validates every binary of its role against the
   manifest, and that one is missing.
3. **Spoke:** add the node to `system_nodes.<role>.nodes` in its `/etc/fluxbee/hive.yaml`.
4. **`update category=core` to the spoke:** it installs the binary; the new orchestrator
   regenerates the units (it creates the node's unit) and starts it.

A hive that joins later needs none of this: the join copies the motherbee's
`system_nodes.<role>`. Neither does the `fluxbee-dist-policy` folder: it inherits the peers of
`fluxbee-dist-vendor` at the next start of the orchestrator on each side. **Only that one:** the
orchestrator does it by its fixed name; a new folder for all roles needs the same code (or
generalizing it).

### The rollback

Three paths:

1. **`core_rollback`** ([U-5](PENDING-BUGS.md#u-5)): `POST /hives/{hive}/core/rollback` returns
   a **spoke** to the generation that its last `update category=core` replaced (each binary
   sealed with its sha256), with a health gate (the unit active). It needs a previous update that
   changed binaries. The motherbee has no generations (dpkg installs its core): for it, as for a
   hive whose orchestrator is what is broken, use the next two. If it restores `sy-orchestrator`,
   the response may be `FORWARD_UNKNOWN`: check `core.installed.source = core_rollback` in
   `/versions`.
2. **Cold snapshot of the VMs** — see §1 on why cold. With the maximum of 3 per VM (§12), it
   covers only the recent past.
3. **Keep the previous `.deb` published** in the apt repo and downgrade with `apt`. It is the path
   for any old version.

**Downgrading from 0.1.63 or later to an earlier version:** Syncthing keeps running as `fluxbee`
(0.1.62 uses that user if it exists), but the 0.1.62 nodes write root-owned blobs that it cannot
read. If a worker produces blobs, switch it back to root: `systemctl stop fluxbee-syncthing`,
`userdel fluxbee` and `systemctl restart sy-orchestrator`. On the motherbee this is not needed,
because the 0.1.62 postinst runs `chown -R` again.

**Downgrading from 0.1.62 or later to an earlier version:** first the spokes (`core_rollback` or
an `update` to the old core), then the motherbee. A 0.1.62 worker facing an old motherbee gets no
heartbeats on its identity subscription: it drops the subscription every 60 s and asks for a full
snapshot, and the old motherbee only cleans up those dead subscriptions when it broadcasts a
change.

**Downgrading from 0.1.41 or later to an earlier version** (before the global OPA policy): first
`POST /opa/policy/clear` and wait for `converged: true`. Otherwise, the workers keep applying the
last global policy (the old version loads `current/` at startup, and its per-hive writes do not
reach the spokes), and the ingress/egress routers keep it in memory until a reboot. After the
downgrade, remove the `fluxbee-dist-policy` folder from Syncthing and restore the previous
`hive.yaml` (`hive.yaml.pre-opa-0-1-41`).

### Identities: mark, restore, purge (since 0.1.34)

Operator's decision (2026-09-28): ILKs and tenants are unique data, so **deleting only marks**,
and **purging** is a separate, deliberate step. Since 0.1.67 (stage 3,
`docs/ilk-tenant-model-v1.md` §2.3): what happens to a tenant never marks or deletes a person.

```bash
A=127.0.0.1:8080; H=motherbee
curl -X DELETE $A/hives/$H/identity/ilks/<ilk_id>                # mark (reversible, idempotent)
curl -X POST   $A/hives/$H/identity/ilks/<ilk_id>/restore        # undo the mark
curl -X POST   $A/hives/$H/identity/ilks/<ilk_id>/purge          # physical: requires the prior mark
curl -X DELETE $A/hives/$H/identity/tenants/<tenant_id>          # marks only the tenant (disables it)
curl -X POST   $A/hives/$H/identity/tenants/<tenant_id>/restore  # brings it back as it was
curl -X POST   $A/hives/$H/identity/tenants/<tenant_id>/purge    # physical: with no people charged to it
curl -X PUT    $A/hives/$H/identity/ilks/<ilk_id>/tenant -d '{"tenant_id":"tnt:…"}'  # charge to another tenant
curl "$A/hives/$H/identity/ilks?include_deleted=true"            # marked ones only show up this way
```

- **Marked = hidden but reserved.** It does not resolve (not in the SHM, not in the routers, not in
  Cloud's `get_ilk`). Its email, its channels and its `node_name`, or the tenant's name and
  domain, **stay taken** until the purge: reusing them gives `ILK_DELETED` or `TENANT_DELETED`.
- **Deleting a tenant marks nobody.** Its people stay usable and are charged to another tenant
  with `set_ilk_tenant` (a temporary only if its tenant is deleted). Restoring the tenant brings
  it back as it was.
- **Purging a tenant** is rejected while people are charged to it (`TENANT_HAS_PEOPLE`): first
  they are charged to another one. The ones that were deleted on their own go with it.
- **A pending or suspended tenant does not start nodes** (`TENANT_NOT_ACTIVE`); the ones already
  running are not killed, but they do not receive secrets.
- **Protections:**
  - the root tenant and the system ILKs are not touched;
  - a tenant with node identities (`TENANT_HAS_NODES`) or with sponsored tenants
    (`TENANT_HAS_CHILDREN`) is neither marked nor purged;
  - purging a tenant that still has secrets in the vault gives `TENANT_HAS_SECRETS`.
- **Fluxbee Cloud** can mark and restore, always scoped to the tenant it claims. **Purging is for
  the operator only**, and Archi cannot do it either.
- **The kill with `purge_instance`** marks and purges the node's ILK, so that the node's name is
  freed.

### Factory reset — `fluxbee-factory-reset` (2026-09-28)

It deletes **what the users created** and leaves the hive as `fluxbee-firstboot` left it, without
reinstalling. It is meant mainly for DEV and tests; in PROD, only on purpose and with a snapshot
first. It ships in the `.deb` and runs **on the motherbee**; it covers **the whole hive**.

```bash
sudo fluxbee-factory-reset                                  # DRY-RUN: lists what it would delete (default)
sudo fluxbee-factory-reset --yes                            # runs it; asks you to type the hive_id
sudo fluxbee-factory-reset --yes --confirm-hive motherbee   # runs it without asking
```

| Deleted | Kept |
|---|---|
| Nodes launched by users, on every hive (kill + purge: they go with their ILK, their secrets and their timers) | The installation: package, `/etc/fluxbee`, vault master key, TLS, mesh and spokes |
| User ILKs (humans, agents, and temporaries created by messages or by Cloud), **purged**, including those that an earlier delete only marked, and **every tenant except the root one** | What starts as system: `SY.*` nodes and their ILKs, and the base nodes from `base-nodes.json` with their ILK, ICH and config (the Cloud door stays published) |
| The message log: every table of `fluxbee_storage`, the SY.cognition cache, the conversation state (`thread-state/`, `immediate-memory/`) and the Archi sessions | Routes, VPNs and taps |
| Every vault secret except the infrastructure ones (`storage_postgres_url`, `edge_tls*`, `edge_channel_secret:*`). Since D32 the old `ssh:*` recovery keys are deleted too. **This includes the AI keys**: the AI stays degraded until they are loaded again | The operational history: the admin audit log, the deploy history and the vault audit |

- **Workflows:** they are deleted through their own path. On each hive with SY.wf-rules
  (motherbee and workers) it calls `wf_rules_delete` with `force`, which deletes the rule, the
  `WF.*` node, its state and its packages, including the instances in progress. An orphan `WF.*`
  node, with no workflow, is killed like any user node.
- **Identity:** since 0.1.34, deleting an ILK or a tenant only **marks** it. The mark is
  reversible, hides it and leaves its keys reserved. The **purge** is physical and requires the
  prior mark. The reset does both: mark and purge. The tenants go after the vault, because purging
  one that has secrets is rejected, and in passes: first the children and then their sponsors.
- **It does not touch yet:** the user OPA policy (the `opa_clear` action is missing, and so is the
  router unloading the policy) and the blobs.
- **It uses the product API** for everything, except the message log, which has no API: its
  tables are truncated with the consumers stopped.
- **It is safe to re-run** if something fails midway: each step deletes only what is still there.
  It leaves a line in `/var/lib/fluxbee/state/factory-reset.log`.
- It is not `scripts/fluxbee_cleanall.sh`: that is the **nuclear** wipe from the old
  install-from-source flow, which takes the installation away too.

---

## 13. What this handbook does NOT cover yet

Honesty about the scope — none of this was tested:
- **Hot-publishing and updating node packages** (`publish_runtime_package`), as distinct from the
  `.deb`. Only the `.deb` path was tested.
- A second work hive, or multi-hive over WAN on this infrastructure.
- Motherbee backup and restore (Postgres + vault).
- End-to-end certificate renewal.
- Recovery from the loss of motherbee.
