# Host posture and exposure — design v1 (revision 3)

**Status:** design. Agreed in principle with the operator on 2026-10-05.

- Revision 1 failed the DTAP panel on all four lenses: 75 findings, none refuted. The full list
  and where each one landed: `docs/audits/2026-10-05-host-posture-dtap-panel.md`.
- Revision 2 folded in every technical fix.
- Revision 3 records the operator's answers to O1–O4 (D16–D19) and the order of work (D20): the
  core first, then the connection with Fluxbee Cloud, then the implementation nodes.
- Nothing is implemented. Part A, the core, goes through a second DTAP round before stage 1.

**Scope:** what each hive exposes on the network, who sets and verifies it, and how operators and
the public reach an installation from outside:

- Part A — host posture, owned by the orchestrator: firewall, bind addresses, Syncthing.
- Part B — web exposure through IO.web: Archi for operators, tenant content.
- Part C — public naming for many installations: one public ID per motherbee, DNS, certificate.

**Findings:**

| Finding | Subject |
|---|---|
| A-46 | Archi's HTTP API has no auth and the ingress reaches it; no hive filters inbound traffic |
| A-47 | Syncthing uses public discovery and relays; worker1 depends on discovery |
| A-48 | `remove_hive` revokes nothing |
| A-49 | The apt repo is unsigned |

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

**One owner.** The orchestrator declares the host posture of its role, applies it whole and
atomically, and verifies it every time it runs; drift is reported. In the operator's words: *"el
que tiene que setear todo lo del entorno en ese sentido es el orchestrator, como concepto al correr
verificaría eso"*.

**Access.**

- The only public port of an installation is the edge port on its ingress hive.
- Archi listens on loopback only.
- Operators reach Archi on the motherbee itself, through an SSH tunnel, or from Fluxbee Cloud
  through IO.web with a URL that Cloud obtains through IO.cloud.
- Each installation has one public ID.

**What this does not fix (stated so nobody assumes it does):**

- **Mesh control plane.** Any orchestrator, the DMZ ingress included, can still send SPAWN, KILL,
  NODE_CONFIG_SET and SYSTEM_CORE_ROLLBACK to any hive (A-20, A-22, postponed).
- **Shared L2.** Source allowlists are not authentication. A compromised ingress can spoof LAN
  addresses. Isolating it at the network level needs infrastructure configuration, so it is not
  done now (D19).
- **Unsigned apt repo.** A LAN attacker who impersonates the repo gets root on the motherbee at the
  next upgrade (A-49).
- **`remove_hive`.** A removed hive keeps a valid mesh certificate and its identity HMAC key
  (A-48).
- **Stage 7 reopens a path.** It adds a session-gated HTTP path to Archi. How much a compromised
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
| SSH | motherbee → spoke | 22/tcp | At `add_hive` and at every motherbee orchestrator start (`reconcile_hive_tls_material`, `src/bin/sy_orchestrator.rs:1408`). No hive ever SSHes into the motherbee. |
| Syncthing | both directions today | 22000/tcp, QUIC on 22000/udp, 21027/udp | |
| Edge | internet → ingress | edge port | |
| Egress NAT | worker → egress → internet | — | Forward/NAT, not input. |
| Outbound | every host → internet | — | DNS, NTP, apt, AI, Slack, Meta; replies through conntrack. |
| io.linkedhelper (when instantiated) | Cloud and the LinkedHelper adapter → node | its own HTTP listener | Outside the edge (§1.6). |

### 1.2 Host firewall: present in code, inert in practice

**Nothing filters.** `ufw` is inactive on the four hosts.

**The orchestrator's ufw rules are inert.** It "opens" ports through ufw, from any source:

- **Core rules:** `ensure_core_firewall_local` (`sy_orchestrator.rs:5802`).
  - Only the motherbee gets them: egress returns early, and spoke templates have no `wan.listen`.
  - It adds the port of `wan.listen` and `identity.sync.port`.
- **Syncthing rules:** `ensure_syncthing_firewall_local` (`:5794`) adds 22000/tcp, 22000/udp and
  21027/udp on every role, egress included.

With ufw inactive, all of these are saved and never enforced.

**Latent lockout.** Enabling ufw would block SSH everywhere and 443 on the ingress.

**Rules never come out.** Core rules are only ever added (`close_firewall_rules_local`, `:5720`,
runs only when Syncthing is disabled). `open_firewall_rules_local` (`:5660`) also drives firewalld.

**Egress table.** `table inet fluxbee_egress` (`egress_nft_ruleset`, `:6085`) has:

- an input chain with `policy accept`;
- forward with `policy drop` and LAN→WAN accept;
- masquerade.

How it is applied:

- `reconcile_egress_nat` (`:6444`) applies it with add + flush + define in one `nft -f`.
- The watchdog re-applies it about every 60 s, not only when it disappears (`:2400-2441`).
- A boot unit loads it before `network-pre.target` (`:6264`).

**Leftovers.** Empty `table ip filter` / `table ip6 filter` exist on all hosts (iptables-nft,
harmless).

**nftables is not a dependency.** The `.deb` does not depend on it. Spokes are clean boxes. Only
the egress path checks for `nft` (`:6447-6450`).

### 1.3 Bind configuration

**Archi**

