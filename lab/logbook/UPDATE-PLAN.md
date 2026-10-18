# UPDATE-PLAN — the sequence to get the fluxbee update working

> **Why this document exists.** Operator's decision (2026-07-30): *"para resolver los problemas
> en prod sería mejor tener el update andando bien, y después ir por los bugs"* ("to solve the
> problems in prod it would be better to have the update working well, and then go after the
> bugs"). Without an operable update, every fix requires reinstalling from scratch — and that does
> not scale, not even for a test prod.
>
> **Sibling documents:** [`HANDBOOK.md`](HANDBOOK.md) (installing from scratch, already validated) ·
> [`PENDING-BUGS.md`](PENDING-BUGS.md) (the findings, the `U-*` series) · [`METHOD.md`](METHOD.md).

---

## 1. The framing: minor and major

The operator's decision, and it narrows the problem at a stroke:

| | what it is | how it is solved | status |
|---|---|---|---|
| **major** | a big change, or something broke | **install from scratch with the `.deb`** | ✅ validated, recipe in the HANDBOOK |
| **minor** | a small change that should not break prod | **hot update** | ❌ this is what has to be made to work |

**The good consequence:** if majors are solved by reinstalling, **the update system only has to
handle minors well.** It needs no complex migrations, no perfect rollback, and no compatibility
between distant versions. It is a much smaller problem than "a complete update system".

**The principle that governs it** (the operator, verbatim): *"nada es importante para mantenerlo, solo el
trabajo de tener que hacerlo de vuelta. Ese trabajo no vale la pena para dejar cosas raras en el
diseño de fluxbee."* ("nothing is important enough to keep, only the work of having to do it
again. That work is not worth leaving odd things in the design of fluxbee.") The prod DNS and
certificate are test ones. **Avoiding redoing work is not a reason to bend the design.**

**So the rollback is external and simple:** a snapshot of the VMs. fluxbee does not need a
perfect rollback for this to move forward.

### The snapshot recipe — cold, and why

Proxmox snapshots are **per-VM: there is no consistency group.** fluxbee has state on all four
machines (Postgres + vault on motherbee; `hive.yaml`, TLS and the identity HMAC key on each
spoke). Four hot snapshots taken milliseconds apart can leave a **torn** state on rollback:
Postgres remembering a join that the spoke's disk does not, or divergent HMAC keys that break
mesh authentication. A snapshot with RAM also captures the identity replica in SHM, which may
disagree with the disk on the way back.

**With the VMs powered off, the problem disappears by construction**, and the downtime is free
because prod serves nobody:

```bash
# 1. power off: spokes first, motherbee last
for v in 101 102 103 100; do POST /nodes/pve/qemu/$v/status/shutdown; done   # wait for each task

# 2. snapshot all 4 (cold, WITHOUT vmstate)
for v in 100 101 102 103; do
  POST /nodes/pve/qemu/$v/snapshot  snapname=pre-update-0.1.1  description="..."
done

# 3. start: motherbee first
for v in 100 101 102 103; do POST /nodes/pve/qemu/$v/status/start; done

# rollback, if needed: power off all 4 -> roll back all 4 -> start
```

**Space:** `local-lvm` is LVM-thin (it supports snapshots), with **156 G free** out of 186 G and
only 29.8 G actually used. Plenty.

