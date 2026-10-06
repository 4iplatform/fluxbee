# Host posture and exposure — design v1 (revision 4)

**Status:** design. Agreed in principle with the operator on 2026-10-05. Nothing implemented.

**History:**

| Revision | What happened |
|---|---|
| 1 | Failed the first DTAP panel on all four lenses (75 findings). |
| 2 | Folded those findings in. |
| 3 | Recorded the operator's answers and the order of work. |
| 4 | Part A of revision 3 failed a second panel (84 findings). Most came from per-hive IP filtering and from the machinery added to steer spokes. Revision 4 **simplifies Part A**: rules by port and interface per role, an automatic mode, no configuration. |

Every finding and where it went: `docs/audits/2026-10-05-host-posture-dtap-panel.md`.

Two simplifications narrow decisions the operator had taken, and wait for his OK: D3 and D16.
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

**Docker lab**

- `lab/lab-install.sh:37` rebinds the admin to `0.0.0.0` inside the container.
- `lab/docker-compose.yml:34-43` publishes 8080, 3000 and 19091-19100 on all host addresses, so the
  developer's LAN reaches them.

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
| D3 *(rev 4, awaiting OK)* | Rules go by port and interface per role, not by per-hive IP. The mesh ports are authenticated, and the DMZ ingress must reach them anyway. Removed hives are kept out at the protocol layer (A-48). |
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
| D16 *(rev 4, awaiting OK)* | SSH is accepted only on each host's internal interface, on every role, with no configuration. The ingress' public interface and the egress' office/WAN interface lose SSH. It is hygiene, not a boundary: sshd's own auth is the control. `operator_sources` is dropped from v1. |
| D17 | Archi and the admin get to loopback through configuration only. Archi's code is not changed. |
| D18 | io.linkedhelper will go through the edge in the last phase. Until then its direct port is closed under enforce and reported. |
| D19 | Ingress hardening is only what `add_hive` with the ingress role and the orchestrator do by themselves. No infrastructure configuration from the user, and nothing odd, now. |
| D20 | Order of work: the core (Part A stages 1–5, A-48, A-49), then the Fluxbee Cloud connection (stages 6–7), then the implementation nodes. |
| D21 *(rev 4)* | Nobody configures the posture. The mode is automatic: observe, then enforce by itself after a clean window fixed in code; a changed ruleset goes back to observe. Two local files exist for us, never for users: break-glass and hold. No settings travel between hives. |
| D22 *(rev 4)* | No verify probes, commit-confirm timers or quarantines. A ruleset is enforced only after it ran clean in observe. It filters only inbound, keeps established connections, and depends on no remote data. |

### 2.2 Open — the operator decides

Before stages 1–5:

- The OK on D3 and D16 as revised in revision 4.
- Whether anyone enters PROD by SSH through the egress' office address (192.168.8.240) or the
  ingress' public one. Under D16 those stop working. The observe window would show it before
  enforce.

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

One pure function computes the posture from the host's own role and configuration. No registry and
no other hive is an input. From it the orchestrator renders:

- the nftables ruleset;
- the expected TCP listeners, used for reporting;
- the Syncthing settings.

**Inputs:**

- **The role.**
- **The motherbee's ports from config:** the `wan.listen` port and `identity.sync.port`.
- **On the ingress:** the edge port from `edge.listen`. A non-443 or plaintext edge is reported.
- **sshd ports:** those `sshd -T` reports, plus any port held by sshd or `ssh.socket` (seen with
  `ss`). The fallback is 22, and the list is never empty.
- **The internal interface**, derived:
  - **motherbee:** every interface. By deployment model the motherbee sits on the internal network;
    a public address on it is reported as a warning.
  - **worker and ingress:** the interface that carries the route to the motherbee
    (`ip route get <motherbee uplink IP>`).
  - **egress:** its `lan_iface`.
  - **If the derivation fails,** the host stays in observe and reports why.
- **Closed lists in code:** the core IO listeners (none in the core phase; IO.web joins at stage 7)
  and the node listeners (none; io.linkedhelper stays out, D18).

### 3.2 Inbound rules

| Role | Accepted | On |
|---|---|---|
| every role | `lo`; established/related; ICMP errors and echo (rate-limited); ICMPv6 neighbor discovery and errors | any interface |
| motherbee | tcp 22 (sshd ports), 9000, 9100, 22000 | any interface |
| worker, egress | tcp 22 | internal interface |
| ingress | the edge port, untracked | any interface |
| ingress | tcp 22 | internal interface |

**Everything else** hits the chain policy: `drop` in enforce, `accept` in observe. INVALID packets
have their own counter; they are dropped in enforce and only counted in observe.

**Spokes expose no Syncthing port.** They dial the motherbee, and their Syncthing listens on
`127.0.0.1` (§3.5).

