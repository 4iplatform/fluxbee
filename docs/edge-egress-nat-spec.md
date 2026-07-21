# Fluxbee — Edge Egress NAT Specification

**Status:** v1.0 — IMPLEMENTED (was "implementation-ready")
**Date:** 2026-06-08

> **Actualización 2026-07-21:** the Mode A egress role is BUILT in `src/bin/sy_orchestrator.rs`:
> `HiveRole::{Motherbee,Worker,Egress}`, the `add_egress_hive_flow` provisioning path, and the
> self-contained `table inet fluxbee_egress` NAT/forward reconciliation (sysctl + nftables, applied
> and drift-reconciled) all exist. A single-NIC guard also shipped: `resolve_egress_nat_config`
> rejects `wan_iface == lan_iface`. The design-tense wording below (§6 "currently binary", §6.5
> "does not exist today", the §12 unchecked checklist) predates this and no longer reflects the code.
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

A single declared parameter (`lan_cidr`) drives derivation. The egress IP defaults to the first usable address of the CIDR but is overridable for dev environments where the internet-facing path is not `.1`.

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
  edge_ip: "192.168.8.1"              # optional; default = first usable IP of lan_cidr
  wan_iface: "eth0"                   # interface toward the internet
  lan_iface: "eth1"                   # interface toward the LAN
  ipv6: "blocked"                     # only "blocked" supported in v1

nats:
  mode: embedded
  port: 4222

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
| `egress.edge_ip` | no | first usable IP of `lan_cidr` | The gateway's LAN address. Overridable for dev |
| `egress.wan_iface` | yes | — | Internet-facing interface name |
| `egress.lan_iface` | yes | — | LAN-facing interface name |
| `egress.ipv6` | no | `"blocked"` | Only `"blocked"` accepted in v1 |

The egress hive does **not** touch its `wan` section to provide egress. WAN is Fluxbee protocol connectivity to motherbee; egress NAT is OS-level network plumbing. They coexist without interaction.

### 5.2 Motherbee `hive.yaml` egress declaration

Motherbee declares which gateway workers should use. This drives route injection into new workers (§7). Works identically for Mode A and Mode B.

```yaml
# hive.yaml of motherbee (NEW section)
egress:
  gateway_ip: "192.168.8.1"      # where workers send their default route
  edge_hive: "edge-1"            # optional; informational, the egress hive_id (Mode A only)
```

| Field | Required | Notes |
|-------|----------|-------|
| `egress.gateway_ip` | yes (to enable injection) | IPv4 of the egress gateway. If absent, `add_hive` does not inject any egress route |
| `egress.edge_hive` | no | The egress hive's `hive_id` for inventory clarity. Absent in Mode B |

### 5.3 New Rust structs in `sy_orchestrator.rs`

Add to `HiveFile` (currently line 132):

```rust
#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    role: Option<String>,
    wan: Option<WanSection>,
    nats: Option<NatsSection>,
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
    edge_ip: Option<String>,       // optional, derived if absent
    wan_iface: Option<String>,     // required when enabled (egress hive)
    lan_iface: Option<String>,     // required when enabled (egress hive)
    #[serde(default = "default_ipv6_policy")]
    ipv6: String,                  // "blocked"
    gateway_ip: Option<String>,    // used on motherbee for worker injection
    edge_hive: Option<String>,     // informational
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

   **Source of the egress params (added v1.0):** the host-specific NAT params (`lan_cidr`, `wan_iface`, `lan_iface`, optional `edge_ip`/`ipv6`) come from the **`add_hive` command payload**, under an `egress` object. They cannot come from motherbee's `hive.yaml`: interface names are host-specific and unknowable in advance. Motherbee validates them at request time (reusing `resolve_egress_nat_config`) and derives `edge_ip` when omitted. This contrasts with the worker-side `egress.gateway_ip`, which **is** declared once in motherbee's `hive.yaml` (§5.2) because it is global to the deployment. Example payload:

   ```json
   {
     "hive_id": "edge-1", "address": "192.168.8.1", "role": "egress",
     "egress": { "lan_cidr": "192.168.8.0/24", "wan_iface": "eth0", "lan_iface": "eth1" }
   }
   ```

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

The orchestrator already has a firewall helper pattern (`open_firewall_rules_local` line 3331, `close_firewall_rules_local` line 3391, `ensure_core_firewall_local` line 3473). The egress NAT reconciliation should follow the same idempotent, marked-block approach but target **nftables** directly (§8), not the existing simple `ufw allow <port>` calls.

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
3. Verify internet reachability through the gateway (ping `fluxbee.ai`, §9).
4. Report verification fields (§9).

Workers provisioned **before** the egress declaration do not carry `egress.gateway_ip` in their yaml and are not retroactively changed by a later `add_hive`; re-emitting their yaml to add the field is an explicit operator action (out of scope for v1, noted in §11).

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

On a `role: egress` host, **nftables is the single firewall backend for both inbound and egress**. The orchestrator does not use `ufw` on egress hosts: the inbound port rules (WAN/identity) that other roles open via `ufw` are instead expressed in the same Fluxbee-owned nftables ruleset on the egress host. This avoids two backends managing forwarding/inbound policy on the same host, and the contradiction of disabling `ufw` while still depending on it for inbound ports.

`nft` must be present on an egress host; if absent, provisioning fails loud (no silent fallback to ufw/iptables). Motherbee and worker hosts are unaffected and keep using their existing `ufw`/firewalld inbound path.

---

## 9. Verification

After provisioning, the orchestrator reports explicit fields. No silent success.

For an egress hive:

```json
{
  "egress_role": "egress",
  "egress_nat_applied": true,
  "egress_ipv4_forwarding": true,
  "egress_ipv6_blocked": true,
  "egress_conntrack_tuned": true,
  "egress_wan_iface": "eth0",
  "egress_lan_iface": "eth1",
  "egress_internet_reachable": true
}
```

For a worker receiving the route:

```json
{
  "egress_configured": true,
  "egress_gateway_ip": "192.168.8.1",
  "egress_ipv4_ready": true,
  "egress_ipv6_blocked": true,
  "egress_internet_reachable": true
}
```

`egress_internet_reachable` is obtained by an ICMP ping to `fluxbee.ai`, with a fallback HTTPS `GET https://fluxbee.ai` when ICMP fails. The HTTPS leg avoids the false-negative where a network filters ICMP but allows 443 (the path egress actually uses), and doubles as a liveness check of the Fluxbee site/cloud. (Public-IP discovery via an IP-echo endpoint is deferred to a later, fuller verification once the complete system is in place.) If IPv4 works but IPv6 is not blocked, the result warns or fails per mode.

