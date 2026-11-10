# Fluxbee — Edge Egress NAT Specification

**Status:** v1.0 — IMPLEMENTED (was "implementation-ready")
**Date:** 2026-06-08

> **Update 2026-07-21:** the Mode A egress role is BUILT in `src/bin/sy_orchestrator.rs`:
> `HiveRole::{Motherbee,Worker,Egress}`, the `add_egress_hive_flow` provisioning path, and the
> self-contained `table inet fluxbee_egress` NAT/forward reconciliation (sysctl + nftables, applied
> and drift-reconciled) all exist. A single-NIC guard also shipped: `resolve_egress_nat_config`
> rejects `wan_iface == lan_iface`. The design-tense wording below (§6 "currently binary", §6.5
> "does not exist today") predates this and no longer reflects the code. §10.2, the §12 checklist
> and the §14 questions were brought up to date on 2026-11-07; the 8.x lab has run an egress hive
> (`egress1`) since 2026-07-29.
>
> **Update 2026-10-06 (design, not yet built): host posture.** `docs/host-posture-and-exposure-spec-v1.md`
> (stages 4–5) supersedes two statements here once it is built:
> - §3.3, that the egress accepts Fluxbee WAN and control SSH from the motherbee on its LAN. The
>   egress gets no WAN listener, and accepts SSH from any host on its internal interface.
> - §8.5, that motherbee and worker hosts keep the ufw/firewalld inbound path. Every role gets the
>   Fluxbee-owned `table inet fluxbee_host`, and `fluxbee_egress` keeps only forward and NAT: its
>   input chain is no longer rendered. It is never deleted explicitly (the file is re-applied every
>   minute, and a delete fails on the second apply); the leftover chain is empty with `policy
>   accept`, which cannot override `fluxbee_host`'s drop, and is gone at the next boot.
>
> The egress still fails loud without `nft`, because its NAT needs it.
>
> **Update 2026-11-10: A-219, built for 0.1.125.** §15 is the operator's explicit action that puts a
> worker that joined before the egress declaration behind the egress (`set_hive_egress`), and takes
> it back (`clear_hive_egress`), which §7 and §11 left out of v1. Its "As built" note says what
> differs from the design.
**Audience:** `SY.orchestrator` developer, deployment tooling, Ops/SRE
**Supersedes:** `edge-egress-nat-spec.md` v0.1 (2026-05-27)
**Related:** `edge-control-protocol.md`, `01-arquitectura.md`, `05-conectividad.md`, `07-operaciones.md`, `sy_orchestrator.rs`

---

## 1. Purpose

This document defines the egress model for Fluxbee deployments where all hives run on an internal LAN and only a designated host has internet access. It enables internal nodes (AI, IO, WF) to make outbound HTTPS calls to internet services (OpenAI, Anthropic, package registries) through a controlled path, without opening any inbound internet access toward workers.

The scope is intentionally narrow:

- Internal hives make outbound HTTPS calls to the internet.
- The outbound path is routed through a controlled gateway.
- No inbound internet access is opened toward workers.
- The model is simple enough for `SY.orchestrator` to provision through `add_hive`.

This document is the **egress** half of the edge story. The **ingress** half (`RT.edge`, public HTTPS termination, JWT validation) is specified separately in `edge-control-protocol.md` and is a later phase. Egress and ingress are independent responsibilities and MAY run on different hosts.

### 1.1 What changed from v0.1

| Area | v0.1 | v1.0 |
|------|------|------|
| Egress identity | Implied a worker-with-NAT or a dedicated `role: edge` running a large profile | `egress` is a **first-class role** with a minimal node profile |
| Firewall backend | nftables preferred, UFW detailed in §7.2 | **nftables is the only supported backend**; UFW is detected and warned about |
| IPv6 | "blocked or reported as unmanaged" | **Blocked hard**: sysctl + explicit nftables drop, fail-loud on bypass |
| Conntrack | Not mentioned | **Tuning documented** (§8) |
| Gateway flexibility | Edge host assumed to be a Fluxbee hive | Gateway MAY be a Fluxbee `role: egress` hive **or** a pre-existing physical router (§4.3) |
| Orchestrator | "should support role=edge" (vague) | **Explicit change map** against `sy_orchestrator.rs` (§6) |
| Hardening | Mixed into the spec | Separated into §11 (future, non-v1) with reserved hooks |

---

## 2. Terms

| Term | Meaning |
|------|---------|
| **Motherbee** | Control center of the deployment. Source of truth for hive inventory, provisioning, and operational state. |
| **Egress hive** | A Fluxbee hive with `role: egress`. Runs a minimal node profile plus OS-level NAT for an internal LAN. Owns internet egress. |
| **Worker hive** | A normal internal hive (`role: worker`) without direct internet access. |
| **Egress gateway** | The IPv4 default gateway through which workers reach the internet. MAY be an egress hive or a physical router. |
| **Egress** | Internal Fluxbee nodes making outbound internet connections through the egress gateway. |
| **WAN** | Fluxbee inter-hive WAN handled by `RT.gateway` over Unix/TCP sockets. This is Fluxbee protocol traffic, **not** generic internet. Egress NAT does not touch the WAN configuration. |
| **LAN** | Internal private network where workers and the egress gateway's internal interface live, e.g. `192.168.8.0/24`. |

---

## 3. Design Principles

### 3.1 Motherbee stays central

The egress hive is not the center of the system. Motherbee remains the operational source of truth. The egress hive participates as a managed hive: it has its own `hive_id`, runs `SY.orchestrator`, runs `RT.gateway` to connect to motherbee, and is provisioned by `add_hive` exactly like any other hive.

### 3.2 Egress is a role, not a special case

`role: egress` is a first-class role alongside `motherbee` and `worker`. It is not a worker with extra flags. This keeps the security boundary clean: an egress hive runs only what egress needs, nothing more. Identity, policy, cognition, and OPA replicas belong to the future ingress (`RT.edge`) role, not to egress.

### 3.3 Everything originates from the inside

Consistent with Fluxbee's core principle: the egress hive opens outbound connections to the internet on behalf of workers. No inbound internet connection is ever accepted toward a worker. The only inbound the egress hive accepts on its LAN interface is Fluxbee WAN and control SSH from motherbee.

### 3.4 Minimum configuration, sane defaults

A single declared network parameter (`lan_cidr`) drives the NAT rules. The egress IP (`edge_ip`) is never guessed: it is the address the LAN interface carries, which the join reads on the host when `add_hive` omits it (FINDINGS A-1).

### 3.5 Fail loud, never silent

If IPv4 egress works but IPv6 can bypass the gateway, provisioning warns or fails depending on mode. No silent partial success.

---

## 4. Topology and Gateway Modes

### 4.1 Reference topology

```
                         Internet
                            │
                            ▼ (WAN NIC, public or NATed by upstream)
                  ┌──────────────────────┐
                  │   Egress gateway     │
                  │  ┌────────────────┐  │
                  │  │ wan_iface      │  │  egress to internet
                  │  └────────────────┘  │
                  │  ┌────────────────┐  │
                  │  │ lan_iface      │  │  192.168.8.1 (edge_ip)
                  │  └────────────────┘  │
                  └──────────┬───────────┘
                             │ LAN 192.168.8.0/24
              ┌──────────────┼──────────────┐
              ▼              ▼               ▼
        ┌──────────┐  ┌──────────┐   ┌──────────┐
        │ motherbee│  │ worker-1 │   │ worker-N │
        │ .220     │  │ .x       │   │ .y       │
        └──────────┘  └──────────┘   └──────────┘
            default route of each worker → 192.168.8.1
```

The egress gateway forwards and MASQUERADEs LAN traffic out `wan_iface`. Workers have their IPv4 default route set to the gateway's LAN IP.

### 4.2 Mode A — Fluxbee egress hive

A host provisioned by `add_hive` with `role: egress`. `SY.orchestrator` on that host reconciles the NAT rules, sysctl, and conntrack tuning. This is the managed mode where Fluxbee owns the egress path end to end.

### 4.3 Mode B — Pre-existing physical router

If the deployment already has internet egress through a physical router or firewall (MikroTik, pfSense, datacenter router), Fluxbee does **not** need an egress hive at all. In this mode:

- No host runs `role: egress`.
- Workers' default route points at the physical router's IP.
- Motherbee still declares `egress.gateway_ip` (§5.2) so that `add_hive` injects that route into new workers automatically.

The `egress.gateway_ip` field abstracts "where workers exit" regardless of whether the gateway is a Fluxbee hive or a third-party box. **`role: egress` is therefore optional.** Use it only when you want Fluxbee to manage the NAT host itself.

The remainder of this document (§6 onward) describes Mode A. Mode B requires only the worker route injection (§7), nothing else.

---

## 5. `hive.yaml` Changes

### 5.1 Egress hive `hive.yaml`

The egress hive declares an `egress` section with the network parameters. Generated by `SY.orchestrator` at provisioning; not hand-edited in normal operation.

