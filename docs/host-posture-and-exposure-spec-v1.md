# Host posture and exposure — design v1 (revision 10)

**Status:** design agreed with the operator on 2026-10-05. Stages 1–3 are live and validated on 8.x
(0.1.57 binds; 0.1.58 Syncthing addresses and folders, with A-50; 0.1.59 Syncthing off public
infrastructure; A-47 closed). Revision 10 applies the operator's D30–D32 (2026-10-06): no observe
phase and no runtime check, one SSH door on the egress, SSH only inside `add_hive`. It folds in what
DTAP round 6 found in revision 9 and still applies (§8). Also live since: 0.1.60 (A-51, A-52), 0.1.61
(D32/A-54), 0.1.62 (A-59, A-57), 0.1.63 (A-58, A-56).

**PARKED (operator, 2026-10-07).** *"Cerrá temas importantes y documentá los menores, costo/beneficio
… problemas en la red local interna… es importante pero puede esperar si no anda lo operativo."* The
reachable holes are closed. From outside, the ingress answers only the edge port and SSH by key
(password and keyboard-interactive off, checked on PROD 2026-10-07). Archi, the admin and Syncthing
are off public infrastructure (stages 1–3). The egress door is key-only too. What remains below is
internal-network or defense-in-depth work, parked until the operational track (Fluxbee Cloud
connection, then implementation nodes) needs it, and **required before the first customer
installation** where marked.

| Item | What it protects against | Exposure today | Cost | Before a customer? |
|---|---|---|---|---|
| Stage 4: host firewall (§3.1–§3.7) and revision 11 with DTAP round 7's 56 findings (docs/audits/2026-10-05-host-posture-dtap-panel.md) | A LAN peer reaching internal ports; SSH on non-internal interfaces | Internal network only; the internal ports are authenticated (mTLS, HMAC) | High: a revision, a panel round, implementation, a gate | Yes |
| A-55: the egress SSH door; D33 (its key in the vault, egress only) | Access to the box that NATs the internal network | The office network (192.168.8.0/24), key only | Low | Yes (close the door) |
| A-48: `remove_hive` revokes nothing | A removed box still on the LAN rejoining with its old leaf and key | Internal | Medium: the A-48 design in §3.8 | Yes |
| A-49: unsigned apt repo (waits for O9) | Whoever answers for the repo address installs root software on the motherbee | Internal network | Medium, after O9 | Yes |
| A-60: managed nodes run as root | A compromised node is root on its host | Local | Medium; to be evaluated | No |
| The `active/` blob GC walks by path (A-56 residual) | Root deleting outside the tree if a prefix dir is swapped | None while `gc.apply` is off (the default) | Low | Only if the GC is turned on |

**History:**

| Revision | What happened |
|---|---|
| 1 | Failed the first DTAP panel on all four lenses (75 findings). |
| 2 | Folded those findings in. |
| 3 | Recorded the operator's answers and the order of work. |
| 4 | Part A of revision 3 failed a second panel (84 findings). Most came from per-hive IP filtering and from the machinery added to steer spokes. Revision 4 **simplifies Part A**: rules by port and interface per role, an automatic mode, no configuration. |
| 5 | A third panel on Part A of revision 4 found 64 issues. Its 6 blockers are all in stages 4–5 and A-48; stage 1 needed only details. Stage 1 is built (§3.4 as built), with the A-48 guard against the spoke-only actions moved into it. The Docker lab is removed (D23). §8 lists what round 3 requires before stages 2–5, A-48 and A-49. |
| 6 | Round 3's findings folded into stages 2–5, A-48 and A-49 (see §8). The main changes: evidence counts only what a drop would break, i.e. packets to a port that has a listener, plus SSH logins through non-internal interfaces; the mode is a persisted pure function; ICMP errors come before conntrack (PMTU); A-48 admits unpinned pre-existing hives; each host installs its own nftables; `remove_hive` and downgrades keep the last ruleset. |
| 7 | Round 4 (68 findings; Acceptance passed) folded in (§8). The main changes: the switch is gated only by SSH logins through non-internal interfaces plus preconditions, with packet accounting report-only (D28); the render hash is mode-independent; sshd ports come from socket activation too; DHCP and ICMPv6 RA/MLD are allowed; the egress input chain is dropped from the render, never deleted; the remove cleanup runs outside the orchestrator; A-48 pins new joins before they dial, admits only legacy hives unpinned, and spokes check that their uplink is the motherbee; A-49 never replaces a key. |
| 8 | Stage 2 as built (§3.5): ownership by recorded device id, the role-only folder rule for A-50, a writer hardened against the Syncthing user's directory. Round 5's stage 2–3 findings folded in: only remote devices are touched (A5-1, P5-1); the throwaway worker joined on 0.1.57 before stage 2 (T5-1). |
| 9 | Round 5's stage 4–5, A-48 and A-49 findings folded in (§8), with the operator's D25–D28 (D25 resolved by the deployment model: Fluxbee runs only on clean boxes created for it). The main changes: the internal interface from `ip route get` of the uplink; the render hash is the observe render itself; the mode is an ordered decision list with outputs; the SSH evidence reads only trusted journal fields; TCP and UDP listeners are reported; a third-party firewall is one that decides admission; no nftables install machinery; A-48 computes legacy hives and pins them on first use, keeps one session per hive, revokes as a reconcile and keys `known_hosts` by address; A-49's switch is an idempotent helper with an append-only keyring. D29 is proposed. |
| 10 | The operator withdrew the observe phase (*"eso de esperar 24hrs es un total parche que no sirve"*) and any check on the host before applying (*"demasiado frágil"*) (D30), kept one SSH door on the egress (D31), and limited SSH to `add_hive` (D32). The mode machinery is gone: no window, no SSH-login evidence, no hold, no release switch, no decision list. Each host applies the rules its role declares; CI proves them before a release. Round 6 folded in where it still applies (§8): the stage-4 gate rewritten into §6 (revision 9 had pasted it into this table), the listener check split by protocol, A-48's legacy defined by a missing pin and pinned by the watchdog, the remove order entry-first, revocation only from a complete listing, A-49 signing in a staging directory. A-49 waits for O9. |

Every finding and where it went: `docs/audits/2026-10-05-host-posture-dtap-panel.md`.

The operator approved the narrowed D3 and D16 on 2026-10-05.

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
- A-54: the motherbee used SSH outside `add_hive` (D32).
- A-55: the egress' SSH door (D31) must close before the first customer install.

**Related:** `io-web-spec-beta-v1.md`, `edge-ingress-spec-v6.md`, `edge-egress-nat-spec.md`,
`fc-edge-manager-spec.md`, `edge-control-protocol-v2.md`, `io-cloud-spec-v1.md`,
`lab/logbook/HANDBOOK.md` §8.

---

## 0. Summary

An installation had no declared exposure model:

- no hive filters inbound traffic;
- core services bind every interface;
- Archi (no auth, full admin power) is reachable from the DMZ;
- Syncthing announces every hive to public discovery servers.

Stages 1–3 fixed the binds and Syncthing; the firewall is stage 4.

**One owner.** The orchestrator computes the posture of its host from its role alone, applies it,
and verifies it every time it runs; drift is repaired and reported. In the operator's words: *"el
que tiene que setear todo lo del entorno en ese sentido es el orchestrator, como concepto al correr
verificaría eso"*.

**What a host accepts:**

- the ports its role serves;
- SSH (tcp 22) on its internal interface;
- on the egress, SSH on its external interface too: one door for maintenance during this
  development (D31);
- the edge port from anywhere, on the ingress only.

Nobody configures anything, and nothing changes mode by itself: each host applies the rules its
role declares from its first check, and CI proves those rules before a release (D30). Fluxbee uses
SSH only inside `add_hive`, and the join revokes its access at the end (D32).

**Archi.** It listens on loopback only. Operators reach it on the motherbee, through an SSH tunnel,
or later from Fluxbee Cloud through IO.web (Part B).

**What this does not fix (stated so nobody assumes it does):**

- **Mesh control plane.** Any orchestrator, the DMZ ingress included, can still send SPAWN, KILL
  and NODE_CONFIG_SET to the motherbee (and SYSTEM_CORE_ROLLBACK until A-48); spoke-to-spoke
  SYSTEM is denied (A-20, A-22).
- **Flat internal network.** The ingress, the motherbee, the workers, fb-build and the hypervisor
  gateway (10.10.10.1) share one L2.
  - Any host on it can reach the motherbee's mesh ports and SSH.
  - The mesh ports are authenticated (mTLS, HMAC, Syncthing device IDs); SSH relies on sshd.
  - A compromised ingress can impersonate other LAN addresses.
  - Isolating it needs infrastructure configuration, so it is not done now (D19).
- **First contact over SSH.** A join trusts whatever answers SSH at the spoke's address first (one
  `known_hosts` file per join, §3.8), and the flat L2 accepts forged transmits. A compromised host
  that takes that address during a join receives the spoke's mesh certificate. After the join the
  motherbee never SSHes into the spoke again (D32).
- **SSH auth.** The VM template ships `PasswordAuthentication yes`, a password published in the
  repo and NOPASSWD sudo. Spokes are hardened only with `harden_ssh:true`. The posture reports it;
  the fix is A-2.
- **Unsigned apt repo** (A-49). A LAN attacker impersonating the repo gets root on the motherbee at
  the next upgrade. A-49 waits for O9.
- **Recovery is console only.** After a join the motherbee has no SSH access to the spoke (D32), and
  `remove_hive` cleanup travels over the mesh. A spoke that loses its mesh certificate is joined
  again.
- **No `nft`.** A host without it is reported `unavailable` and is not filtered.
- **A hive removed while offline** keeps its services. A removed ingress keeps serving its public
  port as long as DNS points at it: A-48 only bars it from the mesh.