- **Default and overrides.** The code default is `127.0.0.1:3000` (`src/bin/sy_architect.rs:84`).
  `JSR_ARCHITECT_LISTEN` or `architect.listen` overrides it (`:5975-5984`).
- **Where `0.0.0.0` ships:** `packaging/hive.yaml.example:45`, `config/hive.yaml:33`,
  `docs/07-operaciones.md:150`.
- **Docs that point at the open port:**
  - `packaging/fluxbee-firstboot:156`;
  - `docs/packaging-and-build.md:232`;
  - `lab/logbook/HANDBOOK.md:445`.
- **The example does not reach installed hosts.** The postinst copies it only when
  `/etc/fluxbee/hive.yaml` does not exist (`packaging/deb-postinst:50`).
- **Routes** (`:6067-6082`):
  - chat, executor plan, package/software publish and upload, attachments, agent assets, sessions;
  - messages, including SSE at `/api/messages/stream`;
  - a `/*path` catch-all that serves `/api/session-meta` and `/api/identity/ich-options`.
- **Uploads** go up to 128 MiB (`:101`).
- **UI.** It builds API URLs relative to `window.location.pathname` (`:17194-17204`), so it works
  unchanged behind a proxy on its own host. Its text tells operators to call
  `http://MOTHERBEE:8080` (`:17125`).
- **No CSRF defence.** Archi has no CSRF protection, and its POST handlers parse any body as JSON
  whatever the Content-Type (panel A-4).

**Admin**

- `JSR_ADMIN_LISTEN` or `admin.listen` (`src/bin/sy_admin.rs:555-558`). The example uses
  `127.0.0.1:8080`, but any address is accepted.
- Its HTTP server has no authentication (`:5962-5993`).
- `lab/lab-install.sh:37` rewrites it to `0.0.0.0` for the Docker lab, which publishes 8080, 3000
  and 19091-19100 (`lab/docker-compose.yml:34-43`).

**Other listeners**

- **Identity sync:** hard-coded `0.0.0.0:<identity.sync.port>` (`src/bin/sy_identity.rs:3766`).
- **WAN:** `wan.listen` (`0.0.0.0:9000` in the example). On spokes the router does not listen.
- **Syncthing GUI:** `--gui-address=127.0.0.1:<api_port>` in its unit (`sy_orchestrator.rs:6714`).
  The unit is only `After=network.target`.

### 1.4 Syncthing

**Options.** On the four hosts these are all `true`, and the orchestrator never sets them:

- `globalAnnounceEnabled`
- `localAnnounceEnabled`
- `relaysEnabled`
- `natEnabled`
- `crashReportingEnabled`

`listenAddress` is `default` and `urAccepted` is 0. Auto-upgrade is off through `--no-upgrade`.

**Consequences.** Every hive:

- announces its device ID and addresses to public discovery servers;
- may relay through public relays;
- tries UPnP/NAT-PMP;
- sends crash reports;
- broadcasts local discovery on every interface, including the ingress' public eth1 and the
  egress' office LAN.

**The install paths diverged.** `vendor/syncthing/config.xml` already has public infrastructure
off, but only the dev install path seeds it (`scripts/install.sh:628`). Hosts installed from the
`.deb`, and every spoke, run Syncthing's self-generated defaults.

**How the orchestrator edits Syncthing today:**

- It edits `config.xml` with regexes and restarts Syncthing.
- There is no `<options>` editor and no use of the REST config API.
- Three places write the file, read-modify-write, without a lock.
- It reads `/rest/system/connections` (`:6841-6866`) but checks only `connected=true`. A relay link,
  or one the motherbee dialed, also passes.

**Peer addresses:**

| From → to | Address |
|---|---|
| ingress → motherbee | static |
| egress → motherbee | static |
| motherbee → ingress | static |
| motherbee → egress | `dynamic` |
| motherbee ↔ worker1 | **`dynamic` on both sides** |

The worker join hard-codes no address on either side, so that link exists only because discovery
finds it. `ensure_syncthing_top_level_device_in_config_xml` writes `dynamic` when it gets no
address (`:7340`).

**Re-running the finalize undoes a repair.** The spoke-side ADD_HIVE_FINALIZE (`:18246`) takes the
address as optional; a finalize without one rewrites an existing address to `dynamic`
(`:7365-7390`). Peer links are written only at join time (`:8021-8023`).

### 1.5 Edge, certificate, DNS, Cloud trust

**Edge**

- SY.edge loads one certificate (`with_single_cert`, `src/bin/sy_edge.rs:1880`).
- It routes by path only, on any Host: `/e/:ich`, `/e/:ich/*extra`, `/public/:key`, `/healthz`
  (`:1737-1740`).
- No SSE or WebSocket: an SSE request to `/e/` is handled as a buffered request/response. The "501
  stub" is only a deferral note in edge v6.
- `/public/<key>` serves HTML with `sandboxed-html-v1` (`:2384-2390`), so active content runs in an
  opaque origin.
- Publication is **single-edge**:
  - externalize pushes a row to one `edge_node`;
  - the channel secret belongs to that edge's ILK;
  - Admin refuses public artifacts when more than one ingress is connected, unless
    `admin.public_edge_node` picks one;
  - IO.cloud trusts exactly one edge.

**Certificate and DNS**

- PROD's certificate is `*.fluxbee.ai` (SAN `*.fluxbee.ai` and `fluxbee.ai`, expires 2027-02-12).
  It covers one label only.