```yaml
hive_id: "edge-1"
role: egress

# WAN / inter-hive Fluxbee gateway (unchanged, same as any hive).
wan:
  gateway_name: "RT.gateway"
  uplinks:
    - address: "192.168.8.220:9000"   # motherbee's RT.gateway listener

# Egress NAT parameters (NEW).
egress:
  enabled: true
  lan_cidr: "192.168.8.0/24"          # the only required network parameter
  edge_ip: "192.168.8.1"              # the address lan_iface carries; add_hive reads it when omitted
  wan_iface: "eth0"                   # interface toward the internet
  lan_iface: "eth1"                   # interface toward the LAN
  ipv6: "blocked"                     # only "blocked" supported in v1
  reachability_url: "https://example.com"   # optional: the internet probe's target (§9)

storage:
  path: "/var/lib/fluxbee"

# Minimal egress node profile.
system_nodes:
  egress:
    nodes:
      - SY.config.routes
    wait_for:
      - SY.config.routes
```

Field rules:

| Field | Required | Default | Notes |
|-------|----------|---------|-------|
| `egress.enabled` | yes | — | Must be `true` for the orchestrator to apply NAT |
| `egress.lan_cidr` | yes | — | IPv4 CIDR of the internal LAN |
| `egress.edge_ip` | no, in `add_hive` | the single address `lan_iface` carries inside `lan_cidr`, read on the host by the join | The gateway's LAN address, which the workers route through. An explicit one must be on `lan_iface`; none or several addresses fail the join. The egress' own hive.yaml always has it |
| `egress.wan_iface` | yes | — | Internet-facing interface name |
| `egress.lan_iface` | yes | — | LAN-facing interface name |
| `egress.ipv6` | no | `"blocked"` | Only `"blocked"` accepted in v1 |
| `egress.reachability_url` | no | none: no probe | The `https://` URL the internet probe GETs (§9). `add_hive` takes the request's, else the motherbee's |

The egress hive does **not** touch its `wan` section to provide egress. WAN is Fluxbee protocol connectivity to motherbee; egress NAT is OS-level network plumbing. They coexist without interaction.

### 5.2 Motherbee `hive.yaml` egress declaration

Motherbee declares which gateway workers should use. This drives route injection into new workers (§7). Works identically for Mode A and Mode B.

```yaml
# hive.yaml of motherbee (NEW section)
egress:
  gateway_ip: "192.168.8.1"      # where workers send their default route
  edge_hive: "edge-1"            # optional; informational, the egress hive_id (Mode A only)
  reachability_url: "https://example.com"   # optional; the internet probe's target (§9)
```

| Field | Required | Notes |
|-------|----------|-------|
| `egress.gateway_ip` | yes (to enable injection) | IPv4 of the egress gateway. If absent, `add_hive` does not inject any egress route |
| `egress.edge_hive` | no | The egress hive's `hive_id` for inventory clarity. Absent in Mode B |
| `egress.reachability_url` | no | The `https://` URL the internet probe GETs (§9): the motherbee's own, and the one `add_hive` writes into new workers (with `gateway_ip`) and into an egress whose request names none. Without it nobody probes |

### 5.3 New Rust structs in `sy_orchestrator.rs`

Add to `HiveFile` (currently line 132):

```rust
#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    role: Option<String>,
    wan: Option<WanSection>,
    storage: Option<StorageSection>,
    blob: Option<BlobSection>,
    dist: Option<DistSection>,
    identity: Option<IdentitySection>,
    government: Option<GovernmentSection>,
    system_nodes: Option<SystemNodesSection>,
    egress: Option<EgressSection>,         // NEW
}

#[derive(Debug, Clone, Deserialize)]
struct EgressSection {
    #[serde(default)]
    enabled: bool,
    lan_cidr: Option<String>,      // required when enabled
    edge_ip: Option<String>,       // optional in add_hive; the join reads it on the host
    wan_iface: Option<String>,     // required when enabled (egress hive)
    lan_iface: Option<String>,     // required when enabled (egress hive)
    #[serde(default = "default_ipv6_policy")]
    ipv6: String,                  // "blocked"
    gateway_ip: Option<String>,    // used on motherbee for worker injection
    edge_hive: Option<String>,     // informational
    reachability_url: Option<String>, // the internet probe's target (§9); none: no probe
}

fn default_ipv6_policy() -> String { "blocked".to_string() }
```

Extend `SystemNodesSection` (currently line 146):

```rust
#[derive(Debug, Deserialize)]
struct SystemNodesSection {
    motherbee: Option<RoleSystemNodes>,
    worker: Option<RoleSystemNodes>,
    egress: Option<RoleSystemNodes>,       // NEW
}
```

---

## 6. `SY.orchestrator` Changes

The orchestrator now recognizes four roles (`motherbee` | `worker` | `egress` | `ingress`) via `HiveRole`; anything else is rejected at the startup gate. This section originally mapped the change from the earlier binary (`motherbee` | `worker`) orchestrator; that work is now implemented.

### 6.1 Role recognition

Add alongside `is_mother_role` / `is_worker_role` (line 16106):

```rust
fn is_egress_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "egress")
}
```

Update the startup gate (lines 503–512):

```rust
let is_motherbee = is_mother_role(hive.role.as_deref());
let is_worker = is_worker_role(hive.role.as_deref());
let is_egress = is_egress_role(hive.role.as_deref());          // NEW
if !is_motherbee && !is_worker && !is_egress {                 // CHANGED
    error!(role = ?hive.role,
        "SY.orchestrator supports only role=motherbee|worker|egress; exiting");
    // exit
}
```

### 6.2 Node profile selection

`system_nodes_for_role` (line 2887) currently switches on `is_motherbee` boolean. Change to select among three roles. Suggested signature change: pass the resolved role enum rather than a bool.

```rust
enum HiveRole { Motherbee, Worker, Egress }   // NEW

fn system_nodes_for_role(
    hive: &HiveFile,
    role: HiveRole,                            // CHANGED from is_motherbee: bool
) -> Result<RoleSystemNodes, OrchestratorError> {
    let section = hive.system_nodes.as_ref()
        .ok_or_else(|| "invalid hive.yaml: system_nodes section is required".to_string())?;
    let role_section = match role {
        HiveRole::Motherbee => section.motherbee.as_ref(),
        HiveRole::Worker    => section.worker.as_ref(),
        HiveRole::Egress    => section.egress.as_ref(),
    }.ok_or_else(|| format!("invalid hive.yaml: system_nodes.{} section is required", role.as_str()))?;
    validate_system_nodes(role_section, role)?;
    Ok(role_section.clone())
}
```

All other call sites that pass `is_motherbee` into `system_nodes_for_role` / `validate_system_nodes` (lines 542, 2166, 2169) must be updated to pass the role.

### 6.3 Node validation

`validate_system_nodes` (line 2910):

- The **"SY.config.routes must be first"** invariant (lines 2934–2940) applies to **all roles** including egress. The egress profile's first (and only) node is `SY.config.routes`, so this passes naturally.
- The **vault ordering / vault-required** checks (lines 2941–2969) are gated by `is_motherbee` and do **not** apply to egress. No change needed beyond passing the role through; egress is not motherbee so the block is skipped.
- The **"workers must not run sy-vault"** check (line 2989) should extend to egress: egress must not run `sy-vault` either. Change the condition from `!is_motherbee` to `role != Motherbee`.

### 6.4 Remote `hive.yaml` generation

`add_hive_flow` (line 14061) generates the worker `hive.yaml` via a `format!` with `role: worker` hardcoded (line 14580–14581). For `role: egress`, generate a distinct yaml that includes `role: egress` and the `egress` section. Two sub-cases:

1. **Provisioning an egress hive** (`add_hive` with `role=egress`): emit a yaml with `role: egress`, the `egress` block (with `lan_cidr`, `edge_ip`, `wan_iface`, `lan_iface`, `ipv6`), and `system_nodes.egress`.

   **Source of the egress params (added v1.0):** the host-specific NAT params (`lan_cidr`, `wan_iface`, `lan_iface`, optional `edge_ip`/`ipv6`) come from the **`add_hive` command payload**, under an `egress` object. They cannot come from motherbee's `hive.yaml`: interface names are host-specific and unknowable in advance. Motherbee validates them at request time (reusing `resolve_egress_nat_config`; an explicit `edge_ip` must be inside `lan_cidr`). Before it installs anything, the join reads the host over SSH (`egress_edge_ip_on_host`): an omitted `edge_ip` becomes the single address `lan_iface` carries inside `lan_cidr`, an explicit one must be on `lan_iface`, and both interfaces must exist; anything else fails the join with `EGRESS_LAN_LEG_INVALID`, asking for an explicit `edge_ip` when the addresses do not settle it (`EGRESS_LAN_LEG_NOT_VERIFIED` when the host cannot be read). Once the egress' orchestrator is up, the join reads the host again: the NAT table, forwarding, the IPv6 block and the LAN leg (`EGRESS_NAT_NOT_READY` when one is missing, `EGRESS_NAT_NOT_VERIFIED` when the host cannot be read). This contrasts with the worker-side `egress.gateway_ip`, which **is** declared once in motherbee's `hive.yaml` (§5.2) because it is global to the deployment. Example payload:

   ```json
   {
     "hive_id": "edge-1", "address": "192.168.8.1", "role": "egress",
     "egress": { "lan_cidr": "192.168.8.0/24", "wan_iface": "eth0", "lan_iface": "eth1" }
   }
   ```

   The `egress` object may also name `reachability_url`, the `https://` URL the egress' internet
   probe GETs (§9). Without it the join takes the motherbee's `egress.reachability_url`; with
   neither, the egress does not probe. Anything but an `https://` URL with a host is refused at
   request time.

   **Source of `system_nodes.egress`:** read from motherbee's own `hive.yaml` `system_nodes.egress` template — symmetric with how `system_nodes.worker` is the worker template. Motherbee must declare a `system_nodes.egress` stanza (minimal: `SY.config.routes`). The egress flow is a **dedicated** function (`add_egress_hive_flow`) that reuses the SSH/core-sync/systemd helpers but skips all worker machinery (blob/dist/identity/syncthing).
2. **Provisioning a worker when motherbee declares egress**: the existing worker yaml generation, plus `egress.gateway_ip` injected **into the worker yaml**. The worker's own orchestrator applies and reconciles the default route + IPv6 block locally on each boot (§7). The route is not pushed over SSH.

`render_worker_system_nodes_yaml` (line 3030) hardcodes `worker:` in the emitted section header. Parameterize it to emit the correct role key:

```rust
fn render_system_nodes_yaml(role: HiveRole, section: &RoleSystemNodes) -> String {
    let mut out = format!("system_nodes:\n  {}:\n    nodes:\n", role.as_str());
    for name in &section.nodes {
        out.push_str(&format!("      - {}\n", name.trim()));
    }
    if !section.wait_for.is_empty() {
        out.push_str("    wait_for:\n");
        for name in &section.wait_for {
            out.push_str(&format!("      - {}\n", name.trim()));
        }
    }
    out
}
```

(Single parameterized function preferred over duplicating per role.)

### 6.5 Network reconciliation (the new work)

This does not exist today and is the core of the egress feature. On startup and on reconcile, an egress hive's orchestrator must apply the network configuration described in §8. This runs only when `is_egress && egress.enabled`.

After applying it, the orchestrator reads back what it set and checks the LAN leg (`serving_problems`, FINDINGS A-1): IPv4 forwarding on, IPv6 disabled, `wan_iface` and `lan_iface` present, `edge_ip` on `lan_iface`. nftables matches interfaces by name, so the table loads without them. Any of them missing fails the boot; after boot each one is a WARN and an `egress` drift alert on every drift tick, in the egress's own drift-alert file, which the motherbee does not read yet (FINDINGS A-175).

The egress NAT reconciliation is idempotent and targets **nftables** directly (§8). The orchestrator's `ufw allow <port>` helpers (`open_firewall_rules_local` and its peers) were removed in 0.1.83 ([control-path hardening](control-path-hardening-v1.md) §6).

### 6.6 Worker route injection

When motherbee has `egress.gateway_ip` set, `add_hive_flow` for a `role=worker` host injects `egress.gateway_ip` into the generated worker `hive.yaml`. The worker's orchestrator then applies the IPv4 default route pointing at `gateway_ip` and the IPv6 block (§8.3) **locally** on each startup/reconcile, and reports the verification payload (§9). Motherbee does not push the route over SSH; it is reconciled locally on the worker, consistent with the orchestrator's local reconciliation model elsewhere and persistent across reboots by reapplication.

### 6.7 Change summary table

| # | Change | Location (current) | Type |
|---|--------|--------------------|------|
| 1 | `is_egress_role()` | new, near 16106 | mechanical |
| 2 | Accept `egress` at startup gate | 503–512 | mechanical |
| 3 | `EgressSection` struct + `HiveFile.egress` | 132 | mechanical |
| 4 | `SystemNodesSection.egress` | 146 | mechanical |
| 5 | `HiveRole` enum, `system_nodes_for_role` by role | 2887 | mechanical |
| 6 | `validate_system_nodes` role-aware; egress excludes vault | 2910, 2989 | mechanical |
| 7 | Parameterized `render_system_nodes_yaml` | 3030 | mechanical |
| 8 | Egress hive yaml generation | 14580 | mechanical |
| 9 | **nftables/sysctl/conntrack reconciliation** | does not exist | **core work** |
| 10 | `egress.gateway_ip` injected into worker yaml; route reconciled locally by worker orchestrator | add_hive_flow + worker startup | integration |
| 11 | Verification payload fields | new | mechanical |

---

## 7. Worker Default Route

When motherbee declares `egress.gateway_ip`, every worker provisioned afterward receives that value **in its `hive.yaml`** (`egress.gateway_ip`). The worker's own orchestrator applies and reconciles the route locally on each startup — consistent with the orchestrator's local reconciliation model, and persistent across reboots by reapplication rather than by writing distro-specific network config (netplan/NetworkManager).