**The second rollback, a complement:** keep the **previous** `.deb` published in the `fb-build`
apt repo, so you can downgrade with `apt`. Today it is the only possible downgrade of the
product, because there is no core rollback command ([U-5](PENDING-BUGS.md#u-5)).

---

## 2. What is already built (and works better than expected)

It is worth saying before the list of problems: **no machinery is missing.**

```
build .deb          ->  apt install on MOTHERBEE  ->  syncthing replicates dist/  ->  POST /hives/{h}/update
  [publishing]            [local install]                [transport]                     [applying, per hive]
```

- `build-deb.sh` bakes each binary **twice**: `/usr/bin/<c>` and `dist/core/bin/<c>`, plus a
  `dist/core/manifest.json` with the **real build-time sha256 and size**.
- The transport is **syncthing** (folder `fluxbee-dist`, `sendonly` on MB / `receiveonly` on the
  spoke), and it can be forced/awaited with `POST /hives/{h}/sync-hint {channel:"dist"}`.
- Applying is **local on the target**: diff by sha → backup of the previous binaries → copy with
  staging + **atomic** `rename()` → regeneration of the units → health gate → local rollback on
  failure. The orchestrator restarts itself deferred, so it does not cut itself off.
- A *staleness* gate by hash: if the local hash does not match the requested one, it answers
  `202 sync_pending` instead of applying something halfway.
- For **runtimes** there is even fanout: `publish_runtime_package` with `sync_to[]` /
  `update_to[]`, strict semver, and **byte-for-byte idempotent** installation.

The problem is **two specific defects that make all of this inoperable** — and both are
silent.

---

## 3. The two blockers

### 🔴 [U-1](PENDING-BUGS.md#u-1) — the update reports success without restarting anything

`restart_local_core_services_with_health_gate` calls **`systemctl start`** on services that are
already running: a no-op, systemd returns success, and the process keeps the old binary. And the
name is pushed onto the `restarted` list that the API returns. **There is no `systemd_restart` in
the file**; the remote paths do use `systemctl restart` → **the local path is the divergent one.**

It is the worst kind of failure: the binaries *are* changed on disk, so the update "works"… at
the next reboot. You can run it, see `ok`, reboot the machine days later for some other reason,
and conclude that the mechanism works fine.

**It is blocking in the literal sense: you cannot test a mechanism whose success signal is false.**

### 🔴 [U-2](PENDING-BUGS.md#u-2) — ingress and egress have no dist folder

`dist.sync.enabled: false` is **hardcoded** in the `format!` calls for the `hive.yaml` of those two
roles (orchestrator:18211 and :18839), while the worker has it parameterized (:17440). Verified
on the machines: the worker has `fluxbee-dist` in `receiveonly`, the ingress **does not have it**.

But syncthing **already runs on the ingress and is already paired** — it receives
`fluxbee-blob-public` in receive-only. The folder only has to be shared with it, and **that does
not clash with the DMZ invariant P5**, which is specific to `blob/active`.

It matters because `sy_edge` —the public door— lives right there.

---

## 4. The sequence

**Rule:** prod only sees a `.deb` that has already passed the dev phases. And **measure first, fix
afterwards**, so that evidence of the bug remains instead of only the fix.

### PHASE 0 — Make the update *measurable* (dev, without touching prod)

1. **Write the missing assert**, which today has to **fail**:
   ```bash
   p=$(systemctl show -p MainPID --value sy-config-routes)
   readlink /proc/$p/exe    # "(deleted)" => it is running the old binary
   systemctl show -p ActiveEnterTimestamp --value sy-config-routes
   sha256sum /usr/bin/sy-config-routes   # vs dist/core/manifest.json
   ```
   That it fails **is** the proof of U-1. Without this assert there is no way to tell a real
   update from a phantom one.
2. **Fix U-1** (one line, aligning with the remote path). *To discuss first:* `restart` vs
   `stop`+`start` changes the spoke's unavailability window.
3. **Fix U-2** (parameterize the two `format!` calls + the folder peering). *To discuss:* whether
   the default for those roles is `true`, and **how the folder is added to boxes that already
   joined** without redoing the join.

### PHASE 1 — dev: upgrade of the `.deb` on motherbee

Cold snapshot first. Before the upgrade, **set the [U-3](PENDING-BUGS.md#u-3) trap**:
hot-publish a runtime and check that it appears in the manifest.

**Success:** 19 nodes `active`, 0 `failed`, `GET /versions` with the new hash, the admin answers.
**It is expected to fail** (and that has to be documented): the hot-published runtime
disappeared from the manifest → U-3 confirmed live.

### PHASE 2 — dev: `category=core` to the worker

```bash
curl -sS $H/hives/motherbee/versions            # new hash (origin)
curl -sS -X POST $H/hives/worker1/sync-hint -d '{"channel":"dist","wait_for_idle":true}'
curl -sS $H/hives/worker1/versions              # must converge
curl -sS -X POST $H/hives/worker1/update -d '{"category":"core","manifest_hash":"<new>"}'
```

**Success = `status: ok` AND the PHASE 0 assert green** (`exe` not deleted, a later
`ActiveEnterTimestamp`, sha == manifest) **AND** the spoke stays `connected`. Also test the
negative path: wrong hash → `202 sync_pending`.

### PHASE 3 — dev: ingress and egress

With U-2 fixed, the same `update category=core`. This is the phase that matters most: **it is the
role where the public door lives**, and the one that has no path today.

### PHASE 4 — dev: node packages

Publish → confirm that the running instance did **not** change (that is the contract, not a bug)
→ rebind `runtime_version` + `restart_node` → verify the new binary **and the preserved config**.

### PHASE 5 — prod, only with the previous phases green

Proposed payload for `0.1.1`: `98adab7` (TLS cert reload) + the U-1 fix + whatever gets resolved
of U-2. **All three are changes that do not touch the protocol**, which matters because of
[U-4](PENDING-BUGS.md#u-4).

Order: cold snapshot of all 4 → check that the apt repo keeps `0.1.0` → build on `fb-build`
(~55 min, `nohup`) → `apt-get install` on motherbee → `sync-hint` + `update` to the worker with
the assert → ingress/egress → **external validation of the edge** with the strict test without
AIA.

**The success criterion for all of this** is nice because it is binary and I already have it
ready: rotate the cert in the vault and see whether `sy-edge` picks it up **without anyone
restarting it by hand**. Today it does not. If it does after the update, the update really
worked — not because the API said `ok`.

---

## 5. About the dev lab

**The operator already defined it:** *"el dev no tiene que ser fiel, tiene que ser útil. Estamos en
terraforming."* ("dev does not have to be faithful, it has to be useful. We are terraforming.")
That is enough — dev does not need to reproduce prod.

**It is good for** the pure fluxbee plane (§4–§9 of the HANDBOOK): it has the 4 roles (VM240–243)
and a build VM with an apt repo, with access through the API + guest-agent, and snapshots
available. It is the right place for phases 0–4.

**It does not reproduce** the nested virt on VMware, the ESXi port security, the SDN with SNAT,
the real public leg, or the Sectigo TLS. And the hardware is **~8× faster** (Rust release
6.5 min vs 55 min) → **no time or timeout measurement from the lab carries over to prod.**

To exercise the cert fix in dev you need to seed **some** cert into the vault of the dev ingress
— a self-signed one is enough, because what is being tested is the reload mechanism, not the
chain.

**Out-of-date items worth fixing along the way** (found in the audit): `lab/STATUS.md` is from
2026-06-25 and describes the **Docker** lab, not the VMs; `METHOD.md:24` says prod has no build
box and brings the binaries from the lab, **false since 2026-07-29**; and
`docs/packaging-and-build.md:158-164` lists servers whose VMs were deleted. The concrete risk of
the second one is that someone takes a `.deb` from the lab to prod and the commit traceability
is lost.

---

## 6. Risks, in order

1. **Phantom update (U-1).** Already explained: a lying `ok`. **Mitigation: do not run a
   `category=core` in prod without the fix + the assert.**
2. **The prod ingress has no update path today (U-2).** And it is where the public HTTPS lives.
3. **A core-down window on motherbee.** The `prerm` stops the 17 units without telling an upgrade
   from a remove; the `postinst` starts 3, and the rest depends on `bootstrap_local` — which is
   validated after a **boot**, not after an **upgrade**. The public HTTPS survives (the edge is on
   another machine), but everything that needs the mesh fails during the window. **Mitigation:
   snapshot + compare `system_nodes` between the new `hive.yaml.example` and the live `hive.yaml`
   before updating.**
4. **Silent loss of hot-published runtimes (U-3).** Today the damage would be **zero** (the 6 prod
   runtimes came from the `.deb`), but as soon as something is hot-published, every upgrade
   deletes it. **It is an argument for testing the update now, while prod is pristine.**
5. **No compatibility check (U-4) and no core rollback (U-5).** Mitigation: a payload without
   protocol changes, a short window, snapshots, and the previous `.deb` kept in the repo.

**Expected noise that is NOT a failure:** `sy-edge` can crash-loop up to ~9 times while waiting
for the vault on a cold start ([PB-4](PENDING-BUGS.md#pb-4)). Wait ~30 s for convergence before
diagnosing.

**What is not known and has to be measured, not assumed:** how long the `apt install` of the
240 MB `.deb` takes on prod's slow hardware, and whether the `prerm`/`postinst` clash with
`TimeoutStopSec=15`; whether `bootstrap_local` brings the 19 nodes back up on its own after an
**upgrade**; and whether the VMs' guest-agent survives the upgrade without repeating the A-5
cycle.