- **fb-build will hold the apt signing key** on the flat L2 the DMZ reaches, outside any posture.
  A-49 stops impersonation of the repo, not a compromise of fb-build.
- **The PROD motherbee accepts password SSH** for `fluxops` (cloud-init's `50-` file wins over
  `60-`). The posture reports it; the fix is A-2.
- **The egress door** (D31). SSH stays open on `egress.wan_iface`. The code cannot tell an office
  network behind a firewall from the internet, so the door closes before the first customer install
  (A-55).
- **A wrong declaration cuts a port at the first check after the upgrade.** Nothing on the host
  second-guesses the rules (D30). CI is the gate (golden renders, a kernel test per role and
  interface, PROD `ss` captures against each declaration), and break-glass is the escape.
- **Hives joined before A-48** are pinned within a minute of their first session after it (trust
  on first use): until then any CA-valid leaf of their `hive_id` is admitted. If two different
  leaves of one such hive connect first, neither is pinned and it is reported.
- **A downgrade below A-48** brings back a router that ignores the view: every removed hive whose
  leaf is still CA-valid is admitted again. The motherbee refuses every `SYSTEM_CORE_ROLLBACK`, so
  only its own operator can roll it back, locally.
- **D26's frozen ruleset** is harmless only while ports and interface names stay the same. A
  renamed NIC on a downgraded host loses SSH on its internal interface.
- **D27:** a removed host keeps its ruleset, so its external interfaces stay closed to SSH (the
  egress keeps its door).
- **After A-49,** the first install trusts the key on first use, and `InRelease` has no
  `Valid-Until`, so a freeze attack goes unnoticed. A removed key comes back while any shipped
  package still carries it, and a downgrade to an older package trusts its keys again.
- **Anything a hive's declaration does not name,** TCP or UDP listeners and any other IP protocol,
  is cut; the listener check reports TCP and UDP listeners.
- **The first apply keeps established connections,** including an SSH session opened through a
  path the rules now close, until that session ends.
- **The motherbee tracks its mesh ports and SSH on every interface,** so a host on the flat L2 can
  fill its conntrack table; while it is full, new connections fail, loopback ones included
  (postgres, NATS, the admin API).
- **A motherbee with a second network** accepts SSH and the mesh ports there too: D6 and D16 hold
  on it by definition only (it is warned).
- **A missing router view** admits any CA-valid peer until the watchdog rewrites it, within a
  minute, and only while sy-orchestrator runs.
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
| SSH | motherbee → spoke | 22/tcp | Only inside `add_hive`, which revokes its access at the end (D32, 0.1.61). No hive SSHes into the motherbee. Before 0.1.61 the motherbee also tried it at every orchestrator start (`reconcile_hive_tls_material`), and `ssh_access=key_only_persist` left a key behind. |
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
| D16 *(rev 4, approved; amended by D31)* | SSH is accepted only on each host's internal interface, on every role, with no configuration. The ingress' public interface and the egress' office/WAN interface lose SSH. It is hygiene, not a boundary: sshd's own auth is the control. `operator_sources` is dropped from v1. **D31:** the egress keeps SSH on its external interface for now. |
| D17 | Archi and the admin get to loopback through configuration only. Archi's code is not changed. |
| D18 | io.linkedhelper will go through the edge in the last phase. Until then its direct port is closed by the host rules and reported. |
| D19 | Ingress hardening is only what `add_hive` with the ingress role and the orchestrator do by themselves. No infrastructure configuration from the user, and nothing odd, now. |
| D20 | Order of work: the core (Part A stages 1–5, A-48, A-49), then the Fluxbee Cloud connection (stages 6–7), then the implementation nodes. **D30:** stage 5 is withdrawn, so the core is stages 1–4, A-48 and A-49. |
| D21 *(rev 4; amended by D30)* | Nobody configures the posture. ~~The mode is automatic: observe, then enforce by itself after a clean window fixed in code; a changed ruleset goes back to observe.~~ ~~Two local files exist for us, never for users: break-glass and hold.~~ One local file exists for us, never for users: break-glass. No settings travel between hives. |
| D22 *(rev 4; amended by D30)* | No verify probes, commit-confirm timers or quarantines. ~~A ruleset is enforced only after it ran clean in observe.~~ It filters only inbound, keeps established connections, and depends on no remote data. **D30:** CI proves a ruleset before a release, and the host applies it as rendered. |
| D23 *(2026-10-06)* | The Docker lab is removed: it was old and unused. Every stage is validated on the 8.x Proxmox testbed. The Docker quickstart leaves the open-source site. The rest of `lab/` (Proxmox, ops, logbooks) stays. |
| D24 *(rev 5)* | The A-48 guard ships with stage 1: the motherbee refuses `ADD_HIVE_FINALIZE` and `REMOVE_HIVE_CLEANUP`. On the motherbee they are a kill switch any orchestrator can pull, and they depend on nothing in the posture work. |
| D25 *(operator, 2026-10-06)* | Fluxbee is installed only on clean Linux boxes created for it, so there is no existing firewall to coexist with. The orchestrator's ufw/firewalld writes are removed (they are inert on such boxes). An active ufw or firewalld found on a host is reported as a third-party firewall (health fails)~~ and the host stays in observe~~; nothing is written into it. |
| D26 *(operator, 2026-10-06)* | No automatic teardown on downgrade or rollback: the host keeps its last ruleset, which is harmless without per-hive sets, and break-glass is the escape. Only package remove/purge tears down. |
| D27 *(operator, 2026-10-06)* | `remove_hive` does not tear the posture down, so a removed ingress or egress does not reopen SSH on its external interface. Repurposing a box is one documented command. |
| D28 *(operator, 2026-10-06; withdrawn by D30 with the observe phase it refined)* | ~~Under D3 enforce can only cut three things: SSH through non-internal interfaces, undeclared listeners (cut on purpose, and reported) and special UDP/ICMP (allowed explicitly). So the automatic switch is gated by SSH logins through non-internal interfaces plus preconditions, and packet accounting is report-only. Nothing an attacker or a scanner sends can hold a host in observe.~~ |
| D29 *(rev 9, proposed; withdrawn by D30)* | ~~Under D28 observe can only detect SSH logins through non-internal interfaces, and only the role, the internal interface and the sshd ports change where SSH is admitted. So only a render change that moves one of them sends a host back to observe; any other render change is applied in the current mode at the next check. Without D29, every release that changes the render reopens the ingress' public SSH and the egress' office SSH for at least 24 hours, for no evidence observe could gather.~~ |
| D30 *(operator, 2026-10-06, after round 6)* | **No observe phase and no check on the host before applying.** In the operator's words: *"eso de esperar 24hrs es un total parche que no sirve"*, and a check of what the new rules would cut is *"demasiado frágil"*. Each host applies the rules its role declares at its first check, and a later render change at the next one. SSH is closed from outside the infrastructure, always: only the internal network has it, plus D31. A ruleset is proven before a release, in CI: golden renders per role, a kernel test per role and interface, and PROD `ss` captures against each role's declaration. On the host it is applied as rendered; an undeclared listener is only reported; a ruleset that cannot be applied leaves the last one loaded and fails health. Break-glass is the escape. It withdraws the automatic mode of D21, the observe clause of D22, D28 and D29. |
| D31 *(operator, 2026-10-06)* | **One SSH door on the egress, for now.** SSH (tcp 22) stays admitted on `egress.wan_iface` for operation and maintenance during this development. In PROD that is 192.168.8.240, an internal network behind a firewall (*"no es la NSA pero es red segura"*); behind the egress is the Fluxbee internal network. Nothing else is admitted there. It is part of the egress role's declaration, not a setting, so closing it is a code change in a release. The code cannot tell a safe office network from the internet, so the door closes before the first customer install (A-55). |
| D32 *(operator, 2026-10-06)* | **SSH only inside `add_hive`.** In the operator's words: *"no deberíamos tener comunicaciones NUNCA por ssh a no ser cuando se hace un add_hive y es totalmente temporal"*. The join revokes its access at the end, as the default `revoke` mode does today. The motherbee's boot-time TLS reconcile over SSH and the `ssh_access=key_only_persist` mode are removed (A-54). A spoke that loses its mesh certificate is joined again (`remove_hive` + `add_hive`), like a new box. |

### 2.2 Open — the operator decides

The core questions are answered: the operator approved D3 and D16 (2026-10-05), D25–D28 and
D30–D32 (2026-10-06; D25 resolved by the deployment model: Fluxbee runs only on clean boxes created
for it).

Before A-49:

| ID | Question | Status |
|---|---|---|
| O9 | How software enters an installation. The operator wants to keep apt, the natural Linux path (*"no quiero perder el APT natural de linux"*); a build node inside the infrastructure that propagates the software to the motherbee with apt is acceptable; publishing through IO.web with a token was raised. | To be discussed with details after revision 10. A-49 (§3.9) waits for it. |

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

One pure function computes the posture from the host's own role, configuration and interfaces. No
registry entry and no other hive is an input. From it the orchestrator renders:

- the nftables ruleset;
- the expected TCP and UDP listeners;
- the Syncthing settings.

Unit tests pin that every allow has a listener and every listener has an allow. CI also checks PROD
`ss` captures of each role against its declaration (§6), so a release that stops declaring a live
core port fails before it ships (D30).

**Inputs**

- **The role.**
- **The motherbee's ports from config:** the `wan.listen` port and `identity.sync.port`, and
  22000 only while Syncthing runs (blob or dist sync on).
- **On the ingress:** the edge port from `edge.listen`. A non-443 or plaintext edge is reported.
- **On the egress:** `egress.wan_iface`, its external interface, for the SSH door (D31).
- **SSH is tcp 22, fixed in code.** `add_hive` dials 22, Fluxbee runs on clean boxes created for
  it (D25), and SSH is only for joins and for maintenance from the internal network (D32), so
  nothing is derived from sshd. An sshd listening elsewhere shows up as an undeclared listener.