Required behavior on the worker (applied locally by the worker's orchestrator):

1. Set/reconcile the IPv4 default route to `egress.gateway_ip` on each boot.
2. Apply the IPv6 block (§8.3).
3. Verify internet reachability through the gateway: an HTTPS GET of its
   `egress.reachability_url`, which `add_hive` copies from the motherbee; without one it does not
   probe (§9).
4. Report verification fields (§9).

Workers provisioned **before** the egress declaration do not carry `egress.gateway_ip` in their yaml and are not retroactively changed by a later `add_hive`. Writing the section into such a worker, and applying it, is the operator's explicit action, `set_hive_egress` (§15, A-219, built for 0.1.125); `clear_hive_egress` takes it back.

---

## 8. Network Configuration on the Egress Hive

nftables is the single supported backend. All rules are written in a Fluxbee-owned, idempotent, marked block.

### 8.1 sysctl

File: `/etc/sysctl.d/99-fluxbee-egress.conf`

```
# BEGIN FLUXBEE EGRESS
net.ipv4.ip_forward = 1
net.ipv6.conf.all.forwarding = 0
net.ipv6.conf.all.disable_ipv6 = 1
net.ipv6.conf.default.disable_ipv6 = 1
net.ipv6.conf.all.accept_ra = 0
net.ipv6.conf.default.accept_ra = 0
# END FLUXBEE EGRESS
```

Applied via `sysctl --system` after writing. IPv4 forwarding on, IPv6 fully disabled (forwarding off, interfaces disabled, router advertisements ignored).

### 8.2 nftables NAT and forwarding

File: `/etc/nftables.d/fluxbee-egress.nft` (included from the main nftables ruleset), or a dedicated table managed entirely by Fluxbee.

```
# BEGIN FLUXBEE EGRESS NAT
table inet fluxbee_egress {
    chain forward {
        type filter hook forward priority 0; policy drop;

        # Stateful return traffic.
        ct state established,related accept

        # LAN -> WAN egress.
        iifname "eth1" oifname "eth0" ip saddr 192.168.8.0/24 accept

        # Explicitly drop all IPv6 forwarding (belt and suspenders with sysctl).
        meta nfproto ipv6 drop
    }

    chain postrouting {
        type nat hook postrouting priority srcnat; policy accept;

        # MASQUERADE LAN out the WAN interface.
        ip saddr 192.168.8.0/24 oifname "eth0" masquerade
    }
}
# END FLUXBEE EGRESS NAT
```

Interface names and CIDR are substituted from the `egress` section. The `forward` chain default policy is `drop`; only stateful return traffic and the explicit LAN→WAN path are accepted. This is the hook point where the future FQDN allow-list (§11) inserts.

### 8.3 IPv6 block on workers

On each worker, the orchestrator applies the same IPv6 sysctl disable as §8.1 (the `disable_ipv6` / `accept_ra` lines, without the forwarding/NAT lines). This prevents a worker from auto-configuring an IPv6 default route via a rogue Router Advertisement and bypassing the gateway. If the worker cannot be made to block IPv6, provisioning reports `EGRESS_IPV6_UNMANAGED` and fails or warns per deployment mode.

### 8.4 conntrack tuning

NAT MASQUERADE is stateful. Under load (many workers streaming to LLM APIs), the connection tracking table fills and packets are dropped silently. Tune on the egress hive:

File: `/etc/sysctl.d/99-fluxbee-conntrack.conf`

```
# BEGIN FLUXBEE CONNTRACK
net.netfilter.nf_conntrack_max = 262144
net.nf_conntrack_max = 262144
# END FLUXBEE CONNTRACK
```

And set the hashtable buckets (boot parameter or module option):

```
# /etc/modprobe.d/fluxbee-conntrack.conf
options nf_conntrack hashsize=65536
```

Defaults shown are sized for a moderate deployment. Document scaling guidance in `07-operaciones.md`: monitor `nf_conntrack_count` vs `nf_conntrack_max`; raise both proportionally when sustained utilization exceeds ~70%.

### 8.5 UFW handling

On a `role: egress` host, **nftables is the single firewall backend for both inbound and egress**. The orchestrator does not use `ufw` on egress hosts: the inbound port rules (WAN/identity) that other roles opened via `ufw` before 0.1.83 are instead expressed in the same Fluxbee-owned nftables ruleset on the egress host. This avoids two backends managing forwarding/inbound policy on the same host, and the contradiction of disabling `ufw` while still depending on it for inbound ports.

`nft` must be present on an egress host; if absent, provisioning fails loud (no silent fallback to ufw/iptables). Since 0.1.83 no role writes ufw or firewalld rules; when either is active, the orchestrator raises the posture alert `host_firewall_active`, since it may close 9000, 9100, 9200, 22000 or 443 ([control-path hardening](control-path-hardening-v1.md) §6).

---

## 9. Verification

After provisioning, the join reports what it read on the box. No silent success.

An egress join answers these fields, besides `status`, `hive_id`, `address`, the SSH fields, `wan_connected`, `orchestrator_connected`, `warnings` and `note`:

```json
{
  "egress_role": "egress",
  "egress_lan_cidr": "192.168.8.0/24",
  "egress_edge_ip": "192.168.8.1",
  "egress_wan_iface": "eth0",
  "egress_lan_iface": "eth1",
  "egress_nat_applied": true,
  "egress_lan_leg_ok": true,
  "egress_ipv4_forwarding": true,
  "egress_ipv6_blocked": true,
  "egress_internet_reachable": null,
  "egress_reachability_url": "https://example.com"
}
```

A worker join answers no egress field. The worker applies `egress.gateway_ip` at its own boot and says what it did in its own journal:

```text
worker egress route reconciled gateway_ip="192.168.8.1" route_applied=true ipv6_blocked=true internet_reachable=true
```

`internet_reachable` is an HTTPS GET (curl, 5 s) of the hive's own `egress.reachability_url` (`check_internet_reachable`), on the egress at boot and every drift tick, and at boot on a worker that routes through it and on the motherbee. HTTPS is the path egress actually uses, so a network that filters ICMP is no false negative. The URL is the operator's ([ACME design §8](acme-edge-certificates-v1.md)): without it the probe does not run and logs `not_configured`, and the code never pings or fetches a target of its own. A host without `curl` does not probe either and logs `not_probed`. A failed probe (`false`) is a warning in that hive's journal; on the egress, since 0.1.110, also an `internet_unreachable` drift alert in its own drift-alert file, which the motherbee does not read yet (FINDINGS A-175). The egress join answers `egress_internet_reachable: null` and the URL the egress got (`egress_reachability_url`, null for none): the probe's result stays in the egress' journal, not transmitted to the motherbee in v1 (T-VER-1). (Public-IP discovery via an IP-echo endpoint is deferred to a later, fuller verification once the complete system is in place.) If IPv4 works but IPv6 is not blocked, the result warns or fails per mode.

---

## 10. Relationships

### 10.1 With WAN / `RT.gateway`

Egress NAT does not touch the `wan` section or `RT.gateway`. WAN is Fluxbee protocol connectivity between hives over sockets; egress NAT is OS-level IP forwarding to the internet. They are orthogonal layers on the same host and coexist without interaction.

### 10.2 With `SY.edge` (ingress)

The public ingress is built: `SY.edge` on a hive with the `ingress` role ([edge-ingress-spec-v6](edge-ingress-spec-v6.md)); the `RT.edge` design it replaced is in `docs/history/rt-edge-spec.md`. It terminates HTTPS and turns HTTP into mesh messages. It is a **separate responsibility** from egress NAT.

A hive has one role, so the ingress and the egress run on separate hives (on 8.x, `ingress1` and `egress1`). The ingress role does not change the egress role defined here.

---

## 11. Future Hardening (non-v1)

These are legitimate production concerns deferred out of v1. The v1 design reserves hooks so they can be added without rework. Do not implement these for the initial egress that unblocks testing.

| Concern | Where it inserts | Note |
|---------|------------------|------|
| FQDN/SNI egress allow-list | `forward` chain in §8.2, before the LAN→WAN accept; or an explicit forward-proxy that AI nodes use | Restricts which destinations workers can reach. The single biggest production control. Reserve the chain position now |
| Egress observability / audit | nftables `log` statements on the forward chain; or NetFlow/sFlow on the egress hive | Forensic visibility of what workers called. Required for any compliance posture |
| Per-worker egress rate limit | nftables `limit` on per-saddr connections/bytes | Caps exfiltration bandwidth from a compromised worker |
| Designated DNS resolver | Worker resolver config + DoT upstream on the egress hive | Avoids DNS leakage to public resolvers and spoofing in transit |
| Egress HA (no SPOF) | keepalived/VRRP for a floating `edge_ip` | Egress hive is currently a SPOF for internet; HA is beta-later per project scope |
| Process/host isolation | AppArmor/SELinux profile, network namespaces if co-located with RT.edge | Reduces blast radius of a host compromise |
| Reconcile existing workers | Operator action to re-apply route to workers provisioned before egress declaration | Built for 0.1.125: `set_hive_egress` and `clear_hive_egress` (§15, A-219) |

The v1 nftables `forward` chain (§8.2) is structured so the allow-list and logging insert cleanly: the allow-list narrows the LAN→WAN accept rule, and logging adds `log` statements on the same chain. No restructuring required to add them later.

---

## 12. Implementation Checklist

**Status (2026-11-07):** built, in `src/bin/sy_orchestrator.rs`. The two boxes left open were never
recorded as checked live.

### Orchestrator — role plumbing (mechanical)

- [x] `is_egress_role()` helper
- [x] Accept `egress` at startup role gate
- [x] `EgressSection` struct, add `egress` to `HiveFile`
- [x] Add `egress` to `SystemNodesSection`
- [x] `HiveRole` enum; refactor `system_nodes_for_role` to select by role
- [x] Update all `is_motherbee` call sites feeding `system_nodes_for_role` / `validate_system_nodes`
- [x] `validate_system_nodes`: skip vault checks for non-motherbee; extend "no sy-vault" to egress
- [x] Parameterize `render_system_nodes_yaml` to emit the role key
- [x] Egress hive `hive.yaml` generation (in `add_egress_hive_flow`)

### Orchestrator — network reconciliation (core work)

- [x] sysctl writer for `/etc/sysctl.d/99-fluxbee-egress.conf` + `sysctl --system`
- [x] nftables ruleset writer (marked, idempotent block) with CIDR/iface substitution
- [x] nftables apply + verify ruleset loaded
- [x] conntrack tuning (sysctl + modprobe option)
- [x] UFW detection: built as a report only. Since 0.1.83 an active ufw or firewalld raises the posture alert `host_firewall_active`, and the orchestrator writes no rule to either ([control-path-hardening-v1](control-path-hardening-v1.md) §6)
- [x] Egress hive verification payload
- [x] Worker default-route injection from motherbee `egress.gateway_ip` (only into workers that join after it is declared)
- [x] Worker IPv6 block (sysctl disable + accept_ra=0)
- [x] Worker verification payload, `EGRESS_IPV6_UNMANAGED` path

### Validation / tests

- [x] `role=egress` passes `validate_system_nodes` with `SY.config.routes`-only profile (unit test; the packaged profile adds `SY.opa.rules`, §14)
- [x] `role=egress` rejected if it lists `sy-vault` (unit test)
- [x] Idempotent re-apply of nftables/sysctl produces no drift (the ruleset adds and flushes its own table in one transaction; the watchdog re-applies it every minute)
- [ ] Worker reaches internet through gateway; IPv6 confirmed blocked (the egress host's NAT, IPv6 block and internet probe were checked on 8.x on 2026-07-29; a worker's route through it was not). The gate of `set_hive_egress` on 8.x (§15.6) is its first live check
- [ ] Mode B: workers receive route from `gateway_ip` with no egress hive present (no egress hive is required by the code; not tested)

---

## 13. References

| Topic | Document |
|-------|----------|
| Ingress runtime (RT.edge) and control protocol | `edge-control-protocol.md` |
| Architecture, islands, naming | `01-arquitectura.md` |
| IRP/WAN connectivity | `05-conectividad.md` |
| Operations, systemd, bootstrap | `07-operaciones.md` |
| Orchestrator source | `sy_orchestrator.rs` |

---

## 14. Open Questions for Implementation

1. ~~**Egress profile floor**~~ — **resolved**: `validate_system_nodes` accepts `SY.config.routes` alone for the egress (unit test), and the egress router runs on 8.x (`egress1`, since 2026-07-29). The packaged profile (`packaging/hive.yaml.example`) adds `SY.opa.rules`, which every role runs so its router enforces the global user policy; it is not a readiness gate.
2. ~~**`edge_ip` derivation**~~ — **resolved (FINDINGS A-1)**: not derived. The first usable address of the CIDR was a guess that could name an address no interface carried, a gateway no worker reaches; the join reads the address `lan_iface` carries instead (§6.4).
3. ~~**nftables include mechanism**~~ — **resolved**: the dedicated `table inet fluxbee_egress` (`EGRESS_NFT_TABLE`). Its file adds and flushes the table in one transaction, so a re-apply never leaves the host without it, and `fluxbee-egress-nft.service` loads it at boot (F17). The orchestrator changes no other host rule: an active ufw or firewalld is reported (`host_firewall_active`), never written.
4. ~~**IP-echo endpoint for `egress_public_ip`**~~ — **resolved**: v1 verification is an HTTPS GET of the operator's `egress.reachability_url` (`internet_reachable`), confirming reachability only; without the URL there is no probe. Public-IP discovery via IP-echo is deferred to the fuller verification of the complete system.
5. **conntrack defaults**: 262144 / hashsize 65536 are starting points. Tune to deployment scale. Built with these values as constants (`EGRESS_CONNTRACK_MAX`, `EGRESS_CONNTRACK_HASHSIZE`), not configurable.

---

## 15. An existing worker goes behind the egress (A-219)

**Status:** built for 0.1.125 (A-219), as the operator approved it on 2026-11-10, every open point
of 15.7 as recommended. 15.8 says what the build does differently. Line numbers are
`src/bin/sy_orchestrator.rs` at `d9ca8a6e`, the design's reading, unless another file is named.

**The problem (FINDINGS A-219):**

- The join writes a worker's `egress` section from the motherbee's declaration
  (`render_worker_egress_yaml`, `:8116-8143`, called at `:29213-29222`).
- Nothing writes a worker's hive.yaml after its join. Only the three joins (`:29261`, `:30048`,
  `:30787`) and `set_storage` (`:33096`) write a hive.yaml, and `set_storage` runs on the
  motherbee's own orchestrator (`action_routes_via_local_orchestrator`,
  `src/bin/sy_admin.rs:22770`).
