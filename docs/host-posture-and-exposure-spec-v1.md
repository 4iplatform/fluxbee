# Host posture and exposure — design v1 (revision 6)

**Status:** design agreed with the operator on 2026-10-05. Stage 1 is live (0.1.57, validated on 8.x).
Revision 6 folds round 3 into stages 2–5, A-48 and A-49. They go through DTAP round 4 before any
of them is built.

**History:**

| Revision | What happened |
|---|---|
| 1 | Failed the first DTAP panel on all four lenses (75 findings). |
| 2 | Folded those findings in. |
| 3 | Recorded the operator's answers and the order of work. |
| 4 | Part A of revision 3 failed a second panel (84 findings). Most came from per-hive IP filtering and from the machinery added to steer spokes. Revision 4 **simplifies Part A**: rules by port and interface per role, an automatic mode, no configuration. |
| 5 | A third panel on Part A of revision 4 found 64 issues. Its 6 blockers are all in stages 4–5 and A-48; stage 1 needed only details. Stage 1 is built (§3.4 as built), with the A-48 guard against the spoke-only actions moved into it. The Docker lab is removed (D23). §8 lists what round 3 requires before stages 2–5, A-48 and A-49. |
| 6 | Round 3's findings folded into stages 2–5, A-48 and A-49 (see §8). The main changes: evidence counts only what a drop would break, i.e. packets to a port that has a listener, plus SSH logins through non-internal interfaces; the mode is a persisted pure function; ICMP errors come before conntrack (PMTU); A-48 admits unpinned pre-existing hives; each host installs its own nftables; `remove_hive` and downgrades keep the last ruleset. |

Every finding and where it went: `docs/audits/2026-10-05-host-posture-dtap-panel.md`.

The operator approved the narrowed D3 and D16 on 2026-10-05.
Part A goes through a third round before stage 1.

**Scope:** what each hive exposes on the network, who sets and verifies it, and how operators and
the public reach an installation from outside:

| Part | Phase | Content |
|---|---|---|
| A | core | Host posture owned by the orchestrator: firewall, bind addresses, Syncthing. Plus A-48 and A-49. |
| B | Cloud | Web exposure through IO.web: Archi for operators, tenant content. |
| C | Cloud | Public naming: one public ID per motherbee, DNS, certificate. |

**Findings:**

- A-46: Archi has no auth and the ingress reaches it; nothing filters inbound traffic.
- A-47: Syncthing uses public infrastructure.
- A-48: `remove_hive` revokes nothing.
- A-49: the apt repo is unsigned.

**Related:** `io-web-spec-beta-v1.md`, `edge-ingress-spec-v6.md`, `edge-egress-nat-spec.md`,
`fc-edge-manager-spec.md`, `edge-control-protocol-v2.md`, `io-cloud-spec-v1.md`,
`lab/logbook/HANDBOOK.md` §8.

---

## 0. Summary

An installation has no declared exposure model today:

- no hive filters inbound traffic;
- core services bind every interface;
- Archi (no auth, full admin power) is reachable from the DMZ;
- Syncthing announces every hive to public discovery servers.

**One owner.** The orchestrator computes the posture of its host from its role alone, applies it,
and verifies it every time it runs; drift is reported. In the operator's words: *"el que tiene que
setear todo lo del entorno en ese sentido es el orchestrator, como concepto al correr verificaría
eso"*.

**What a host accepts:**

- the ports its role serves;
- SSH on its internal interface;
- the edge port from anywhere, on the ingress only.

Nobody configures anything. A host first observes and later enforces by itself.

**Archi.** It listens on loopback only. Operators reach it on the motherbee, through an SSH tunnel,
or later from Fluxbee Cloud through IO.web (Part B).

**What this does not fix (stated so nobody assumes it does):**

- **Mesh control plane.** Any orchestrator, the DMZ ingress included, can still send SPAWN, KILL,
  NODE_CONFIG_SET and SYSTEM_CORE_ROLLBACK to any hive (A-20, A-22).
- **Flat internal network.** The ingress, the motherbee, the workers, fb-build and the hypervisor
  gateway (10.10.10.1) share one L2.
  - Any host on it can reach the motherbee's mesh ports and SSH.
  - The mesh ports are authenticated (mTLS, HMAC, Syncthing device IDs); SSH relies on sshd.
  - A compromised ingress can impersonate other LAN addresses.
  - Isolating it needs infrastructure configuration, so it is not done now (D19).
- **SSH host keys.** The motherbee's SSH client accepts any host key. A compromised ingress that
  takes a spoke's address can receive that spoke's mesh certificate the next time the motherbee
  reconciles TLS material. A-48 closes this.
- **SSH auth.** The VM template ships `PasswordAuthentication yes`, a password published in the
  repo and NOPASSWD sudo. Spokes are hardened only with `harden_ssh:true`. The posture reports it;
  the fix is A-2.
- **Unsigned apt repo** (A-49). A LAN attacker impersonating the repo gets root on the motherbee at
  the next upgrade.
- **Recovery is console only.** Default joins leave the motherbee no SSH key on spokes, and
  `remove_hive` cleanup travels over the mesh.
- **No `nft`.** A host without it is reported `unavailable` and is not filtered.
- **A hive removed while offline** keeps its services. A removed ingress keeps serving its public
  port as long as DNS points at it: A-48 only bars it from the mesh.
- **fb-build will hold the apt signing key** on the flat L2 the DMZ reaches, outside any posture.
  A-49 stops impersonation of the repo, not a compromise of fb-build.
- **The PROD motherbee accepts password SSH** for `fluxops` (cloud-init's `50-` file wins over
  `60-`). The posture reports it; the fix is A-2.
- **Stage 7.** It adds a session-gated path to Archi through the edge. How much a compromised
  ingress can do with it is decision O5.

---

## 1. Current state (verified 2026-10-05, PROD 0.1.56, read-only)

### 1.1 What listens where

Interfaces in PROD are static. DEV hives use DHCP (`lab/pve.py:176-177`).

| Host | Interfaces |
|---|---|
| `fb-mb` | eth0 10.10.10.10/24 |
| `fb-worker1` | eth0 10.10.10.20/24 |
| `fb-ingress` | eth0 10.10.10.30/24 · eth1 71.182.182.80/24 (public) |
| `fb-egress` | eth0 10.10.10.40/24 · eth1 192.168.8.240/24 (shared office LAN) |

10.10.10.0/24 is flat. It also holds `fb-build` (10.10.10.50), which serves the apt repo, and the
Proxmox host (10.10.10.1).

| Host | Non-loopback listeners | Notes |
|---|---|---|
| motherbee | 22 sshd · 3000 sy-architect (`0.0.0.0`) · 9000 rt-gateway (`0.0.0.0`, mTLS) · 9100 sy-identity (`0.0.0.0`, HMAC) · 22000 tcp+udp, 21027 udp and a random udp port (syncthing) | Archi has no auth (A-46) |
| worker | 22 · 22000 · 21027 | |
| ingress | 443 sy-edge · 22 · 22000 · 21027 | all of them also on eth1 (public) |
| egress | 22 · 22000 · 21027 | all of them also on eth1 (office LAN) |