- **The internal interface:**
  - **motherbee:** every interface (deployment model). A public address, or more than one
    non-loopback interface, is reported as a warning.
  - **every spoke** (worker, ingress, egress): fixed at the first successful `ip route get <IP of
    the first wan.uplinks entry>`, and persisted. It changes only when the persisted interface no
    longer exists: then it is derived again and the change reported. A different answer while the
    persisted interface exists is reported, never adopted, so an internal NIC that loses its route
    for a while cannot move SSH to the external NIC. On the egress, a result that differs from
    `egress.lan_iface` is reported; on the ingress and the egress, an internal interface with a
    public (non-RFC1918) address is reported.
  - **Never derived** (the uplink never answered `ip route get`): nothing is applied yet, and it is
    reported. **The persisted interface is gone and nothing answers:** the last ruleset stays
    loaded, and it is reported.
- **Closed lists in code:** the core IO listeners (none in the core phase; IO.web joins at stage 7)
  and the node listeners (none; io.linkedhelper stays out, D18).

**The render hash** is the sha256 of the role's ruleset, with no comments and no build strings. It
follows every template change with no constant to bump. Ports are literals in the rules: the render
has no named sets. Live listeners and the interface list are not in the render.

**A render change** is applied at the next check (D30).

### 3.2 Inbound rules

The order of the render matters:

1. `iif lo` accept.
2. ICMP and ICMPv6 errors by type, whatever the conntrack state: destination-unreachable
   (including frag-needed), packet-too-big, time-exceeded, parameter-problem. The edge port is
   untracked, so the Path MTU errors about its connections are INVALID to conntrack.
3. ICMPv6 neighbor discovery, router advertisements and multicast listener queries; ICMP and ICMPv6
   echo, rate-limited.
4. DHCP client replies: udp 67 → 68, and udp 547 → 546 from `fe80::/10`. DHCPv4 OFFER/ACK arrive
   on packet sockets anyway, but renewals and DHCPv6 replies do not.
5. `ct state established,related` accept.
6. `ct state invalid`: counted and dropped.
7. The role's allows (table below).
8. A report-only counter and rate-limited `log` (prefix `fluxbee-posture: `), then the chain
   policy `drop`.

| Role | Accepted | On |
|---|---|---|
| motherbee | tcp 22, 9000, 9100, 22000 (while Syncthing runs) | any interface |
| worker | tcp 22 | internal interface |
| egress | tcp 22 | internal interface and `egress.wan_iface` (D31) |
| ingress | the edge port, untracked | any interface |
| ingress | tcp 22 | internal interface |

**Spokes expose no Syncthing port.** They dial the motherbee, and since stage 3 their Syncthing
listens on loopback (§3.5).

**Untracked ports.** Only the edge port: raw `notrack` on prerouting for packets to it and on output
for packets from it, so the internet cannot fill conntrack through it. The ingress' own dials use
ephemeral ports and stay tracked.

**Outbound is not filtered** (D19).

**What the rules cut.** Under D3, internal interfaces have no source restrictions, so every
declared listener is accepted there. The rules cut:

1. **SSH through a non-internal interface** (D16), except the egress door (D31). Nobody uses those
   paths (evidence below), and Fluxbee itself uses SSH only inside joins, over the internal network
   (D32).
2. **Undeclared listeners, TCP and UDP, and any other IP protocol** (ESP, GRE…). They are closed on
   purpose ("lo demás tiene que estar cerrado", D18); the listener check reports TCP and UDP
   listeners.
3. **Special UDP and ICMP** not allowed above. The kernel test in CI exercises every allow.

Nothing on the host decides whether to apply them (D30): CI proves the rules for every role and
interface before a release.

**Evidence from PROD.** In the journal since August:

- nobody SSHed into the motherbee (it is operated through the Proxmox guest agent);
- the only SSH logins on the spokes came from the motherbee (10.10.10.10), over eth0;
- none came in through the egress' office address or the ingress' public one.

The PROD path for an Archi tunnel is a jump through a host on the internal LAN that the posture
does not filter (fb-build or the Proxmox host).

### 3.3 Apply, status, drift

**No modes (D30).** Every check takes the facts in this order and acts on the first that holds:

1. **Break-glass** (`/etc/fluxbee/posture.disabled`) present → status `off`: remove the table if
   one is loaded. The boot unit does not load one either. Removing the file applies the rules at
   the next check.
2. **`nft` absent** → status `unavailable`: nothing is applied; reported with the command to run.
3. **The internal interface was never derived** (a spoke) → status `not_derived`: nothing is
   applied yet; reported.
4. **Otherwise** render. If the render differs from the applied one, or the kernel's table drifted
   from the baseline, apply it: status `applied`. If the persisted interface is gone and nothing
   answers, the last ruleset stays loaded: status `derivation_failed`.

A failed `nft -f` changes nothing in the kernel (the file is applied as one transaction): the
previous table stays, and the failure is reported.

**Console escape.** `touch /etc/fluxbee/posture.disabled; nft delete table inet fluxbee_host` works
without the orchestrator.

**Apply.** In one `nft -f`:

```
table inet fluxbee_host {}
delete table inet fluxbee_host
<full definition>
```

**Persist.**

- `/etc/fluxbee/posture.nft`: the last applied ruleset.
- `/var/lib/fluxbee/state/posture.json`: the internal interface, the applied render hash and the
  drift baseline.

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

- **The baseline** is nft's own listing, normalized (no handles, counters or limit state), taken
  right after the apply and stored in the state. Later listings are compared with it.
- **Re-apply only on drift**, and honour break-glass.
- **`nftables.service`:** an enabled one that runs `flush ruleset` is reported as a conflict; its
  effect is repaired as drift.

**Where it runs.** In its own task, about every 60 seconds, with every subprocess bounded to a few
seconds, like the stage-3 Syncthing task. Never inside `watchdog_tick`, which restarts rt-gateway
and repairs the egress NAT and must not wait on it. `/versions` reads the cached result.

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

**Addresses and folders (stage 2) — as built, live in 0.1.58.** Reconciled by the watchdog about once a
minute, not only at join. Only **remote** devices are touched: never the local device (its id comes
from the running Syncthing, `/rest/system/status`), nor the `<defaults>` templates, nor folder
members. Top-level devices are found by structure (`<device>` outside `<folder>` and `<defaults>`),
so a device without a name is seen too.

**Spoke side.** The spoke's device named after the motherbee's `hive_id` gets
`Static(<first wan.uplinks IP>:22000)`. No uplink, or a host that is not an IP literal, is reported
and nothing changes.

**Motherbee side.**

- **Which device is a hive's.** The device id its registry entry records, whatever the device's
  name. Every join records it (the egress joins did not; now they do). An entry that records none
  takes the one remote device named after it, and its id is backfilled; two such devices are
  reported and kept.
- **Hives' devices** get `AcceptOnly`: address `dynamic`, so the motherbee never dials once
  discovery is off. A device left with several addresses ends up with exactly one.
- **Folders (A-50).** A spoke is taken out of every Fluxbee folder its role must never have:
  `blob/active` and `runtimes` outside workers, `blob/public` outside the ingress, another role's
  core folder. The rule is the role alone, never hive.yaml's enable flags, so turning a sync off
  for a while strips nobody. Only the joins add a spoke to a folder, and they never add one of
  these; a test pins that under every flag combination. A new folder starts with no member
  (Syncthing adds the local device when it loads its config): it is never cloned from another
  folder with its members, nor seeded with `config.xml`'s first device, which can be a peer.
  **Known limit:** a folder created after spokes joined (blob or public sync turned on later)
  stays empty until `add_hive` is run again for them; a backfill by role, like policy's from
  vendor, is the planned fix.
- **Orphans are removed** from every Fluxbee folder and from the declarations, connected or not, so
  they stop being authorized: a device no hive claims, or a second device under a hive's name.
- **When it acts.** Nothing while any `add_hive` or `remove_hive` holds or is taking a topology
  lock. Only hives at rest are touched: `status: connected` and their join `done`. Orphans are
  removed only when the registry was read completely, is not empty, and every hive in it is at rest
  and records its device. The registry is read under the config lock: a join creates its entry
  before it links, and links under the same lock.

**The address type** is `AcceptOnly | Static(SocketAddr)`, IP literals only.

- A finalize that links a peer without a static address, or with one that is not a
  `tcp://<ip>:<port>` literal, is refused, never written as `dynamic`.
- The worker join derives the motherbee address from the uplink, as ingress and egress do.
- The local device id comes from `syncthing --home <dir> device-id`, bounded to 5 s, when
  `cert.pem` and `key.pem` are regular files (Syncthing v2 dropped the `--device-id` flag, so that
  call always failed: A-53), otherwise from the running Syncthing; never from `config.xml`'s first
  device, which can be a peer.

**One serialized writer** for `config.xml`. The directory belongs to the Syncthing user and the
orchestrator is root, so no name in it is trusted:

- one lock; the update never calls Syncthing (the REST read happens before the lock, bounded);
- the file is read with `O_NOFOLLOW|O_NONBLOCK` and refused unless it is a single-link regular file
  of at most 16 MiB;
- the temp is created with `O_EXCL` under a random name, gets the owner and permission bits of the
  file that was read (special bits dropped) through its descriptor, and is renamed over the file;
  the directory is then fsynced, and temps left by a crash are swept under the lock;
- the API key is read without following a symlink or blocking on a FIFO (hard links allowed, so
  hard-link backups do not break Syncthing management);
- a CI guard keeps the atomic replace to one caller and no other write on a line naming the file.