- So a worker that joined before the declaration never goes out through the egress. On 8.x the
  motherbee declares `gateway_ip` 10.10.10.40 (egress1). worker1 has no `egress` section: its
  default route is still 10.10.10.1 (the hypervisor's SNAT), its IPv6 is on, no probe runs, and
  AI.pruebas@worker1 reaches its provider outside the egress. Nothing reports it.

**The operator's call (2026-11-10):** *"si workers tienen que salir a internet, si b)"*. Workers go
out through the egress, by an explicit action per worker, from Archi with the operator's CONFIRM,
mediated by SY.admin.

**What does not change:**

- The motherbee's route. Fluxbee never rewrites it (PB-8; `verify_motherbee_egress`,
  `:10639-10650`), and the action refuses the motherbee.
- A worker's `egress` stays local, and no update carries it (hive-config-update-v1 §2). This action
  is the one exception: per worker, on the operator's word.
- §7: the boot applies the section, and the watchdog keeps the route.

### 15.1 The action

- **Name:** `set_hive_egress`, `PUT /hives/{hive}/egress` (open point 1). Between the
  orchestrators it travels as a new SYSTEM message, `SYSTEM_EGRESS_ROUTE`.
- **Who calls it:**
  - Archi stages it with `fluxbee_plan_compiler`, one step per worker. The operator writes
    `CONFIRM`, and SY.admin's executor runs the step as the operator.
  - The operator, on SY.admin's loopback HTTP API (`ops.py admin PUT /hives/worker1/egress '{}'`).
  - Nobody else. Its `mesh_access` is `Nobody` (`sy_admin.rs:5785-5787`), as for
    `update_installation` (`sy_admin.rs:5815`): not Archi's own `ADMIN_COMMAND` or SCMD, not
    IO.cloud.
- **Payload:** `{}`, with an optional `access_from`: the IPv4 addresses or networks the operator
  manages the box from (G3 below; open point 3). It names no gateway: the gateway is the
  motherbee's declaration, never the caller's.
- **The path:** SY.admin → `SY.orchestrator@motherbee` (an admin action through the local
  orchestrator, as `core_rollback`) → `SY.orchestrator@<worker>` (`SYSTEM_EGRESS_ROUTE`, forwarded
  as `core_rollback` forwards `SYSTEM_CORE_ROLLBACK`, `:3589-3635`).
- **What SY.admin checks,** before it sends anything:
  - The hive is a worker of the registry with its join done (`HiveRegistry::role`, the strict
    reading, `crates/fluxbee_sdk/src/mesh/registry.rs:70`). Else `NOT_A_WORKER`, naming why: the
    motherbee, an ingress, an egress, or a hive whose join is not done.
  - No installation update run is under way (`INSTALLATION_UPDATE_RUNNING`, as
    `set_installation_ai`): its core step restarts the worker's orchestrator.
- **What the motherbee's orchestrator checks:**
  - Its own `egress.gateway_ip` (`NO_EGRESS_DECLARED`) and `egress.reachability_url`
    (`NO_REACHABILITY_URL`): the switch is judged by the probe, and the code never picks a target
    of its own (§9).
  - The section, rendered by `render_worker_egress_yaml`, the join's renderer. A declaration the
    join would refuse is refused here (`EGRESS_DECLARATION_INVALID`).
  - No join or removal of that hive. It takes the hive's topology lock without waiting
    (`try_lock_hive_topology`, `:740`) and holds it for the whole action (`JOIN_IN_PROGRESS`).
  - The worker is reachable: its orchestrator is visible in the LSA, the forward's own check
    (`forward_system_action_to_hive_with_timeout`, `:21702`; `HIVE_UNREACHABLE`).
  - Its part runs in a task of its own, as `node_help` does (`:3521-3525`). The motherbee's admin
    inbox takes one message at a time (`run_admin_worker`, `:1158`), and the action may take 40 s.
- **What the worker's orchestrator does.** It owns its routing table, so it judges the guards.
  - It refuses on any role but worker (`NOT_A_WORKER`), and while another switch is open
    (`EGRESS_SWITCH_OPEN`).
  - It parses the section it got as its `EgressSection`.
  - When its section and its default route already match, it answers `unchanged`.
  - Otherwise it runs the guards (15.2), then switches, checks, and commits or reverts (15.3).
  - The switch runs in a task of its own. The worker's SYSTEM inbox also takes one message at a
    time (`run_system_worker`, `:1205`), and it must take the motherbee's confirm while the switch
    waits for it.
- **An older worker** ignores a SYSTEM message it does not know (`_ => {}`, `:3857`). The forward
  ends at its timeout and nothing changes there. The answer says to bring the worker to the
  motherbee's release first (`update_installation`).
- **The policy:** `SYSTEM_EGRESS_ROUTE` is a protected SYSTEM action (`PROTECTED_SYSTEM_ACTIONS`,
  `src/router/system_policy.rs:37`). It is an orchestrator mutation that only
  `SY.orchestrator@motherbee` sends (rule (3), `orchestrator_mutations`, `policy/system.rego:66`),
  and a spoke-only action that no rule admits on the motherbee (`spoke_only_actions`,
  `policy/system.rego:75`; `SPOKE_ONLY_SYSTEM_ACTIONS`, `:3269`).

### 15.2 The guards, before the switch

The switch changes the IPv4 default route, and turns IPv6 off. A destination the worker reaches on
a directly connected subnet, or by a more specific route, keeps its path. A destination it reaches
by the default route moves to the egress. So each guard is a route lookup,
`ip -4 route get <address>`, as the egress join reads `ip -o route get <client>` for
`SSH_FROM_WAN_LOCKOUT` (`EGRESS_SSH_ARRIVAL_COMMAND` and `route_get_dev`, `:9794-9804`).

- The guards only read: each command under a timeout, 5 s in all.
- A guard that fails refuses and changes nothing. The refusal names the address and the route it
  found.
- A worker without `curl` cannot probe, and is refused too (`PROBE_UNAVAILABLE`).

**G1. The gateway is on a directly connected subnet of the worker.**

- **Check:** the lookup of `gateway_ip` has no `via`, and `gateway_ip` is not one of the worker's
  own addresses. Refusal: `GATEWAY_NOT_ON_LINK`.
- **Without it:** the kernel itself refuses an off-link gateway (`Nexthop has invalid gateway`), so
  the guard prevents no break: it says why before anything is touched, instead of a kernel error in
  the middle of the switch. A gateway off the worker's subnet means this worker cannot use the
  egress as its gateway at all. The egress' LAN leg must be on the worker's network (§4.1), or the
  worker's own router must send its traffic there.

**G2. The mesh does not depend on the default route.**