Loopback only, correct today: 4222 embedded NATS, 5432 postgres (motherbee), 8080 sy-admin
(motherbee), 8384 Syncthing GUI/REST.

**Who connects to whom** (checked against the code by the panel):

| Flow | Direction | Port | Notes |
|---|---|---|---|
| Router WAN | spoke → motherbee | 9000/tcp | Spokes get `wan.uplinks` and no `wan.listen`. |
| Identity sync | worker → motherbee | 9100/tcp | Ingress and egress run no SY.identity. |
| SSH | motherbee → spoke | 22/tcp | At `add_hive`, and attempted at every motherbee orchestrator start (`reconcile_hive_tls_material`). The motherbee keeps a key only with `ssh_access=key_only_persist`; default joins revoke it. No hive SSHes into the motherbee. |
| Syncthing | both directions today | 22000/tcp, QUIC on 22000/udp, 21027/udp | |
| Edge | internet → ingress | edge port | |
| Egress NAT | worker → egress → internet | — | Forward/NAT. |
| Outbound | every host → internet | — | DNS, NTP, apt, AI, Slack, Meta; replies through conntrack. |
| io.linkedhelper (when instantiated) | Cloud and the LinkedHelper adapter → node | its own HTTP listener | Outside the edge (§1.6). |

### 1.2 Host firewall: present in code, inert in practice

**Nothing filters.** `ufw` is inactive on the four hosts.

**The orchestrator's ufw rules are inert.** It "opens" ports through ufw, from any source:

- **Core rules** (`ensure_core_firewall_local`, `sy_orchestrator.rs:5802`): only the motherbee gets
  them.
- **Syncthing rules** (`ensure_syncthing_firewall_local`, `:5794`): on every role.

With ufw inactive they are saved and never enforced. Enabling ufw would block SSH everywhere and
443 on the ingress. Core rules are never removed. `open_firewall_rules_local` (`:5660`) also drives
firewalld.

**Egress table.** `table inet fluxbee_egress` (`:6085`) has:

- an input chain with `policy accept`;
- forward with `policy drop` and LAN→WAN accept;
- masquerade.

How it is applied:

- `reconcile_egress_nat` (`:6444`) applies it with add + flush + define;
- the watchdog re-applies it about every 60 s (`:2400-2441`);
- a boot unit loads it before `network-pre.target` (`:6264`).

**nftables is not a dependency.** The `.deb` does not depend on it. Only the egress checks for
`nft`, locally, and its orchestrator fails if it is missing (`:6447-6450`).

**Leftovers.** Empty `ip filter` / `ip6 filter` tables (iptables-nft) exist on every host.

### 1.3 Bind configuration

**Archi**

- **Default and overrides.** The code default is `127.0.0.1:3000` (`src/bin/sy_architect.rs:84`).
  `JSR_ARCHITECT_LISTEN` or `architect.listen` overrides it (`:5975-5984`).
- **Where `0.0.0.0` ships:** `packaging/hive.yaml.example:45`, `config/hive.yaml:33`,
  `docs/07-operaciones.md:150`.
- **Docs that point at the open port:** `packaging/fluxbee-firstboot:156`,
  `docs/packaging-and-build.md:232`, `lab/logbook/HANDBOOK.md:445`.
- **The example does not reach installed hosts.** The postinst copies it only when
  `/etc/fluxbee/hive.yaml` is missing (`packaging/deb-postinst:50`).
- **Routes** (`:6067-6082`): chat, executor, publish/upload, attachments, agent assets, sessions,
  messages (SSE at `/api/messages/stream`), and a `/*path` catch-all.
- **Uploads** go up to 128 MiB (`:101`).
- **UI.** Its URLs are relative to the page path (`:17194-17204`). Its text tells operators to call
  `http://MOTHERBEE:8080` (`:17125`).
- **No CSRF defence.** Archi has none, and it parses any POST body as JSON whatever the
  Content-Type.

**Admin**

- `JSR_ADMIN_LISTEN` or `admin.listen` (`src/bin/sy_admin.rs:555-558`), `127.0.0.1:8080` in the
  example, any address accepted.
- No HTTP authentication (`:5962-5993`).

**Docker lab** (removed on 2026-10-06, D23)

- It rebound the admin to `0.0.0.0` inside the container.
- It published 8080, 3000 and 19091-19100 on all host addresses.

**Other listeners**

- **Identity sync:** hard-coded `0.0.0.0:<port>` (`src/bin/sy_identity.rs:3766`).
- **WAN:** `wan.listen` (`0.0.0.0:9000` in the example).
- **Syncthing GUI:** loopback (`sy_orchestrator.rs:6714`).

**SSH**

- The VM template sets `PasswordAuthentication yes`, with a password published in the repo and
  NOPASSWD sudo (`lab/template-prep.sh:42`, `:46-47`).
- Spokes are hardened only with `harden_ssh:true`; the default is false (A-2).
- On Ubuntu 24.04 sshd is socket-activated (`ssh.socket`).

### 1.4 Syncthing

**Options.** On the four hosts these are all `true`, and the orchestrator never sets them:

- `globalAnnounceEnabled`
- `localAnnounceEnabled`
- `relaysEnabled`
- `natEnabled`
- `crashReportingEnabled`

`listenAddress` is `default`.

**Consequences.** Every hive announces itself to public discovery servers, may use public relays,
tries UPnP/NAT-PMP, sends crash reports, and broadcasts local discovery on every interface.

**The install paths diverged.** `vendor/syncthing/config.xml` already has public infrastructure
off (with `reconnectionIntervalS` 20), but only the dev install path seeds it
(`scripts/install.sh:628`). `.deb` hosts and spokes run Syncthing's defaults.

**How the orchestrator edits Syncthing:** `config.xml` by regex, then a restart, from three places
without a lock. It reads `/rest/system/connections` (`:6841-6866`) and checks only
`connected=true`.

**Peer addresses:**

| From → to | Address |
|---|---|
| ingress → motherbee | static |
| egress → motherbee | static |
| motherbee → ingress | static |
| motherbee → egress | `dynamic` |
| motherbee ↔ worker1 | **`dynamic` on both sides** |

The worker join passes no address on either side, so that link exists only through discovery.

**Address handling:**

- `ensure_syncthing_top_level_device_in_config_xml` writes `dynamic` when it gets no address
  (`:7340`).
- A spoke-side finalize without an address rewrites an existing one to `dynamic` (`:7365-7390`).
- The current address helper accepts only IP literals (`:7055-7071`).
- The egress registry entry has no `syncthing_device_id`. The motherbee names each spoke device
  after its `hive_id` (`:20802-20811`).

### 1.5 Edge, certificate, DNS, Cloud trust