**Restarts.** Syncthing restarts only when the reconcile wrote something. The pending restart is a
marker file, so it survives an orchestrator restart, and it waits while a topology operation runs
(a join waiting for its spoke to connect must not see every link drop).

**Reports.** A condition that lasts (an ambiguous name, a kept orphan, a hive not at rest, an
invalid id, an unknown role, a missing uplink) is logged once, when it appears. Nothing is reported
while a topology operation runs; one that lasts more than 15 minutes is. The device-id backfill
waits for an entry's join record to say `done`, so it never races a join's tail.

**An ingress join waits 90 s** for the spoke to connect: the motherbee only accepts now, so the
spoke dials, and Syncthing's default reconnection interval is 60 s until stage 3.

**Listen addresses do not change in stage 2.**

**Options (stage 3) — as built (0.1.59).** The orchestrator owns them, and leaves every other
option as it is:

- global and local announce, relays, NAT and crash reporting off;
- `urAccepted=-1` (no usage reports), `autoUpgradeIntervalH=0`, `stunKeepaliveStartS=0` (STUN off);
- `reconnectionIntervalS=10`;
- exactly one `listenAddress`: on the motherbee `tcp://:22000` (dual-stack), on spokes
  `tcp://127.0.0.1:22000` (they only dial).

With these, Syncthing v2.0.14 opens no UDP socket (no local discovery, QUIC, UPnP or STUN) and
reports `discoveryEnabled: false`. The whole set switches together, in one write.

**The order is enforced in code, and each switch happens once:**

1. **A spoke switches** when its motherbee device has the static address in config.xml and a
   plain TCP connect to it succeeds. Both facts are recorded the first time they hold, in
   `/var/lib/fluxbee/orchestrator/syncthing-posture.json` (written durably; a file that does not
   decode is moved aside by its writer and the facts are derived again), and never undone (D22).
   From the switch on, the spoke keeps the owned options. While the spoke runs an
   `ADD_HIVE_FINALIZE`, its reconcile neither writes config.xml nor restarts Syncthing, and the
   finalize's convergence probe asks again while the API port refuses connections (a restart),
   for at most 30 s.
2. **Every hive reports its facts** in its `/versions` snapshot (`syncthing_posture`: whether it
   runs Syncthing, the two facts, switched).
3. **The motherbee polls**, about every 60 s, in its own task (not the single-flight watchdog
   tick). It waits for every registered spoke, and for any device still linked in a Fluxbee
   folder that no registry entry records. A spoke that runs no Syncthing is not waited for. An
   offline spoke, or one on a release before stage 3, keeps it waiting, and the report names it
   with the reason.
4. **The motherbee switches** once all of them report both facts. The task records the switch;
   the watchdog's peer reconcile then writes the owned options in the same write as the peer plan
   and restarts Syncthing once. A lost record with the owned options already in place is recorded
   again without asking the spokes.
5. **Both switches are one-way.**

**Unit tests** cover the options edit on the PROD motherbee's block, both predicates, the
snapshot-to-facts contract, the spoke's static fact on PROD shapes (IPv6, the local device), the
wait set from the registry and the folders, and the motherbee's combined write.

**Residuals.**

- "Reachable" is a TCP connect to the first WAN uplink at the Syncthing port. In Fluxbee's
  topology that uplink is the motherbee; a NAT or a balancer in between is not supported.
- After the motherbee switches, a spoke restored to a state before stage 2 (an old snapshot or
  release, with the motherbee `dynamic`) cannot sync, so it cannot get a core update through dist.
  It is recovered by re-joining it over SSH.

### 3.6 Reports and checks (report-only)

| Check | What it reports |
|---|---|
| Listener | From stage 1, on every role: `sy-architect` or `sy-admin` off loopback. From stage 4: every non-loopback TCP and UDP listener not in the declaration, by process name and port, the DHCP client excluded. `ss -H -tlnp` and `ss -H -ulnp` run separately: their layout is the one the stage-1 parser reads (`-tulnp` adds a column). A `systemd`-owned listener on tcp 22 is sshd. One alert kind per process and port, as stage 1 does per process, so a second listener is not hidden by the first one's dedup hour. |
| sshd | `PasswordAuthentication yes` and `PermitRootLogin yes`, as warnings. The fix is A-2; the PROD motherbee has password login on. |
| Mesh auth | `wan.mtls` other than `required` (motherbee) and `identity.sync.auth: disabled`, as warnings. D3 and A-48 rest on them. |
| Third-party firewall | An active ufw (`/etc/ufw/ufw.conf` `ENABLED=yes`, independent of the locale) or a running firewalld: health fails (D25); nothing is written into it. Any other non-Fluxbee base chain on the input hook that has rules or a policy other than accept (fail2ban, LXD) is reported as information; empty accept chains, such as iptables-nft leftovers, are not. |
| Packets | The report-only drop counter and the INVALID counter. The rate-limited log lines stay in the journal (`journalctl -k -g fluxbee-posture`) for a person to read. |
| Conntrack | Fill level. Loading `fluxbee_host` enables conntrack on hosts that did not track before. |
| apt source | Whether the Fluxbee source is `signed-by` or still `[trusted=yes]` (A-49). |
| Motherbee | A public address, or more than one non-loopback interface, as a warning. |
| Uplinks | A spoke with more than one `wan.uplinks` entry, as a warning. A spoke has exactly one uplink, the motherbee; every join renders one. |

**Where it shows.**

- The posture task (§3.3) computes it and caches it; `local_versions_snapshot` returns the cached
  value, and `/versions` brings every hive's to the motherbee. The posture block carries the active
  conditions (kind, since, detail): that is where a spoke's conditions are read, because
  `GET /hives/<h>/drift-alerts` is answered by the motherbee from its own store. The alerts also go
  to each host's local drift store (`category=posture`): the motherbee's history is at
  `GET /hives/motherbee/drift-alerts`, a spoke's is in its local file.
- **`ops.py health` fails on:** `off` (break-glass), drift that was not repaired, `unavailable`, a
  failed apply, a third-party firewall, and `not_derived` or `derivation_failed` older than 5
  minutes. Before 5 minutes those two are warnings, since a WAN outage after a reboot or a deploy is
  normal.

### 3.7 Lifecycle

**nftables.** The orchestrator installs nothing.

- Ubuntu 24.04 images ship `nft`, and the four PROD hosts have it. The stage-4 gate checks a fresh
  clone of template 9000 (`command -v nft`); if it lacks `nft`, nftables goes into the template,
  never into the orchestrator. The motherbee's `.deb` depends on nftables. The HANDBOOK's "install
  nftables before an egress join" is corrected with the stage.
- Without `nft` a host is `unavailable` (unfiltered), and health fails naming
  `apt-get install nftables`.
- The egress keeps failing loud without `nft`, because its NAT needs it.

**ufw / firewalld** (D25, operator 2026-10-06).

- Fluxbee runs only on clean Linux boxes created for it, so the orchestrator's ufw/firewalld
  writes are removed.
- An active ufw or firewalld found on a host is a third-party firewall (§3.6): health fails, and
  nothing is written into it.

**`add_hive`.**

- It refuses `hive_id` `motherbee` before issuing any leaf.
- Nothing is admitted on the motherbee beyond the A-48 pin.
- The join's bootstrap deletes a `posture.disabled` and a posture state left by an earlier life of
  the box.
- SSH exists only inside it, and its access is revoked at the end (D32).

**`remove_hive`** (D27, operator 2026-10-06).

- The cleanup script runs in a transient unit outside sy-orchestrator's cgroup, two seconds later,
  so the REMOVE_HIVE_CLEANUP reply leaves first:
  `systemd-run --unit=fluxbee-remove-cleanup --collect --on-active=2s …`, as the self-restart does.
  Today the script dies when it stops sy-orchestrator, so nothing after it in the loop ever ran:
  fluxbee-syncthing stayed up and enabled on removed spokes.
- It stops and disables sy-orchestrator first, so its watchdog and reconcile cannot restart what
  the script stops; then the managed nodes (`systemctl stop 'fluxbee-node-*'`); then the other core
  services and fluxbee-syncthing. It deletes node state and the mesh TLS directory, on every role.
- On an ingress it also stops and disables sy-edge and deletes its publications.
- The posture is **not** torn down: the removed host keeps its rules (SSH on its internal interface
  only, plus the egress door).
- **To repurpose a box:** `nft delete table inet fluxbee_host; systemctl disable
  fluxbee-host-nft.service; rm /etc/fluxbee/posture.nft /var/lib/fluxbee/state/posture.json`.

**Upgrade.** A render change is applied at the next check (D30).

**Downgrade or rollback to a build without the posture** (D26, operator 2026-10-06). The host keeps
its last ruleset, which the boot unit keeps loading. That is harmless: SSH on the internal
interface, the motherbee's mesh ports and the edge port are admitted. Break-glass is the escape.
There are no version comparisons.

**Package remove / purge.**

- The prerm tears down the table, the file, the state and the unit only on `remove`; a new postrm
  does it on `purge` (build-deb.sh ships it). Never on `upgrade`, `failed-upgrade`, `deconfigure`
  or the abort cases: dpkg runs the old package's prerm at the next upgrade, so a teardown reachable
  there could not be fixed by a later release. Today's prerm sends `deconfigure` to its removal
  branch; it gets its own arm.
- `nft delete table` tolerates a missing table (the scripts run under `set -e`), and the teardown
  does not depend on systemd being up.
- **Test:** on the built package, `dpkg-deb -I` shows the prerm and the postrm; each script runs
  with every dpkg argument, with `nft` and `systemctl` stubs on PATH under a temporary root,
  including a table that is already gone.

