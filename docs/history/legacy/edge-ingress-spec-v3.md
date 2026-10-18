# Fluxbee — Edge / INGRESS Specification v3

**Status:** v3.0 alpha — design, supersedes the v1/v2 draft trio
**Date:** 2026-07-02
**Supersedes / merges:** `rt-edge-spec.md`, `fc-edge-manager-spec.md`, `edge-control-protocol-v2.md`
**Audience:** SY.edge + orchestrator + core-ingress developers
**Related:** `02-protocolo.md`, `05-conectividad.md`, `10-identity-v2.md`, `sy-vault-spec.md`, `edge-egress-nat-spec.md`

Legend used below: **✅ CLOSED** (decided, the fluxbee way) · **⚠️ TRADEOFF** (decided with a known cost) · **🔶 COMPLEX** (flagged, revisit before building it).

---

## 0. What changed from the v2 trio, and why

The v2 trio modelled the edge as a runtime on separate **Fluxbee Cloud** infrastructure, controlled by a bespoke **FC.edge-manager** over a bespoke **WSS control protocol**, with credentials "in SY.vault" that FC (being off-mesh) had no wire to reach. Reviewing the specs against the code exposed that separation as the source of nearly every hard problem (unimplementable vault custody, a whole second control protocol, a JWT-user-provisioning gap).

v3 collapses it:

| v2 | v3 | Why |
|----|----|-----|
| Edge = runtime on separate Cloud infra | **Edge = a fluxbee hive** (role `Ingress`, like `Egress`) | It already fits the mesh; nothing new to invent |
| FC.edge-manager = separate control-plane process | **Orchestrator + SY.vault on motherbee** distribute over the mesh | The orchestrator already issues+distributes per-hive mesh certs at `add_hive` |
| Bespoke WSS control channel (`edge-control-protocol-v2`) | **RETIRED.** Control = mesh (motherbee→edge) + SY.admin/heartbeat API (Cloud→motherbee) | ✅ nothing special needed a bespoke channel — see §11 |
| Certs "in vault" but FC can't reach vault | **Certs in vault, orchestrator distributes, edge caches** (§7) | Edge is on the mesh; vault access is normal |
| Public URL `/x/<tenant>/<ilk>` (leaks identity) | **Opaque hash** `/e/<hash>` and `/b/<hash>` (§4) | No identity leak; hash = capability |
| User-JWT everywhere | **API/shared-secret first; Fluxbee Cloud enters m2m, no login** | The publish-an-endpoint flow needs no user login |

**One-line model:** an internet-facing fluxbee node (SY.edge) is a **reverse proxy** that maps an opaque public URL to an internal `ilk` and forwards the request **under the target's own family**, letting the mesh do the rest. Identity, resolution, and secrets stay behind it, inside fluxbee.

---

## 1. Topology (✅ CLOSED)

```
        Internet                          Fluxbee mesh (mTLS)
           │                                     │
           ▼ :443 (TLS)                          │
   ┌──────────────────┐  internal NIC   ┌────────┴────────┐
   │   EDGE HIVE       │  (mesh mTLS)    │    MOTHERBEE     │
   │                   │ ───WAN inward──▶│                 │
   │  SY.edge  (node)  │                 │ rt-gateway (L3) │◀── resolves dst_ilk→handler (§6)
   │  rt-gateway       │                 │ sy-orchestrator │◀── issues+distributes
   │  SY.config.routes │                 │ SY.vault        │    certs/keys (§7)
   │  sy-orchestrator  │                 │ SY.identity     │
   │  (minimal SY set) │                 │ target handlers │◀── the published SY/IO nodes
   └──────────────────┘                 └─────────────────┘
```

- The edge is a **hive**, role `Ingress`, provisioned by `add_hive` exactly like a worker/egress hive. Reduced SY set: `rt-gateway` (router) + `SY.config.routes` + `sy-orchestrator` + the `SY.edge` node. **No SY.identity, no vault, no full blob store on the edge.**
- **Two NICs.** SY.edge binds `:443` on the **internet** NIC. The hive's `rt-gateway` speaks WAN mTLS to motherbee on the **internal** NIC. Public traffic is terminated by SY.edge and re-injected as L2; it **never touches the mesh transport directly**. Mesh stays outbound-only (the edge initiates the WAN connection inward).
- **Bootstrap is over the internal NIC only** (motherbee `add_hive` → edge internal IP). The public NIC only ever serves `:443`.
- The "local edge router / RT.gateway" of the v2 drafts is **not a new component** — it is the normal fluxbee router binary (`src/main.rs` logs itself as `rt-gateway`).