- **What:** each `wan.uplinks` address, and `identity.sync.upstream`. The join writes both with one
  host, the motherbee's address (`resolve_worker_uplink_address`, `:32939-32954`; `:29210`).
  Syncthing dials the motherbee at that host too (`reconcile_spoke_motherbee_address`,
  `:13490-13502`). So one lookup covers the WAN (9000), identity (9100) and Syncthing (22000).
- **Check:** each is an IPv4 address, and its lookup does not go through the default route: no
  `via`, or the `via` of a more specific route. A host name is refused: its DNS would go through the
  default route, and a redial would then depend on the egress. Refusal: `UPLINK_ON_DEFAULT_ROUTE`.
- **Without it:** where the motherbee is on another network, reached through the LAN's router, the
  switch sends the uplink to the egress.
  - The live WAN session's packets leave through the egress' NAT with another source, and the
    session dies. The worker drops off the mesh, and the action's answer is lost.
  - At best the control path comes back through the egress' NAT: a new dependency. At worst the
    worker is cut off until someone restores its route from the console.
  - The second lookup and the confirm (15.3) would revert it, after up to 40 s without the mesh;
    the guard refuses before anything moves.
  - hive-config-update-v1 §2 keeps the control path out of every update for the same reason.
- **On 8.x** it passes: the motherbee is on worker1's own subnet.

**G3. The operator's access does not depend on the default route.**

- **What:** the peers of the worker's established inbound SSH sessions, on the ports `sshd -T`
  reports (`sshd_listen_ports`, `:9188`), and each entry of `access_from`. Archi and SY.admin run on
  the motherbee, whose route the action never touches: this guard is about the worker's own box.
- **Check:** each is IPv4, and its lookup (for a network, the route that covers it) does not go
  through the default route. An established IPv6 session is refused too, since the IPv6 block cuts
  it. Refusal: `ACCESS_ON_DEFAULT_ROUTE`.
- **Without it:** an operator who reaches the worker from a network that is not directly connected
  (an office or a VPN network, behind the LAN's router) sends the requests in through that router.
  - After the switch the worker's replies leave through the egress: an asymmetric return path. The
    egress drops them, or sends them out NATed with its own address. Either way the session breaks,
    and new sessions never complete.
  - On a worker with two NICs, a strict reverse-path filter drops the requests themselves.
  - The mesh and the console still work, but the box is closed to SSH from that network, and
    nothing says so.
- **How it is kept:** the refusal names each peer or network, and the way to keep it: a static route
  at the OS level (netplan) through the current gateway, which the check then sees as a more
  specific route. Fluxbee adds no route of its own for it (open point 3), the line PB-8 draws for
  the motherbee's route.
- **The answer** always names where SSH still reaches the box from: its connected subnets and its
  more specific routes. The hypervisor's console is never affected.
- **On 8.x** it passes: worker1's network (10.10.10.0/24) is not reachable from outside, and the
  operator reaches the VMs through the hypervisor's console and guest agent, which use no network.

### 15.3 After the switch: the check, the commit, the way back

**The steps on the worker:**

1. **Record.** It writes `egress-switch.json` atomically, next to `config-applied.json`
   (`orchestrator_runtime_dir`). The record holds the switch id, the target section, the default
   routes as `ip -4 route show default` lists them, the four IPv6 sysctls' live values, whether
   `/etc/sysctl.d/99-fluxbee-egress.conf` existed, and `state: open`. The routes and values the
   worker had before it first went behind an egress are carried over by later switches. Then it
   pauses the watchdog's egress step.
2. **Switch** (5 s). `ip route replace default via <gateway_ip>`, as the boot does
   (`:11191-11200`). Then the IPv6 block: the worker's sysctl file and `sysctl --system`
   (`:11202-11206`). The block is judged by what the kernel reads back (`ipv6_disabled_now`,
   `:10852`), as the egress judges it (`:11072-11085`), not by the exit code of `sysctl --system`.
3. **Check** (12 s).
   - The probe: an HTTPS GET of `reachability_url` (`check_internet_reachable`, curl with 5 s,
     `:10721-10732`), up to two attempts 1 s apart, so that a cold ARP entry or DNS cache is no
     failure. With IPv6 off it can only go out by IPv4, through the egress.
   - A failed probe carries curl's reason (no DNS, no connection, a timeout), which
     `check_internet_reachable` drops today.
   - Then G2's lookups again: they must be unchanged.
4. **Confirm** (15 s). The worker answers `pending_confirm`, with the probe and the route it sees,
   and waits for the motherbee's confirm: a second `SYSTEM_EGRESS_ROUTE` carrying the switch id.
   - Its answer reaching the motherbee proves the way from the worker on the new route. The confirm
     reaching the worker proves the way back.
   - This is the uplink check: a change that stays only once confirmed, as `netplan try` does.
   - So the worker never keeps a switch the motherbee did not see.
5. **Commit** (2 s). It writes the section into hive.yaml, and this write is the commit.
   - Atomically: a temp file and a rename (`write_file_atomic`, `:26570`), changing only `egress`
     and keeping every other key.
   - Then it sets its in-memory section, marks the record `committed`, resumes the watchdog, and
     answers `ok`.
6. **Revert** (5 s), on a failure in steps 2 to 5, or when no confirm comes in time.
   - It restores the recorded default routes, the four IPv6 values, and the sysctl file as it was.
   - hive.yaml and the in-memory section are as they were: the commit never happened.
   - It marks the record `reverted`, with the step and the reason, and resumes the watchdog.
   - It answers, when the motherbee still waits: `EGRESS_PROBE_FAILED`, `EGRESS_IPV6_UNMANAGED`,
     `UPLINK_ROUTE_CHANGED`, `EGRESS_CONFIRM_MISSED` or `EGRESS_COMMIT_FAILED`.

**The bounds:**

| Step | Bound | Where |
| --- | --- | --- |
| The guards | 5 s | the worker |
| The switch | 5 s | the worker |
| The probe, and the lookups again | 12 s | the worker |
| `pending_confirm` answered | 22 s after the request | the worker |
| The confirm window | 15 s after that answer | the worker |
| The commit, or the revert | 5 s | the worker |
| The forward of the switch | 30 s | the motherbee |
| The forward of the confirm | 10 s | the motherbee |
| The whole action | 60 s (`admin_action_timeout`) | SY.admin |

When the forward of the switch times out, the motherbee sends no confirm. The worker then reverts by
itself when its window ends, and the answer says `not_answered`. If a guard ever misses, this is the
backstop: no confirm, no switch.

**Why this order: the watchdog reads memory, not the file.**

- The orchestrator reads hive.yaml's `egress` once, at its start (`egress: hive.egress.clone()`,
  `:972`, into `OrchestratorState.egress`, `:663`). The watchdog re-applies the route from that copy
  when it drifts, every 5 s (`:3120-3123`, `:3196-3220`).
- So writing the section into hive.yaml alone changes nothing until a restart. Removing it from the
  file alone does not stop the watchdog in this run, only at the next start.
- The design therefore puts `state.egress` behind a lock, set only at the commit, right after the
  file. A failure before the commit has nothing to remove. The clear (15.4) removes both: memory
  first, then the file.
- The watchdog's egress step skips while a switch is open. Otherwise, on a worker already behind
  another gateway, it would put the old route back within 5 s of the switch.

**An orchestrator that stops mid-switch.** At its start, before `reconcile_egress` (`:1739`), it
reads the record:

- `open`, and hive.yaml without the target section: the switch never committed. It restores the
  recorded routes and IPv6 values, marks the record `interrupted`, and says so in its journal and
  its drift alerts.
- `open`, and hive.yaml with it: the commit happened. It marks the record `committed`, and the boot
  applies the section as it does today (§7).
- A host reboot loses the kernel's route anyway; the record still brings the IPv6 values back.

**Connections open at the switch.** A connection that goes through the default route breaks: an AI
node's keep-alive to its provider, say. Its next packets leave through the egress' NAT with another
source. The node's next call opens a new connection. Run the action while the worker's agents are
idle.

### 15.4 The reverse action

**Proposed:** `clear_hive_egress`, `DELETE /hives/{hive}/egress`, with the same path, record and
confirm.

- It restores the default routes the worker had before it first went behind the egress, which the
  record keeps. On a worker with no record (one that joined after the declaration), the request
  names the gateway to go back to, and G1 checks it.
- The same guards. The probe is reported, but it does not revert: the way back may have no internet
  by design. The confirm still decides.
- The commit removes the section from memory, then from hive.yaml, and puts back the IPv6 values
  the record kept.

**Is it needed now? Recommended: yes.**

- The egress is a single point of failure (§11). Once the workers are behind it, an egress outage
  cuts every agent's provider. The clear is the way around it from Archi while the egress is fixed,
  and G2 keeps the mesh off the egress, so the clear always reaches the worker.