**Edge**

- SY.edge loads one certificate (`src/bin/sy_edge.rs:1880`) and routes by path on any Host
  (`:1737-1740`).
- No SSE or WebSocket.
- `/public/<key>` HTML is sandboxed (`:2384-2390`).
- Publication is single-edge (externalize, channel secrets, `admin.public_edge_node`, IO.cloud's
  single trusted edge).
- sy-edge runs as root.

**Certificate and DNS**

- PROD's certificate is the brand wildcard `*.fluxbee.ai`, held on a DMZ host. Edge-control v2 C1
  rules that out.
- The hostname `hive-k3m9x7q2.fluxbee.ai` and its A record were made by the operator by hand. DNS is
  Azure, managed manually.
- A key change in the vault restarts the edge (PB-1). Nothing watches expiry.

**Cloud trust today**

- The edge checks Cloud's bearer (`sy_edge.rs:2590-2603`) and strips `Authorization` (`:2929-2944`).
- IO.cloud trusts the router-stamped edge source (`nodes/io/io-cloud/src/main.rs:768-788`).
- So the DMZ host is the trust anchor of every IO.cloud action, and it sees the replies in clear.

**Mesh certificates and removal**

- Mesh leaves carry only the `hive_id`, are never pinned, and are valid until the year 4096.
- With `wan.authorized_hives` empty, the router admits any CA-valid peer (`src/router/mod.rs:2853`).
- `remove_hive`:
  - keeps the HMAC key;
  - unlinks Syncthing only when a device id was recorded;
  - never stops sy-edge on an ingress.
- REMOVE_HIVE_CLEANUP has no role check, and any orchestrator may send it (A-20/A-22).

### 1.6 IO.web and listening runtimes

**IO.web.** Spec only (`io-web-spec-beta-v1.md`). It will be a core managed runtime, but it is not
in `base-nodes.json` yet; only the SDK's root-tenant allowlist names it. The beta spec:

- assumes a firewall that admits only ingress hives;
- models tenant apps only;
- keeps long streams and large uploads out of scope;
- uses `<52-char key>.apps.<domain>` hosts;
- plans a unit, a `system_nodes` entry and a `web:` block, which predate the packaging model.

`io-cloud-spec-v1.md:372`'s per-tenant `IO.archi` is superseded (D10).

**io.linkedhelper.** In the base set with `boot:false`. When instantiated it serves plain HTTP on a
configured address (schema example `0.0.0.0:19091`), and Cloud and its adapter reach it directly.

---

## 2. Decisions

### 2.1 Agreed with the operator (2026-10-05)

| ID | Decision |
|---|---|
| D1 | The orchestrator owns the host posture of its role. It declares it, applies it whole and atomically, verifies it at bootstrap and in the watchdog, corrects drift and reports it. |
| D2 | One Fluxbee firewall owner per host: the nftables table `inet fluxbee_host` on every role. `fluxbee_egress` keeps only forward and NAT. Fluxbee stops writing ufw/firewalld rules and leaves existing ones alone. Third-party firewalls are reported, never changed. |
| D3 *(rev 4, approved)* | Rules go by port and interface per role, not by per-hive IP. The mesh ports are authenticated, and the DMZ ingress must reach them anyway. Removed hives are kept out at the protocol layer (A-48). |
| D4 | Archi and the admin HTTP API listen on loopback only. Operators reach them on the motherbee or through an SSH tunnel. DEV switches to the tunnel. |
| D5 | Syncthing never uses public infrastructure. Spokes dial the motherbee at its static address; the motherbee does not dial. |
| D6 | The only public port of an installation is the edge port of its ingress hive. |
| D7 | IO.web is the URL plane; IO.cloud stays the command plane. IO.web v2 is session-gated Archi plus artifacts and nothing more: *"no quiero complicarla"*. |
| D8 | Cloud is trusted blindly for operator identity. The operator mint is its own action and is logged. |
| D9 | Cloud gives the operator a URL to open top-level, never embedded. `frame-ancestors 'none'`. |
| D10 | Archi is per installation, never per tenant, never public. Its app, UI and API are not changed. |
| D11 | One public ID per installation, picked by the operator with the DNS record; PROD `hive-k3m9x7q2`. The installation reads it from `admin.public_base_url`. |
| D12 | DNS stays manual (operator, Azure). |
| D13 | No second registrable domain. Tenant content is a directory of the installation host, and Archi never shares a host with it. Conditions: Cloud and the brand site use host-only cookies; CSRF relies on Origin checks; no installation certificate covers Cloud's host. |
| D14 | IO.web will handle artifacts (security, maybe sharing); not designed here. |
| D15 | Process: document, then DTAP panel, then staged implementation, each stage validated on 8.x (the authorized testbed) before the next. Never one pass. |
| D16 *(rev 4, approved)* | SSH is accepted only on each host's internal interface, on every role, with no configuration. The ingress' public interface and the egress' office/WAN interface lose SSH. It is hygiene, not a boundary: sshd's own auth is the control. `operator_sources` is dropped from v1. |
| D17 | Archi and the admin get to loopback through configuration only. Archi's code is not changed. |
| D18 | io.linkedhelper will go through the edge in the last phase. Until then its direct port is closed under enforce and reported. |
| D19 | Ingress hardening is only what `add_hive` with the ingress role and the orchestrator do by themselves. No infrastructure configuration from the user, and nothing odd, now. |
| D20 | Order of work: the core (Part A stages 1–5, A-48, A-49), then the Fluxbee Cloud connection (stages 6–7), then the implementation nodes. |
| D21 *(rev 4)* | Nobody configures the posture. The mode is automatic: observe, then enforce by itself after a clean window fixed in code; a changed ruleset goes back to observe. Two local files exist for us, never for users: break-glass and hold. No settings travel between hives. |
| D22 *(rev 4)* | No verify probes, commit-confirm timers or quarantines. A ruleset is enforced only after it ran clean in observe. It filters only inbound, keeps established connections, and depends on no remote data. |
| D23 *(2026-10-06)* | The Docker lab is removed: it was old and unused. Every stage is validated on the 8.x Proxmox testbed. The Docker quickstart leaves the open-source site. The rest of `lab/` (Proxmox, ops, logbooks) stays. |
| D24 *(rev 5)* | The A-48 guard ships with stage 1: the motherbee refuses `ADD_HIVE_FINALIZE` and `REMOVE_HIVE_CLEANUP`. On the motherbee they are a kill switch any orchestrator can pull, and they depend on nothing in the posture work. |
| D25 *(rev 6, proposed)* | The ufw/firewalld writes into an inactive firewall are removed (the inert code of the approved stage 4). Where ufw is active or firewalld is running, the orchestrator still opens the role's declared ports there, so a customer host with its own firewall keeps working with no configuration. |
| D26 *(rev 6, proposed)* | No automatic teardown on downgrade or rollback: the host keeps its last ruleset, which is harmless without per-hive sets, and break-glass is the escape. Only package remove/purge tears down. |
| D27 *(rev 6, proposed)* | `remove_hive` does not tear the posture down, so a removed ingress or egress does not reopen SSH on its external interface. Repurposing a box is one documented command. |