**Egress.** `fluxbee_egress` stops rendering its input chain. No `delete chain` statement: the file
is re-applied every minute and loaded at every boot, and a delete would fail on the second apply.
The leftover chain is empty with `policy accept`, which cannot override fluxbee_host's drop, and it
is gone at the next boot. This supersedes edge-egress-nat-spec §3.3 and §8.5; a dated note there
says so.

### 3.8 A-48 — `remove_hive` revokes the hive

**The router's view.** The motherbee orchestrator builds `/var/lib/fluxbee/state/router-hives.json`
from the registry, under one global lock, and writes it atomically. It is a pure function of the
registry, rendered deterministically (sorted, no timestamps), so it can be compared byte for byte.

- **Contents:** every registered hive with its leaf pin, or `legacy` (below).
- **Only from a complete listing.** If listing the registry fails, the previous view stays and
  nothing is revoked. An unreadable entry keeps its line from the previous view and is reported. A
  complete, empty listing is a valid, empty view (a motherbee with no spokes).
- **When:** at every boot before rt-gateway starts, on `add_hive` / `remove_hive`, and on every
  watchdog run when the file is missing, when it differs from the registry, or when the router has
  recorded a legacy leaf that is not pinned yet.
- **The router reloads the view when it changes**, and on every load, the first included,
  re-checks every live session against it.
  - No readable view at startup → today's admission (any CA-valid peer), a loud report and a
    health failure, until the watchdog rewrites it.
  - A read error after a good load → keep the last good view.

**The pin** is the SHA-256 of the leaf's DER encoding, on both sides: the orchestrator decodes the
PEM it issued, the router hashes the DER that rustls hands it.

**Legacy is a missing pin.** An entry with no `pin` key at all is `legacy`; only a build before
A-48 writes one. An A-48 join writes `pin: pending` on its accept path and the real pin right after
it issues and pushes the leaf, before the spoke's router starts; `pending` is refused. The worker
join's socket-only path (an orchestrator already answering on the socket) keeps an existing pin;
with none, the join takes the SSH path, which issues and pins a leaf. Status no longer decides
admission, so a pre-A-48 hive that a late join error left `failed` while it runs is still admitted,
and pinned like the rest.

**Legacy hives are pinned once.** While a view is loaded, the router records each legacy admission
(`hive_id` → leaf hash) in `/var/lib/fluxbee/state/router-legacy-leaves.json`, written atomically.
The watchdog reads it under the view lock. A hive with exactly one distinct recorded leaf gets that
pin, and the view is rebuilt. A hive with two different recorded leaves is pinned to neither and
reported as `legacy_conflict`. A record is dropped once its pin is written or its entry is removed.
The three PROD spokes are therefore pinned within a minute of their first session after A-48, with
no console work.

**Admission, on the motherbee's accept path:**

- A WAN peer is admitted only if it presents a CA-valid TLS leaf, its `hive_id` is in the view, and
  the leaf matches the pin, or the entry is `legacy`. A legacy admission still requires the TLS
  leaf: under `wan.mtls: permissive` a plaintext HELLO is never admitted as a legacy hive.
- A join's leaf push and pin write are fatal, like the HMAC key push: a join whose push or pin
  write fails stops there with that error.
- A spoke whose certificate is gone cannot connect (mTLS is required). It shows as disconnected and
  is joined again (D32); nothing is re-issued over SSH.
- **Sessions.** There is one live session per hive, and a spoke has one uplink (§3.6). Each
  `wan_peers` entry stores its session epoch, the leaf hash it was admitted with and a cancel handle
  the read loop selects on. Admitting a session for a hive closes the previous one. A view reload
  cancels the entries whose hive is gone, or whose leaf hash differs from a pin now in the view; a
  legacy session whose leaf became the pin stays. An exiting session removes the entry only if its
  epoch still matches.
- Every admission and refusal is logged with the `hive_id`, the leaf hash and the verdict
  (`pinned`, `legacy`, `not_in_view`, `pending`, `pin_mismatch`); pin writes and `legacy_conflict`
  are logged too.

**The other side.** A spoke accepts a WAN uplink only if the server's leaf carries `hive_id`
`motherbee`. A removed box or a compromised hive that takes the motherbee's address cannot become
a spoke's peer.

**`remove_hive`, in this order:**

1. REMOVE_HIVE_CLEANUP (§3.7).
2. Read what the next steps need (its device id) and delete the registry entry.
3. Run the revocation reconcile.