- That brand wildcard sits on a DMZ host, which edge-control v2 C1 rules out.
- The ingress hostname `hive-k3m9x7q2.fluxbee.ai` was chosen by the operator, and its A record was
  created by hand (HANDBOOK §8).
- DNS is Azure DNS, managed manually by the operator.
- A TLS key change in the vault makes the edge restart itself (PB-1). Nothing watches certificate
  expiry.

**Cloud trust today**

- The edge checks Cloud's bearer (`sy_edge.rs:2590-2603`) and strips `Authorization` before it
  forwards (`:2929-2944`).
- IO.cloud authorizes a request only when the router-stamped source is its configured edge and the
  ICH is its own (`nodes/io/io-cloud/src/main.rs:768-788`).
- So the DMZ host, not Cloud, is the trust anchor of every IO.cloud action, and the edge sees every
  reply in clear (`sy_edge.rs:2757-2762`).

**Not implemented.** FC.edge-manager and edge-control protocol v2 never got built (§5.5 says what
carries over).

### 1.6 IO.web and listening runtimes

**IO.web exists as a spec only** (`io-web-spec-beta-v1.md`); there is no node code. The spec
already fixes:

- one origin per app;
- a single upstream reached over mTLS;
- Admin as authority;
- `mint_web_access` requested by Cloud through IO.cloud;
- a `__Host-` cookie, a fixed CSP and Origin-based CSRF.

Its assumptions and limits:

- It assumes a firewall that admits only ingress hives (§1.5 of that spec). That firewall does not
  exist.
- It models tenant apps only.
- It keeps large uploads and long streams out of scope, with 30 s timeouts.
- Its Host rule (`<52-char key>.apps.<domain>`) does not fit the naming in Part C.
- Its §6/§17 plan (a unit, a `system_nodes` entry, a `web:` block in `hive.yaml`) predates the
  packaging model. IO.web is a core managed runtime in `base-nodes.json`.
- `io-cloud-spec-v1.md:372` keeps a deferred per-tenant `IO.archi`. D10 supersedes it.

**io.linkedhelper.** It is in the base set (`boot:false`). When instantiated, it serves plain HTTP
on an address and port from its config (schema example `0.0.0.0:19091`). Cloud's `/schema` probe
and the adapter's `/v1/poll` reach it directly, outside the edge.

---

## 2. Decisions

### 2.1 Agreed with the operator (2026-10-05)

| ID | Decision |
|---|---|
| D1 | The orchestrator owns the host posture of its role. It declares it, applies it whole and atomically, verifies it at bootstrap and in the watchdog, corrects drift and reports it. |
| D2 | One Fluxbee firewall owner per host: the nftables table `inet fluxbee_host` on every role. `fluxbee_egress` keeps only forward and NAT. The ufw/firewalld code paths go away. Third-party firewalls are reported and never changed. |
| D3 | Allowed sources come from the hive registry, as IP literals per hive (§3.2). They are not a CIDR. Multi-site needs routed private connectivity without NAT between hives (v1 scope). |
| D4 | Archi and the admin HTTP API listen on loopback only. Operators reach them on the motherbee or through an SSH tunnel (`ssh -L 3000:127.0.0.1:3000 ...`). DEV switches to the tunnel. How they get there: D17. |
| D5 | Syncthing never uses public infrastructure. Spokes dial the motherbee at its static address; the motherbee only accepts. The address is derived at `add_hive`, and the hive is refused only when it cannot be derived. |
| D6 | The only public port of an installation is the edge port of its ingress hive. |
| D7 | IO.web is the URL plane: Archi, artifacts, documents, web traffic between Fluxbee and Cloud. IO.cloud stays the command plane. IO.web v2 is session-gated Archi plus artifacts, and nothing more unless the operator re-approves it: *"no quiero complicarla"*. |
| D8 | Cloud is trusted blindly for operator identity, as with every IO.cloud function. Cloud authenticates the human and obtains an access URL; the motherbee keeps no operator list. Whatever minting Archi access involves gives installation-root power, so the operator mint is its own action, and Admin logs each one with the Cloud subject. |
| D9 | Cloud gives the operator a URL to open top-level; it never embeds Archi. `frame-ancestors 'none'`. |
| D10 | Archi is per installation, never per tenant, never public. Its app, UI and API are not changed. The "infra" screen and what Cloud replicates are decided later. |
| D11 | One public ID per installation (motherbee): opaque, a single DNS label. For now the operator picks it together with the DNS record. PROD's is `hive-k3m9x7q2`. The installation stores it in `admin.public_base_url`. |
| D12 | DNS stays manual (operator, Azure) per motherbee. Automation comes later. |
| D13 | No second registrable domain for now. Tenant content is a directory of the installation host, and Archi never shares a host with it. This holds only under these conditions: Cloud and the brand site set host-only cookies (never `Domain=fluxbee.ai`); CSRF protection relies on Origin checks, not SameSite; no installation holds a certificate that covers Cloud's host. |
| D14 | IO.web will handle artifacts (security, maybe sharing). Kept in mind, not designed here. |
| D15 | Process: document, then DTAP panel, then staged implementation. Each stage is validated in infra before the next. Never one pass. The 8.x installation is the authorized testbed (2026-09-28). |
| D16 | Operator SSH sources are optional and nobody has to configure them. Without them, SSH stays open to anyone except the registered hives, so no hive (the DMZ ingress included) can SSH into the motherbee or between spokes. On the ingress it is open only on the internal interface. Declaring `posture.operator_sources` narrows it further. Enforce does not depend on it. |
| D17 | Archi and the admin get to loopback through configuration only. Archi's code is not changed. |
| D18 | io.linkedhelper will take the same path through the edge as every other IO, in the last phase (implementation nodes). Until then it is outside the core posture: if someone instantiates it, its direct port is closed under enforce and its listener is reported. |
| D19 | Ingress hardening is only what `add_hive` with the ingress role and the orchestrator do by themselves: its posture (the public port, SSH on the internal interface, nothing else). Nothing that needs infrastructure configuration from the user (VLANs, hypervisor anti-spoofing), and nothing odd, is done now; those stay as stated residuals. *"el usuario no puede hacer config complicado desde infra, si queda algo raro no lo hagamos ahora"*. |
| D20 | Order of work: the core first (Part A stages 1–5, A-48, A-49), then the connection with Fluxbee Cloud (Parts B and C, stages 6–7), then the implementation nodes (io.linkedhelper and the rest). *"quiero cerrar el core primero, luego conexión con fluxbee cloud y luego los nodos de implementación"*. |

