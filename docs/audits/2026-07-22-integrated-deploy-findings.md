# Integrated from-scratch deploy — findings (2026-07-22)

**Goal (user):** not that it "funcione" ("works"), but to **learn what to change so it ends up right, and to detect bugs**.
Full deploy from scratch on PC-004-157: motherbee (apt+firstboot) + worker + ingress (2 NICs) +
egress (NAT), on the internal apt repo (192.168.4.200). Each friction point is recorded as
**finding → what to change (template / firstboot / scripts / code)**.

Legend for where the fix goes: `[TEMPLATE]` base-image prep · `[FIRSTBOOT]` fluxbee-firstboot ·
`[DEPLOY]` add_hive/install.sh/scripts · `[CODE]` binaries/logic · `[DOC]` documentation.

---

## F-1 `[TEMPLATE]` — machine-id baked into the template ⇒ IP collision on every clone

**Symptom:** the 4 clones of template 9000 booted with the **same IP `192.168.4.74`** via DHCP.

**Root cause:** the template `ubuntu-2404-fb` (9000) has a **populated** `/etc/machine-id` (not
empty), and networking via cloud-init/DHCP. systemd-networkd/cloud-init derive the **DHCP
DUID/client-id from the machine-id**. Since all the clones share the machine-id, the DHCP server
hands them the **same lease** → IP collision (and potential identity collisions downstream).

**What to change `[TEMPLATE]`:** before turning the VM into a template, **empty** the machine-id so
that each clone regenerates a unique one on first boot (standard cloud-template practice). Script:

```bash
# run INSIDE the base VM, then shut it down and convert it to a template
truncate -s0 /etc/machine-id
rm -f /var/lib/dbus/machine-id && ln -s /etc/machine-id /var/lib/dbus/machine-id
cloud-init clean --logs --seed          # clears the cloud-init state (instance-id, net)
rm -f /etc/ssh/ssh_host_*               # regenerates unique host keys per clone (avoids identical SSH host keys)
truncate -s0 ~/.bash_history 2>/dev/null || true
```

**Workaround applied in this run (NOT the real fix):** manual regen of the machine-id + a unique
static IP per VM (`.210` MB, `.211` worker, `.212` ingress, `.213` egress) above the DHCP pool
(`.20–.150`). It lets the test go on, but the correct fix is in the template.

**Bonus finding:** the template's SSH host keys are also cloned identical → when the MB does the
SSH bootstrap to the clean boxes, they all present the same host key. The `rm ssh_host_*` above
covers it. Check whether the add_hive bootstrap pins/verifies host keys (see the next F- items).

---

## F-2 `[TEMPLATE]` — interrupted dpkg state in the base image ⇒ broken apt-install on the clones

**Symptom:** on the MB, `apt-get install fluxbee` aborted with
`E: dpkg was interrupted, you must manually run 'dpkg --configure -a'` (exit 100).

**Root cause:** the template was left with a half-finished dpkg transaction (probably from an
apt/reboot during the image prep). Every clone inherits that dirty state and cannot install
anything until dpkg is repaired.

**What to change `[TEMPLATE]`:** leave the package tree clean before making the template:

```bash
dpkg --configure -a
apt-get -f install -y
apt-get clean && rm -rf /var/lib/apt/lists/*
```

**Workaround applied:** `dpkg --configure -a` on each clone before the install.

---

## F-3 `[TEMPLATE]/[DOC]` — the clean box has neither sshd running nor a bootstrap user

**Symptom:** on the spokes (worker/ingress/egress) the template boots with **`ssh` inactive** and
**no `administrator` user** (only `root` + `fluxbee`). `add_hive` bootstraps over SSH
(`ssh_user` required, default `administrator`; `ssh_password` key-first) → with no sshd and no user,
the MB has no way in.

**Root cause:** the deploy model is "spoke = a clean Linux box with ONLY SSH, bootstrapped by
the MB" (correct for prod), but (a) the lab template neither enables sshd nor creates a bootstrap
user, and (b) the clean-box prerequisites are not documented as a contract.