### 2.2 Open — the operator decides

The core questions are answered: the operator approved D3 and D16 (2026-10-05). §8 lists what the
design itself still has to fix before stages 2–5.

Before stages 6–7 (the Cloud phase):

| ID | Question | Options and recommendation |
|---|---|---|
| O5 | The trust anchor for Archi through Cloud. Today a compromised ingress can mint and use an Archi session by itself. | **(a) Recommended: end to end.** Cloud signs the mint with a key pinned on the motherbee; the code goes back encrypted to Cloud; the edge passes `archi.<id>` through by SNI to IO.web, which holds the only certificate for it. (b) Accept the risk in A-46. |
| O6 | Certificate source with manual DNS. | **(a) Recommended for now:** a 1-year per-installation certificate. (b) ACME with `_acme-challenge.<id>` delegated by CNAME. Under O5(a), the edge certificate does not cover `archi.<id>`. |
| O7 | More than one ingress per installation. | Publication is single-edge today, so DNS points at one ingress. *"Edge replicado"* / *"cloud replica"* reads as Cloud mirroring Archi. To confirm. |
| O8 | Tenant label in tenant paths, and per-tenant hosts. | io.web v2. Never the customer name. |

---

## 3. Part A — Host posture owned by the orchestrator (the core)

### 3.1 One declaration per role

One pure function computes the posture from the host's own role, configuration, interfaces and
listeners. No registry entry and no other hive is an input. From it the orchestrator renders:

- the nftables ruleset;
- the expected TCP listeners;
- the Syncthing settings.

Unit tests pin that every allow has a listener and every listener has an allow.

**Inputs**

- **The role.**
- **The motherbee's ports from config:** the `wan.listen` port and `identity.sync.port`.
- **On the ingress:** the edge port from `edge.listen`. A non-443 or plaintext edge is reported.
- **sshd ports:** those `sshd -T` reports, plus any port held by sshd or `ssh.socket` (seen with
  `ss`). The fallback is 22, and the list is never empty. On the PROD motherbee `sshd -T` fails,
  because `/run/sshd` exists only once a connection arrives; the `ss` source covers that case.
- **The internal interface:**
  - **motherbee:** every interface (deployment model). A public address on it is reported as a
    warning.
  - **every spoke** (worker, ingress, egress): the interface of the route to the motherbee's
    uplink IP. It is derived once the uplink is connected, from the WAN session's local address.
    On the egress, a result that differs from `lan_iface` is reported.
  - **if the derivation fails,** the applied ruleset and mode stay as they are and the failure is
    reported. A failure never reopens a host.
- **Closed lists in code:** the core IO listeners (none in the core phase; IO.web joins at stage
  7) and the node listeners (none; io.linkedhelper stays out, D18).

### 3.2 Inbound rules

The order of the render matters:

1. `iif lo` accept.
2. ICMP and ICMPv6 errors by type, whatever the conntrack state: destination-unreachable
   (including frag-needed), packet-too-big, time-exceeded, parameter-problem. The edge port is
   untracked, so the Path MTU errors about its connections are INVALID to conntrack, and dropping
   them would break PMTU discovery on the public port.
3. ICMPv6 neighbor discovery; ICMP and ICMPv6 echo, rate-limited.
4. `ct state established,related` accept.
5. `ct state invalid`: counted; dropped in enforce, nothing more in observe.
6. The role's allows (table below).
7. The accounting rule (§3.3), then the chain policy: `drop` in enforce, `accept` in observe.

| Role | Accepted | On |
|---|---|---|
| motherbee | tcp 22 (sshd ports), 9000, 9100, 22000 | any interface |
| worker, egress | tcp 22 | internal interface |
| ingress | the edge port, untracked | any interface |
| ingress | tcp 22 | internal interface |

**Spokes expose no Syncthing port.** They dial the motherbee, and from stage 3 their Syncthing
listens on loopback (§3.5).

**The edge port runs untracked.** Raw `notrack` on prerouting for packets to the edge port, and on
output for packets from it. The ingress' own dials use ephemeral ports and stay tracked. A
connection flood on the public port cannot fill conntrack.

**Outbound is not filtered** (D19).

**Why there are no per-hive sources (D3).** The mesh ports are authenticated, and the DMZ ingress
must reach them anyway. Removed hives are kept out by A-48, at the protocol layer.

**SSH (D16).** Accepted only on internal interfaces, with no configuration.

**Evidence from PROD.** In the journal since August:

- nobody SSHed into the motherbee (it is operated through the Proxmox guest agent);
- the only SSH logins on the spokes came from the motherbee (10.10.10.10), over eth0;
- none came in through the egress' office address or the ingress' public one.

So D16 cuts nothing in use. The PROD path for an Archi tunnel is a jump through a host on the
internal LAN that the posture does not filter (fb-build or the Proxmox host).

### 3.3 Modes, evidence, apply

**The mode is a pure function, unit-tested with an injected clock.** Its inputs:

- the persisted state: mode, render hash, observed time, last reset and its reason;
- the current render hash;
- relevant would-drops since the last tick;
- SSH logins through a non-internal interface since the last tick;
- whether `nft` is present;
- third-party firewall, hold and break-glass;
- whether the derivation succeeded;
- whether the expected listeners are up;
- on spokes, the two Syncthing facts (§3.5);
- the tick's monotonic delta.

**Its rules:**

- **Break-glass** (`/etc/fluxbee/posture.disabled`) present → `off`.
  - There is no table, and the boot unit does not load one.
  - The orchestrator removes the table and never re-applies it while the file exists.
  - Removing the file starts observe with a fresh window.
- **A render change** (new hash) → observe, with the window reset.
- **In observe, observed time accumulates.** It is the sum of tick deltas while the orchestrator
  runs, persisted with the render hash, so restarts and reboots keep it.
- **A reset.** A relevant would-drop, or an SSH login through a non-internal interface, resets the
  window and is reported with its tuple.
- **observe → enforce** once observed time reaches 24 hours and all of these hold:
  - `nft` is present;
  - there is no third-party firewall;
  - the derivation succeeded;
  - the expected listeners are up;
  - there is no hold;
  - on spokes, both Syncthing facts are true;
  - the release enables the switch (from stage 5).
- **enforce → observe** only on a render change, a hold file or a third-party firewall that
  appears. A would-drop in enforce is the firewall doing its job: it is reported, and the mode does
  not change.
- **A derivation failure** keeps the applied ruleset and mode, and is reported.
- **Hold** (`/etc/fluxbee/posture.hold`): never switch to enforce, and go back to observe if
  enforcing. Ours, for the rollout order.
- **Console escape.** `touch /etc/fluxbee/posture.disabled; nft delete table inet fluxbee_host`
  works without the orchestrator.