### 2.2 Open — the operator decides

O1–O4 were answered on 2026-10-05: O1 → D16, O2 → D17, O3 → D18, O4 → D19.

Needed before stages 6–7 (the Cloud phase):

| ID | Question | Options and recommendation |
|---|---|---|
| O5 | The trust anchor for Archi through Cloud. Today a compromised ingress can mint and use an Archi session by itself. | **(a) Recommended: end to end.** Cloud signs the mint request with a key pinned on the motherbee; Admin verifies it; the code goes back encrypted to Cloud; the edge passes `archi.<id>` through by SNI to IO.web, which terminates TLS with a certificate only the motherbee holds. (b) Accept the risk, and write it into A-46. |
| O6 | Certificate source with manual DNS. | **(a) Recommended for now:** a 1-year per-installation certificate, one manual step a year. (b) ACME with `_acme-challenge.<id>` delegated once by CNAME to a zone automation can write. Manual DNS-01 every ~60 days is rejected. Under O5(a), the edge certificate must not cover `archi.<id>`. |
| O7 | More than one ingress per installation. | Publication is single-edge today, so DNS points at one ingress. The operator's *"edge replicado"* / *"cloud replica"* reads as Cloud mirroring Archi, not several edges. To confirm. |
| O8 | Tenant label in tenant paths, and whether per-tenant hosts are needed later. | Defined in io.web v2. The label is never the customer name. Per-tenant hosts only if tenant content needs sessions. |

---

## 3. Part A — Host posture owned by the orchestrator

### 3.1 One declaration per role

One pure function computes the posture of a host from its inputs. Everything else is rendered
from it, so the parts cannot disagree:

- the nftables ruleset;
- the expected listeners;
- the Syncthing settings;
- the verify probe plan.

Unit tests pin that every allow has a listener and every listener has an allow.

**Inputs**

- **Role and ports:**
  - the motherbee's `wan.listen`;
  - `identity.sync.port`;
  - the edge port, taken from `edge.listen` and not hard-coded.

  A non-443 or plaintext edge is reported.
- **Installation posture settings** (§3.2): `mode`, `operator_sources`, `extra_inbound`.
- **The hive registry** with per-hive `source_ips` (§3.2).
- **On spokes:** the motherbee's address from `wan.uplinks`, as an IP literal (resolved and stored
  at join time if it was a name).
- **Core IO listeners** from the managed-node inventory, e.g. IO.web on the motherbee. Not from
  `system_nodes`, because IO.web is a managed runtime.

### 3.2 Inputs that need new plumbing

**Installation posture settings.** The motherbee owns `posture.mode` (`off` | `observe` |
`enforce`), `posture.operator_sources` (CIDRs) and `posture.extra_inbound` (port, protocol,
sources, for customer services).

- They live in the motherbee's `hive.yaml`.
- The motherbee pushes them to every spoke over the mesh: a new protected SYSTEM action, sent on
  connect, on every spoke boot and on every change.
- The spoke stores them in its state dir.
- A spoke that never received settings stays in `observe`.

Today a spoke's `hive.yaml` is written once at join and never re-sent, and firstboot never runs
on spokes, so this channel is required.

**Source addresses.** The registry `address` is the SSH target the operator typed. It may be a
name, and it is not necessarily the address a spoke connects from. So:

- Each hive gets `source_ips`: IP literals only.
- At join, the spoke reports the source it uses toward the motherbee (`ip route get <motherbee>`)
  in its ADD_HIVE_FINALIZE reply.
- Existing hives are backfilled from the peer address of their authenticated WAN (mTLS) session.
- The motherbee keeps comparing the observed peer addresses of WAN, identity and Syncthing sessions
  with `source_ips`. A mismatch is reported and blocks enforce.

**Admitted hives.** Every registry entry is admitted, whatever its state, until `remove_hive`. A
failed join can then resume, and the hive still has to authenticate (mTLS/HMAC).

The motherbee recomputes and applies its sets in the join-accept path, before
`spawn_background_join` (`sy_orchestrator.rs:18993`). So a new spoke is admitted before it dials.

