# Host posture and exposure — design v1 (revision 8)

**Status:** design agreed with the operator on 2026-10-05. Stage 1 is live (0.1.57, validated on 8.x).
Stage 2 is built for 0.1.58 after four adversarial code reviews (§3.5 as built), with A-50 found on
PROD and fixed in it. DTAP round 5 (on revision 7) is recorded: its stage 2–3 findings are folded
here; its stage 4–5, A-48 and A-49 findings, and the decisions they need from the operator, go into
revision 9 before stage 4.

**History:**

| Revision | What happened |
|---|---|
| 1 | Failed the first DTAP panel on all four lenses (75 findings). |
| 2 | Folded those findings in. |
| 3 | Recorded the operator's answers and the order of work. |
| 4 | Part A of revision 3 failed a second panel (84 findings). Most came from per-hive IP filtering and from the machinery added to steer spokes. Revision 4 **simplifies Part A**: rules by port and interface per role, an automatic mode, no configuration. |
| 5 | A third panel on Part A of revision 4 found 64 issues. Its 6 blockers are all in stages 4–5 and A-48; stage 1 needed only details. Stage 1 is built (§3.4 as built), with the A-48 guard against the spoke-only actions moved into it. The Docker lab is removed (D23). §8 lists what round 3 requires before stages 2–5, A-48 and A-49. |
| 6 | Round 3's findings folded into stages 2–5, A-48 and A-49 (see §8). The main changes: evidence counts only what a drop would break, i.e. packets to a port that has a listener, plus SSH logins through non-internal interfaces; the mode is a persisted pure function; ICMP errors come before conntrack (PMTU); A-48 admits unpinned pre-existing hives; each host installs its own nftables; `remove_hive` and downgrades keep the last ruleset. |
| 8 | Stage 2 as built (§3.5): ownership by recorded device id, the role-only folder rule for A-50, a writer hardened against the Syncthing user's directory. Round 5's stage 2–3 findings folded in: only remote devices are touched (A5-1, P5-1); the throwaway worker joined on 0.1.57 before stage 2 (T5-1). |
| 7 | Round 4 (68 findings; Acceptance passed) folded in (§8). The main changes: the switch is gated only by SSH logins through non-internal interfaces plus preconditions, with packet accounting report-only (D28); the render hash is mode-independent; sshd ports come from socket activation too; DHCP and ICMPv6 RA/MLD are allowed; the egress input chain is dropped from the render, never deleted; the remove cleanup runs outside the orchestrator; A-48 pins new joins before they dial, admits only legacy hives unpinned, and spokes check that their uplink is the motherbee; A-49 never replaces a key. |

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
  NODE_CONFIG_SET and SYSTEM_CORE_ROLLBACK to the motherbee; spoke-to-spoke SYSTEM is denied
  (A-20, A-22).
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
- **Every release that changes the render** puts every host back into observe for at least 24 h at
  once. In that window the ingress' public SSH and the egress' office SSH are open again.
- **Hives joined before A-48** stay unpinned and admit any CA-valid leaf of their `hive_id` until
  they are re-added once.
- **A downgrade below A-48** brings back a router that ignores the view: every removed hive whose
  leaf is still CA-valid is admitted again.
- **D26's frozen ruleset** is harmless only while ports and interface names stay the same. A
  renamed NIC on a downgraded host loses SSH on its internal interface.
- **D27** keeps SSH closed only on a host that was enforcing when it was removed.
- **After A-49,** the first install trusts the key on first use, and `InRelease` has no
  `Valid-Until`, so a freeze attack goes unnoticed.
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
| D28 *(rev 7)* | Under D3 enforce can only cut three things: SSH through non-internal interfaces, undeclared listeners (cut on purpose, and reported) and special UDP/ICMP (allowed explicitly). So the automatic switch is gated by SSH logins through non-internal interfaces plus preconditions, and packet accounting is report-only. Nothing an attacker or a scanner sends can hold a host in observe. |

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

One pure function computes the posture from the host's own role, configuration and interfaces. No
registry entry and no other hive is an input. From it the orchestrator renders:

- the nftables ruleset;
- the expected TCP listeners;
- the Syncthing settings.