---

## 2. SY.edge — what it is and isn't (✅ CLOSED)

SY.edge is a normal fluxbee node (attaches with `NodeConfig{name:"SY.edge@<edgehive>", router_socket}` → `RouterDispatcher::connect_with_retry`, exactly like `nodes/io/io-api`). It is essentially a **public, hardened `io-api`**.

**Does:** TLS termination; hash→ilk reverse-proxy lookup; per-endpoint auth (§5); rate-limit; forward under the target's declared family (§3); serve published blobs (§8); hold its own TLS cert + the JWT **public** key (cached from vault via the orchestrator).

**Does NOT:** run SY.identity; resolve `ilk → handler node` (core does, §6); mint any key (validates with public key only); hold any crown-jewel secret; talk to other edges; serve HTML/UI.

**Vault footprint ≈ zero.** The only secret it holds is its own `:443` TLS key (cached locally, pushed from vault). It never reads signing keys. → a compromised edge cannot sign JWTs or read other tenants' secrets. (The signer lives on motherbee, §7.)

---

## 3. HTTP → L2: forward under the target's own family (✅ CLOSED — Option A)

> **RETIRED — `http.req` / `http.res`.** An earlier draft of this section defined a bespoke `http.req` / `http.res` L2 family. That was a **mutation**: a parallel HTTP protocol every handler would have to learn or a shim would have to translate. It reinvented what io-nodes already do (speak an existing family over the router). **Deleted.** The edge does **not** invent a request family — it forwards under the family the target already speaks (**Option A**).

Reuses the existing wire types `fluxbee_sdk::protocol::{Message, Meta, Routing}` — no protocol change.

**Forward** — after TLS + hash lookup + door-auth (§5.2), SY.edge builds one normal mesh message. It is **family-agnostic** (never parses HTTP semantics into a schema) and does **NO request-time resolution** — the v2 principle (§2.3/§2.4 of `edge-control-protocol-v2.md`): *everything needed at request time is already cached in the endpoint row*. The `dst_ilk → handler_node` resolve happened **once, at publication time, at core** (§5.1/§6); the edge just reads the cached name and forwards by name:
- `routing.dst = Destination::Unicast(<handler_node>)` — the **pre-resolved** handler L2 name from the endpoint row (e.g. `AI.handler@motherbee`). Name-based delivery forwards cross-hive via LSA → `ForwardHive`, which **works from the identity-less edge** (verified: cross-hive routing is name-based; `dst_ilk` never travels in LSA and cannot be resolved on a hive without `SY.identity`).
- `meta.msg_type = <inbound_family>` — the family the target declared when it published (§5.1). The node receives the request in **its own language**.
- `meta.dst_ilk = <ilk from the endpoint row>` — carried for the handler's own info/authz; **routing does not depend on it**.
- `meta.src_ilk = <the edge's own baked-in system ilk>` (deterministic, *a fuego*). **The edge never resolves external identity.** Any external principal (when authenticated via shared-secret, §5.2) rides as opaque `meta.context`, never as a minted ilk. External-caller identity is arranged out-of-band by Fluxbee Cloud.
- `routing.src_l2_name = SY.edge@<edgehive>` — REQUIRED so the handler's reply routes back cross-hive **by name**.
- `routing.trace_id = <fresh uuid>` — the wire correlation key.
- `payload` = the request body, passed through (JSON when the content-type is JSON, else an opaque UTF-8/base64 string). Request metadata (`method`, subpath, `query`) rides in `meta.context`.

**Reply** — the edge awaits the target's reply correlated purely by `trace_id` (`send_with_matcher`, `RouteMatch::Any` success + a transport-error guard, timeout 30 s) and returns the reply payload as the HTTP response body (`200` on success). It does **not** require the handler to speak any response family. Transport failures map via the existing `RpcError → HTTP` table (§10): `Unreachable`/`TtlExceeded → 502`, `Timeout → 504`.