### 3.3 Inbound matrix

**Every role:**

- `iif lo` accept;
- `ct state established,related` accept;
- `ct state invalid` dropped in enforce, only counted in observe;
- ICMP errors and echo, rate-limited;
- ICMPv6 neighbor discovery and errors;
- DHCPv6 replies (udp 547 → 546 from `fe80::/10`) only where DHCPv6 is in use.

DHCPv4 is not affected: clients receive OFFER/ACK on packet sockets, and renewals match conntrack.

Outbound is not filtered in v1 (D19). Syncthing's public traffic is removed by configuration
(§3.5), not by the firewall.

| Role | Port | From |
|---|---|---|
| motherbee | 22/tcp (the ports `sshd -T` reports) | operator sources if declared, otherwise anyone; never a registered hive |
| motherbee | 9000/tcp | every registered hive |
| motherbee | 9100/tcp | registered hives that run SY.identity (workers) |
| motherbee | 22000/tcp | every registered hive (TCP only, no QUIC) |
| motherbee | IO.web listener | ingress hives (when IO.web runs) |
| worker, egress | 22/tcp | the motherbee, plus operator sources if declared, otherwise anyone; never another hive |
| ingress | edge port | anywhere, untracked (below) |
| ingress | 22/tcp | as for worker, and only on the internal interface |
| any | `extra_inbound` entries | as declared, reported |

**Spokes expose no Syncthing port.** They dial out, and their Syncthing listens on loopback.

**The ingress public port runs untracked.** Raw `notrack` both ways plus a stateless accept, so a
connection flood cannot fill conntrack and cut the ingress' own dials to the motherbee. Conntrack
fill is reported.

**SSH and operator sources (D16):**

- **Optional.** With no declared sources, SSH is open to anyone except the registered hives, and
  the report says so. Nobody has to configure anything.
- **When declared:** only explicit CIDRs. Nothing is inferred from a shared subnet; the
  motherbee's own /24 in PROD contains the DMZ ingress.
- **Hive addresses are always excluded.** No hive gets SSH to the motherbee or between spokes. The
  motherbee keeps SSH to every spoke (`add_hive`, `reconcile_hive_tls_material`).
- **The ingress' internal interface is derived** by the orchestrator from the route to the
  motherbee (`ip route get`). Nobody configures it.

Rule order on 22/tcp: drop on the ingress' non-internal interfaces; accept the motherbee on spokes;
drop registered hives; accept the declared sources, or anyone when none are declared.

### 3.4 Binds (stage 1)

The target is that Archi and the admin listen only on `127.0.0.1`, through configuration only
(D17):

- `architect.listen` leaves the example and `config/hive.yaml`; the code default is already
  loopback.
- The postinst rewrites the exact `0.0.0.0:3000` value it used to ship. Any other value is left
  alone and reported.
- `lab/lab-install.sh` stops rewriting `admin.listen`.
- **Reporting.** The orchestrator reports any non-loopback `architect.listen`, `admin.listen` or
  `JSR_*_LISTEN` through the existing drift store (`append_drift_alert`, `GET /drift-alerts`)
  until the posture report exists.
- **Docs.** Firstboot, `packaging-and-build`, `07-operaciones` and the HANDBOOK say "loopback,
  through a tunnel". The docs also say the admin API is reachable only that way, because Archi's UI
  text (`http://MOTHERBEE:8080`) stays as it is (D10).
- **Docker lab.**
  - Published ports cannot reach loopback binds, so the lab reaches admin and Archi through
    `docker exec` or a tunnel.
  - The 8080 and 3000 mappings go away.
  - Posture mode is `off` there.
  - Never `network_mode: host`: the containers are privileged and would write the host's
    firewall.
- **CI.** A guard rejects non-loopback 3000/8080 in shipped config and docs.

### 3.5 Syncthing posture (stages 2 and 3)

**Addresses are reconciled, not just set at join.** At every boot and watchdog run:

- The spoke sets its motherbee device to `Static(<motherbee uplink IP>:22000)`.
- The motherbee sets every registered spoke device to `AcceptOnly`: address `dynamic` with
  discovery off, so it never dials. It finds the device by the id recorded in the registry; egress
  now records `syncthing_device_id` as well.
- The address type becomes an explicit `AcceptOnly | Static(SocketAddr)`, replacing
  `Option<&str>` + `unwrap_or("dynamic")`.
- A finalize without a derivable address is refused, never written as `dynamic`.
- The worker join derives the motherbee address the way ingress and egress already do.

**The orchestrator is the single owner of `<options>`.** The settings:

- `globalAnnounceEnabled=false`
- `localAnnounceEnabled=false`
- `relaysEnabled=false`
- `natEnabled=false`
- `crashReportingEnabled=false`
- `urAccepted=-1`
- `autoUpgradeIntervalH=0`
- STUN off
- `listenAddresses`: on the motherbee, `tcp://<address written into the spokes' uplinks>:22000`;
  on spokes, `tcp://127.0.0.1:22000`.

The motherbee's Syncthing unit gets `After=network-online.target`, so it can bind that address at
boot. `vendor/syncthing/config.xml` is aligned to the same values, so the dev and `.deb` paths
cannot drift apart.

**One writer.** Options, addresses and folders go through one serialized writer. Preferred: the
Syncthing REST config API, which needs no restart. Otherwise a mutex plus atomic writes.