**Evidence: only what a drop would actually break.** A SYN to a port with no listener is refused
today, so enforcing changes nothing for it. So:

- **`@listening`.** The orchestrator keeps a small named set with the ports that have a
  non-loopback listener, from the listener check. Updating its elements is data, not a re-render.
- **The accounting rule.** Before the policy, on internal interfaces, packets whose destination
  port is in `@listening` hit a counter and a rate-limited `log` (prefix `fluxbee-posture: `).
  - The counter's delta is the relevant would-drop count; the log gives the tuples for the report.
  - There is no per-tuple dynamic set an attacker could fill, and scans of closed ports do not
    count.
- **Non-internal interfaces** admit only the edge port. SSH there is judged by successful logins
  in the sshd journal, whose source routes through a non-internal interface, not by packets. That
  way internet scanners cannot hold the ingress or the egress in observe.

**Apply.** In one `nft -f`:

```
table inet fluxbee_host {}
delete table inet fluxbee_host
<full definition>
```

Then the elements of `@listening` are set.

**Persist.**

- `/etc/fluxbee/posture.nft`: the last applied ruleset.
- `/var/lib/fluxbee/state/posture.json`: mode, render hash, observed time, last reset and its
  reason, and the drift baseline.

Neither lives under `/etc/nftables.d`.

**Boot unit** (`fluxbee-host-nft.service`). The orchestrator writes it on every role, as it does the
egress unit:

- `DefaultDependencies=no`
- `After=nftables.service`
- `Before=network-pre.target`, `Wants=network-pre.target`
- `ConditionPathExists=/etc/fluxbee/posture.nft`
- `ConditionPathExists=!/etc/fluxbee/posture.disabled`

Rules use `iifname` / `oifname` only. A failed load is reported.

**Drift.**

- **The baseline** is nft's own listing, normalized (no handles, counters or set elements), taken
  right after the apply and stored in `posture.json`. Later listings are compared with it.
- **Re-apply only on drift**, and honour break-glass.
- **`nftables.service`:** an enabled one that runs `flush ruleset` is reported as a conflict; its
  effect is repaired as drift.

### 3.4 Binds (stage 1) — as built (0.1.57)

Archi and the admin listen only on `127.0.0.1`, through configuration only (D17). Archi's code is
not changed.

- **The shipped config.** `packaging/hive.yaml.example` and `config/hive.yaml` set
  `architect.listen: "127.0.0.1:3000"` explicitly, with a comment on the SSH tunnel. The design
  said to drop the key; keeping it explicit shows the operator where the bind lives.
- **The migration.** `packaging/fluxbee-migrate-config`, installed under `/usr/share/fluxbee/`, runs
  from the postinst before the orchestrator starts.
  - It rewrites exactly `listen: "0.0.0.0:3000"` under `architect:` to `127.0.0.1:3000` in an
    existing `/etc/fluxbee/hive.yaml`. Any other value is left alone.
  - It follows a symlinked file, keeps mode, owner and CRLF endings, is idempotent and never fails
    the install.
  - On an upgrade sy-architect still runs the old binary. The orchestrator's boot restarts it (A-45),
    and it reads the rewritten file.
- **The listener check.** Report-only, about every 60 s from the watchdog, on every role:
  - parses `ss -H -tlnp`: drops the scope before the brackets, reads `*`, and handles several
    owners or none;
  - flags `sy-architect` or `sy-admin` on any address outside 127.0.0.0/8, `::1` and mapped
    loopback, whatever bound it there;
  - writes one drift alert per process: category `posture`, kind
    `loopback_service_exposed_<process>`, severity `error`, at most once an hour;
  - warns when `ss` printed something and nothing parsed.
- **The spoke-only actions** (D24): the motherbee refuses `ADD_HIVE_FINALIZE` and
  `REMOVE_HIVE_CLEANUP` with `FORBIDDEN`, and logs the sender.
- **Docs.** Firstboot, `packaging-and-build` and `07-operaciones` point to loopback plus an SSH
  tunnel that carries both ports (`-L 3000:… -L 8080:…`), because Archi's UI tells operators to call
  the admin on 8080. The HANDBOOK notes the change.
- **CI:** the `posture-guards` workflow, on every push.
  - `scripts/check_loopback_binds.sh`:
    - `architect.listen` and `admin.listen` in the two shipped `hive.yaml` files must be loopback;
    - firstboot, `packaging-and-build` and `07-operaciones` may carry no `:3000`/`:8080` URL on a
      non-loopback host (each URL is checked on its own) and no `0.0.0.0` listen;
    - a missing input fails the guard.

    History (logbooks, HANDBOOK, FINDINGS) is out of scope.
  - `scripts/packaging_tests/migrate_config_test.sh`: 19 checks of the migration.
- **Docker lab:** removed (D23).

### 3.5 Syncthing (stages 2 and 3)

**Addresses (stage 2)** are reconciled at every boot and watchdog run, not only at join:

- **Spoke side.** The spoke has exactly one remote device, named after the motherbee's `hive_id`.
  It gets `Static(<motherbee uplink IP>:22000)`.
- **Motherbee side.**
  - Every device named after a registered hive gets `AcceptOnly`: address `dynamic`, so the
    motherbee never dials once discovery is off.
  - A device whose name has no registry entry is reported as an orphan. A-48's `remove_hive`
    removes such devices by name.
- **The address type** becomes `AcceptOnly | Static(SocketAddr)`, IP literals only.
  - A finalize without a derivable address is refused, never written as `dynamic`.
  - The worker join derives the motherbee address the way ingress and egress do.
- **One serialized writer** for `config.xml` (a mutex plus atomic writes), kept that way by a CI
  grep guard. Syncthing restarts only when something changed.
- **Listen addresses do not change in stage 2.**

**Options (stage 3).** The orchestrator owns them, and they are aligned with
`vendor/syncthing/config.xml`:

- global and local announce, relays, NAT and crash reporting off;
- `urAccepted=-1`, `autoUpgradeIntervalH=0`, STUN off;
- `reconnectionIntervalS=10`;
- `listenAddresses`: on the motherbee `tcp://:22000` (dual-stack), on spokes
  `tcp://127.0.0.1:22000`.

The whole set switches together.

**The order is enforced in code, and each switch happens once:**

1. **A spoke switches** when its motherbee device is `Static` and a plain TCP connect to
   `<motherbee>:22000` succeeds. It reports both facts in its `/versions` snapshot.
2. **The motherbee polls**, while it has not switched yet. A background task, not the
   single-flight watchdog tick, reads the snapshots of the spokes that are registered devices
   through the existing GET_VERSIONS, about every 60 s.
3. **The motherbee switches** when every one of them reports both facts. An offline spoke that
   never reported keeps it waiting, and the report names it.
4. **Both switches are one-way.** Once off, the public set stays off: a later outage does not turn
   discovery back on.