**Door guard:** the public HTTP path may only target handler_nodes present in the edge's registry; no URL, header, or body field can select an arbitrary `routing.dst`.

### 3.1 Correlation (✅ CLOSED — resolves the v2 conn_id/trace_id confusion)

- **`trace_id` is the ONLY wire correlation key.** The SDK dispatcher keys its pending-waiter map strictly on `routing.trace_id` (`crates/fluxbee_sdk/src/rpc.rs` `send_with_matcher`) and rejects duplicate active trace_ids.
- The edge keeps a **bounded** `trace_id → (held client connection, deadline)` map; mints a unique `trace_id` per request; awaits the reply with `send_with_matcher` correlated strictly by `trace_id` (timeout 30 s; terminal on `UNREACHABLE`/`TTL_EXCEEDED`). Same dispatcher mechanism as `nodes/io/io-sim`. The reply's family is whatever the handler speaks — the edge matches on the trace, not on a fixed response type.

### 3.2 Body handling (✅ CLOSED for alpha)

- **Inline only, hard cap 64 KiB computed on the FULL envelope bytes, before `write_frame`, `413` above.** Non-negotiable from day one: the router frame limit is **128 KiB and an oversized frame tears down the whole node socket** (dropping *every* in-flight request, not just the offender), and the SDK's `text/v1` auto-spill does NOT fire for a `type`-based family. 64 KiB leaves headroom for the envelope.
- **No blob-in-body for the alpha.** Large payloads and file transfer go through the blob egress route (§8), not the request path. Non-UTF-8 body is base64; UTF-8 is passed through as-is.

### 3.3 Sync model (✅ CLOSED)

Request/response, client held open keyed by `trace_id`. Inward-response timeout 30 s → `504` + drop the pending entry. Idle client 60 s → close. Streaming / SSE / WebSocket-upgrade → `501` (fluxbee has no streaming inter-node contract). 🔶 streaming is a real future gap, declared now so it doesn't surface as a bug.

---

## 4. The public door — opaque hash URLs (✅ CLOSED)

```
https://<edge>.fluxbee.ai/e/<HASH>[/extra/path]   → invoke a published endpoint
https://<edge>.fluxbee.ai/b/<HASH>                → fetch a published blob (§8)
```

- **`<HASH>` is a random opaque capability** (≥128-bit). It carries no tenant/ilk in the clear (unlike the v2 `/x/<tenant>/<ilk>`). Possession of the URL is the first gate.
- **Separate prefixes** `/e/` (invoke) and `/b/` (blob) — distinct handling, one shared capability namespace lookup.
- `[/extra/path]` travels as `meta.context.path`; the target ilk never sees the mount/hash prefix.
- **Revoke = delete the registry entry.** Rotation = new hash for the same `(tenant, ilk)`.
- Registry entry: `hash → { tenant_id, ilk, inbound_family, auth_mode, methods, rate_policy, lease_until }`.

---

## 5. Publication + entry auth (✅ CLOSED)

### 5.1 Publication — a named contract, authorized at core, delivered to the edge as config

An IO node that wants a public endpoint **self-publishes** with a small named contract (not an HTTP mutation) over the normal mesh:

```
meta.type = "system", meta.msg = "EDGE_REGISTER"
payload = { ilk, inbound_family, auth_mode, methods, secret_ref?, lease_seconds }
```