---

## 10. Relationships

### 10.1 With WAN / `RT.gateway`

Egress NAT does not touch the `wan` section or `RT.gateway`. WAN is Fluxbee protocol connectivity between hives over sockets; egress NAT is OS-level IP forwarding to the internet. They are orthogonal layers on the same host and coexist without interaction.

### 10.2 With NATS

Egress to the internet does not use NATS. An AI node calling OpenAI opens HTTPS directly; the OS routes it through the egress gateway. NATS configuration is not coupled to egress.

### 10.3 With `RT.edge` (ingress, future)

`RT.edge` is the future public ingress runtime (`edge-control-protocol.md`). It handles HTTPS termination, TLS material, JWT validation, tenant cache, and HTTP→L2 translation. It is a **separate responsibility** from egress NAT.

The two MAY run on the same host or on different hosts. Running both on one host concentrates trust (internet-facing ingress + LAN NAT + the secrets RT.edge holds). For production, separating them onto different hosts is recommended; for beta, the same host is acceptable. The egress spec does not require or assume co-location either way. When `RT.edge` is introduced, its role profile (identity replica, OPA, etc.) is additive and does not change the egress role defined here.

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
| Reconcile existing workers | Operator action to re-apply route to workers provisioned before egress declaration | v1 only injects into new workers |

The v1 nftables `forward` chain (§8.2) is structured so the allow-list and logging insert cleanly: the allow-list narrows the LAN→WAN accept rule, and logging adds `log` statements on the same chain. No restructuring required to add them later.

---

## 12. Implementation Checklist

### Orchestrator — role plumbing (mechanical)

- [ ] `is_egress_role()` helper
- [ ] Accept `egress` at startup role gate (lines 503–512)
- [ ] `EgressSection` struct, add `egress` to `HiveFile`
- [ ] Add `egress` to `SystemNodesSection`
- [ ] `HiveRole` enum; refactor `system_nodes_for_role` to select by role
- [ ] Update all `is_motherbee` call sites feeding `system_nodes_for_role` / `validate_system_nodes`
- [ ] `validate_system_nodes`: skip vault checks for non-motherbee; extend "no sy-vault" to egress
- [ ] Parameterize `render_system_nodes_yaml` to emit the role key
- [ ] Egress hive `hive.yaml` generation in `add_hive_flow`

### Orchestrator — network reconciliation (core work)

- [ ] sysctl writer for `/etc/sysctl.d/99-fluxbee-egress.conf` + `sysctl --system`
- [ ] nftables ruleset writer (marked, idempotent block) with CIDR/iface substitution
- [ ] nftables apply + verify ruleset loaded
- [ ] conntrack tuning (sysctl + modprobe option)
- [ ] UFW detection: warn/refuse or disable-on-confirm
- [ ] Egress hive verification payload
- [ ] Worker default-route injection from motherbee `egress.gateway_ip`
- [ ] Worker IPv6 block (sysctl disable + accept_ra=0)
- [ ] Worker verification payload, `EGRESS_IPV6_UNMANAGED` path

### Validation / tests

- [ ] `role=egress` passes `validate_system_nodes` with `SY.config.routes`-only profile
- [ ] `role=egress` rejected if it lists `sy-vault`
- [ ] Idempotent re-apply of nftables/sysctl produces no drift
- [ ] Worker reaches internet through gateway; IPv6 confirmed blocked
- [ ] Mode B: workers receive route from `gateway_ip` with no egress hive present

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

1. **Egress profile floor**: this spec sets the minimal profile to `SY.config.routes` only (plus `RT.gateway` and `SY.orchestrator` implicit in being a hive). Confirm during implementation that the local router boots cleanly with only `SY.config.routes`. If the router needs another SY node to register/initialize, add it and update §5.1.
2. ~~**`edge_ip` derivation**~~ — **resolved**: "first usable IP" = `(network & mask) + 1`, computed with std `Ipv4Addr ↔ u32` bit-math; valid for any mask (no CIDR crate needed). `edge_ip` stays overridable, so config is unchanged either way.
3. **nftables include mechanism**: dedicated `table inet fluxbee_egress` (self-contained, recommended) vs include file merged into the host's main ruleset. The dedicated table is cleaner for idempotent management; confirm no conflict with any existing host nftables policy.
4. ~~**IP-echo endpoint for `egress_public_ip`**~~ — **resolved**: v1 verification is an ICMP ping to `fluxbee.ai` (`egress_internet_reachable`), confirming reachability only. Public-IP discovery via IP-echo is deferred to the fuller verification of the complete system.
5. **conntrack defaults**: 262144 / hashsize 65536 are starting points. Tune to deployment scale.