Unit tests pin that every allow has a listener and every listener has an allow.

**Inputs**

- **The role.**
- **The motherbee's ports from config:** the `wan.listen` port and `identity.sync.port`.
- **On the ingress:** the edge port from `edge.listen`. A non-443 or plaintext edge is reported.
- **sshd ports**, from all of these:
  - `sshd -T`, run after `install -d -m0755 /run/sshd` (what `ssh.service`'s RuntimeDirectory
    does). On a socket-activated host with no connection since boot, `sshd -T` otherwise fails;
    it does on the PROD motherbee today.
  - `systemctl show -p Listen --value ssh.socket`, when `ssh.socket` is active;
  - sshd-owned lines in `ss`.

  A `systemd`-owned listener on one of those ports is sshd (socket activation). If no source
  answers, that is a derivation failure: the host does not switch to enforce. It never assumes 22.
- **The internal interface:**
  - **motherbee:** every interface (deployment model). A public address on it is reported as a
    warning.
  - **every spoke** (worker, ingress, egress): the interface that carries the established WAN
    session to the motherbee. It is read from the session's local address
    (`ss -Htn state established '( dport = :<wan port> )'` mapped with `ip -o addr`). On the
    egress, a result that differs from `lan_iface` is reported.
  - **If the derivation fails** (no uplink session yet, an unknown address), the applied ruleset
    and mode stay as they are and the failure is reported. A failure never reopens a host.
- **Closed lists in code:** the core IO listeners (none in the core phase; IO.web joins at stage 7)
  and the node listeners (none; io.linkedhelper stays out, D18).

**The render hash** covers a canonical, mode-independent declaration:

- the role;
- the internal interface name (spokes);
- the sshd ports;
- the wan, identity and edge ports;
- a rules-template version constant.

It never covers the policy, the INVALID verdict, set elements, live listeners, the interface list
or build strings. Switching observe → enforce therefore never changes the hash.

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
6. `ct state invalid`: one counter; dropped in enforce, nothing more in observe.
7. The role's allows (table below).
8. A report-only counter and rate-limited `log` (prefix `fluxbee-posture: `), then the chain
   policy: `drop` in enforce, `accept` in observe.

| Role | Accepted | On |
|---|---|---|
| motherbee | tcp 22 (sshd ports), 9000, 9100, 22000 | any interface |
| worker, egress | tcp 22 | internal interface |
| ingress | the edge port, untracked | any interface |
| ingress | tcp 22 | internal interface |

**Spokes expose no Syncthing port.** They dial the motherbee, and from stage 3 their Syncthing
listens on loopback (§3.5).

**The edge port runs untracked.** Raw `notrack` on prerouting for packets to the edge port, and on
output for packets from it. The ingress' own dials use ephemeral ports and stay tracked.

**Outbound is not filtered** (D19).

**What enforce can break, and how each is covered.** Under D3, internal interfaces have no source
restrictions, so every declared listener is accepted there. Enforce can only cut:

1. **SSH through a non-internal interface** (D16). Covered by the SSH-login evidence (§3.3), which
   holds the host in observe while that path is used.
2. **Undeclared listeners.** They are closed on purpose ("lo demás tiene que estar cerrado", D18).
   The listener check reports them before and after.
3. **Special UDP and ICMP.** Covered by the explicit allows above.

So no packet accounting gates the switch. The counter and log in step 8 are for the report only.

**Evidence from PROD.** In the journal since August:

- nobody SSHed into the motherbee (it is operated through the Proxmox guest agent);
- the only SSH logins on the spokes came from the motherbee (10.10.10.10), over eth0;
- none came in through the egress' office address or the ingress' public one.

The PROD path for an Archi tunnel is a jump through a host on the internal LAN that the posture
does not filter (fb-build or the Proxmox host).

### 3.3 Modes, evidence, apply

**The mode is a pure function, unit-tested table by table with an injected clock.** Its inputs:

- the persisted state: mode, render hash, observed time, `render_changed_at`, last reset and its
  reason;
- the current render hash;
- SSH logins through a non-internal interface since the last check. Optional: unread means
  unknown;