**What to change:**
- `[TEMPLATE]` (lab): enable `ssh` + create the bootstrap user (`administrator`, sudo,
  a known password or the MB's pubkey) so that the clone works as a spoke without touching anything.
- `[DOC]` (prod): document the clean-box contract in 07-operaciones: **sshd active, a sudo
  user, and PasswordAuthentication on (if `ssh_password` is used) or the MB's pubkey in
  `authorized_keys` (key-first)**. Today it is not explicit.

**Workaround applied:** on 241/242/243 → enable sshd + create `administrator:magicAI` (sudo
NOPASSWD) + PasswordAuthentication yes.

**Nuance (SSH password auth):** the Ubuntu cloud image ships
`/etc/ssh/sshd_config.d/50-cloud-init.conf` with `PasswordAuthentication no`. sshd is *first-match*,
so a `99-*` drop-in does NOT override it (50 < 99 wins). You have to use a drop-in with a **lower**
number (e.g. `00-fbi.conf`) or edit the cloud-init one. Direct impact on `add_hive` with `ssh_password`:
if the deploy uses a password, this silently blocks it, even if the operator "has enabled"
password auth in a 99- file. Document it. Ideally the bootstrap should prefer **key-first**
(the MB's pubkey) and not depend on a password.

---

## F-4 `[CODE]/[DOC]` — `externalize` / edge probe not reachable on an installed backend

**Symptom:** to close the edge request-flow (`curl /e/<ich>`) an IO node has to be
**externalized**. But: (a) `externalize`/`list_externalized` are NOT in the admin's REST API (`POST /admin
{action}` → `not_found`; there are REST routes only for `/hives`, `/hives/*/nodes`, `/hives/*/vault/*`);
(b) they live in the **internal-action dispatcher** (socket `admin.sock`), which the e2e invokes with a
**dev** binary `target/debug/admin_internal_command_diag` (not in the `.deb`); (c) `sy-admin
--help` on the MB **hangs** (it is not an action CLI). In other words: on an installed box there is no
"out-of-the-box" way to externalize/probe the edge without dev tools or `deploy-io-api.sh` from a
checkout.

**What to change `[CODE]/[DOC]`:** expose the channel actions through an operational path — either REST
(`POST /channels/externalize`), or a real `sy-admin` subcommand (today it hangs), or a mini socket
client packaged in the `.deb`. Document the "publish a node to the edge" flow in
07-operaciones. Today the operator has no obvious path.

**Edge status (verified):** `SY.edge@ingress1` active, `:8443` LISTEN, answers `404` to
`/` cross-hive from the MB (edge alive, no channel published). Only the step of publishing an ICH
is missing, blocked by the above. **Full request-flow = next test** (via `deploy-io-api.sh
--edge-base http://192.168.4.212:8443` from a checkout, or the operational path of F-4).

---

## F-5 `[CODE]` (minor) — `role` is not recorded for the worker (default) in `/hives`

**Symptom:** in `GET /hives`, `ingress1 role=ingress` and `egress1 role=egress`, but `worker1
role=None`. The default role (worker) is not stamped/echoed in the hive inventory.

**What to change `[CODE]`:** stamp `role="worker"` explicitly when provisioning a default spoke,
so that the inventory is consistent (today "worker" has to be inferred from the absence of a role).

---

## Summary — integrated from-scratch deploy on PC-004-157

**Topology brought up from zero** (apt install from the internal repo → firstboot → add_hive):

| Hive | Role | Status | Notes |
|------|-----|--------|-------|
| motherbee (240 @.210) | motherbee | alive, 18/18 nodes active | core SY.* + orchestrator + rt.gateway (:9000) + edge + IO/AI |
| worker1 (241 @.211) | worker | connected, api_healthy, dist-sync ok | bootstrap SSH + vendor-push OK |
| ingress1 (242 @.212) | ingress | connected, edge :8443 LISTEN | **dual-NIC** eth0 .212 + ens19 10.10.10.12 ✓ |
| egress1 (243 @.213) | egress | connected, NAT applied | **NAT** nft masquerade 10.10.10.0/24→eth0, ip_forward=1 ✓ |

**Works:** the full infra deploy from zero, the 4 roles, mesh mTLS (:9000, required),
orchestrator, rt.gateway, edge (listen+reachable), dual-homed ingress, egress NAT.

**What we learned has to change:** F-1 (template machine-id → IP collision), F-2 (template with
interrupted dpkg), F-3 (clean box without sshd/bootstrap user + cloud-init overrides PasswordAuth),
F-4 (no operational path to externalize/probe the edge on an installed backend), F-5 (the
worker's role is not stamped). F-1/F-2/F-3 are **template/prep fixes** → the scripts go to the user.

---

## Resolution (2026-07-22) — fixes implemented

Triage verified against the code (6-lens workflow) → 11 mechanical fixes + 2 decisions. Findings
F-1/F-2 and part of F-3 are **lab-template artifacts** (a clean Ubuntu cloud image does not have
them) → covered by `lab/template-prep.sh`; they are not fluxbee bugs. Fixes to the code
and the packaging:

| Fix | What was done | Where |
|-----|-------------|-------|
| **G1** | `curl` + `python3` added to Depends (firstboot requires them; without them the first boot was a silent no-op) | build-deb.sh |
| **G2** | firstboot: explicit error + exit 1 if the `vault_put` of the postgres secret runs out of retries (before, it failed silently) | fluxbee-firstboot |
| **G3** | build-deb fails closed if the vendored syncthing is missing (opt-out `FLUXBEE_ALLOW_NO_SYNCTHING=1`) | build-deb.sh |
| **G4** | Anthropic provider note (the default is openai) in the firstboot banner + docs | firstboot, packaging-and-build.md |
| **G5/G6** | docs: wf.engine does not start at boot; only sy-orchestrator stays enabled (it brings up the rest) | packaging-and-build.md, 07-operaciones.md |
| **F5** | stamp `role:"worker"` in the worker's 3 info_payload (before, `/hives` showed role=None) | sy_orchestrator.rs |
| **F4b** | `--help`/`--version` guard before the runtime init (before, `sy-admin --help` hung forever) | sy_admin.rs |
| **F3** | password-channel failure → classified as `SSH_AUTH_FAILED` + a cloud-init `hint` (before, a silent `SSH_KEY_FAILED`) | sy_orchestrator.rs |
| **F3-docs** | §8.0 clean-box contract + §8.1 note on cloud-init PasswordAuth first-match | 07-operaciones.md |
| **F4a** | REST ops `/channels/externalize|unexternalize` + `GET /channels/externalized` (localhost, caller=None trusted-internal like add_hive/vault_put) | sy_admin.rs |

**Decisions (low risk, non-blocking):** model-id `gpt-5.5` in hive.yaml.example → **left as is**
(the user's choice, not verifiable from code). syncthing → **fail-closed** (matches install.sh).

Build green; tests: sy_admin 85/85, sy_orchestrator 132/132 + 1 new (ssh_bootstrap_error).
Code changes adversarially reviewed before committing.

---

## Clean-slate re-test (2026-07-22) — fixes VALIDATED LIVE, end-to-end deploy from zero

**Everything was torn down and recreated** with the fixes in: `.deb` rebuilt (64117d3) + template fixed
with `lab/template-prep.sh` (VM 250) + integrated hive recreated from zero on PC-004-157 (MB + worker +
dual-NIC ingress + egress NAT), **without any manual workaround**.

| Finding | Before (1st deploy) | Now (fixed template/binaries) |
|---------|--------------------|--------------------------------------|
| **F-1** machine-id | the 4 clones collided on `.74` | **unique IPs via DHCP** (`.25/.97/.77/.47`) — resolved |
| **F-2** dpkg | broken `apt install` (`dpkg was interrupted`) | **install OK without `dpkg --configure -a`** |
| **F-3** clean box | no sshd and no user → manual setup | **sshd+administrator+PasswordAuth from the template**; add_hive bootstrapped cleanly |
| **G1** deps | firstboot was a no-op | curl+python3 present; firstboot `first-boot done` |
| **F-4b** cli | `sy-admin --help` hung | **answers exit 0** with the help |
| **F-5** role | `worker1 role=None` | **`worker1 role=worker`** in `/hives` |
| **F4a** externalize | unreachable on an installed backend | **`GET/POST /channels/*` work**: `list_externalized` via REST = ok; public externalize via REST = ok |
| **Edge request-flow** | never closed | **`curl POST :8443/e/<ich>` = HTTP 200** with a structured response from io.api (edge→cross-hive→MB io.api→reply) |

Final mesh: motherbee(alive, 18 nodes) + worker1 + ingress1(dual-NIC, edge :8443) + egress1(NAT).
An extra bug found + fixed along the way: `scripts/apt-repo-publish.sh` + `lab/template-prep.sh`
had no +x bit (commit 5be0ae5). Commits: 64117d3 (fixes) + 5be0ae5 (+x).

**Minor observation (io.api, outside the installer scope):** io.api's inbound answers
`field 'message' is required` even when sending a top-level `{"message":...}` → the field is expected
nested (envelope). It gives HTTP 200 + a structured reply (edge OK); it is an ergonomics issue of io.api's payload contract.