**The order is enforced in code**, so skipping releases is safe:

1. **A spoke turns discovery off** only after its motherbee address is static and its link is up.
2. **The motherbee turns discovery off** only when every registered spoke reports a static
   motherbee address and shows on the motherbee as a `tcp-server` connection (the spoke dialed),
   not a relay.
3. **Otherwise discovery stays on**, and the report names the hive that blocks.

Both predicates are pure and unit-tested.

### 3.6 Apply, persist, verify

**Render.**

```
table inet fluxbee_host {}
delete table inet fluxbee_host
<full definition>
```

All of it in one `nft -f`. A plain `flush table` leaves named sets and chains behind: a removed
hive would stay admitted, and a removed chain would keep its policy.

**Observe.**

- The full ruleset, but the final rule records the tuple in a dynamic set keyed
  `saddr . l4proto . dport`, with counters and a timeout, and does not drop.
- The orchestrator reads that set every watchdog tick and accumulates it in the report, so counts
  survive re-apply and reboot.
- Counts are split by source class: known hive, operator, unknown.

**Enforce.** Identical to observe except for the final verdict. A unit test pins that.

**Drift.**

- The live table, normalized (no handles, counters or set elements), is compared with the
  rendered one.
- The ruleset is re-applied only on drift, never on a blind timer. That differs from the egress
  watchdog.
- Debian's `nftables.service` runs `flush ruleset` when it starts. An enabled one is reported as a
  conflict; its effect is repaired as drift.

**Persist.**

- `/etc/nftables.d/fluxbee-host.nft` only ever holds a ruleset that passed verify, or an observe
  ruleset; the previous one is kept as `.prev`.
- The file carries a format version.

**Boot unit** (`fluxbee-host-nft.service`):

- `DefaultDependencies=no`
- `After=nftables.service`
- `Before=network-pre.target`, `Wants=network-pre.target`
- `ConditionPathExists=` the ruleset file
- `ConditionPathExists=!/etc/fluxbee/posture.disabled` (break-glass)

Rules use `iifname` / `oifname` only, so a missing interface cannot stop the load. A failed load is
reported.

**Commit-confirm.** Before a changed ruleset is enforced:

- A `systemd-run --on-active` timer is armed; when it fires it restores `.prev`, live and on disk.
- Verify cancels it on success. If the orchestrator dies halfway, the timer reverts.
- After a revert, the failed ruleset hash is quarantined until an input changes, with a hold-down.

**Verify-after-apply.**

- **The probe plan comes from the same declaration.** Each role has its own set; 9100 is probed
  only from hives that run SY.identity.
- **A baseline runs before the apply.** After it, only probes that passed in the baseline count.
  - Hives not connected at baseline are skipped and reported.
  - Joining hives are never probed.
- **Probe outcomes:**
  - a positive probe that fails triggers the revert;
  - a negative probe (expected closed, e.g. ingress → motherbee:9100, ingress → motherbee:22)
    reports a hole and never reverts;
  - a timeout is never a pass.
- **Probe requests carry no target.** The prober derives its targets from its own declaration.
- **The probe is a new SYSTEM action.** It is added to `PROTECTED_SYSTEM_ACTIONS` together with the
  Rego/wasm policy, in the same commit; otherwise it would be delivered ungated.

**Preconditions for enforce, per host.** If any fails, the host stays in observe and the report
says why:

- `nft` is present;
- zero would-drops from known sources over the observation window;
- no source-address mismatch;
- no active third-party firewall;
- if operator sources are declared, observe has seen SSH from one of them, so a mistyped CIDR
  cannot lock operators out.

Errors never abort bootstrap.

### 3.7 Lifecycle

**`add_hive`**

- Preflight `nft` on every role: fail with a clear error, as egress does.
- Admit the new hive's `source_ips` before the remote bootstrap.
- The spoke applies its posture before rt-gateway and Syncthing start.

**`remove_hive`**

- The motherbee drops the hive from its sets and flushes conntrack for its `source_ips`.
- `REMOVE_HIVE_CLEANUP` tears the spoke's posture down (table, file, boot unit) before the service
  kill, as egress F11 does, and warns when the spoke was offline.
- Revoking the hive's credentials is tracked separately (A-48).

**Upgrade.** The new orchestrator recomputes; an enforce change goes through commit-confirm.

**Downgrade.** Documented step: set `posture.mode: off`, let it apply, then downgrade. `off` tears
down the table, file and unit. Downgrade is a stage-5 break test.

**Package removal / purge.** prerm removes the table, file and unit.

**Without `nft` on an already-joined hive.** The report says `firewall: unavailable` and the hive
keeps running.

### 3.8 Third-party firewalls

**Classification:**

- empty iptables-nft tables are benign;
- ufw enabled (or with loaded rules) and firewalld active are third-party;
- `fluxbee_*` tables are ours.

**Old Fluxbee ufw rules** are removed, exact tuples only, and only when ufw is inactive. The
posture never runs `ufw enable` or `ufw disable`.

**With an active third-party firewall** the host stays in observe. The report lists the exact
ports and sources the operator must open there.

### 3.9 Report

`posture` goes into `local_versions_snapshot`, so `/versions` fans it out per hive with a fleet
verdict. `ops.py health` exits non-zero on a bad posture.