- whether `nft` is present;
- third-party firewall, hold and break-glass;
- whether the derivation succeeded;
- whether the expected listeners are up;
- on spokes, the two persisted Syncthing facts (§3.5);
- whether the release enables the switch (from stage 5);
- the check's monotonic delta.

**Its rules:**

- **Break-glass** (`/etc/fluxbee/posture.disabled`) present → `off`. There is no table, the boot
  unit does not load one, and the orchestrator removes it and never re-applies it while the file
  exists. Removing the file starts observe with a fresh window.
- **A render change** (new hash) → observe, with the window reset and `render_changed_at` set.
- **Observed time grows** only on a check where:
  - the table is applied in observe;
  - every evidence read succeeded;
  - `nft` is present.

  The first check after a process start adds 0. Observed time is persisted with the render hash,
  so restarts and reboots keep it.
- **An unread evidence input** pauses the clock and is reported. It never counts as clean.
- **An SSH login through a non-internal interface** resets the window and is reported with its
  source.
- **observe → enforce** once observed time reaches 24 hours and all of these hold:
  - the release enables the switch;
  - `nft` is present;
  - there is no third-party firewall;
  - the derivation succeeded;
  - the expected listeners are up;
  - there is no hold;
  - on spokes, both Syncthing facts are true.

  Observed time starts at 0 with the stage-5 release: time observed under stage 4 does not count.
- **enforce → observe** only on a render change, a hold file or a third-party firewall that
  appears. Drops in enforce are the firewall doing its job: they are reported, and the mode does
  not change.
- **A derivation failure** keeps the applied ruleset and mode, and is reported.
- **Missing state** starts a fresh observe. **Unreadable state** starts a fresh observe and is
  reported; it never leads to enforce.
- **Hold** (`/etc/fluxbee/posture.hold`): never switch to enforce, and go back to observe if
  enforcing. Ours, for the rollout order.
- **Console escape.** `touch /etc/fluxbee/posture.disabled; nft delete table inet fluxbee_host`
  works without the orchestrator.

**SSH-login evidence.**

- The source is the sshd journal (identifiers `sshd` and `sshd-session`): successful logins
  (`Accepted … from <ip>`) whose source routes through a non-internal interface (`ip route get`),
  IPv4 or IPv6.
- Read from a cursor persisted in the state, so nothing is missed across restarts.
- A journal read error is an unread input.
- Failed logins and bare packets never count, so internet scanners cannot hold the ingress or the
  egress in observe.

**Apply.** In one `nft -f`:

```
table inet fluxbee_host {}
delete table inet fluxbee_host
<full definition>
```

**Persist.**

- `/etc/fluxbee/posture.nft`: the last applied ruleset.
- `/var/lib/fluxbee/state/posture.json`: mode, render hash, observed time, `render_changed_at`,
  last reset and its reason, the journal cursor, and the drift baseline.

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
  right after the apply and stored in the state. Later listings are compared with it.
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

**Addresses and folders (stage 2) — as built (0.1.58).** Reconciled by the watchdog about once a
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

**Options (stage 3).** The orchestrator owns them, aligned with `vendor/syncthing/config.xml`:

- global and local announce, relays, NAT and crash reporting off;
- `urAccepted=-1`, `autoUpgradeIntervalH=0`, STUN off;
- `reconnectionIntervalS=10`;
- `listenAddresses`: on the motherbee `tcp://:22000` (dual-stack), on spokes
  `tcp://127.0.0.1:22000`.

The whole set switches together.

**The order is enforced in code, and each switch happens once:**

1. **A spoke switches** when its motherbee device is `Static` and a plain TCP connect to
   `<motherbee>:22000` succeeds. Both facts are persisted the first time they are true and are
   reported in its `/versions` snapshot. Later outages do not undo them, so they are not live
   remote data for the posture (D22).
2. **The motherbee polls**, while it has not switched yet. A background task, not the
   single-flight watchdog tick, reads the snapshots of the spokes that are registered devices
   through the existing GET_VERSIONS, about every 60 s.
3. **The motherbee switches** when every one of them reports both facts. An offline spoke that
   never reported keeps it waiting, and the report names it.
4. **Both switches are one-way.**