- The gate needs it, to run from zero and to leave 8.x as it found it.
- Without it, the way out is a hand edit on the box from its console: the section out of hive.yaml,
  a restart of `sy-orchestrator`, and the route by hand.
- It costs little next to the set: the same message, guards, record and confirm.

### 15.5 What the operator sees

- **The action's answer:** `status` (`ok`, `unchanged`, or `error` with `error_code` and the step),
  `hive`, `gateway_ip`, `previous_gateway`, `ipv6_blocked`, `internet_reachable` with the probe's
  URL and reason, `ssh_reachable_from`, `switch_id`, and `timings_ms` per step.
  - `not_answered` never means "not applied" (hive-config-update-v1 §7, P-13). The worker keeps
    the switch only if it committed; otherwise it reverts within 15 s of its answer. `/versions`
    says which.
- **`/versions`:**
  - Each worker gains `egress`: its section (`gateway_ip` and `reachability_url`, or null), the
    default gateway it sees (`observed_default_gateway`, `:10609`), `ipv6_blocked`, and
    `last_switch` from the record (id, operation, state, time, previous gateway, reason).
  - The motherbee adds its declaration, and compares each worker against it, as it compares the
    installation file (`add_installation_state_to_answer`, `:7209`). `egress.state` is `behind`,
    `not_behind` (A-219's case), `other_gateway` (the declaration changed since), `drifted` (the
    section is right, the route is not), or null where it does not apply.
  - This is what reports A-219's case, which nothing reports today.
- **The logs:**
  - The worker's journal: a line per step, and the outcome with its times.
  - SY.admin's command log (`GET /hives/<worker>/commands`; the path's hive filters it): the action
    and its outcome.
  - An interrupted switch: a drift alert in the worker's own file, which the motherbee does not
    read yet (FINDINGS A-175), and `last_switch.state: interrupted` in `/versions`.
- **Archi's text** (its handbook):
  - Before staging, it tells the operator what changes. The worker's agents reach the internet
    through the egress, which becomes their dependency. A call in flight at the switch may fail
    once. Afterwards SSH reaches the box only from what the answer names.
  - It stages the action only when the operator asks. When `/versions` shows a worker `not_behind`,
    it tells the operator, and never stages it on its own.
  - After the run, it reports from the answer and `/versions`. Each refusal comes with what it means
    and what the operator can do.

### 15.6 Every leg it touches

- **SY.orchestrator, the motherbee's:** the admin action and its checks; the two forwards, in a
  task of its own; the `egress.state` comparison in `/versions`.
- **SY.orchestrator, the worker's:**
  - `SYSTEM_EGRESS_ROUTE` on a worker only: the guards, the record, the switch in a task of its own,
    the commit and the revert, and the clear.
  - The start reading an open record.
  - `state.egress` behind a lock, and the watchdog's pause.
  - The hive.yaml writer: atomic, under one lock shared with `set_storage`. hive-config-update-v1 §5
    asks for it, and `persist_storage_path_in_hive` still writes in place (`:33096-33130`).
  - `/versions`' `egress`.
  - `remove_hive`'s cleanup of a worker removes its IPv6 file and the record. Today only an egress
    gets its teardown (`remove_hive_cleanup_full_script`, `:18254`), so a removed worker that went
    out through the egress keeps IPv6 off.
- **SY.admin:** the action's spec, route, help, example and function schema; `read_only: false`,
  `confirmation_required: true`, `executor_available: true`; `mesh_access` `Nobody`; not in
  `IO_CLOUD_EXPOSED_ACTIONS`; its timeout; the two refusals it owns; `classify_admin_action`
  (`SystemConfig`).
- **The policy:** `SYSTEM_EGRESS_ROUTE` in `PROTECTED_SYSTEM_ACTIONS`, in rule (3)'s
  `orchestrator_mutations`, and in `spoke_only_actions`; the baked wasm compiled again, which CI
  compares byte for byte (FINDINGS A-179); `classify_system_message`
  (`crates/fluxbee_sdk/src/policy.rs:145`).
- **Archi:**
  - The catalog needs nothing of its own: Archi reads SY.admin's.
  - Its wait for a plan grows with each `set_hive_egress` step (`architect_admin_action_timeout`,
    `src/bin/sy_architect.rs:13090`), as it does for `set_installation_ai`.
  - Its verdict reads each step's `status`, and `/versions` on `not_answered`.
  - The handbook: a subsection next to §4.4.4, and §3.2's paragraph on egress hives
    (`docs/archi/handbook_fluxbee.md:159`). That paragraph says workers get the route
    "automatically": only the workers that join after the declaration do.