| Field | Content |
|---|---|
| `mode` | the configured mode |
| `effective` | `off` / `observe` / `enforce` / `unavailable` |
| `blocked_by` | why the host is not enforcing |
| `table`, `boot_unit`, `syncthing` | state of each |
| `listeners` | unexpected and missing, from a pure parser of `ss -H -tulnp`; managed-node listeners by name |
| `third_party` | detected firewalls |
| `would_drop` | per source class, top tuples, `since` |
| `conntrack` | fill level (ingress) |
| `cert_not_after` | ingress, from stage 6 |

---

## 4. Part B — Web exposure through IO.web (inputs for the io.web spec v2)

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
- The upstream is fixed in code: loopback on the motherbee. IO.web runs there as a core IO; its
  listener comes from the managed-node config, aligned with `base-nodes.json`.
- Access only through an operator session. Never public, by construction.

**Archi is proxied unchanged** (D10). It needs:

- SSE (`/api/messages/stream`);
- uploads up to 128 MiB;
- its `/*path` catch-all, so no narrow path allowlist.

**CSRF, which Archi cannot do itself.** IO.web rejects every non-GET/HEAD request whose `Origin` is
not exactly the Archi origin. It also rejects any `Sec-Fetch-Site` other than `same-origin` when
the header is present.

**CSP.** A named header/CSP profile for Archi tolerates its inline scripts but keeps
`frame-ancestors 'none'` and `nosniff`.

**Audit.** IO.web strips client-supplied identity headers and logs the Cloud subject per request.
Archi reads no headers, so attribution lives in IO.web.

**Internal path.** Loopback or the SSH tunnel, with no login.

### 4.3 Operator access (shape; final form depends on O5)

1. The operator is logged into Cloud. Cloud asks Admin, through IO.cloud, for an operator mint
   (`archi`, subject).
   - Under O5(a) the request is signed by Cloud and the code comes back encrypted to Cloud.
2. Admin mints a one-time code (≤120 s, `aud` = the Archi host, `jti` stored durably) and logs it.
3. Cloud sends the operator top-level to `https://archi.<id>.fluxbee.ai/_fluxbee/session/exchange`.
4. IO.web validates and consumes the code, sets `__Host-fb_session` and redirects.
   - There is a single IO.web, so replicated edges cannot replay a code.
5. Following requests go edge → IO.web → Archi on loopback.
   - Under O5(a) the edge forwards `archi.<id>` as raw TLS by SNI.

**Superseded.** Edge v6 invariant I2 ("no path from the public internet to the admin plane") gets
one named exception: operator-session access to Archi. That is recorded in edge v6 when stage 7
lands. io-cloud spec §3.2/§8.4/§10 are updated in the same change.

### 4.4 Tenant content as a directory (D13)

- **Where:** under the installation host, e.g. `https://<id>.fluxbee.ai/t/<label>/…` (O8), next to
  `/e/` and `/public/`.
- **Why it is safe:** that host carries no cookies, and every active content type is sandboxed.
- **Edge invariants:**
  - never pass `Set-Cookie`;
  - sandbox active content;
  - `nosniff` everywhere;
  - reject service-worker script fetches.
- **Host-first routing at the edge, before stage 7.** An exact host table:

  | Host | Serves |
  |---|---|
  | `<id>.fluxbee.ai` | `/e/`, `/public/`, `/t/` |
  | `archi.<id>.fluxbee.ai` | only the IO.web system route |
  | anything else | 404 |

### 4.5 Artifacts (D14)

IO.web will serve artifacts for isolation and sharing. Today they are `/public/<key>` (IO.blob →
edge, sandboxed). Share modes, origin per artifact vs directory, and the relation with `/public/`
are for io.web v2.

---

## 5. Part C — Public naming for many installations

### 5.1 Installation public ID (D11)

- Opaque, one DNS label, immutable.
- Never the customer name, never the internal `hive_id`: every installation has a "motherbee".
- The operator picks it today, together with the DNS record; registration through Cloud comes
  later.
- The installation reads it from `admin.public_base_url`.

PROD: `hive-k3m9x7q2`.

### 5.2 Hostnames

| Host | Serves |
|---|---|
| `<id>.fluxbee.ai` | `/e/<ich>`, `/public/<key>`, tenant directories |
| `archi.<id>.fluxbee.ai` | Archi, operators only |

### 5.3 DNS and certificate (manual now)

- **DNS** (operator, Azure): `<id>` and `archi.<id>` point to the installation's ingress. There is
  one ingress until multi-edge publication exists (O7). Failover is manual.
- **Certificate:** per installation, per O6, never the brand wildcard. Under O5(a) the edge's
  certificate does not cover `archi.<id>`.
- **Move PROD off the brand wildcard early.** Issue PROD's own certificate independently of
  stages 1–5, so `*.fluxbee.ai` leaves the DMZ.
- **Renewal runbook:** new certificate → vault → the edge restarts itself (PB-1). One vault entry
  per edge.
- **Expiry:** `notAfter` in the report, alert at 21 days.
- **CAA** on `fluxbee.ai`: issuer plus `validationmethods=dns-01`.
- **Before a public IP is released**, delete `<id>` and `archi.<id>`; otherwise the names can be
  taken over.
- **DNS credentials** never live on an installation host.

### 5.4 Relation with FC.edge-manager and edge-control v2