**Unit tests** cover both predicates on fixtures captured read-only from the four PROD hosts
(worker1 `dynamic`↔`dynamic`, ingress static, egress without a device id), plus synthetic reports:
fields absent, static false, connect false, both true. The captures drop the `<gui>` block.

### 3.6 Reports and checks (report-only)

| Check | What it reports |
|---|---|
| Listener | From stage 1, on every role: `sy-architect` or `sy-admin` off loopback. From stage 4: every non-loopback TCP listener not in the declaration, by process name. A `systemd`-owned listener on an sshd port is sshd. |
| sshd | `PasswordAuthentication yes` and `PermitRootLogin yes`, as warnings. The fix is A-2; the PROD motherbee has password login on. |
| Mesh auth | `wan.mtls` other than `required` (motherbee) and `identity.sync.auth: disabled`, as warnings. D3 rests on them. |
| Third-party firewall | ufw active, firewalld running, or any non-Fluxbee base chain on the input hook with a non-accept policy or rules. The host stays in observe, and the report names the ports to open there. |
| Packets | The report-only drop counter and the INVALID counter, with top tuples from the log. |
| Conntrack | Fill level. Loading `fluxbee_host` enables conntrack on hosts that did not track before. |
| apt source | Whether the Fluxbee source is `signed-by` or still `[trusted=yes]` (A-49). |
| Motherbee | A public address, as a warning. |

**Where it shows.**

- `watchdog_tick` computes the posture and caches it; `local_versions_snapshot` returns the cached
  value, and `/versions` fans it out.
- **`ops.py health` fails on:** break-glass, drift, `unavailable`, a third-party firewall, or a
  derivation failure.
- **It warns** on hold: the line prints "held" on every run, so a forgotten hold stays visible.
- **From the stage-5 release** it also fails when a host that is not held is not in enforce 48 hours
  after its last render change.

### 3.7 Lifecycle

**nftables.**

- The orchestrator ensures it once per boot, in a background task that never blocks bootstrap:
  `command -v nft || (apt-get update && apt-get -o DPkg::Lock::Timeout=300 install -y nftables)`,
  with a bounded time. The result goes into the report.
- The motherbee's `.deb` depends on nftables. The 4 PROD hosts already have it.
- Without `nft` the host is `unavailable` (unfiltered), and health names it with the command to
  run.
- The egress keeps failing loud without `nft`, because its NAT needs it.

**ufw / firewalld** (D25, proposed).

- The writes into an inactive ufw are removed.
- Where ufw is active or firewalld is running, the orchestrator opens the role's declared accepts
  there: motherbee 9000, 9100, 22000; ingress its edge port. SSH is left to the customer's own
  rules. The host stays in observe, and the report says so.

**`add_hive`.**

- It refuses `hive_id` `motherbee` before issuing any leaf.
- Nothing is admitted on the motherbee beyond the A-48 pin.
- The spoke starts in observe.

**`remove_hive`** (D27, proposed).

- The cleanup script runs in a transient unit outside sy-orchestrator's cgroup
  (`systemd-run --unit=fluxbee-remove-cleanup --collect`), as the self-restart already does.
  Today the script dies when it stops sy-orchestrator, so nothing after it in the loop ever ran:
  fluxbee-syncthing stayed up and enabled on removed spokes.
- The script stops and disables the core services and fluxbee-syncthing, and deletes node state
  and the mesh TLS directory, on every role.
- On an ingress it also stops and disables sy-edge and deletes its publications.
- The posture is **not** torn down: the removed host keeps SSH on its internal interface only.
- **To repurpose a box:**
  `touch /etc/fluxbee/posture.disabled; nft delete table inet fluxbee_host; systemctl disable fluxbee-host-nft.service; rm /etc/fluxbee/posture.nft`.

**Upgrade.** If the render changes, the host goes back to observe for a new window.

**Downgrade or rollback to a build without the posture** (D26). The host keeps its last ruleset,
which the boot unit keeps loading. That is harmless: SSH on the internal interface, the motherbee's
mesh ports and the edge port are admitted. Break-glass is the escape. There are no version
comparisons.

**Package remove / purge.** The prerm tears down the table, the file, the state and the unit.