- **`inbound_family` (Option A)** — the `msg_type`/subject the node already speaks. The edge forwards every external request to this ilk under **exactly this family** (§3). This is what lets the edge stay dumb and identity-free: it never learns a per-node protocol, it just labels the message with the family the node declared.
- **Self-publication, authorized at core — never on the edge.** The edge has no identity and cannot authorize a publication. Validation happens where identity lives (router L3 authority, §5.1.1): the router stamps the authoritative `src_l2_name`, and `payload.ilk` must resolve to `handler_node == src_l2_name`.
- **Delivery to the edge reuses `NODE_CONFIG_SET` — and carries the pre-resolved handler name (Z).** The core authority that owns the master endpoint table runs at core **with identity SHM**, so it resolves `ilk → handler_node` **once, at publication**, and pushes each row — `{ hash, ilk, handler_node, inbound_family, auth_mode, methods }` — to the edge as a node-config `endpoints` section (`apply_mode: replace`, versioned), the same mechanism `sy_config_routes` uses for `routes`. **The edge caches the resolved `handler_node` and forwards by name (§3), doing no request-time resolution.** It mints no hashes and authorizes no publications; when a handler moves, the authority re-pushes the row (v2 cached-state-with-refresh, §2.4).
- Lease + refresh (publisher refreshes before expiry; `EDGE_UNREGISTER` or lapse removes after a grace). Re-publishing the same `(ilk, subpath)` is idempotent and preserves the URL.
- 🔶 **Where the master table / authority lives** is open: the orchestrator (already admin-privileged, already runs `add_ingress`) vs. a dedicated publication node. Alpha leans orchestrator.
- 🔶 Publishing an ilk you do NOT own (a manager on behalf of another node) needs an explicit cross-ILK authz check — deferred; alpha is self-publication only.

### 5.1.1 Publication authority — at core, not the router (deferred for alpha)

Self-publication (`EDGE_REGISTER`) must be authorized where identity lives — at **core**, by the authority that owns the master table (§5.1). The check: the requesting node's router-stamped `routing.src_l2_name` must match the `handler_node` of the `payload.ilk` it is publishing (self-publication only). Because that authority runs at core with identity SHM, it does the check and the `ilk → handler_node` resolve in one step, then pushes the row to the edge (§5.1, Z).

**No router change is required for routing** — Z routes by the cached `handler_node` name (§6), so the v2-draft idea of "a direct L3 `dst_ilk` resolver in the router" is **not built** (`src/router/mod.rs` untouched). The one hardening item that remains is authoritative `src_ilk` stamping for `EDGE_REGISTER` (the router today leaves `meta.src_ilk` sender-supplied); that is **deferred** — the alpha table is operator-pushed / pre-seeded, so no live self-publication authz runs yet. 🔶 Cross-ILK publication (a manager on behalf of another node) needs a separate rule — deferred.

### 5.2 Entry auth modes (per endpoint, set at register)

| `auth_mode` | Edge validates | Use |
|-------------|----------------|-----|
| `public` | the hash only | open endpoints |
| `shared-secret` **(preferred)** | hash + a bearer/HMAC the edge checks against `secret_ref` | **webhooks, machine-to-machine, Fluxbee Cloud** |
| `jwt` 🔶 | user JWT (EdDSA) vs the cached public key | later, only if a web-user UI appears |

- **`tenant_id` is the SCOPE (rate-limit, ownership), NOT the entry credential.**
- **Alpha:** `public` + `shared-secret`. **No user-JWT.** Fluxbee Cloud enters in **API mode** (`shared-secret`), there is no user login in this model.
- 🔶 `shared-secret` is the *preferred* mode but its exact shape (static bearer vs per-request HMAC-of-body à la Stripe, where the secret lives, rotation) needs a security pass before hardening.

### 5.3 MEJORA — shared-secret alpha contract

The walking skeleton depends on `shared-secret`, so it cannot stay abstract during implementation. Pick one alpha contract before coding SY.edge. Recommended alpha: static bearer secret stored in the endpoint registry row, checked as `Authorization: Bearer <secret>`, rotated by re-registering or updating the row. HMAC-of-body is stronger for webhooks and should be the hardening target, but it requires canonical body bytes, timestamp tolerance, replay cache, and rotation semantics.

---

## 6. Routing: resolve once at publication, forward by name (✅ CLOSED — Option Z)

The `dst_ilk → handler_node` resolve needs identity SHM, which the edge hive does not have. Rather than resolve per-request at the edge (impossible) or add a resolve hop at core, the resolve is done **once, at publication time**, by the core authority (which has identity), and the resulting **handler name is cached in the edge's endpoint row** (§5.1). This is the v2 principle: *no per-request lookups; everything needed at request time is cached locally* (`edge-control-protocol-v2.md` §2.3/§2.4).