**The edge port is untracked.**

- Raw `notrack` on prerouting for packets to the edge port, and on output for packets from it.
- The ingress' own dials use ephemeral ports and stay tracked.
- Connection floods on the public port cannot fill conntrack.

**Outbound is not filtered** (D19). Syncthing's public traffic is removed by configuration (§3.5).

**Why there are no per-hive source rules** (D3, revision 4):

- The mesh ports are authenticated, and the DMZ ingress must reach them anyway.
- Per-IP admission cut off re-addressed hives.
- It needed the registry on every spoke.
- It made joins circular.

Removed hives are kept out by A-48 at the protocol layer.

**SSH** (D16): only on internal interfaces, with no configuration. The posture also reports, without
changing anything, `PasswordAuthentication yes` and `PermitRootLogin yes` from `sshd -T`.

### 3.3 Modes, accounting, apply

**The mode is automatic** (D21):

- A host is in observe whenever its rendered ruleset changes: first install, or an upgrade that
  changes the render.
- It moves to enforce by itself after a clean window of 24 hours, fixed in code. Clean means:
  - zero would-drops on its internal interfaces;
  - no third-party firewall;
  - `nft` present.
- A would-drop restarts the window and is reported with its tuple (source, protocol, port).
- The stage-4 release only observes. The stage-5 release turns the automatic switch on.
- Spokes may switch in any order: their inbound is only SSH (plus the public port on the ingress),
  so they cannot cut the mesh.

**Would-drop accounting never carries the verdict.**

- One rule records the tuple in a size-capped dynamic set with a timeout. It records only packets
  addressed to the host (`meta pkttype host`) on internal interfaces; internet noise on the
  ingress' public interface is not recorded.
- The verdict is always the chain policy.
- If the set is full, the update fails and the packet still meets the policy, so enforce fails
  closed.
- The orchestrator reads the set every tick and accumulates the tuples in its report.

**Local files.** Ours, never user configuration:

- **`/etc/fluxbee/posture.disabled`, the break-glass.**
  - The boot unit does not load the ruleset.
  - The orchestrator tears the table down and never re-applies it while the file exists, and
    reports `effective: off (break-glass)`.
  - Creating it needs the console (the guest agent in the lab).
  - The Docker lab writes it in every container.
- **`/etc/fluxbee/posture.hold`.** Keeps a host in observe. Used through `ops.py run` while rolling
  out.

**Apply.** In one `nft -f`:

```
table inet fluxbee_host {}
delete table inet fluxbee_host
<full definition>
```

**Persist.** The last applied ruleset and its mode go to `/etc/fluxbee/posture.nft`, outside
`/etc/nftables.d`, so a customized `nftables.conf` cannot load it behind the break-glass.

**Boot unit** (`fluxbee-host-nft.service`):

- `DefaultDependencies=no`
- `After=nftables.service`
- `Before=network-pre.target`, `Wants=network-pre.target`
- `ConditionPathExists=/etc/fluxbee/posture.nft`
- `ConditionPathExists=!/etc/fluxbee/posture.disabled`

Rules use `iifname` / `oifname` only. A failed load is reported.

**Drift.**

- A normalized comparison of the live table (handles, counters and set elements stripped) against
  the render.
- The ruleset is re-applied only on drift, never on a timer, and break-glass is honoured.
- An enabled `nftables.service` whose config runs `flush ruleset` is reported as a conflict; its
  effect is repaired as drift.

### 3.4 Binds (stage 1)

Archi and the admin listen only on `127.0.0.1`, through configuration only (D17):

- `architect.listen` leaves `packaging/hive.yaml.example` and `config/hive.yaml`; Archi's code
  default is already loopback.
- The postinst rewrites exactly `listen: "0.0.0.0:3000"` under `architect:`, the value it used to
  ship, to `127.0.0.1:3000`. Any other value is left alone. Fixture tests cover the shipped example,
  PROD's shape, a file already on loopback, another value and a file without `architect:`.
- **The listener check** is report-only and starts on the motherbee:
  - a pure parser over `ss -H -tlnp` flags `sy-architect` or `sy-admin` on a non-loopback address,
    whatever the cause (config, env, code);
  - it records the finding with `append_drift_alert`;
  - from stage 4 the same parser covers every role's expected listeners.
- **Docs:** firstboot, `packaging-and-build`, `07-operaciones` and HANDBOOK §5 say "loopback,
  through a tunnel". They also say the admin API is reachable only that way, since Archi's UI text
  stays as it is.
- **Docker lab:**
  - it keeps the in-container rebind (the containers sit on a private bridge);
  - it publishes only on the host's loopback: `127.0.0.1:8080:8080`, `127.0.0.1:3000:3000`;
  - it writes the break-glass file.