**Revocation is a reconcile from the registry**, like the Syncthing orphans. The view is rebuilt
(the router closes the removed hive's sessions); every identity HMAC key with no registry entry is
deleted, and sy-identity restarts only when a key was actually deleted (its replicas reconnect); the
removed hive's Syncthing device is unlinked. It needs a complete listing, like the view. It runs at
every boot, after every `remove_hive`, and for a `remove_hive` on a missing entry, which answers ok.
A crash at any step therefore heals at the next boot, and the view never has to differ from the
registry.

**SSH host keys** (D32). SSH happens only inside a join. Every `ssh` and `scp` is built by one
helper, which adds `-o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=<file>` with one file
per join under `/var/lib/fluxbee/ssh/known_hosts.d/` (outside the registry root; the directory is
mode 0700), deleted when the join ends. Within a join the box's host key cannot change; each join
trusts the first answer (§0). "Host key verification failed" and "REMOTE HOST IDENTIFICATION HAS
CHANGED" are classified before any auth-failure check: the join fails with `ssh_host_key_changed`.
A CI guard checks that `Command::new("ssh")`, `Command::new("scp")` and `.arg("ssh")` appear only
in that helper, and a unit test pins its argv.

**Registry writes** become read-modify-write merges under the per-hive lock, so a join keeps keys
it does not own (the pin, the device id). Registry writes never create a hive's directory; only
the `add_hive` accept path does, so a late write cannot bring a removed hive back.

**The motherbee refuses every `SYSTEM_CORE_ROLLBACK`.** Nothing legitimate sends one to it: its own
`core_rollback` runs locally. It joins the spoke-only actions (D24), so a rollback to a core before
A-48, which would bring back a router that ignores the view, cannot be pushed to the motherbee.

**What it rests on.** `wan.mtls: required` on the motherbee (every join renders spokes with
`mtls: required`) and `identity.sync.auth` on; §3.6 warns otherwise.

**Already in 0.1.57:** the motherbee refuses `REMOVE_HIVE_CLEANUP` and `ADD_HIVE_FINALIZE` (D24).

### 3.9 A-49 — signed apt repo (waits for O9)

How software enters an installation is open (O9): the operator wants to keep apt, the natural
Linux path, and decides after a detailed discussion. This section keeps round 6's corrections so
they are ready; nothing here is built before O9.

- **The key.** One ed25519 (or RSA ≥ 3072) key. The public key is a file committed to the repo,
  also published next to the apt repo with its fingerprint in the install docs. The private key has
  no passphrase and no expiry; it lives on the build host and is backed up off it.
- **Publishing, signed first.** `scripts/apt-repo-publish.sh` builds `Packages*`, `Release` (with
  `Origin: Fluxbee`) and `InRelease` in a staging directory, and moves them in only after signing
  succeeded: a publish without the key fails before it touches the repo. A client that straddles
  the move can still get a Hash Sum mismatch, so `ops.py deploy` retries `apt-get update` once when
  it sees one.
- **First install.** One path: the documented `[trusted=yes]` line. The helper switches it at the
  first postinst; the user does nothing.
- **The switch** is one idempotent helper, like `fluxbee-migrate-config`, that the postinst calls
  with `|| true`: it never fails the install and never runs network I/O.
  - The Fluxbee source is the entry whose cached `InRelease` verifies with `gpgv` against the
    shipped keys and says `Origin: Fluxbee`. No address is written in code. With other than exactly
    one such entry it changes nothing and reports it.
  - It rewrites that line in place to `[signed-by=/etc/apt/keyrings/fluxbee.gpg]`. The keyring is
    one binary file, mode 0644, owned by root and by no package (a downgrade keeps it), so the line
    never changes after the switch.
  - The keyring is append-only: every run adds a shipped key that is missing, and never removes or
    replaces one.
- **Key rotation.** Sign `InRelease` with the old and the new key; ship a package that carries the
  new key (the keyring adds it); drop the old signature; ship a package without the old key; then
  remove it from the keyring with the runbook step. A downgrade to a package that still carries the
  old key adds it back (§0).
- **`ops.py deploy`** prints the `W:` and `E:` lines of `apt-get update`, so a signature failure is
  visible.
- **Residuals:** the build host holds the signing key, so A-49 stops impersonation of the repo, not
  a compromise of the build host.

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
| 0 | Revisions 4–10 and DTAP rounds 1–6; the operator approved D3 and D16 (2026-10-05), D25–D28 and D30–D32 (2026-10-06). | A focused re-check of revision 10 (§3.1–§3.3, §3.6–§3.9, §6) before stage 4. |
| 1 | Binds, the listener check and the spoke-only guards (§3.4), released as 0.1.57. | **Done: validated on 8.x on 2026-10-06 (see the ledger).** Unit: listener fixtures (the PROD capture flags only sy-architect 0.0.0.0:3000; `*`, `[::]`, scope before and after the brackets; several owners or none); the spoke-only guard; 19 migration checks; the CI guard. Infra, after `ops.py deploy`: (1) `ss` on the motherbee shows 3000 and 8080 only on 127.0.0.1; (2) `lab/posture-probe.sh 10.10.10.10 3000 8080 9000` from VMs 101, 102, 103 and 110 gives refused, refused, open (baseline on 0.1.56: open, refused, open); (3) on the motherbee, `curl 127.0.0.1:3000/` returns the UI and `curl 127.0.0.1:8080/hives` answers; (4) positive control: a dummy listener whose process is named `sy-admin`, on a spare port of 10.10.10.10 for about 20 s, produces `loopback_service_exposed_sy-admin` within two minutes (`GET /hives/motherbee/drift-alerts?category=posture`), and sy-architect raises no alert after the deploy; (5) the postinst printed its migration line. The operator checks the tunnel once, outside the gate. |
| 2 | Syncthing addresses and folders (§3.5). **Live in 0.1.58, gate passed on 2026-10-06.** | **Unit:** the address type; IP literals only; v6 formatting; the finalize rule; the spoke's uplink address; PROD-shaped fixtures (both member forms) reach the target in two rounds and then change nothing; the egress leaves `blob/active`; the role rule under every enable-flag combination; ownership by recorded id; ambiguous names, missing recorded devices and nameless devices; nothing while busy, nothing on a hive not at rest, no removal on an unreadable, empty or unrecorded registry; the writer refuses symlinks, FIFOs and hard links, never opens a planted temp, keeps the mode and drops special bits. **CI:** the single-writer guard. **Before the deploy (T5-1, done 2026-10-06):** the throwaway VM 104 joined as `worker2` on 0.1.57 (`dynamic` on both sides) and was stopped; it stays registered and stopped until stage 3. **Infra** (`lab/posture-check.py -- --offline worker2` through `ops.py run`, on the 4 hives): the target on each; completion 100% on the motherbee for the 3 PROD spokes; the egress' pending `blob/active` offer gone; egress1's device id in its registry entry; `ops.py opa-status` in_sync 4/4; listen addresses unchanged; Syncthing uptime growing across three checks (no restart loop). The "a join on this release gives `Static`" check moves to the A-48 join. |
| 3 | Syncthing options and the ordering predicates (§3.5). **Live in 0.1.59, gate passed on 2026-10-06** (worker2 skipped from 0.1.57). | **Skip-upgrade, with no PROD rollback:** `worker2`, joined on 0.1.57 before the stage-2 deploy and kept stopped (T5-1), is the `dynamic`↔`dynamic` case, like worker1 was. After the deploy, the motherbee keeps discovery on and names `worker2` in its journal as the spoke it waits for, while the three PROD spokes switch (`lab/posture-check.py -- --offline worker2 --stage 2` still passes on the motherbee; `--stage 3` passes on the spokes). Boot it and update it directly from 0.1.57: it gets a static address and switches, then the motherbee switches. **Then, on the 4 hives and worker2** (`--stage 3`), with Syncthing uptime growing across three checks: `/rest/config/options` exact; `/rest/system/status` shows discovery off; no relay or QUIC connections; no non-loopback UDP for Syncthing in `ss`; the motherbee listens on `tcp://:22000`; after a motherbee Syncthing restart every spoke reconnects within 30 s (motherbee `/rest/system/connections`); a dist publish reaches 4/4. **Unit:** both predicates on PROD fixtures and synthetic reports. Remove the throwaway afterwards. |
| 4 | The firewall, applied directly (D30): §3.1–§3.3, §3.6, §3.7. Before it: 0.1.60 (A-51, A-52) and 0.1.61 (A-54: SSH only inside `add_hive`, D32). | **Unit:** per-role render invariants: on a non-internal interface the only accepts of new connections are the ingress edge port and the egress door (tcp 22 on `egress.wan_iface`); no role accepts 3000, 8080, 5432, 4222 or 8384; ICMP errors and DHCP replies come before established; only the edge port is untracked; no named sets. Golden renders per role, kept byte-equal to the renderer by a test. The `delete table` render is deterministic; the egress render has no input chain and survives a double apply. The status order as a table (break-glass, `nft` absent, never derived, derivation failed, a failed apply, drift, applied). The interface derivation: IPv4, IPv6 with a scope, the persisted value kept while it exists, a different answer reported and not adopted, a vanished interface derived again. PROD `ss` captures (TCP and UDP) of each role against its declaration: every rt-gateway, sy-identity, syncthing, sy-edge and sshd listener is admitted on at least the internal interface. Listener fixtures by process and port; third-party fixtures (`ufw.conf` `ENABLED=yes`, a running firewalld; the PROD `nft list ruleset` capture gives none; a foreign chain with rules is information); the drift normalizer. **CI:** a network-namespace job in its own workflow with sudo, fed the committed golden renders. For each role, a host namespace with an internal and a non-internal veth (the motherbee two internal ones), probed over IPv4 and IPv6: tcp 22 open on the internal veth and a timeout on the other (open on the egress' `wan_iface` veth); 9000, 9100 and 22000 open on the motherbee; the edge port open on both veths of the ingress; 9000 and 22000 time out on spokes; an unlisted port times out. It delivers udp 67 → 68 and udp 547 → 546 from `fe80::` to bound sockets, completes an IPv6 connect (NS/NA), accepts an RA, answers a ping, and accepts a frag-needed and a packet-too-big for an untracked edge flow. For the egress it applies the 0.1.57 render, then the new one twice: forward and NAT stay. The packaging test (§3.7). **Baseline, before the deploy:** reboot the motherbee and compute the rejoin time from timestamps: rt-gateway's `ActiveEnterTimestamp` on the motherbee, the motherbee's Syncthing connection lines and each spoke's first answer to `/versions` (clocks synced). **On the throwaway first** (VM 104 at `clean`, joined as a worker on this release): status `applied`, `lab/posture-check.py -- --stage 4` passes against its oracle, and the join itself ran over the internal network. **Infra, after the deploy** (snapshot first): `ops.py run <vm> lab/posture-check.py -- --stage 4` on every hive, against the oracle inside the script: internal interface eth0 on the three spokes; edge port 443 on ingress1; the egress door on its `wan_iface`; `fluxbee-host-nft` enabled; `posture.nft` present. From fb-build (`ops.py run 110`), `lab/posture-probe.sh`: 10.10.10.10 gives 22, 9000, 9100 and 22000 open and 9999 a timeout; 10.10.10.20, .30 and .40 give 22 open; 192.168.8.240 gives 22 open (D31) and 9999 a timeout. From the operator's Mac (a manual step): on the ingress' public address only 443 connects (`nc -z -G 3` on 443, 22, 3000, 8080, 9000, 9100, 22000), and `curl -so /dev/null -w '%{http_code}'` through the public name answers (its TLS handshake is larger than one MTU). From worker1 (`ops.py run 101`): `curl -s https://ifconfig.me` shows the egress' address. Injections on worker1, each undone once seen (at most two checks): flush the input chain (drift reported and repaired); a dummy UDP listener on a spare port (an alert for that process and port in worker1's `/versions` posture block); break-glass (the table goes; a reboot with sy-orchestrator disabled loads none; removing the file and re-enabling applies the rules again). The egress reboots: `ops.py health` `failed=0`, ifconfig.me from worker1 again, `fluxbee_egress` has no `chain input`. A motherbee reboot: the rejoin time, computed the same way, within the baseline + 60 s (the WAN backoff reaches 60 s). Completion 100%, `ops.py opa-status` in_sync 4/4, a dist publish reaches 4/4, `ops.py health` `failed=0`. `command -v nft` on a fresh clone of template 9000. |
| 5 | — | Withdrawn by D30: with no observe phase there is no separate enforce stage. |

**After stage 4:**

| Item | Gate |
|---|---|
| A-48 (its spoke-only guard shipped with stage 1, D24) | **Unit:** the admission function as a table (pinned; legacy = no `pin` key; `pending` refused; not in the view; pin mismatch; no view at startup; a read error after a good load; plaintext never legacy); the pin as the DER's SHA-256 on both sides; the spoke's uplink check with fixture leaves (CA, motherbee, worker); the view build (byte-deterministic; only from a complete listing, and a complete empty one is valid; an unreadable entry carried forward; legacy from a missing pin); first-use pins (one recorded leaf pins and rebuilds the view; two give `legacy_conflict`; a record is dropped on removal; nothing recorded while no view is loaded); the session map (admitting B closes A; A's late exit leaves B; a reload cancels a removed hive and a changed pin, and keeps a legacy session whose leaf became the pin); the socket-only join keeps a pin and takes the SSH path without one; a join's push or pin-write failure stops it; registry writes keep the pin and the device id and never create a directory; `remove_hive` deletes the entry before it reconciles; the revocation reconcile deletes only HMAC keys with no entry; the motherbee refuses every `SYSTEM_CORE_ROLLBACK`; the host-key classifier on OpenSSH stderr fixtures ("REMOTE HOST IDENTIFICATION HAS CHANGED", "Host key verification failed", exit 255 with empty stderr); the SSH helper's argv. **CI:** `Command::new("ssh")`, `Command::new("scp")` and `.arg("ssh")` appear only in the helper. **Infra**, on one `hive_id` on VM 104, in this order: (1) **Join:** roll back to `clean` and join as a worker: the pin is written before the spoke dials (logged); the per-join `known_hosts` file is gone afterwards; after the join, and across a restart of the motherbee's orchestrator, the box's sshd journal has no line naming 10.10.10.10 (D32). (2) **Remove with the box down:** copy its TLS directory to /root, stop sy-orchestrator on it, remove the hive (`remote_cleanup` `local_only` or `socket_timeout`): no ESTABLISHED 4-tuple from the box on 9000/9100 in two `ss` samples 2 s apart; the router logs `not_in_view` and sy-identity "no key for hive"; worker1's identity replica reconnected; the device is gone from the motherbee's `/rest/config/devices`. (3) **Refusal:** start sy-orchestrator on the box: it keeps being refused. (4) **Re-add:** give the box bootstrap access again from the console (`pve.py`; D32: like a new box) and re-add the hive: admitted by its new pin. Swap the old leaf back and restart rt-gateway on the box: `pin_mismatch`; swap the new one back: admitted. Its motherbee device is `Static(10.10.10.10:22000)` and completion reaches 100%. (5) **Ingress branch:** roll back to `clean`; check that `admin.public_edge_node` names `SY.edge@ingress1`, setting it first if needed; join the box as an ingress with `ingress.listen` on a spare port and `allow_plaintext`: sy-edge is active and enabled. Remove it: sy-edge and fluxbee-syncthing inactive and disabled, the mesh TLS directory gone, the rules still loaded, and after a reboot the table loads again. (6) **No view:** stop VM 104; copy the view, delete it and restart rt-gateway: the spokes reconnect within about 10 s, the report fires and health fails; within a minute the watchdog rewrites the view, byte-identical to the copy, and health passes. (7) **Legacy:** after the deploy the 3 PROD spokes are admitted as legacy and pinned within a minute (pin writes logged with the leaf hash), and stay connected after a motherbee reboot. (8) **Stale state:** worker2's HMAC key, left by the stage-3 remove, is deleted at A-48's first boot; a hand-made key for a hive with no entry is deleted at the next orchestrator restart, and :9100 refuses that hive. |
| A-49 | **Waits for O9.** When it goes ahead: `ops.py publish` signs without a terminal. A publish without the key, run on a copy of the repo (`--repo`), exits non-zero, leaves every index file byte-identical (sha256 of `Packages*`, `Release` and `InRelease` before and after), and a client pointed at the copy still updates with no `E:`. After the deploy the motherbee's source line reads `[signed-by=/etc/apt/keyrings/fluxbee.gpg]`, and a second `apt-get update` prints no `W:`/`E:` for it. **Tamper test** on a copy (`Release` included) with `-o Dir::Etc::sourcelist=… -o Dir::Etc::sourceparts=- -o Dir::State::Lists=/tmp/lists -o Dir::Cache=/tmp/cache`: first the untouched copy updates cleanly (positive control); one flipped byte gives `E:`; a deleted `InRelease` with `Release` kept gives "not signed". **First install** on VM 104 at `clean`: the documented one-liner, then a second update with no `W:`/`E:`, then a rollback. **Helper fixtures:** a valid cached `InRelease` adds the key and switches; a missing or bad one, or no `Origin: Fluxbee`, keeps `[trusted=yes]`; two matching entries change nothing; `[arch=amd64 trusted=yes]` and a symlinked list file are handled; an `InRelease` signed by two keys, one of them unknown; the cached-`InRelease` file name taken from PROD; an existing key is never replaced, a missing shipped key is added. `/versions` shows the source mode. |

The throwaway hive is VM 104 (10.10.10.60), a 2 GB clone of template 9000. For stage 4 and A-48 it is cloned again and settled once; its apt timers (`apt-daily.timer`, `apt-daily-upgrade.timer`) are disabled on it alone, so a rollback does not start an upgrade at boot (the joins would wait for it anyway, A-52). It is snapshotted `clean` and rolled back to `clean` before each join that needs a fresh box; that uses 1 of its 3 snapshots. It is kept stopped between uses and during builds (host RAM, B-15).

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
- **2026-10-06, DTAP round 4 on revision 6:** 68 findings, 2 blockers (the egress `delete chain`;
  A-48 admitting the old box during a re-join). Acceptance passed with observations for the first
  time.
- **2026-10-06, revision 7:** round 4 folded in (§8).
- **2026-10-06, stage 2 built:** four adversarial code reviews. The first found the writer
  following symlinks planted in the Syncthing user's directory (major); the second found the folder
  rule driven by hive.yaml's enable flags, which would have stripped every spoke for good (major).
  The third and fourth found minors only: new folders cloned or seeded with peers, Syncthing v2's
  missing `--device-id` (A-53), races with a join's tail. All fixed; see §3.5.
- **2026-10-06, A-50:** found on PROD with `lab/posture-check.py`: the motherbee shared
  `blob/active` with the egress. Fixed in stage 2.
- **2026-10-06, DTAP round 5 on revision 7:** 65 findings, 3 blockers. A5-1 and P5-1 (the orphan
  rule, read literally, removes the motherbee's own device) and T5-1 (the skip-upgrade precondition
  had to be built before stage 2) are folded into revision 8. The rest is for revision 9.
- **2026-10-06, stage 2 live:** 0.1.58 deployed and validated on 8.x: the first round did exactly
  what the plan predicted (one Syncthing restart on the motherbee and one on worker1), every gate
  item passed. See lab/DEPLOYMENTS.md.
- **2026-10-06, stage 3 built:** two adversarial code reviews. The first found that a spoke's
  first switch could restart Syncthing under a join's finalize probe (major); fixed with a
  finalize guard and a probe that rides out a restart. The rest were minors: one writer of the
  motherbee's options, the wait set from the registry and the folders, a durable facts file.
- **2026-10-06, stage 3 live:** 0.1.59 deployed; the three PROD spokes switched by themselves,
  the motherbee waited for `worker2` by name, `worker2` skipped from 0.1.57 to 0.1.59 and
  switched, then the motherbee switched with one restart. No Syncthing UDP socket on any hive;
  spokes reconnect in 10.5 s; a dist publish reached every spoke. See lab/DEPLOYMENTS.md.
- **2026-10-06, the operator decided D25–D28;** D25 is resolved by the deployment model (Fluxbee
  runs only on clean boxes created for it, so nothing coexists with another firewall).
- **2026-10-06, revision 9:** round 5 folded into stages 4–5, A-48 and A-49 (§8); D29 proposed.
- **2026-10-06, DTAP round 6 on revision 9:** 70 findings, no blocker. Testability failed: revision
  9 had pasted its stage-4 and stage-5 gates into the History table, and the decision list still
  could not become a table test. The other three lenses passed with observations.
- **2026-10-06, the operator decided D30–D32:** no observe phase and no check on the host before
  applying; one SSH door on the egress for now; SSH only inside `add_hive`. O9 (how software enters
  an installation) is open, with apt kept.
- **2026-10-06, revision 10:** the mode machinery removed; round 6 folded in where it still applies
  (§8).
- **Next:** a focused re-check of revision 10 (§3.1–§3.3, §3.6–§3.9, §6), then stage 4, after
  0.1.60 and 0.1.61.

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

### Round 6 → revision 10

Withdrawn with the mode machinery (D30): the decision list, the SSH-login evidence, the hold, the
release switch, `switch_enabled_since` and the 48-hour health rule. A finding that was only about
those is marked "withdrawn".

| Round-6 findings | Where they went |
|---|---|
| D6-1, T6-1, A6-1, P6-1 | §6: the stage-4 gate written into §6; stage 5 withdrawn; the History rows for revisions 4 and 5 restored and the table ordered. |
| D6-2, D6-3, T6-2, A6-3, P6-11 | Withdrawn (D30); §3.3's status order replaces the decision list. |
| D6-6, A6-2, P6-6, T6-13 (D29 part) | Withdrawn with D29 (D30). No check on the host: CI checks PROD `ss` captures against each declaration (§3.1, §6). |
| D6-8, T6-10, A6-4, P6-5, P6-12 | Withdrawn with the SSH-login evidence (D30). |
| T6-4, A6-7 | Withdrawn with the 48-hour rule (D30). |
| P6-13 | §3.1: SSH is tcp 22, fixed; nothing derived from sshd. |
| D6-4, P6-8 | §3.8: the registry entry is deleted first, then the revocation reconcile. |
| D6-5, A6-8, P6-7 | §3.8: the router records legacy leaves; the watchdog pins exactly one distinct leaf; two give `legacy_conflict`; a join's pin write is fatal; the socket-only join keeps a pin or takes the SSH path. |
| D6-7 | §3.6: `ss -tlnp` and `ss -ulnp` separately; alert kinds by process and port. |
| D6-9 | Refuted by its verifier; no change. |
| D6-10 | §3.3: the posture runs in its own task with bounded subprocesses. |
| D6-11 | §3.8: the pin is the DER's SHA-256; sessions keep their leaf hash; the view is byte-deterministic. |
| D6-12, P6-9 | §3.8: `known_hosts` files under `/var/lib/fluxbee/ssh/known_hosts.d/`, one per join (D32). |
| D6-13, A6-9 | Withdrawn with SSH outside `add_hive` (D32): no certificate check over SSH; the gate expects sy-identity's "no key for hive". |
| D6-14, A6-11 (first part), P6-21 (4) | §3.8, §0: the motherbee refuses every `SYSTEM_CORE_ROLLBACK`. A6-11's conntrack part was refuted by its verifier. |
| D6-15 | §3.7: the cleanup stops sy-orchestrator first. |
| D6-16, T6-9, P6-17 | §3.9: signed in a staging directory before anything moves; the gate runs on a copy with `--repo`. |
| D6-17, A6-10, P6-18 | §3.9, §0: the source found by its signature and `Origin: Fluxbee`; one keyring file; no gpgv branch; one first-install path; rotation ships a package without the old key first; a downgrade re-trusts old keys. |
| D6-18, T6-13 (D25 part), A6-5, P6-19 | D25 kept as decided: health fails on a third-party firewall; §3.6 aligned. |
| A6-6 | §3.6: third-party means ufw or firewalld only; other foreign chains with rules are information. |
| T6-3 | §3.6: the posture block in `/versions` carries the active conditions. |
| T6-5 | §6 stage 4: the netns matrix per role and interface, over IPv4 and IPv6, on committed golden renders. |
| T6-6 | §6 stage 4: `posture-check.py` called with `--`, with an oracle per hive. |
| T6-7, T6-8 | §6 A-48: a numbered script on one `hive_id`, and unit rows for the session map, the classifier, first-use pins and the deterministic view. |
| T6-11 | §6 stage 4: rejoin times computed from timestamps, baseline first. |
| T6-12, P6-14 | §0: the first apply keeps established connections (the sshd ports are tracked again). |
| T6-14 | §6 stage 4: curl through the public name, and ifconfig.me from worker1. |
| T6-15 | §3.7: a postrm, `deconfigure` out of the removal branch, a tolerant teardown, tested on the built package. |
| T6-16 | §3.8: one SSH helper and a CI guard on where ssh and scp are spawned. |
| T6-17 | §6 A-49: positive control, `Release` copied, `Dir::Cache`, a first install, a two-key fixture, the PROD file name. |
| T6-18 | §6: the throwaway's apt timers disabled before `clean`; A-52's wait covers them anyway. |
| A6-12 | §3.7, §6: `command -v nft` on a fresh template clone; the template, never the orchestrator. |
| A6-13, P6-20 | §3.1, §3.3: no named sets; "set elements" gone from the hash and the normalizer. |
| P6-2 | §3.8 and D32: nothing re-issues a leaf; the boot TLS reconcile is deleted (A-54). |
| P6-3 | §3.8: legacy is a missing `pin` key; joins write `pin: pending` at accept. |
| P6-4 | §3.8: view and revocation only from a complete listing; a complete empty one is valid. |
| P6-10 | §3.1: the internal interface fixed once; it changes only when it is gone; a public address on it is reported. |
| P6-15 | §3.6, §3.8: a spoke has one uplink; more is warned. |
| P6-16 | §0: the motherbee's conntrack, loopback included, named; no untracking added. |
| P6-21 | §0: items 2–5 and 7 added or corrected; item 1 withdrawn with the evidence; item 6 is §3.8's "what it rests on"; item 8 fixed by P6-3 and P6-7. |

### Round 5 → revision 8 (stages 2–3) and revision 9 (the rest)

| Round-5 findings | Where they went |
|---|---|
| A5-1, P5-1 (blockers) | §3.5: only remote devices are touched, never the local one (its id from the running Syncthing), the `<defaults>` templates or folder members; a fixture holds the motherbee's own device. |
| T5-1 (blocker) | §6: `worker2` joined on 0.1.57 before the stage-2 deploy and stopped; the stage-2 completion check excludes it; the "join gives `Static`" check moves to the A-48 join. |
| D5's stage-2 note | §3.5: no orphan is removed while any registered hive records no device. |
| D5-3, P5-9, T5-4 (a) | §3.1: the internal interface from `ip route get` of the uplink IP; the last good value persisted; a derivation failure only if never derived or the interface is gone. |
| D5-4, T5-3 | §3.1: the render hash is the observe render's sha256; no constant to bump. |
| T5-4 (b)–(d) | §6 stage 4: classifier, normalizer, listener and ufw fixtures. |
| A5-14 | §3.1, §3.6, §0: a motherbee with more than one non-loopback interface is warned. |
| T5-6 | §6 stage 4: the netns job exercises DHCPv4/v6, ND/RA, echo, and the egress apply from the 0.1.57 table. |
| A5-2 | D28 approved by the operator; §3.2 and §0 say observe no longer catches a template or port bug. |
| A5-7, P5-3, A5-12 | §3.2, §3.6: TCP and UDP listeners reported; other IP protocols named as cut; packets reduced to counters. |
| P5-7 | §3.2: the sshd ports on non-internal interfaces are untracked; §0 names the motherbee's conntrack. |
| D5-5 | §3.1, §3.3: no sshd means an empty set; Syncthing not enabled meets its precondition; 22000 declared only while Syncthing runs. |
| D5-6, P5-8 | §3.3: the cursor advances only after a whole batch; it starts at the tail; a loglevel below INFO or a journald suppression is unread. |
| T5-2, A5-11 | §3.3: the mode is an ordered decision list with outputs; `switch_enabled_since`; a 5 → 4 rollback applies observe. |
| A5-3 | D29 (proposed). |
| A5-4 | §3.3, §0: the return-route classification is kept and its limit named (no sshd change). |
| P5-2 | §3.3: only trusted journal fields and an anchored `Accepted` line count. |
| D5-1 | §3.7: no install machinery; `nft` ships with the images, the `.deb` depends on it, a missing one is `unavailable`. |
| D5-7, A5-8 | D25 (operator): no ufw/firewalld writes. |
| D5-8 | §3.7: the remove cleanup starts 2 s later and stops `fluxbee-node-*`. |
| T5-5 | §3.7, §6: a prerm packaging test. |
| A5-10 | §3.7: the join's bootstrap deletes a leftover `posture.disabled` and posture state. |
| T5-17 | §3.6: a derivation failure fails health only after 5 minutes. |
| A5-9, P5-13 | §3.6: a third-party firewall is one that decides admission; drop-only foreign rules are information; it warns; the stage-4 injection is harmless. |
| T5-7, T5-16, T5-18, T5-19 | §6 stages 4–5: per-host enforce probes with an unlisted port, a reachability control, re-checks after the motherbee enforces, a baseline measured on the motherbee with a 60 s margin, named alert sources and controls, the Mac check with `nc`, the break-glass procedure. |
| A5-15 | Header, §2.2, §6 stage 0. |
| P5-14 | §0 residuals; legacy admission requires a TLS leaf; the motherbee refuses a foreign `SYSTEM_CORE_ROLLBACK`. |
| D5-2 | §3.8: one session per hive with an epoch and a cancel handle. |
| D5-10, A5-6, P5-4 | §3.8: legacy computed at every view build, never stored; a pin always wins. |
| P5-12 | §3.8: legacy hives pinned by their first session after A-48. |
| D5-11 | §3.8: the TLS push is fatal. |
| D5-12, P5-5, P5-10 | §3.8: the view rewritten before the entry goes; revocation as a reconcile; the watchdog restores a missing view; an unreadable entry is left out. |
| D5-13 | §3.8: registry writes never create a hive's directory. |
| D5-14, P5-11, T5-15 | §3.8: `known_hosts` by address; host-key failures classified first, warned in every mode. |
| T5-8–T5-14 | §6 A-48: unit tests, the CI grep guard, and precise infra steps on a clean throwaway. |
| D5-15 | §3.9: no "never"; deploy retries `apt-get update` once on a Hash Sum mismatch. |
| D5-16, A5-13, P5-6 | §3.9: an idempotent helper with `\|\| true`, one entry only, binary keys 0644, gpgv in Depends, an append-only keyring with one key per file. |
| T5-20 | §6 A-49: the second update, an isolated tamper test, publish without the key, more helper fixtures. |

### Round 4 → revision 7

| Round-4 findings | Where they went |
|---|---|
| D4-1, A4-4, P4-4, T4-5 (blocker) | §3.7: the egress input chain is dropped from the render, never deleted; double apply in CI; egress reboot in the stage-4 gate. |
| D4-3, P4-1 (blocker), P4-3, A4-5, D4-6 | §3.8: `legacy_unpinned` only for entries that existed at A-48's first boot; new joins pinned before they dial; pin written only after a successful push; sessions closed on a view or pin change; `wan_peers` removed by the owning session; global lock and atomic write; `remove_hive` fails if the view write fails; no view at startup admits as today, with a report. |
| P4-2 | §3.8: spokes accept only a motherbee leaf as uplink; `add_hive` refuses `hive_id` `motherbee`. |
| D4-2 | §3.7: the remove cleanup runs in a transient unit outside the orchestrator's cgroup. It also fixes today's cleanup, which died at sy-orchestrator. |
| D4-4, T4-2, A4-9 | §3.1 and §3.3: a mode-independent render hash; unread evidence pauses the clock; the first check adds 0; missing or unreadable state; `render_changed_at`. |
| D4-5, T4-1, P4-5 | §3.1: `/run/sshd` created before `sshd -T`; `ssh.socket` Listen; `systemd`-owned listener means sshd; no source is a derivation failure. |
| P4-6, P4-7, P4-8, A4-6, A4-7, T4-4, T4-16, D4-11, P4-12 | §3.2 and D28: no packet gating; DHCP and ICMPv6 RA/MLD allowed; io.linkedhelper is cut on purpose. |
| D4-10, T4-3 | §3.3: journal identifiers, IPv6, persisted cursor; a live SSH-login injection in the stage-4 gate. |
| D4-7 | §3.1: the interface from the WAN session's local address. |
| D4-8 | §3.7: nftables installed in a background task, with a lock timeout. |
| D4-9, T4-17, P4-10 | §3.7 (D25): the ingress edge port is included. D25 itself awaits the operator. |
| D4-12 | §3.5: the temp file keeps owner and mode. |
| D4-13 | §3.8: `accept-new` at every SSH call site. |
| D4-14, A4-8, P4-15 | §3.5: orphans removed; the egress device id backfilled by name. |
| D4-15, T4-10, A4-10 | §6: the invariant reworded for non-internal interfaces. |
| D4-16, P4-14, T4-9, T4-17 | §6 A-48 gate rewritten. |
| D4-17, T4-15, P4-9 | §3.9 and the A-49 gate: tamper a copy of `InRelease`; the key is never replaced; rotation; `InRelease` swapped after `Packages`; deploy prints apt warnings. |
| D4-18, A4-14, P4-17 | §0 residuals. |
| D4-19 | §3.2: ICMPv6 RA and MLD. |
| T4-6, T4-7, T4-8, A4-12, T4-11, T4-12, T4-13, T4-14, T4-18, P4-11 | §6 gates: positives in stage 5; hold is a warning; the skip-upgrade test on the throwaway worker with no PROD rollback; precise injections; a defined rejoin baseline; render changes in unit tests; `lab/posture-check.sh`; observed time starts at 0 with the stage-5 release. |
| P4-13 | §3.6: conntrack fill reported. |
| P4-16 | §3.7: the full repurpose command. |
| A4-1, A4-3 | D25 and D27 await the operator. A4-2 found D26 consistent. |
| A4-11 | §3.5: the Syncthing facts are persisted once true. |
| A4-13 | Header and status. |