**Unit tests** cover both predicates on fixtures captured read-only from the four PROD hosts
(worker1 `dynamic`↔`dynamic`, ingress static, egress without a device id), plus synthetic reports:
fields absent (an old spoke), static false, connect false, both true. The captures drop the `<gui>`
block, whose API key cannot go into the public repo.

### 3.6 Reports and checks (report-only)

| Check | What it reports |
|---|---|
| Listener | From stage 1, on every role: `sy-architect` or `sy-admin` off loopback. From stage 4: every non-loopback TCP listener not in the declaration, by process name; this list also feeds `@listening`. |
| sshd | `PasswordAuthentication yes` and `PermitRootLogin yes`, as warnings. The fix is A-2; the PROD motherbee has password login on. |
| Mesh auth | `wan.mtls` other than `required` (motherbee) and `identity.sync.auth: disabled`, as warnings. D3 rests on them. |
| Third-party firewall | ufw active, firewalld running, or any non-Fluxbee base chain on the input hook with a non-accept policy or rules. The host stays in observe, and the report names the ports to open there. |
| Recorder | INVALID counts per interface, the relevant would-drop count, and the size of `@listening`. |
| Motherbee | A public address, as a warning. |

**Where it shows.**

- `watchdog_tick` computes the posture and caches it; `local_versions_snapshot` returns the cached
  value, and `/versions` fans it out.
- `ops.py health` fails on drift, `unavailable`, a third-party firewall, a derivation failure, or
  break-glass or hold present.
- From the stage-5 release it also fails when a host is not in enforce 48 hours after its last
  render change. Before that, an open window is not a failure.

### 3.7 Lifecycle

- **nftables.**
  - Each host's orchestrator ensures it once per boot, non-fatal:
    `command -v nft || (apt-get update && apt-get install -y nftables)`, with a dpkg lock timeout.
    The result goes into the report.
  - The motherbee's `.deb` depends on nftables. The 4 PROD hosts already have it.
  - Without `nft` the host is `unavailable`, which means unfiltered. Health names it, with the
    command to run.
  - The egress keeps failing loud without `nft`, because its NAT needs it.
- **ufw / firewalld.**
  - The writes into an inactive ufw are removed.
  - Where ufw is active or firewalld is running, the orchestrator still opens the role's declared
    ports there (motherbee: 9000, 9100, 22000; spokes: none). A customer host with its own firewall
    keeps working with no configuration, stays in observe, and the report says so.
- **`add_hive`.** Nothing to admit on the motherbee. The spoke starts in observe.
- **`remove_hive`.** The posture is **not** torn down: the removed host keeps SSH on its internal
  interface only, so a removed ingress does not reopen SSH on its public interface.
  - For an ingress, REMOVE_HIVE_CLEANUP also stops and disables sy-edge, and deletes its
    publications and its mesh TLS directory.
  - To repurpose a box: `touch /etc/fluxbee/posture.disabled; nft delete table inet fluxbee_host`.
- **Upgrade.** If the render changes, the host goes back to observe for a new window.
- **Downgrade or rollback to a build without the posture.** The host keeps its last ruleset, which
  the boot unit keeps loading. That is harmless: it admits SSH on the internal interface, the
  motherbee's mesh ports and the edge port. Break-glass is the escape. There are no version
  comparisons.
- **Package remove / purge.** The prerm tears down the table, the file, the state and the unit.
- **Egress.** `fluxbee_egress` loses its input chain, with an explicit `delete chain` in the same
  transaction. This supersedes edge-egress-nat-spec §3.3 and §8.5; a dated note there says so.

### 3.8 A-48 — `remove_hive` revokes the hive

**The router's view.** The motherbee orchestrator writes `/var/lib/fluxbee/state/router-hives.json`.
It lists every registered hive, whatever its status, with its leaf pin when known.

- It is written at every boot, before rt-gateway starts, and on `add_hive` / `remove_hive`.
- The router reloads it. On a read error it keeps the last good view.

**Admission, on the motherbee's accept path only.**

- A WAN peer is admitted if its `hive_id` is in the view.
- If the view has a pin for it, the presented leaf must match.
- A registered hive without a pin (one joined before A-48) is admitted and listed as unpinned.
- A pin is written whenever the motherbee issues a leaf (`add_hive`, TLS reconcile).
- Spokes' dials to the motherbee are not checked against a view.

**`remove_hive`, in this order:**

1. REMOVE_HIVE_CLEANUP (§3.7).
2. Delete the registry entry. The view drops the hive and the router closes its sessions.
3. Delete its HMAC key and restart sy-identity on the motherbee; the replicas reconnect, as after
   any deploy.
4. Unlink its Syncthing device by name, on every role.
5. Delete its `known_hosts`.

**SSH host keys.** `StrictHostKeyChecking=accept-new`, with one `known_hosts` per hive
(`UserKnownHostsFile=<hives>/<id>/known_hosts`, `HostKeyAlias=<hive_id>`). The first connection
records the key, existing spokes included, and checking is strict afterwards.

**Registry writes** become read-modify-write merges under the per-hive lock, so a join keeps keys
it does not own (the pin, the device id).

**Already in 0.1.57:** the motherbee refuses `REMOVE_HIVE_CLEANUP` and `ADD_HIVE_FINALIZE` (D24).

**Residual:** the old box of a hive re-added before A-48, when it had no pin, stays admissible until
that hive is re-added again.

### 3.9 A-49 — signed apt repo

- **Signing.** `scripts/apt-repo-publish.sh` signs `InRelease` with a key that has no passphrase
  and no expiry. The key lives on fb-build and is backed up off fb-build. The public key file is
  published next to the repo, and its fingerprint goes into the install docs.
- **First install.** It keeps `[trusted=yes]` once, or downloads the key from the repo (trust on
  first use).
- **The postinst:**
  - copies the key to `/etc/apt/keyrings/fluxbee.gpg`, which no package owns, so a downgrade
    keeps it;
  - switches an existing Fluxbee source to `[signed-by=/etc/apt/keyrings/fluxbee.gpg]`, but only
    if apt's cached `InRelease` for that source verifies with `gpgv`. There is no network I/O
    inside dpkg. Otherwise it leaves the source alone and says so.
- **Residual:** fb-build holds the signing key on the flat L2. A-49 stops impersonation of the
  repo, not a compromise of fb-build.

---

## 4. Part B — Web exposure through IO.web (Cloud phase; inputs for the io.web spec v2)

### 4.1 Mirrored from IO.cloud

| Layer | IO.cloud (commands) | IO.web (URLs) |
|---|---|---|
| Who authenticates the human | Cloud | Cloud |
| What crosses the edge | Cloud service bearer | one-time code (≤120 s) exchanged for a `__Host-` cookie; under O5(a) only TLS the edge cannot read |
| Authority | Admin | Admin (operator mint as its own action, logged) |
| Allowlist | exposed actions, default deny | exact host table, default 404 |
| Internal hop | mesh message | mTLS from the edge to the IO.web private listener |