At request time the edge forwards by **name**: `routing.dst = Destination::Unicast(<handler_node>)`. Name-based delivery is what the mesh already does cross-hive — the edge's LSA FIB turns `handler@<homehive>` into `FibNextHop::Hive → ForwardHive →` the WAN uplink to core, and core delivers locally (or forwards on by name if the handler lives on a third hive).

**Why not `Destination::Resolve + dst_ilk` from the edge (verified in the router):** `Destination::Resolve` is a strictly *local-first* pipeline — Stage 1 (`dst_ilk → handler_node`) reads the **local** hive's identity SHM only, and `dst_ilk` is **never** advertised in LSA. The edge runs no `SY.identity`, so that resolve fails, falls through to OPA, and with no ingress OPA rule the router `OPA_NO_TARGET`-**drops** it — it does *not* forward to the home hive. **Z avoids that path entirely: no `Destination::Resolve` from the edge, no router change, no core resolve node.**

The reply routes back to the edge by the router-authoritative `routing.src_l2_name = SY.edge@<edgehive>` (same `ForwardHive` return leg as the SY.identity join), correlated by `trace_id`.

**Consequence — the router stays untouched.** Because the edge routes by name, ingress needs no new router L3 behavior; `src/router/mod.rs` is not modified. Caveat: when a handler moves (its `handler_node` changes), the cached row is stale until the core authority re-pushes the `endpoints` config — the accepted v2 cached-state-with-refresh model (§2.4).

🔶 Multi-hop cross-hive (handler on a hive other than motherbee) relies on LSA flooding the handler name to the edge via core; confirm on the first end-to-end run for non-motherbee handlers. For the alpha (handlers on motherbee) it is a single hop.

---

## 7. Cert & key custody — in vault, distributed by the orchestrator (✅ CLOSED + ⚠️)

**Target architecture:** all secrets live in SY.vault; the orchestrator (motherbee, vault-privileged) distributes; the edge caches locally.

MEJORA / revision general: the current code path for WAN mesh TLS does not yet use SY.vault as the source of truth. The orchestrator currently creates/persists the mesh CA on disk and distributes per-hive leaf certs / HMAC keys over the `add_hive` SSH bootstrap path. Keep that as the current implementation fact, and treat "vault owns public TLS/JWT/mesh material" as the desired custody model to align before production hardening.

| Secret | In vault | Pushed to edge? |
|--------|----------|-----------------|
| Public `:443` TLS cert **+ private key** | yes | **yes** (edge must terminate TLS at line rate; cached `0600`, hot-reloadable via `arc-swap`) |
| JWT signing keypair (**private**) | yes | **no** — signer stays on motherbee; only the **public** key is pushed |
| Mesh CA (`src/mesh_tls.rs` `MeshCa`) | yes | leaf only, as today |