**Egress.** `fluxbee_egress` stops rendering its input chain. No `delete chain` statement: the file
is re-applied every minute and loaded at every boot, and a delete would fail on the second apply.
The leftover chain is empty with `policy accept`, which cannot override fluxbee_host's drop, and it
is gone at the next boot. This supersedes edge-egress-nat-spec §3.3 and §8.5; a dated note there
says so.

### 3.8 A-48 — `remove_hive` revokes the hive

**The router's view.** The motherbee orchestrator rebuilds `/var/lib/fluxbee/state/router-hives.json`
from the whole registry, under one global lock, and writes it atomically.

- **Contents:** every registered hive with its leaf pin, plus a `legacy_unpinned` flag for the
  entries that existed when A-48 first booted.
- **When:** at every boot before rt-gateway starts, and on `add_hive` / `remove_hive`.
- **If the write fails, `remove_hive` fails.**
- **The router reloads the view when it changes.**
  - No readable view at startup → today's admission (any CA-valid peer), plus a loud report and a
    health failure.
  - A read error after a good load → keep the last good view.

**Admission, on the motherbee's accept path:**

- A WAN peer is admitted only if its `hive_id` is in the view and its presented leaf matches the
  pin.
- The only exception is an entry marked `legacy_unpinned`, admitted without a pin and listed.
- A new or re-added hive has no such mark. Its pin is written right after the motherbee issues and
  pushes its leaf, before the spoke's router starts. The pin is written only after the push
  succeeded.
- A hive that has a pin and whose certificate goes missing is reported, never silently re-issued.
- Each WAN session keeps the leaf pin it was admitted with. When the view drops a hive, or its pin
  changes, the router closes the sessions that no longer match.
- A `wan_peers` entry is removed only by the session that owns it.

**The other side.** A spoke accepts a WAN uplink only if the server's leaf carries `hive_id`
`motherbee`. A removed box or a compromised hive that takes the motherbee's address cannot become
a spoke's peer.

**`remove_hive`, in this order:**

1. REMOVE_HIVE_CLEANUP (§3.7).
2. Delete the registry entry and rewrite the view. The router closes the hive's sessions.
3. Delete its HMAC key and restart sy-identity on the motherbee; the replicas reconnect.
4. Unlink its Syncthing device on every role.
5. Delete its `known_hosts`.

**SSH host keys.** `StrictHostKeyChecking=accept-new`, with one `known_hosts` per hive
(`UserKnownHostsFile=<hives>/<id>/known_hosts`, `HostKeyAlias=<hive_id>`), at every SSH call site.
The first connection records the key, existing spokes included; checking is strict afterwards.

**Registry writes** become read-modify-write merges under the per-hive lock, so a join keeps keys
it does not own (the pin, the device id, the legacy mark).

**Already in 0.1.57:** the motherbee refuses `REMOVE_HIVE_CLEANUP` and `ADD_HIVE_FINALIZE` (D24).

**Residual:** a `legacy_unpinned` hive's old box stays admissible until that hive is re-added once.

### 3.9 A-49 — signed apt repo

- **The key.** The public key is a file committed to the repo, also published next to the apt repo
  with its fingerprint in the install docs. The private key has no passphrase and no expiry; it
  lives on fb-build and is backed up off fb-build.
- **Publishing.** `scripts/apt-repo-publish.sh` builds `InRelease` from `Release.new` and swaps it
  after `Packages`, so a client updating mid-publish never sees a mismatched pair.
- **First install.** It keeps `[trusted=yes]` once, or downloads the key from the repo (trust on
  first use).
- **The postinst** touches the keyring only together with a verified switch:
  - if apt's cached `InRelease` for the Fluxbee source verifies with `gpgv` against the shipped key,
    it adds the key to `/etc/apt/keyrings/fluxbee.gpg` (owned by no package, so a downgrade keeps
    it) and switches the source to `[signed-by=…]`;
  - it never replaces a key that is already there;
  - there is no network I/O inside dpkg.
- **Key rotation.** Sign `InRelease` with the old and the new key, ship a package that adds the new
  one, then drop the old.
- **`ops.py deploy`** prints the `W:` and `E:` lines of `apt-get update`, so a signature failure is
  visible.