### 4.2 System apps (Archi)

**The `system` class.**

- A closed list in code; today only `archi`.
- The upstream is fixed in code: loopback on the motherbee, where IO.web runs as a core IO. Its
  listener joins Part A's closed list at stage 7.
- Operator session only. Never public, by construction.

**Archi is proxied unchanged** (D10). It needs SSE, uploads up to 128 MiB and its `/*path`
catch-all, so no narrow path allowlist.

**CSRF.** IO.web rejects every non-GET/HEAD request whose `Origin` is not exactly the Archi origin,
and every `Sec-Fetch-Site` other than `same-origin` when the header is present.

**CSP.** A named profile for Archi tolerates its inline scripts but keeps `frame-ancestors 'none'`
and `nosniff`.

**Audit.** IO.web strips client-supplied identity headers and logs the Cloud subject per request.

### 4.3 Operator access (shape; the final form depends on O5)

1. The operator is logged into Cloud. Cloud asks Admin, through IO.cloud, for an operator mint.
   - Under O5(a) the request is signed and the code comes back encrypted.
2. Admin mints a one-time code (≤120 s, `aud` = the Archi host, `jti` stored durably) and logs it.
3. Cloud sends the operator top-level to `https://archi.<id>.fluxbee.ai/_fluxbee/session/exchange`.
4. IO.web consumes the code, sets `__Host-fb_session` and redirects.
5. Following requests go edge → IO.web → Archi.
   - Under O5(a) the edge forwards `archi.<id>` as raw TLS by SNI.

**Superseded.** Edge v6 I2 gets one named exception: operator-session access to Archi. io-cloud spec
§3.2/§8.4/§10 are updated in the same change.

### 4.4 Tenant content as a directory (D13)

- **Where:** under `https://<id>.fluxbee.ai/t/<label>/…` (O8), next to `/e/` and `/public/`.
- **Why it is safe:** that host carries no cookies, and active content is sandboxed.
- **Edge invariants:**
  - never pass `Set-Cookie`;
  - sandbox active content;
  - `nosniff` everywhere;
  - reject service-worker script fetches.
- **Host-first routing at the edge, before stage 7:**

  | Host | Serves |
  |---|---|
  | `<id>` | `/e/`, `/public/`, `/t/` |
  | `archi.<id>` | only the IO.web system route |
  | anything else | 404 |

### 4.5 Artifacts (D14)

IO.web will serve artifacts for isolation and sharing; today they are `/public/<key>`. Designed in
io.web v2.

---

## 5. Part C — Public naming for many installations (Cloud phase)

### 5.1 Installation public ID (D11)

- Opaque, one DNS label, immutable.
- Never the customer name, never the internal `hive_id`.
- Picked by the operator with the DNS record.
- Read from `admin.public_base_url`.

PROD: `hive-k3m9x7q2`.

### 5.2 Hostnames

| Host | Serves |
|---|---|
| `<id>.fluxbee.ai` | `/e/<ich>`, `/public/<key>`, tenant directories |
| `archi.<id>.fluxbee.ai` | Archi, operators only |

### 5.3 DNS and certificate (manual now)

- **DNS** (operator, Azure): `<id>` and `archi.<id>` point to the one ingress (O7). Failover is
  manual.
- **Certificate:** per installation, per O6, never the brand wildcard. Under O5(a) the edge's
  certificate does not cover `archi.<id>`.
- **Move PROD off the brand wildcard early,** independently of the core stages.
- **Runbook:** new certificate → vault → the edge restarts itself. One entry per edge.
- **Expiry:** `notAfter` in the report, alert at 21 days.
- **CAA** on `fluxbee.ai`.
- **Before a public IP is released,** delete its records.
- **DNS credentials** never live on an installation host.

### 5.4 Relation with FC.edge-manager and edge-control v2

| | |
|---|---|
| **Superseded for now** | Per-edge certificates become one per installation. `<edge_id>` names become `<id>`. Cloud-owned DNS becomes manual. |
| **Still holds** | A certificate per scope; never distribute the brand wildcard; validate IP changes. |
| **Out of this work** | The Cloud control channel. |

---

## 6. Work plan

**Every stage goes through:**

1. code and unit tests;
2. an adversarial review of the diff;
3. a release (version bump, ledger);
4. validation on the 8.x testbed (snapshot first, at most 3 per VM);
5. FINDINGS and logbook.

The next stage starts only when the previous one is validated.

### Core

| Stage | Content | Gate |
|---|---|---|
| 0 | Revisions 4–6 and DTAP rounds 1–3; the operator approved D3 and D16. Round 4 on revision 6 is next. | Round 4 before stage 2. |
| 1 | Binds, the listener check and the spoke-only guards (§3.4), released as 0.1.57. | **Done: validated on 8.x on 2026-10-06 (see the ledger).** Unit: listener fixtures (the PROD capture flags only sy-architect 0.0.0.0:3000; `*`, `[::]`, scope before and after the brackets; several owners or none); the spoke-only guard; 19 migration checks; the CI guard. Infra, after `ops.py deploy`: (1) `ss` on the motherbee shows 3000 and 8080 only on 127.0.0.1; (2) `lab/posture-probe.sh 10.10.10.10 3000 8080 9000` from VMs 101, 102, 103 and 110 gives refused, refused, open (baseline on 0.1.56: open, refused, open); (3) on the motherbee, `curl 127.0.0.1:3000/` returns the UI and `curl 127.0.0.1:8080/hives` answers; (4) positive control: a dummy listener whose process is named `sy-admin`, on a spare port of 10.10.10.10 for about 20 s, produces `loopback_service_exposed_sy-admin` within two minutes (`GET /hives/motherbee/drift-alerts?category=posture`), and sy-architect raises no alert after the deploy; (5) the postinst printed its migration line. The operator checks the tunnel once, outside the gate. |
| 2 | Syncthing addresses (§3.5). | **Unit:** the address type; IP literals only; v6 formatting; a finalize without an address refused; PROD fixtures reconcile to the target; a second pass is a no-op; orphan devices reported. **CI:** a grep guard keeps `config.xml` writes in the single writer. **Infra:** `/rest/config/devices` on the 4 hives matches the target; `/rest/db/completion` is 100% for every folder and device on the motherbee; `ops.py opa-status` in_sync 4/4; listen addresses unchanged. |
| 3 | Syncthing options and the ordering predicates (§3.5). | **The first deploy of stage 3 is the skip-upgrade test.** With the operator's OK at that time, roll the 4 VMs back to the pre-stage-2 snapshot, then deploy stage 3 directly. **Then:** `/rest/config/options` exact on the 4 hives; `/rest/system/status` shows discovery off; no relay or QUIC connections; `ss` shows no non-loopback UDP for Syncthing; the motherbee listens on `tcp://:22000`; after a motherbee Syncthing restart every spoke reconnects within 30 s (measured on the motherbee's `/rest/system/connections`); a dist publish reaches 4/4. **Unit:** both predicates on PROD fixtures and on synthetic reports. |
| 4 | Firewall in observe only (§3.1–§3.3, §3.6, §3.7). | **Unit:** role × mode render invariants: observe and enforce differ only in the policy and the INVALID verdict; the only accept of new connections without a source restriction is the ingress edge port; no role accepts 3000, 8080, 5432, 4222 or 8384; ICMP errors come before established; the `delete table` render is deterministic. The mode function, with an injected clock, through every transition: restart, reboot, break-glass removal, hold, render change, derivation failure. **CI:** a network-namespace job in its own workflow with sudo applies every role, checks connect/timeout over veth, and accepts a frag-needed for an untracked edge flow in enforce. **Infra:** the posture in `/versions` for 4/4. A canary on every hive: a short-lived listener on an unlisted port of the internal address, connected from another VM, succeeds and appears as a relevant would-drop within two ticks. Injections on worker1, each under a minute: flush the input chain (drift reported and repaired); add a foreign input base chain (third-party firewall reported, health fails); break-glass (the table goes and does not come back; health shows it). The table is there after a spoke reboot with sy-orchestrator disabled. Measure how long spokes take to rejoin after a motherbee reboot; that is the stage-5 baseline. |
| 5 | Automatic enforce. | Put `posture.hold` on every host before the deploy. Release worker1, then the other spokes, then the motherbee last, a day apart. The canary now times out with no RST from other VMs and still connects from loopback. SSH to 192.168.8.240 from fb-build is refused. On the ingress' public address, from the operator's workstation, only the edge port answers (a manual step). After a motherbee reboot, spokes rejoin within the stage-4 baseline. An enforcing ingress reboots and stays in enforce. An upgrade with an unchanged render stays in enforce; a changed one goes back to observe. Break-glass from the console works without the orchestrator. |