- **Docs:** this spec (§7, §11, and §12's open box); the operations reference, where the action
  replaces the hand edit at `docs/reference/operations.md:218-219`; HANDBOOK §7; PB-8's last
  paragraph; hive-config-update-v1 §2 (the one exception) and §9; the node pages (sy-orchestrator,
  sy-admin, sy-architect); the protocol and routing-and-policy references (rule (3)'s tables).
- **Tests, in CI:**
  - The route-lookup reader against fixtures: on-link, through the default route, through a more
    specific route, a host name, IPv6.
  - The guards as a table.
  - The switch as a state machine against fakes: a failed probe reverts; no confirm in time
    reverts; the confirm commits, writing only `egress` and setting memory; a failed write reverts;
    a confirm after a revert answers `EGRESS_CONFIRM_MISSED`.
  - The watchdog skipping while a switch is open; the start with an open record, with and without
    the section; `unchanged`; the clear.
  - The motherbee: its checks and their codes; no confirm after a lost answer; the `/versions`
    comparison.
  - SY.admin: a test like `update_installation_is_the_operators_and_closed_to_the_mesh`
    (`sy_admin.rs:34109`): route, help, example, confirmation and executor; closed to every mesh
    caller; a timeout above the motherbee's bound.
  - The policy: the baked wasm's truth table for the new message.
  - Archi: the verdict of a plan with the step: ok, an error, `not_answered`.

**The gate on 8.x.** It measures every step's time, live and afterwards from each node's logs
(`ops timeline`). It is also the first live check of a worker behind the egress, §12's open box.

- **Before:**
  - The release on every hive.
  - The motherbee's `egress` holds `gateway_ip` 10.10.10.40 and a `reachability_url`, read on the
    box. Without the URL the action refuses. Choosing it is the operator's, and the motherbee reads
    its `egress` only at its start: a hand edit, and a restart of its orchestrator.
  - One snapshot of worker1, and AI.pruebas idle.
- **1. worker1 as it is:** default via 10.10.10.1, no `egress` in its hive.yaml, IPv6 on;
  `/versions` says `not_behind`.
- **2. Refusals that change nothing,** worker1's route checked after each:
  - `access_from: ["192.168.8.0/24"]`, the admin network, which worker1 reaches through
    10.10.10.1: `ACCESS_ON_DEFAULT_ROUTE`.
  - egress1, ingress1 and the motherbee: `NOT_A_WORKER`.
  - Archi's own `ADMIN_COMMAND`, and an SCMD: refused by the mesh.
- **3. The revert:**
  - On egress1, a throwaway table `inet gate0219` drops what egress1 forwards from 10.10.10.20.
  - The action answers `EGRESS_PROBE_FAILED`: worker1 is back on 10.10.10.1, IPv6 as before, no
    section. The revert's time is noted.
  - `gate0219` is removed.
- **4. The switch,** staged by Archi and confirmed by the operator:
  - The answer is `ok`, with its times.
  - On worker1: default via 10.10.10.40, IPv6 off, and the section in hive.yaml as the join's
    renderer writes it. `/versions` says `behind`.
  - worker1 stays connected throughout, and neither side's journal shows a WAN session lost at the
    switch.
  - Every node of worker1 still runs: no worker has run with IPv6 off on 8.x before.
- **5. Through egress1:**
  - On egress1, `gate0219` now counts what egress1 forwards from 10.10.10.20.
  - From worker1, `ip -4 route get 1.1.1.1` says via 10.10.10.40, and a GET of the probe's URL
    moves the counter.
  - AI.pruebas answers a message, and the counter moves on 443 during the call.
  - `gate0219` is removed.
- **6. Kept:**
  - A hand `ip route replace default via 10.10.10.1` on worker1 is undone within 5 s; the journal
    says the route drifted.
  - A restart of worker1's `sy-orchestrator`: the boot applies the section, and the probe logs
    `internet_reachable=true`.
  - The action again: `unchanged`, and nothing moves.
- **7. The way back** (if built): the clear puts worker1 on 10.10.10.1 with IPv6 as before; then the
  switch again. worker1 ends behind egress1, as the operator decided.
- **Not provoked on 8.x:**
  - An uplink lost after the switch: tested in CI against a missing confirm. Cutting worker1's
    uplink on purpose would test A-203 and A-207 (a WAN session that ends badly lingers), not this.
  - G1 and G2 refusing: tested in CI. On 8.x, egress1 and the motherbee are on worker1's only
    subnet, so both guards can only pass.

### 15.7 Open points for the operator

1. **The names.** `set_hive_egress` and `clear_hive_egress` (`PUT /hives/{hive}/egress` and
   `DELETE /hives/{hive}/egress`), with `SYSTEM_EGRESS_ROUTE` between the orchestrators. They follow
   `set_storage`, `set_node_config` and `set_installation_ai`, and the section's name in hive.yaml.
   Other names: `route_hive_through_egress`, `set_worker_egress`. **Recommended:** `set_hive_egress`
   and `clear_hive_egress`.
2. **Build the reverse now?** **Recommended: yes** (15.4). It is the way around an egress outage
   from Archi, the gate needs it, and it shares the set's machinery.
3. **How the operator's access is protected:**
   - (a) Check and refuse: the live SSH sessions and `access_from`. A management network that must
     stay off the egress gets a static route at the OS level from the operator, which the check then
     passes.
   - (b) Fluxbee keeps a route through the previous gateway for each `access_from` network, written
     into the section and kept by the watchdog.
   - (c) No check: the console is the access.

   **Recommended: (a).** Fluxbee owns no new route, the same line PB-8 draws for the motherbee, and
   nothing breaks silently.
4. **The uplink check, and its bounds.** **Recommended:** the confirm round trip (15.3, step 4),
   rather than the lookups alone, which prove the kernel's path but not the mesh; the probe twice at
   5 s, a 15 s confirm window, 60 s for the action.
5. **No `reachability_url` on the motherbee.** **Recommended:** refuse. A switch without a probe is
   the silence A-219 is about.
6. **A worker whose section names another gateway** (the declaration changed after its join or its
   switch). **Recommended:** the same action moves it, with the same guards; `/versions` says
   `other_gateway`.
7. **Where the operator sees it.** **Recommended:** `/versions`: the worker's `egress` and the
   motherbee's comparison. The hive registry would record what the motherbee did, not what the
   worker runs.
8. **The removal.** **Recommended:** `remove_hive` of a worker removes its IPv6 file and the record,
   in the same change.

**The operator's call (2026-11-10):** every point as recommended, *"sobre 1, 2, 3 y 4 hacelos como
recomendas"*, and the minor points as proposed.

### 15.8 As built (0.1.125)

Built as 15.1 to 15.6 say. What follows is what the build adds, or does differently.

**Where it lives.**

- SY.orchestrator (`src/bin/sy_orchestrator.rs`): the motherbee's part in `hive_egress_flow`
  (its checks in `prepare_hive_egress` and `hive_egress_registry_refusal`, the forwards in
  `hive_egress_exchange`, in a task of its own, `spawn_hive_egress`); the worker's in
  `egress_route_message`, `egress_switch_flow` (the guards in `egress_switch_guards`, the commit in
  `egress_commit`, the revert in `egress_revert`) and `egress_confirm_answer`; the start's reading of
  an open record in `resume_egress_switch_at_start`; `/versions` in `egress_versions_block` and
  `add_egress_state_to_answer`. The host is behind a trait (`EgressHost`), so the decisions are
  tested against a fake; `LiveEgressHost` runs each command under coreutils' `timeout`.
- SY.admin (`src/bin/sy_admin.rs`): `handle_hive_egress_command` and `hive_egress_worker_refusal`.
- The policy: `SYSTEM_EGRESS_ROUTE` in `PROTECTED_SYSTEM_ACTIONS`, `orchestrator_mutations` and
  `spoke_only_actions`, and in the orchestrator's `SPOKE_ONLY_SYSTEM_ACTIONS`; `policy/system.wasm`
  and `policy/system_route.wasm` compiled again; `classify_system_message` and
  `classify_admin_action` say `system_config`.

**The message.** `SYSTEM_EGRESS_ROUTE`'s payload is `{target, operation: set | clear, phase:
switch | confirm, section_yaml (a set's), gateway_ip (a clear's, optional), access_from,
switch_id (a confirm's)}`. The worker makes the switch id when it takes the switch.

**The route lookups** are `ip -4 route get fibmatch <address>`: the route that decides, with its
prefix, so "through the default route" is the matched route being `default`, never a guess from a
`via` (a more specific route may go through the same gateway). An `iproute2` without `fibmatch`
(before 4.13) cannot be read, and the guards refuse.

**The answers.**

- Every answer carries `outcome`: `switched`, `cleared`, `unchanged`, `refused` (nothing changed),
  `reverted`, or `not_answered`.
- `unchanged` is `status: ok` with `outcome: unchanged`: SY.admin and the executor read any other
  status but `ok` as an error.
- `not_answered` is `status: error`, `error_code: EGRESS_NOT_ANSWERED` (HTTP 504), with the step
  whose forward got no answer (`switch` or `confirm`).
- Codes 15.1 to 15.3 do not name: `EGRESS_GUARD_UNREADABLE` (a lookup, sshd's ports or hive.yaml
  could not be read within the guards' 5 s; 503), `EGRESS_SWITCH_FAILED` (the route command
  failed), `EGRESS_RECORD_FAILED` and `EGRESS_RECORD_UNREADABLE`, `HIVE_UNREACHABLE` (503, nothing
  sent), `PREVIOUS_GATEWAY_UNKNOWN` (a clear on a worker whose record keeps no route from before the
  egress, and no `gateway_ip`), `EGRESS_SWITCH_NOT_FOUND` (a confirm for a switch the worker does
  not know). A record the start finds open and undoes says `interrupted`, with
  `EGRESS_SWITCH_INTERRUPTED`.
- The refusals before anything changes are 409, a failed step after the switch 502, and
  `EGRESS_CONFIRM_MISSED` 504.

**The checks.**

- The motherbee's orchestrator reads its registry too (`NOT_A_WORKER`), and the worker's refuses on
  any role but worker. A body with another field than `access_from` (and a clear's `gateway_ip`) is
  refused (`INVALID_REQUEST`).
- SY.admin also refuses while a task of the installation's update acts on the worker (one that may
  send it a core update; a read holds nothing back), with `INSTALLATION_UPDATE_RUNNING`. It holds
  the explicit run's mark while the action runs, as `set_installation_ai` does, so an
  `update_installation` started meanwhile is refused, naming it.
- G2 refuses a worker whose hive.yaml names no `wan.uplinks` address, and an IPv6 uplink, which the
  IPv6 block would cut.
- G3's refusal names every session and network at once, the current gateway to route each through,
  and where SSH still reaches the box from.

**The switch and the revert.**

- `ip route replace default via <gateway_ip>`, as the boot, replaces the default route of the same
  metric. The revert replaces each recorded line again (the flags `ip route` only prints dropped),
  then deletes the default routes through the gateway it leaves that the record does not hold; a
  default route through another gateway (a DHCP lease's) is left alone.
- The record keeps the sysctl file's content, not only whether it existed.

**The clear.**

- A worker with no egress section answers `unchanged`.
- With a record, a `gateway_ip` other than the record's is refused: the clear puts the record's
  routes back.
- Without a record, the IPv6 block's file is removed and the live values stay until the box's next
  boot; `notes` says so.
- Its probe GETs the worker's own `reachability_url`, and decides nothing.

**`/versions`.** A worker's `egress` also says `switch_open`, and `last_switch_error` when its record
cannot be read; the motherbee adds `declared_gateway_ip` beside `state`. `state` compares the
gateway only: a section whose `reachability_url` differs from the declaration's still reads
`behind`.

**The hive.yaml writer.** One lock in the process (`lock_hive_yaml`), taken by the commit and by
`set_storage`. `set_storage`'s own atomic write is A-224's (0.1.125, `coa/orch-125`).

**The removal (A-225).** A removed worker's cleanup removes `/etc/sysctl.d/99-fluxbee-egress.conf`
and the record, once its services are stopped, and runs `sysctl --system`. IPv6, and the default
route its network configuration gives it, come back at the box's next boot: the live values are not
reset.

**A-225's other two items.** The worker's boot judges its IPv6 block by what the kernel reads back,
`sysctl --system`'s error only logged, so its probe always runs; and every probe says why it failed
(`check_internet_reachable` answers curl's reason: no DNS, no connection, a timeout, an HTTP error,
TLS), in the journal of the egress, the worker and the motherbee, and in the switch's answer.

**Archi.** Its wait for a plan grows 70 s per `set_hive_egress` or `clear_hive_egress` step
(`EXECUTOR_EGRESS_SWITCH_WAIT_SECS`). A step that ends `EGRESS_NOT_ANSWERED` fails the run, as any
error does: no code of Archi's reads `/versions` by itself; its handbook (§4.4.5) tells it to read
`GET /hives/<hive>/versions` and report `egress.state` and `last_switch`.

**Docs left for later.** hive-config-update-v1 §2 and §9 (the one exception to "a worker's `egress`
never travels") are not edited here: `coa/orch-125` rewrites those sections for A-224 in the same
release.