- **Residual:** fb-build holds the signing key on the flat L2. A-49 stops impersonation of the repo,
  not a compromise of fb-build.

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
| 2 | Syncthing addresses and folders (§3.5). Built for 0.1.58. | **Unit:** the address type; IP literals only; v6 formatting; the finalize rule; the spoke's uplink address; PROD-shaped fixtures (both member forms) reach the target in two rounds and then change nothing; the egress leaves `blob/active`; the role rule under every enable-flag combination; ownership by recorded id; ambiguous names, missing recorded devices and nameless devices; nothing while busy, nothing on a hive not at rest, no removal on an unreadable, empty or unrecorded registry; the writer refuses symlinks, FIFOs and hard links, never opens a planted temp, keeps the mode and drops special bits. **CI:** the single-writer guard. **Before the deploy (T5-1, done 2026-10-06):** the throwaway VM 104 joined as `worker2` on 0.1.57 (`dynamic` on both sides) and was stopped; it stays registered and stopped until stage 3. **Infra** (`lab/posture-check.py -- --offline worker2` through `ops.py run`, on the 4 hives): the target on each; completion 100% on the motherbee for the 3 PROD spokes; the egress' pending `blob/active` offer gone; egress1's device id in its registry entry; `ops.py opa-status` in_sync 4/4; listen addresses unchanged; Syncthing uptime growing across three checks (no restart loop). The "a join on this release gives `Static`" check moves to the A-48 join. |
| 3 | Syncthing options and the ordering predicates (§3.5). | **Skip-upgrade, with no PROD rollback:** `worker2`, joined on 0.1.57 before the stage-2 deploy and kept stopped (T5-1), is the `dynamic`↔`dynamic` case, like worker1 was. After the deploy, the motherbee keeps discovery on and names `worker2` as the spoke it waits for. Boot it and update it directly from 0.1.57: it gets a static address and switches, then the motherbee switches. **Then, on the 4 hives:** `/rest/config/options` exact; `/rest/system/status` shows discovery off; no relay or QUIC connections; no non-loopback UDP for Syncthing in `ss`; the motherbee listens on `tcp://:22000`; after a motherbee Syncthing restart every spoke reconnects within 30 s (motherbee `/rest/system/connections`); a dist publish reaches 4/4. **Unit:** both predicates on PROD fixtures and synthetic reports. Remove the throwaway afterwards. |
| 4 | Firewall in observe only (§3.1–§3.3, §3.6, §3.7). | **Unit:** role × mode render invariants: observe and enforce differ only in the policy and the INVALID verdict; on a non-internal interface the only accept of new connections is the ingress edge port; no role accepts 3000, 8080, 5432, 4222 or 8384; ICMP errors and DHCP replies come before established. The render hash does not change between observe and enforce. The `delete table` render is deterministic, and the egress render has no input chain and survives a double apply. The mode function, table-driven with an injected clock: every transition, including restart, reboot, unread evidence, missing or unreadable state, hold, break-glass, render change and derivation failure; and observe → enforce → the next check stays in enforce. The sshd-port derivation on fixtures: the PROD socket-activated capture gives {22}; a socket on a non-22 port with `sshd -T` failing; sshd running; no source at all gives a derivation failure. The SSH-login evidence on journal fixtures (`sshd` and `sshd-session`, internal and external sources, IPv6). **CI:** a network-namespace job in its own workflow with sudo applies every role, checks connect/timeout over veth, and accepts a frag-needed for an untracked edge flow in enforce. **Infra:** the posture in `/versions` for 4/4, every host in observe, reasons named. SSH-login evidence: authorize a throwaway key on the egress (`ops.py run 103`), log in from fb-build to 192.168.8.240 (`ops.py run 110`), and expect a window reset naming 192.168.8.180 within two checks; then remove the key. Injections on worker1, each kept until detected (at most two checks) and then undone: flush the input chain (drift reported and repaired); a foreign chain `hook input priority 10; policy accept; tcp dport 9 drop` (third-party firewall reported, health fails); break-glass (the table goes and does not come back; reboot with sy-orchestrator disabled: still no table; remove the file and re-enable: back in observe with a fresh window). The table is there after a spoke reboot with sy-orchestrator disabled. The egress reboots and its NAT and table come back. Rejoin baseline: from the motherbee's boot (`uptime -s`) until `ops.py versions` answers for every spoke and the motherbee's Syncthing shows the 3 spokes connected, sampled twice. Stage 4 closes with every host ready to enforce except for the release switch. |
| 5 | Automatic enforce. | Put `posture.hold` on every host before the deploy; health shows them as held (a warning). Release worker1, then the other spokes, then the motherbee last, a day apart. After each host enters enforce, run `lab/posture-probe.sh` from fb-build (`ops.py run 110`): 10.10.10.10 gives 22, 9000, 9100 and 22000 open, and a canary listener on an unlisted port times out; 10.10.10.20, .30 and .40 give 22 open; 192.168.8.240 gives 22 timeout. From the operator's workstation, on the ingress' public address, 443 answers and everything else times out (a manual step). After a motherbee reboot, spokes rejoin within the slower stage-4 sample + 30 s. An enforcing ingress reboots and stays in enforce. Break-glass from the console works without the orchestrator. Render changes are covered by the unit tests, not by a special release. |