| | |
|---|---|
| **Superseded for now** | Per-edge non-wildcard certificates become one certificate per installation. `<edge_id>` names become `<id>`. Cloud-owned DNS becomes manual. |
| **Still holds** | A certificate per scope; never distribute the brand wildcard; validate IP changes before repointing DNS. |
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

| Stage | Content | Gate |
|---|---|---|
| 0 | Revision 3; a second DTAP round on Part A. | Operator approval. |
| 1 | Binds (§3.4). | On PROD, without touching its `hive.yaml` by hand: `ss -ltn` shows 3000 and 8080 only on 127.0.0.1. Connections from worker, ingress, egress and fb-build are refused. The tunnel works, SSE included. The drift alert appears for a non-loopback value. The Docker lab procedure is rewritten and re-run. The CI guard is on. |
| 2 | Syncthing addresses (§3.5). | Unit tests: derivation for v4, v6 and a rejected name; finalize without an address refused; today's PROD shapes reconcile; a second pass is a no-op. `/rest/config/devices` on every hive shows spoke → motherbee static and motherbee → spoke accept-only. A fresh `add_hive` of a throwaway hive writes the same. |
| 3 | Syncthing options (§3.5). | `/rest/config/options` is exact. `/rest/system/status` shows discovery off. No relay or QUIC connections. The motherbee sees `tcp-server` from every spoke. Sampled `ss` shows only registry peers and no non-loopback UDP. Reconnection after a motherbee Syncthing restart and after a motherbee reboot is no slower than today. A skip-upgrade from 0.1.56 straight to this build converges. CI: two Syncthing instances; `dynamic`↔`dynamic` without discovery does not connect, static → accept-only does. |
| 4 | Firewall in observe (§3.1–3.3, §3.6–3.9). | Unit tests for role × mode invariants. The only port accept without a source is the ingress' public port; no role accepts 3000, 8080, 5432, 4222 or 8384; 9100 never admits ingress or egress. The `delete table` render is deterministic. CI applies every role and registry shape in a network namespace (`unshare --net`) and checks sets, hash stability and connect/timeout over veth. Listener-audit fixtures captured read-only from the 4 PROD hosts. During the window, an exercise script restarts rt-gateway, sy-identity and Syncthing, reboots a spoke, deploys a patch release, opens SSH from every operator location, and adds then removes a throwaway hive. Pass: zero would-drops from known sources, no address mismatch, and the table present after a spoke reboot with sy-orchestrator disabled. |
| 5 | Enforce, host by host: egress, worker, ingress, motherbee last. | A versioned probe script run through `ops.py run`. From fb-build, motherbee ports time out. From the ingress, motherbee 9100/3000/8080/5432/4222/8384 fail. From outside, only the public port answers. Spoke-to-spoke traffic and a removed hive are filtered. After a motherbee reboot, spokes rejoin within the HANDBOOK §9 baseline with zero drops from known sources; again with sy-orchestrator disabled. Existing sessions survive the switch. An upgrade that changes the ruleset goes through. Downgrade path. `add_hive` under enforce. A spoke offline during a motherbee apply. Commit-confirm revert. Break-glass. A connection flood on the ingress. |
| 6 | Naming and certificate (O6, O7) + Host-first routing in the edge. | Strict TLS for `<id>`, `archi.<id>` and a random `x.<id>`. Chain of at least 2 certificates. SAN as designed. The edge serves the new fingerprint. Cloud → `/e/<io.cloud ich>` returns 200, Meta verification passes, `/public/` URLs work. Expiry at least 30 days away, with its signal. `archi.<id>/e/…` and `x.<id>/…` return 404. The brand wildcard is gone from the ingress. |
| 7 | io.web v2 spec (scope D7, O5, O8), its own DTAP panel, then Archi through Cloud. | Defined in that spec. Includes: replayed, expired or wrong-`aud` codes rejected; 8443 unreachable from non-ingress hosts and without a client certificate; identity headers stripped; frame-ancestors; SSE and uploads. |

Stages 1–5 close the inbound and HTTP side of A-46, and A-47. They need nothing from Cloud.

**Order of work (D20):**

1. **Core.** Stages 1–5, then:
   - A-48: `remove_hive` revokes the hive. The router admits only registered hives, the HMAC key is
     deleted, and Syncthing is unlinked on every role.
   - A-49: sign the apt repo and drop `[trusted=yes]`.
2. **Connection with Fluxbee Cloud.** Stages 6–7 (O5–O8).
3. **Implementation nodes.** io.linkedhelper through the edge (D18), then the rest.

**Not now (D19), stated residuals revisited after the core:**

- network-level isolation of the ingress (VLAN, hypervisor anti-spoofing);
- outbound filtering on the ingress (pointless while SY.edge runs as root);
- an unprivileged, sandboxed SY.edge.

---

## 7. Review log

- **2026-10-05, DTAP panel on revision 1.** Development, Test, Acceptance and Production all
  failed: 75 findings, 51 confirmed and 24 partial by the adversarial verifiers, none refuted.
  - Blockers: the nft idiom with sets (D-1), O1 (A-1), apply/rollback state (T-2), Syncthing
    ordering across releases (T-1), and the DMZ as trust anchor for Archi (P-1).
  - Each finding and where it landed: `docs/audits/2026-10-05-host-posture-dtap-panel.md`.
- **Next:** a second DTAP round on Part A of this revision.