- Rotation = rotate in vault → orchestrator re-distributes → edge hot-reloads. No edge downtime.
- **Alpha TLS cert = the existing real WILDCARD `*.fluxbee.ai`** (not self-signed), so testing is genuine.
  ⚠️ **TRADEOFF:** the wildcard is exactly what the v2 `C1` per-edge decision removed. A popped public edge leaks a cert valid for the **whole brand** (MITM any `*.fluxbee.ai`), not one host. Accepted as an operational-first alpha choice; **per-edge ACME (Let's Encrypt DNS-01) is the production target** (public CA required so external clients trust it out of the box; the internal `MeshCa` is a *separate* CA world — do not conflate). With secrets in vault, migrating wildcard→per-edge is a vault rotation + re-distribute, no architecture change.
- 🔶 ACME automation + DNS-provider client are net-new; they live on the **edge's own internet NIC** (which already has outbound internet) or are manual for alpha. They do not force a separate cloud.

---

## 8. Blob egress — `/b/<HASH>` (✅ CLOSED shape, 🔶 details)

Serving published blobs (file attachments, and **software-update files for external adapters**) **through the edge**, as a separate STATIC route — one hardened front door, not a second public surface.

- `GET /b/<HASH>` → capability check (+ optional per-blob `shared-secret`) → **stream from a LOCAL copy**. No core round-trip, no `http.req` family, no 128 KiB issue.
- **The local copy comes from syncthing, but syncthing only SYNCS — it is not an HTTP server** (never expose its `:8384`). The external serving is a small SY.edge route.
- **Do NOT sync the whole (private) blob store to a public edge.** Use a **dedicated "public-blob" syncthing folder** holding only explicitly-published blobs. Publishing a blob = link it into that folder + mint its `/b/<hash>`. (Delta vs the egress role, which skips syncthing/blob entirely — the Ingress role must add the public-blob folder.)
- BlobRef reuses the canonical shape `{ ref_type:"blob_ref", blob_name, size, mime, filename_original, spool_day }` (`crates/fluxbee_sdk/src/send_normalization.rs`).
- ⚠️ **Sign software-update blobs** (update = code execution on the adapter). This is the one non-trivial security item kept even in the light version: with a signed update the adapter rejects a poisoned blob store. **Virus/content scanning is deferred** ("if a virus gets in we're dead — another story").

---

## 9. Provisioning — the `Ingress` HiveRole (🔶 orchestrator work)

- Add `Ingress` to the orchestrator `HiveRole` enum (today `Motherbee | Worker | Egress`, `src/bin/sy_orchestrator.rs:181`) + `from_role`/`as_str`, and an `add_ingress` flow modelled on `add_egress_hive_flow` (~70% template: routes-only, no SY.identity/vault, WAN uplink + mesh mTLS required-by-default).
- Deltas vs egress: (a) two NICs with the public one bound only by SY.edge `:443`; (b) the `SY.edge` node in the launched set; (c) the public-blob syncthing folder if blob egress is used; (d) `SY.edge` is a `SY.*` system node (deterministic ilk + tenant *a fuego*, like every SY node), but its binary is not in the default SY set — `add_ingress` ships its component + systemd unit into the ingress hive.
- Config `/etc/fluxbee/edge.conf` (the only hand-set file): `edge_id`, `router_socket`, `listen_https`, `expected_public_ip_cidr`, `log_level`. Routing is by the pre-resolved `handler_node` cached in each endpoint row (§6, Z) — no request-time resolve, no `core_ingress_l2`. Everything else (certs, JWT pubkey, endpoint table) arrives over the mesh / is minted locally.

---

## 10. Errors, degraded modes, lifecycle (✅ CLOSED — reuse existing)

HTTP mapping is driven by the existing `RpcError` mapping (`crates/fluxbee_sdk/src/rpc.rs`): `Unreachable`/`TtlExceeded` → `502`, `Timeout` → `504`, router socket down → `502`. Plus: bad hash/unpublished → `404`; auth fail → `401`; rate limit → `429`; oversized body → `413`; upgrade → `501`; the handler's reply is wrapped as the HTTP `200` body. Degraded: control/router down → keep serving cached state, reconnect with backoff; cert expiring → warn, keep serving. Shutdown: drain in-flight, close.

---

## 11. Control channel — RETIRED (✅ DECIDED, one sub-point open)

The v2 `edge-control-protocol-v2` (a bespoke WSS+mTLS channel with bootstrap tokens, its own internal CA + CRL, `EDGE_HELLO`/`CERT_ROTATE`/`KEY_ROTATE`/`TENANT_ASSIGN`, fingerprint-delta resync, takeover) existed **only because FC was modelled off-mesh**. Two separate control relationships were bundled into it; in v3 both dissolve into things fluxbee already has.

### 11.1 motherbee → edge control = the mesh (zero bespoke channel)

The edge is a mesh node. Its `rt-gateway` already holds a **persistent, outbound-initiated, NAT-friendly** WAN mTLS connection to motherbee, and motherbee already **pushes** over it (SY.identity deltas, config, cert/key distribution at `add_hive` — all validated). So the entire push side is the mesh:

- `CERT_ROTATE`/`KEY_ROTATE` → orchestrator push over the mesh (as `add_hive` already distributes certs/keys).
- `TENANT_ASSIGN`/`REVOKE`, endpoint publication → mesh RPC / config / `EDGE_REGISTER` (§5).
- health/heartbeat/liveness → the mesh already has it (LSA, router node registry, WAN reconnect).
- emergency JWT-key revoke → the signer is on **motherbee** (§7), so revoke = rotate in vault → orchestrator pushes the new public key to edges over the mesh. Fast, no external actor in the loop.

**Nothing here needs a second protocol.** The v2 bootstrap-token / control-CA / CRL / fingerprint-resync machinery all vanish — the mesh's own mTLS join (`add_hive`) + LSA + orchestrator push already solve enrollment, auth, liveness, and push.

### 11.2 Fluxbee Cloud → motherbee control = plain API (the "quilombo" disappears)

The secondary concern (motherbees registering directly and being controlled through the channel, beyond certs/vault) also reduces to API, because in v3 **Fluxbee Cloud holds no crown jewels and is just an API client**:

- **Register / heartbeat:** the motherbee **dials home** to a Fluxbee Cloud API (`POST /fleet/register` + periodic heartbeat). Preserves outbound-only; works through NAT; no inbound to customer motherbees. This replaces "motherbee registers through the control channel."
- **Control (assign tenant, publish, add_hive, rotate):** these are **SY.admin operations that already exist as an API** (`:8080`, e.g. `add_hive`, `list_hives`). Cloud calls SY.admin (if reachable) or the motherbee **polls** Cloud for pending commands on its heartbeat. Either way: plain request/response, no persistent bespoke channel.
- Secrets stay in the motherbee's vault (§7); Cloud never distributes keys, so it never needs the push-crypto machinery the v2 channel carried.

### 11.3 What genuinely stays "external" (and is NOT a control channel)

- **ACME (public cert issuance) + DNS management** — outbound API calls to Let's Encrypt + a DNS provider. They live on the edge's internet NIC or a tiny helper. Third-party API calls, not a fluxbee control protocol.

### 11.4 Cloud→motherbee latency (✅ DECIDED — poll)

The only thing a persistent channel uniquely gave was **low-latency server-PUSH to a NAT'd motherbee**. In v3 that need evaporates (crown jewels are in the mesh; emergency revoke propagates motherbee→edge over the mesh, not Cloud→motherbee). **Decided: `register + heartbeat + SY.admin + poll` is the model** — Cloud→motherbee latency is bounded by the poll interval, which is fine for control ops (tenant assign, publish, rotate — none are sub-second-critical). No persistent Cloud→motherbee channel. If a future op ever needs sub-second push, add a thin motherbee-held long-poll to Cloud then — additive, optional, and still not the v2 protocol.

**Decision: `edge-control-protocol-v2.md` and `fc-edge-manager-spec.md` are RETIRED — no bespoke control channel is built.** Control = mesh (motherbee→edge push, internal) + SY.admin/heartbeat/poll API (Cloud↔motherbee) + ACME/DNS outbound (external). Less is more; whatever works with the plumbing we already have.

---

## 12. Deferred to a hardening pass (security-later, by decision)

JWT user auth (whole stack); per-edge ACME certs (alpha uses wildcard, §7); CRL / revocation-at-handshake; C2 bootstrap hardening (short-TTL HMAC token, opaque reject, per-IP limit); cross-ILK publication authz (§5.1); blob virus/content scanning (§8); per-tenant metric obfuscation.

Not deferred for alpha:

- **Scope down the public edge's reachability.** `SY.edge` is a `SY.*` system node, but it is **public-facing** and must NOT inherit the broad cross-VPN reachability system nodes normally get. Router/config policy must constrain `SY.edge@<edgehive>` to the forward path to core plus the small set of edge system messages it needs. (This replaces the v2 concern about `RT.`-prefixed nodes being auto-system-level.)
- **Restrict the edge's `routing.dst`.** SY.edge's public HTTP path must only target the `handler_node`s present in its registry, via `Destination::Unicast` on the cached name; enforce this in the node and, preferably, in router policy as defense in depth.
- **Edge `src_ilk` trust boundary.** Core ingress/router L3 is the first trusted identity boundary; ignore or re-derive any edge-asserted caller `src_ilk` unless a real principal-binding mechanism is in place.
- **Header allowlist + stripping.** Strip `Authorization` and hop-by-hop headers before forwarding; pass only an allowlisted subset inward.
- **Bounded eviction.** The edge's pending `trace_id` map must be bounded and evict on timeout/load from day one.

---

## 13. Walking skeleton — first end-to-end proof (✅ target)

**Use case: Fluxbee Cloud calls the architect through the edge reverse-proxy.** Machine-to-machine, `shared-secret`, no login.

1. **No new wire family.** The edge forwards under the target's declared `inbound_family` (§3); nothing to define in a shared crate.
2. **Edge frontend (`sy_edge`, model on `io-api`):** terminate `:443` (wildcard cert), `/e/<hash>` → registry lookup → build one mesh message (`meta.msg_type = inbound_family`, `meta.dst_ilk`, `routing.dst = Destination::Unicast(handler_node)` from the cached row, `routing.src_l2_name = SY.edge@<edgehive>`, fresh `trace_id`, body passthrough) → `send_with_matcher` await the reply (30 s) → map back to the held HTTP response.
3. **Delivery (§6, Z):** the name `handler_node@motherbee` forwards cross-hive by LSA → core delivers to the handler under the declared family; the handler replies to `src_l2_name`. The `ilk → handler_node` resolve already happened at publication time. First proof: point one endpoint at an echo handler that speaks the declared family.
4. Bring the edge up as a real WAN-joined `Ingress` hive (or, for the very first run, hand-copy the egress `hive.yaml` + add the edge node), so the edge is LSA-advertised into motherbee and vice-versa.
5. Pre-register one endpoint (`hash → target ilk`, `inbound_family`, `shared-secret`) by pushing the edge's `endpoints` config via `NODE_CONFIG_SET`; pre-create the tenant/ilk in SY.identity.

**Stub/defer for the skeleton:** JWT, ACME, blob egress, the control-channel question (§11), per-edge certs, OPA-at-ingress, and full `Ingress`-role productization. This proves attach + **native-family forward** + `trace_id` correlation + cross-hive WAN round-trip + `dst_ilk`→handler resolution — the whole ingress spine — touching none of the security/control-plane machinery.

---

## 14. Implementation checklist (alpha)

- [ ] Forward under the target's declared `inbound_family` — **no new wire family** (http.req/http.res deleted)
- [ ] Body passthrough (`json | utf8 | base64`) with full-envelope 64 KiB size check before send, `413` above
- [ ] `SY.edge` node (`sy_edge`): axum `:443` (wildcard TLS, arc-swap reload), attach via `RouterDispatcher`, `/e/<hash>` + `/b/<hash>` routers
- [ ] Endpoint registry (`hash → {tenant, ilk, inbound_family, auth_mode, ...}`) delivered via `NODE_CONFIG_SET` (`endpoints` section)
- [ ] Core authority resolves `ilk → handler_node` at publish time (identity SHM) + validates `EDGE_REGISTER` self-publication — **no router change** (Z routes by name)
- [ ] Shared-secret alpha contract (static bearer vs HMAC; storage and rotation)
- [ ] `trace_id` bounded pending map + `send_with_matcher` 30 s await, reply wrapped as HTTP response
- [ ] Confirm resolve path (§6): router cross-hive `Resolve` vs `resolve`-mode `sy_edge`
- [ ] Cert/JWT-pubkey distribution from vault via orchestrator; edge local cache + hot reload; align current disk/SSH mesh TLS flow with target custody model
- [ ] `Ingress` HiveRole + `add_ingress` flow + manifest/systemd for `SY.edge`
- [ ] Blob egress `/b/<hash>` + public-blob syncthing folder + signed updates
- [ ] Error mapping, degraded modes, 501 for upgrades
- [ ] Rate limit (per-IP, per-tenant)
- [ ] SY.edge outbound allowlist + router-side policy restricting public edge routes
- [ ] Decide §11 (control channel) before building any of it

---

## 15. References

| Topic | Doc |
|-------|-----|
| L2 message format / framing | `02-protocolo.md` |
| WAN / connectivity | `05-conectividad.md` |
| Identity, tenants, ilk, `handler_node` | `10-identity-v2.md` |
| Vault (cert/JWT/CA custody) | `sy-vault-spec.md` |
| Egress sibling role | `edge-egress-nat-spec.md` |
| Superseded drafts | `rt-edge-spec.md`, `fc-edge-manager-spec.md`, `edge-control-protocol-v2.md` |