**After stage 5:**

| Item | Gate |
|---|---|
| A-48 (its spoke-only guard shipped with stage 1, D24) | Join a throwaway VM as a worker and snapshot it. Remove it, re-add the same `hive_id` (new leaf and pin), roll the VM back to the snapshot and boot it: its WAN dial and its 9100 attempts are refused (router reject counter and log). Within 10 s of `remove_hive`, `ss` on the motherbee shows no established 9000/9100 from its address. Re-join it as an ingress and remove it: sy-edge is inactive and disabled, and its posture is still there. Regenerate its SSH host keys: the motherbee's next SSH is refused. The registry keeps foreign keys across a join. |
| A-49 | `ops.py publish` signs without a terminal. The motherbee's `apt-get update` verifies the signature (output read, not through deploy). A copy of the repo with one byte of `Release` changed, used as a temporary source, fails with a signature error; the live repo is never touched. Postinst fixtures: a valid cached `InRelease` switches the source; a missing or bad one keeps `[trusted=yes]`. A downgrade below A-49 still verifies, and the re-upgrade works. |

The throwaway hive is a 2 GB clone of template 9000, kept stopped between uses.

### Cloud phase

| Stage | Content |
|---|---|
| 6 | Naming and certificate (O6, O7), plus Host-first routing at the edge. |
| 7 | io.web v2 spec (D7, O5, O8) with its own DTAP panel, then Archi through Cloud. |

### Implementation nodes

io.linkedhelper through the edge (D18), then the rest.

**Not now (D19), revisited after the core:**

- network-level isolation of the ingress;
- outbound filtering on the ingress;
- an unprivileged SY.edge;
- `operator_sources`.

---

## 7. Review log

- **2026-10-05, DTAP round 1 on revision 1:** all four lenses failed, 75 findings.
- **2026-10-05, DTAP round 2 on Part A of revision 3:** all four lenses failed, 84 findings. The
  common cause was per-hive IP filtering and the machinery around it; revision 4 simplifies.
- Details and dispositions: `docs/audits/2026-10-05-host-posture-dtap-panel.md`.
- **2026-10-05/06, DTAP round 3 on Part A of revision 4:** all four lenses failed, 64 findings.
  - The 6 blockers are all beyond stage 1: A-48 would cut every pre-existing hive (no pin); the
    observe window is blind on non-internal interfaces; `ADD_HIVE_FINALIZE` is a kill switch on the
    motherbee.
  - Stage 1 got only details. They are folded into §3.4 as built, and the kill switch is guarded
    (D24).
  - An adversarial review of the stage 1 diff found no blocker or major.
- **2026-10-06, stage 1 live:** 0.1.57 deployed and validated on 8.x. See lab/DEPLOYMENTS.md.
- **2026-10-06, revision 6:** round 3 folded into stages 2–5, A-48 and A-49.
- **Next:** DTAP round 4 on revision 6 (stages 2–5, A-48, A-49), then stage 2.

## 8. Round 3 → revision 6

| Round-3 findings | Where they went |
|---|---|
| D3-11, D3-12, A3-9, P3-13, T3-8, T3-9 | §3.5: the motherbee polls the spokes' snapshots; registered devices only, orphans reported; one-way switches; dual-stack listen; the skip-upgrade test is the first stage-3 deploy. |
| P3-1, A3-2, T3-7, D3-4 | §3.2–§3.3: SSH through non-internal interfaces is judged by logins in the sshd journal. PROD evidence: nobody uses those paths. |
| A3-3, P3-5, P3-16, T3-14, D3-14 | §3.3: the mode is a persisted pure function with an injected-clock test; transitions written down; drift baseline taken from nft's own listing. |
| P3-6, P3-7, T3-10, A3-6 | §3.3: evidence counts only packets to ports with a listener (counter + log on `@listening`); no fillable dynamic set; the canary in the stage-4 gate. |
| P3-8, T3-12 | §3.2: ICMP errors before conntrack; reworded invariant; netns workflow. |
| D3-5, A3-5, P3-9 | §3.7: each host installs its own nftables at boot. |
| D3-6, A3-7 | §3.7 and D25: writes go only into active third-party firewalls; the egress input chain is deleted explicitly. |
| D3-7, P3-10, A3-11, T3-15 | §3.7 and D26: no downgrade teardown machinery. |
| P3-4 | §3.7 and D27: `remove_hive` keeps the posture. |
| D3-13 | §3.1: one derivation for every spoke. |
| D3-15 | §3.6: mesh auth reported. |
| T3-11, T3-13 | §6 stages 4–5: injections, canary, measured rejoin baseline, vantage points. |
| A3-8 | §3.7, plus a dated note in edge-egress-nat-spec. |
| P3-14 | §0 residuals. |
| D3-1, T3-16, A3-1, P3-2, D3-16, T3-17, P3-12 | §3.8: unpinned pre-existing hives admitted; view at boot; sessions closed by reload and restart; `accept-new` host keys; the A-48 gate. |
| D3-17, A3-12, P3-11, T3-18 | §3.9: first install, gpgv on the cached `InRelease`, a keyring that no package owns, tampering only on a copy. |