**After stage 5:**

| Item | Gate |
|---|---|
| A-48 (its spoke-only guard shipped with stage 1, D24) | **Host keys:** join the throwaway VM as a worker with `ssh_access=key_only_persist`, snapshot it, regenerate its host keys and restart sy-orchestrator on the motherbee: a host-key failure appears in its log. **Sessions:** stop sy-orchestrator on the throwaway, then remove the hive. The cleanup times out, and within 10 s `ss` on the motherbee shows no established 9000 or 9100 from its address. **Refusal:** roll the throwaway back to the snapshot and boot it; it is refused (WAN in the router log, 9100 in the sy-identity log). **Re-add:** re-add the hive while the rolled-back box keeps dialing. The old box is never admitted, the socket-first fast path is not taken, and the new box is admitted by its pin. **Ingress branch:** re-join it as an ingress (after confirming `admin.public_edge_node`) and remove it. sy-edge and fluxbee-syncthing are inactive and disabled, and its posture is still there. **Legacy hives:** `router-hives.json` lists the 3 PROD spokes as `legacy_unpinned`, and they stay connected after the deploy and after a motherbee reboot. **No view:** delete the view and restart rt-gateway; the spokes stay connected and the report fires. **Registry:** a join keeps foreign keys. |
| A-49 | `ops.py publish` signs without a terminal. On the motherbee, `apt-get update` verifies the signature, and deploy prints its `W:`/`E:` lines. A copy of the repo with one byte of `InRelease` changed, used as a temporary source, fails with a signature error; the live repo is never touched. Postinst fixtures: a valid cached `InRelease` adds the key and switches the source; a missing or bad one keeps `[trusted=yes]`; an existing key is never replaced. `/versions` shows the source mode. |

The throwaway hive is VM 104 (`fb-worker2`, 10.10.10.60), a 2 GB clone of template 9000, kept stopped between uses. A fresh clone is settled before any join (HANDBOOK §3.4): cloud-init's first boot runs a dist-upgrade that restarts sshd (A-52), so wait for it and reboot if one is required.

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
- **Next:** deploy and validate stage 2 (0.1.58); stage 3; revision 9 and round 6 before stage 4.

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

### Round 5 → revision 8 (stages 2–3) and revision 9 (the rest)

| Round-5 findings | Where they went |
|---|---|
| A5-1, P5-1 (blockers) | §3.5: only remote devices are touched, never the local one (its id from the running Syncthing), the `<defaults>` templates or folder members; a fixture holds the motherbee's own device. |
| T5-1 (blocker) | §6: `worker2` joined on 0.1.57 before the stage-2 deploy and stopped; the stage-2 completion check excludes it; the "join gives `Static`" check moves to the A-48 join. |
| D5's stage-2 note | §3.5: no orphan is removed while any registered hive records no device. |
| Every other round-5 finding | Recorded in `docs/audits/2026-10-05-host-posture-dtap-panel.md`; folded into revision 9 before stage 4. |

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