- **CI:** a shell guard, in a workflow that runs on every push, over an explicit list:
  - `packaging/hive.yaml.example`, `config/hive.yaml`, `packaging/fluxbee-firstboot`,
    `lab/lab-install.sh`, `lab/docker-compose.yml`;
  - the four docs above.

  It checks non-loopback `listen:` values for Archi and the admin, and `:3000` / `:8080` URLs and
  port mappings.

### 3.5 Syncthing (stages 2 and 3)

**Addresses (stage 2)**, reconciled at every boot and watchdog run, not only at join:

- **Spoke side.**
  - The spoke has exactly one remote device, named after the motherbee's `hive_id`.
  - It gets `Static(<motherbee uplink IP>:22000)`.
  - Its own listen address is `tcp://127.0.0.1:22000`.
- **Motherbee side.** Every spoke device (named after its `hive_id`) gets `AcceptOnly`: address
  `dynamic`, so the motherbee never dials once discovery is off. It listens on `tcp://0.0.0.0:22000`;
  the firewall and device IDs restrict who connects.
- **The address type** becomes `AcceptOnly | Static(SocketAddr)`, with IP literals only.
  - A finalize without an address is refused, never written as `dynamic`.
  - The worker join derives the motherbee address the way ingress and egress do.
- **One serialized writer** (a mutex plus atomic writes) for options, addresses and folders.
  Syncthing restarts only when something changed.

**Options (stage 3).** The orchestrator owns them, and they are aligned with `vendor/syncthing/config.xml`:

- `globalAnnounceEnabled=false`
- `localAnnounceEnabled=false`
- `relaysEnabled=false`
- `natEnabled=false`
- `crashReportingEnabled=false`
- `urAccepted=-1`
- `autoUpgradeIntervalH=0`
- STUN off
- `reconnectionIntervalS=10`, because only spokes dial now and they should come back fast

**The order is enforced in code.** It is safe across skipped releases, and the whole public set
(announce, relays, NAT, listen address) switches together:

1. **A spoke switches** when its motherbee device is `Static` and a plain TCP connect to
   `<motherbee>:22000` succeeds.
2. **Each spoke reports both facts in its `/versions` snapshot**, through the existing GET_VERSIONS.
3. **The motherbee switches** when every spoke that is a device in its Syncthing config has
   reported both facts.
   - A spoke that is offline and never reported keeps the motherbee waiting, and the report names
     it.
   - Registry entries of failed joins never became devices, so they do not count.

Both predicates are pure and unit-tested on fixtures captured read-only from the four PROD hosts.
The captures keep the device addresses and options and drop the `<gui>` block, whose API key cannot
go into the public repo.

### 3.6 Reports and checks (report-only)

| Check | What it reports |
|---|---|
| Listener | Non-loopback TCP listeners not in the declaration, by process name. Motherbee from stage 1, every role from stage 4. |
| sshd | `PasswordAuthentication yes` and `PermitRootLogin yes`, as warnings. The fix is A-2. |
| Third-party firewall | ufw active, firewalld running, or any non-Fluxbee base chain on the input hook with a non-accept policy or rules. The host stays in observe. |
| Motherbee | A public address, as a warning. |

**Where it shows.**

- `watchdog_tick` computes the posture and caches it; `local_versions_snapshot` returns the cached
  value, so `/versions` fans it out.
- `ops.py health` fails on drift, `unavailable`, a third-party firewall or a derivation failure. An
  open observe window is not a failure.

### 3.7 Lifecycle

- **`add_hive`.**
  - The SSH bootstrap installs nftables if it is missing
    (`command -v nft || apt-get install -y nftables`).
  - If that fails, the join goes on and the posture is `unavailable`.
  - Nothing has to be admitted on the motherbee.
  - The spoke starts in observe.
  - The motherbee's `.deb` depends on `nftables`.
- **`remove_hive`.** REMOVE_HIVE_CLEANUP runs first; then A-48 removes the registry entry and
  revokes the hive. The cleanup includes the posture teardown (table, file, boot unit), and for an
  ingress it also:
  - stops and disables sy-edge;
  - deletes its publications;
  - deletes its mesh TLS directory.
- **Upgrade.** If the render changes, the host goes back to observe for a window.
- **Downgrade or rollback to a build without the posture.** The outgoing code tears the posture
  down:
  - the prerm on `upgrade <version older than the first posture release>`;
  - `core_rollback_local` before it restores such a generation;
  - the prerm on `remove` / `purge`.

### 3.8 A-48 — `remove_hive` revokes the hive

**Admission by certificate, not just by name.**

- Each hive's mesh leaf is pinned (SHA-256) in its registry entry when it is issued.
- The router admits a WAN peer only if its `hive_id` is registered **and** its leaf matches the pin.
- The orchestrator writes the router's registry view on `add_hive` and `remove_hive`, and the router
  reloads it. On a read error the router keeps its last good view.

**`remove_hive`, in this order:**

1. REMOVE_HIVE_CLEANUP (§3.7).
2. Remove the registry entry.
3. Delete the HMAC key.
4. Unlink the Syncthing device by name, on every role.
5. Close the hive's live WAN and identity sessions.

**Other changes:**

- REMOVE_HIVE_CLEANUP refuses to run on the motherbee. That closes an existing kill switch any
  orchestrator could send.
- `add_hive` records the spoke's SSH host key, and later SSH uses `StrictHostKeyChecking=yes`
  against it.
- Registry writes become read-modify-write merges under the per-hive lock, so a join keeps keys it
  does not own, such as the pin.

### 3.9 A-49 — signed apt repo

- `scripts/apt-repo-publish.sh` signs `InRelease` with a key that needs no passphrase prompt, since
  `ops.py publish` runs without a terminal. The key is backed up off fb-build.
- The `.deb` ships the public key (`/usr/share/keyrings/`).
- The postinst switches an existing Fluxbee source from `[trusted=yes]` to `[signed-by=…]` only once
  the repo serves a valid `InRelease`.
- The install docs use `signed-by`.

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
| 0 | Revision 4; the operator's OK on D3/D16; a third DTAP round on Part A. | Operator approval. |
| 1 | Binds and the listener check on the motherbee (§3.4). | Unit: the listener parser over fixtures (0.0.0.0, `::` and 10.10.10.10 flagged; 127.0.0.1 and ::1 not; PROD's motherbee `ss` captured read-only today). Unit: postinst rewrite fixtures. CI: the guard on every push. Infra, after `ops.py deploy`: `ss` shows 3000 and 8080 only on 127.0.0.1; connections from VMs 101, 102, 103 and 110 are refused; on the motherbee, `curl 127.0.0.1:3000/` returns the UI and `/api/messages/stream` answers `text/event-stream`; no listen alert. The operator checks the tunnel once, outside the gate. |
| 2 | Syncthing addresses (§3.5). | Unit: the address type; IP literals only; v6 formatting; a finalize without an address refused; PROD fixtures (worker1 `dynamic`↔`dynamic`, ingress static, egress with no device id) reconcile to the target; a second pass is a no-op; only the single writer touches `config.xml`. Infra: `/rest/config/devices` on the 4 hives matches the target, and dist, blob and policy sync keep working. |
| 3 | Syncthing options and the ordering predicates (§3.5). | Unit: both predicates on the PROD fixtures. Infra: `/rest/config/options` exact on the 4 hives; no relay or QUIC connections; `ss` for Syncthing shows no non-loopback UDP; after a motherbee Syncthing restart every spoke reconnects within 30 s; dist publish reaches 4/4. A skip-upgrade from a 0.1.56 mesh cannot run on 8.x through `ops.py`: it is covered by the predicate tests and recorded as an accepted residual, unless the operator offers a mesh still on 0.1.56. |
| 4 | Firewall in observe only (§3.1–§3.3, §3.6, §3.7). | Unit: role × mode render invariants (the only accept on a non-internal interface is the ingress edge port; no role accepts 3000, 8080, 5432, 4222 or 8384; observe and enforce differ only in the policy); the render is deterministic. CI: apply every role in a network namespace, check fail-closed with a full set, connect/timeout over veth. Infra: the posture in `/versions` for 4/4 hives; 24 h with zero would-drops on internal interfaces while restarting rt-gateway, sy-identity and Syncthing, rebooting a spoke and deploying a patch release; after a spoke reboot with sy-orchestrator disabled the table is there; break-glass removes the table and the orchestrator does not bring it back. |
| 5 | Automatic enforce. | A versioned probe script through `ops.py run`: from fb-build, the motherbee's 3000 and 8080 are refused and 9000 answers; from the ingress, the motherbee's 3000/8080/5432/4222/8384 fail and 9000/22000 answer; from outside, only the public port; SSH on the ingress' public address and on the egress' office address is refused; spokes rejoin after a motherbee reboot within the HANDBOOK §9 baseline; an upgrade with an unchanged render stays in enforce, a changed one goes back to observe; break-glass; edge-port connections do not grow `nf_conntrack_count`. |

**After stage 5:**

| Item | Gate |
|---|---|
| A-48 | Remove a throwaway hive, re-add the same `hive_id` with a new certificate, and the old box is refused. Live sessions close. On an ingress, sy-edge stops and the public port refuses. The registry keeps foreign keys across a join. |
| A-49 | `ops.py publish` signs without a terminal. The motherbee's `apt-get update` verifies the signature. A tampered `Release` is refused. The postinst switches the source only when `InRelease` is valid. |

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
- **Next:** the operator's OK on D3/D16, then round 3 on Part A of revision 4.
