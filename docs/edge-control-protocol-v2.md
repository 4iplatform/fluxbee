# Fluxbee — Edge Control Protocol Specification

**Status:** v2.0 alpha — incorporates security review (per-edge certs, hardened bootstrap)
**Date:** 2026-06-30
**Audience:** RT.edge developer, FC.edge-manager developer
**Supersedes:** v1.0 (2026-05-19)
**Related:** `rt-edge-spec.md`, `fc-edge-manager-spec.md`, `sy-vault-spec.md`, `10-identity-v2.md`, `05-conectividad.md`, `edge-egress-nat-spec.md`

---

## 0. Changes from v1.0

The v1.0 draft was reviewed for security. The following structural decisions are frozen and take precedence over v1.0 wherever they conflict:

| # | v1.0 | v2.0 | Why |
|---|------|------|-----|
| **C1** | Wildcard `*.fluxbee.ai` cert distributed to every edge | **Per-edge cert** (`<edge_id>.fluxbee.ai`, SAN-specific) | Compromising one edge no longer yields a cert valid for the whole brand. Biggest blast-radius reduction in the system |
| **C2** | Bootstrap token TTL 24 h, detailed reject reasons | TTL ≤ 15 min, **opaque reject** to client, HMAC-bound CSR, rate-limited | Limits token-leak window and enumeration |
| **C3** | `EDGE_PUBLIC_IP_CHANGED` applied automatically | **Validated against expected ranges**, large changes alert + hold | Prevents DNS-repoint MITM via stolen edge_cert |
| **C4** | JWT key grace fixed at 7 days | Added `EMERGENCY_KEY_REVOKE` (no grace) | Emergency path when a signing key is compromised |
| **C5** | Time sync unmentioned | **NTP required**, `CLOCK_DRIFT` error | All cert/JWT validity depends on correct clock |
| **C6** | Resync "from scratch" vs "deltas" contradiction | Clarified: **fingerprint-driven delta** (§9) | Removes ambiguity; avoids 10k-message storms |

Items deferred to a separate `edge-security-hardening.md` (not this doc): CRL/OCSP from RT.edge toward FC server cert, TPM sealing, opaque edge_id DNS names, per-tenant metric obfuscation. These were judged non-v1 hardening, not protocol structure.

---

## 1. Purpose

This document specifies the wire protocol between **RT.edge** instances (the public ingress runtime on Fluxbee infrastructure) and **FC.edge-manager** (the control plane process inside Fluxbee Cloud).

The protocol is bidirectional, persistent, and carries **control plane traffic only** — never user data, never application traffic.

It must allow:

- A new RT.edge to bootstrap and authenticate against FC.edge-manager.
- Continuous health monitoring of RT.edges.
- Distribution of credentials (the edge's own per-edge TLS certificate, the JWT signing public key) from FC.edge-manager to each RT.edge.
- Assignment and revocation of tenants to/from specific RT.edges.
- Reporting of edge state (public IP, capacity, load).
- Graceful shutdown coordination.
- Emergency credential revocation.

It explicitly does **NOT** carry:

- User HTTP traffic. Browsers and API clients connect directly to RT.edges using the per-edge cert. RT.edges translate requests to Fluxbee messages and inject them into the local edge router; that router / `RT.gateway` owns the WAN path to motherbee.
- Endpoint registration of IO.api or any application node (`EDGE_REGISTER` / `EDGE_UNREGISTER`). That is Fluxbee data plane, routed to RT.edge as standard L2 messages through the local edge router / WAN path as needed (see `rt-edge-spec.md`).
- Admin queries against Fluxbee data. FC.edge-manager performs those as a regular HTTPS client of an RT.edge using its own admin-scoped JWT.

---

## 2. Design Principles

**2.1 RT.edge always initiates.** RT.edge connects outbound to FC.edge-manager on a stable, hardcoded URL. FC never connects to edges. Preserves Fluxbee's outbound-only posture.

**2.2 Single persistent connection per edge.** One active control channel per `edge_id`. Duplicates trigger takeover (§9.3).

**2.3 Control plane only.** No user data, no per-request lookups. Anything needed at HTTP request time must be cached locally at the RT.edge.

**2.4 Cached state outlives the channel.** The data plane keeps serving when the control channel is down, using last-known cert, JWT key, and tenant assignments.

**2.5 JSON envelope for evolution.** Additive evolution; unknown message types are logged and discarded, unknown fields ignored.

**2.6 Versioned protocol.** Every message carries `v`. Negotiated on connect.

**2.7 mTLS after bootstrap.** Bootstrap token authenticates the first connection only; thereafter mTLS with a revocable per-edge control cert.

**2.8 Fingerprint-driven resync on reconnect.** On reconnect the edge reports fingerprints of what it holds; FC pushes only deltas (§9). No session continuity assumed, but no blind full-state retransmission either.

**2.9 No silent failures.** Every long-running operation produces an explicit ack; missing acks are detectable and retried.

**2.10 Least blast radius.** Per-edge certs (C1), scoped tenants, opaque rejects (C2), validated IP changes (C3): a single compromised edge cannot impersonate the platform.

---

## 3. Terminology

| Term | Meaning |
|------|---------|
| **RT.edge** | Rust process on a Fluxbee DMZ host. One per edge instance. |
| **FC.edge-manager** | Control plane process inside Fluxbee Cloud. Manages all edges. |
| **Control channel** | Persistent WebSocket-over-TLS between an RT.edge and FC.edge-manager. |
| **Data channel** | HTTPS from any client to an RT.edge for application traffic. Out of scope here. |
| **edge_id** | Stable identifier of an RT.edge (e.g. `eu-edge-1`). Assigned at provisioning, immutable. |
| **edge_bootstrap_token** | Single-use, ≤15-min token issued by FC admin UI for first-time enrollment. |
| **edge_control_cert** | Long-lived X.509 issued by FC's internal CA. Authenticates the control channel via mTLS. Revocable per edge. Distinct from the public TLS cert below. |
| **edge_tls_cert** | The **per-edge public** TLS cert for `<edge_id>.fluxbee.ai`, issued by a public CA (Let's Encrypt) via DNS-01. Used by the edge to terminate user HTTPS. **Per-edge, not wildcard.** |
| **jwt_public_key** | Public key of Fluxbee Cloud's JWT signing keypair. Lets edges validate user JWTs locally. |
| **tenant_assignment** | Declares that a `tenant_id` is served by a specific RT.edge. |

Two separate cryptographic worlds, do not conflate: **edge_control_cert** (private CA, mTLS, control channel) vs **edge_tls_cert** (public CA, browser-facing, per-edge).

---

## 4. Transport

### 4.1 WebSocket over TLS

- Scheme `wss://`, port `443`, TLS 1.3 minimum.
- Authentication: mTLS using `edge_control_cert` (except bootstrap, §6).
- Subprotocol header: `fluxbee-edge-control.v2`.
- RT.edge validates FC's server cert against a pinned CA bundle at `/etc/fluxbee/edge.ca.pem`. Browser trust stores are not used.

### 4.2 Endpoint

```
wss://edge-control.fluxbee.ai/v2/edge
```

Loaded from `/etc/fluxbee/edge.conf` at boot. Stable; changing it requires a coordinated edge upgrade.

### 4.3 Framing

WebSocket text frames, one JSON message per frame. Max frame 1 MiB; larger ⇒ close `1009`.

### 4.4 Keepalive

| Mechanism | Direction | Interval | Timeout | On timeout |
|-----------|-----------|----------|---------|-----------|
| WebSocket ping | RT.edge → FC | 15 s | 30 s | Close, reconnect |
| `EDGE_HEARTBEAT` | RT.edge → FC | 30 s | 90 s | FC marks edge unhealthy |

### 4.5 Reconnection

Exponential backoff: initial 1 s, max 60 s, factor 2, jitter ±20%. Unconditional. Data plane keeps serving during reconnection. FC rate-limits `EDGE_HELLO` per `edge_id` against flapping.

### 4.6 Clock requirement (C5)

Both peers MUST run NTP (e.g. chronyd) with controlled sources. The RT.edge checks its own drift; if absolute drift vs FC's `ts` exceeds 30 s on `EDGE_HELLO_ACK`, it emits `EDGE_ERROR { code: CLOCK_DRIFT }` and continues in a degraded posture (it does not start terminating user TLS with a clock it cannot trust if drift exceeds a hard ceiling of 300 s — it waits for NTP to converge). All cert/JWT validity logic depends on this.

---

## 5. Message Envelope

```json
{
  "v": 2,
  "msg_id": "550e8400-e29b-41d4-a716-446655440000",
  "msg_type": "EDGE_HELLO",
  "ts": "2026-06-30T10:00:00.000Z",
  "in_reply_to": null,
  "payload": { }
}
```

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `v` | int | yes | Protocol version, `2`. |
| `msg_id` | UUID v4 | yes | Correlation id. |
| `msg_type` | string | yes | See §6–§8. |
| `ts` | ISO-8601 | yes | Sender wall clock. Used for drift detection (§4.6), not ordering. |
| `in_reply_to` | UUID v4 / null | conditional | Required on responses. |
| `payload` | object | yes | Message body; may be `{}`. |

Unknown `msg_type` ⇒ log + discard, do not close. Unknown fields ⇒ ignore.

---

## 6. Bootstrap Protocol (hardened, C2)

### 6.1 Pre-conditions

| File | Perms | Content |
|------|-------|---------|
| `/etc/fluxbee/edge.conf` | `0644` | `edge_id`, FC control URL, local router socket, core ingress L2 target |
| `/etc/fluxbee/edge.bootstrap` | `0600` | The bootstrap token (pre-bootstrap only) |
| `/etc/fluxbee/edge.ca.pem` | `0644` | Pinned FC CA bundle |

No `edge_control_cert` yet. First connection uses server-side TLS only.

### 6.2 EDGE_BOOTSTRAP

```json
{
  "v": 2,
  "msg_type": "EDGE_BOOTSTRAP",
  "payload": {
    "edge_id": "eu-edge-1",
    "csr": "<PEM PKCS#10 CSR>",
    "csr_hmac": "<HMAC-SHA256(csr, bootstrap_token), hex>",
    "edge_version": "2.0.0",
    "edge_platform": "linux-x86_64",
    "public_ip": "203.0.113.42"
  }
}
```

Note: the **token itself is not sent in clear**. Instead the CSR is HMAC'd with the token (C2). The server, which knows the token, recomputes the HMAC and verifies. This proves possession of the token without transmitting it, and binds the token to this specific CSR (a captured CSR cannot be reused with a different token, and a captured HMAC cannot be reused with a different CSR).

### 6.3 EDGE_BOOTSTRAP_ACK / REJECT

FC validates: token exists, unused, unexpired (≤15 min), `edge_id` matches the token, `csr_hmac` verifies, CSR algorithm acceptable (Ed25519 or P-256).

On success:

```json
{
  "msg_type": "EDGE_BOOTSTRAP_ACK",
  "in_reply_to": "<bootstrap msg_id>",
  "payload": {
    "edge_control_cert": "<PEM>",
    "edge_control_cert_chain": "<PEM>",
    "valid_until": "2027-06-30T10:00:00Z"
  }
}
```

The edge persists cert + chain + its locally-generated private key, deletes `edge.bootstrap`, closes (`1000`, `BOOTSTRAP_COMPLETE`), and reconnects presenting the control cert via mTLS.

On failure, the client receives a **single opaque reason** (C2):

```json
{
  "msg_type": "EDGE_BOOTSTRAP_REJECT",
  "in_reply_to": "<bootstrap msg_id>",
  "payload": { "reason_code": "BOOTSTRAP_REJECTED" }
}
```

The detailed cause (token expired vs unknown vs edge_id mismatch vs HMAC fail) is logged **server-side only**, never disclosed to the client, to prevent enumeration. FC closes with `1008`. FC rate-limits bootstrap attempts per source IP (C2).

### 6.4 Control-cert renewal

Control certs valid 1 year. From 30 days before expiry, after a successful heartbeat the edge sends `EDGE_CONTROL_CERT_RENEW { csr }` (authenticated by the still-valid mTLS cert) and receives `EDGE_CONTROL_CERT_RENEW_ACK { edge_control_cert, chain, valid_until }`. Atomic persist; new cert used on next reconnect.

---

## 7. Steady-State: RT.edge → FC.edge-manager

### 7.1 EDGE_HELLO

First message after every mTLS connect. Reports state fingerprints for delta resync (§9).

```json
{
  "msg_type": "EDGE_HELLO",
  "payload": {
    "edge_id": "eu-edge-1",
    "edge_version": "2.0.0",
    "public_ip": "203.0.113.42",
    "region": "eu-west-1",
    "capacity": { "max_concurrent_requests": 10000, "max_tenants": 1024 },
    "current_state": {
      "tenants_fingerprint": "sha256:...",
      "tenants_count": 128,
      "edge_tls_cert_fingerprint": "sha256:...",
      "jwt_key_id": "fc-jwt-key-2026-06",
      "router_connected": true,
      "core_ingress_reachable": true,
      "uptime_seconds": 3600,
      "clock_drift_seconds": 0.4
    }
  }
}
```

`tenants_fingerprint` is a stable hash over the sorted set of assigned tenant_ids, so FC can detect "edge already has the right set" in one comparison instead of streaming the whole list (C6).

### 7.2 EDGE_HELLO_ACK

```json
{
  "msg_type": "EDGE_HELLO_ACK",
  "in_reply_to": "<hello msg_id>",
  "payload": {
    "accepted": true,
    "fc_version": "2.0.0",
    "server_ts": "2026-06-30T10:00:00.123Z",
    "config": {
      "heartbeat_interval_seconds": 30,
      "rate_limit_per_ip_rps": 100,
      "rate_limit_per_tenant_rps": 1000
    }
  }
}
```

`server_ts` lets the edge compute clock drift (§4.6). If `accepted:false`, includes `reason_code` and FC closes.

After ACK, FC pushes only the deltas implied by the fingerprint comparison (§9).

### 7.3 EDGE_HEARTBEAT

Every 30 s. Payload: `uptime_seconds`, `router_connected`, `core_ingress_reachable`, `active_requests`, `memory_mb`, `cpu_percent`. `router_connected` is RT.edge's local router socket state. `core_ingress_reachable` is derived from recent successful sends / responses to the configured core ingress L2 target, not from running identity on the edge. No per-heartbeat ack required (WS ping covers liveness); FC may piggyback config nudges on an optional `EDGE_HEARTBEAT_ACK`.

### 7.4 EDGE_PUBLIC_IP_CHANGED (validated, C3)

```json
{
  "msg_type": "EDGE_PUBLIC_IP_CHANGED",
  "payload": { "old_ip": "203.0.113.42", "new_ip": "203.0.113.43", "detected_at": "..." }
}
```

FC does **not** blindly update DNS. It validates `new_ip` against the expected range(s) configured for this edge:

- If `new_ip` is within the edge's expected CIDR/AS ⇒ update DNS, reply `EDGE_DNS_UPDATED`.
- If `new_ip` is outside (different subnet/AS) ⇒ **hold the change, alert ops, do not update DNS automatically.** Reply `EDGE_PUBLIC_IP_HELD { reason: "OUT_OF_EXPECTED_RANGE" }`. Operator approves manually.

This blocks the stolen-control-cert MITM where an attacker repoints `<edge_id>.fluxbee.ai` to their own IP.

### 7.5 EDGE_METRICS

Optional, every 60 s if enabled: totals, by-status, by-tenant, latency p50/p99, bytes in/out. Forwarded to observability. No reply.

### 7.6 EDGE_ERROR

```json
{
  "msg_type": "EDGE_ERROR",
  "payload": { "severity": "warning", "code": "ROUTER_DISCONNECTED", "message": "...", "details": {} }
}
```

Severities: `info | warning | error | critical`. Codes include `ROUTER_DISCONNECTED`, `ROUTER_RECONNECTED`, `CORE_INGRESS_UNREACHABLE`, `CORE_INGRESS_REACHABLE`, `EDGE_TLS_CERT_EXPIRING`, `RATE_LIMIT_TRIPPED`, `JWT_VALIDATION_FAILURES_HIGH`, `CLOCK_DRIFT` (C5), `INTERNAL_ERROR`.

### 7.7 EDGE_GOODBYE

Graceful shutdown: `{ reason, drain_seconds }`. FC marks draining, stops targeting this edge with new JWTs, removes from DNS pool, waits for close.

---

## 8. Steady-State: FC.edge-manager → RT.edge

### 8.1 TENANT_ASSIGN / 8.2 TENANT_REVOKE

Idempotent. Assign: `{ tenant_id, tenant_name, policy: { rate_limit_rps, max_concurrent_requests } }` → edge adds to accepted set, persists `/var/lib/fluxbee/edge.tenants.json`, replies `TENANT_ASSIGN_ACK`. Revoke: `{ tenant_id, reason }` → remove, persist, `TENANT_REVOKE_ACK`. Requests for unserved tenants return `404`.

### 8.3 KEY_ROTATE

```json
{
  "msg_type": "KEY_ROTATE",
  "payload": {
    "key_id": "fc-jwt-key-2026-07",
    "algorithm": "EdDSA",
    "public_key_pem": "<PEM>",
    "effective_at": "2026-07-01T00:00:00Z",
    "previous_key_id": "fc-jwt-key-2026-06",
    "previous_key_grace_until": "2026-07-08T00:00:00Z"
  }
}
```

Edge stores both. JWT validated against the new key if `iat >= effective_at`, or the previous key if `iat < effective_at` and now < grace. After grace, previous key discarded. Reply `KEY_ROTATE_ACK`.

### 8.4 EMERGENCY_KEY_REVOKE (C4)

```json
{
  "msg_type": "EMERGENCY_KEY_REVOKE",
  "payload": { "key_id": "fc-jwt-key-2026-06", "reason": "SUSPECTED_COMPROMISE" }
}
```

Edge **immediately** removes `key_id` from its accepted set, ignoring any remaining grace window. JWTs signed with that key are rejected from this instant. Reply `EMERGENCY_KEY_REVOKE_ACK`. This is the break-glass path the fixed 7-day grace of v1 lacked.

### 8.5 CERT_ROTATE (per-edge, C1)

```json
{
  "msg_type": "CERT_ROTATE",
  "payload": {
    "cert_id": "eu-edge-1-2026-06",
    "fqdn": "eu-edge-1.fluxbee.ai",
    "cert_pem": "<PEM>",
    "chain_pem": "<PEM>",
    "private_key_pem": "<PEM>",
    "valid_until": "2026-09-28T00:00:00Z"
  }
}
```

This carries the **per-edge** public TLS cert, valid only for `<edge_id>.fluxbee.ai`, not a wildcard. The `fqdn` MUST match this edge's own hostname; an edge receiving a cert for a different fqdn rejects it (`CERT_ROTATE_ACK { status: "error", reason: "FQDN_MISMATCH" }`). Edge persists atomically under `/var/lib/fluxbee/edge.tls.*` (`0600` for the key), hot-reloads its HTTPS listener, replies `CERT_ROTATE_ACK { status: "ok" }`.

Because the cert is per-edge, a compromised edge leaks only its own cert. The attacker can impersonate `eu-edge-1.fluxbee.ai`, not `*.fluxbee.ai`. Revocation and reissue affect one edge.

### 8.6 CONFIG_UPDATE

Partial update of runtime config: rate limits, heartbeat interval, metrics_enabled, log_level. Edge applies immediately, replies `CONFIG_UPDATE_ACK { applied_fields }`.

### 8.7 EDGE_SHUTDOWN

`{ drain_seconds, reason }`. Edge behaves as on SIGTERM: stop new requests, drain in-flight, send `EDGE_GOODBYE`, close.

### 8.8 EDGE_DNS_UPDATED / EDGE_PUBLIC_IP_HELD

Informational responses to §7.4. `EDGE_DNS_UPDATED { fqdn, ip, ttl_seconds, updated_at }` on success; `EDGE_PUBLIC_IP_HELD { reason }` when the change is withheld for manual approval.

### 8.9 EDGE_RECONNECT_REQUEST

`{ reason }`. Edge closes (`1000`, `RECONNECT_REQUESTED`) and reconnects immediately, skipping backoff. Used to force a resync.

---

## 9. Reconnection and Resync (C6)

### 9.1 Flow

1. WS drops; edge reconnects with backoff, mTLS.
2. Edge sends `EDGE_HELLO` with `current_state` fingerprints.
3. FC replies `EDGE_HELLO_ACK`.
4. FC compares fingerprints:
   - `tenants_fingerprint` differs ⇒ FC sends only the missing `TENANT_ASSIGN` / `TENANT_REVOKE` to converge (a true delta, computed by diffing the edge's claimed set against the authoritative set — for which FC may request the full list once via `TENANT_LIST_REQUEST` only when the fingerprint mismatches).
   - `edge_tls_cert_fingerprint` differs ⇒ `CERT_ROTATE`.
   - `jwt_key_id` differs ⇒ `KEY_ROTATE`.
5. Steady state.

This resolves the v1 contradiction: conceptually stateless, but fingerprint-gated so a healthy reconnect with unchanged state sends near-zero messages, and a 10k-tenant edge does not receive 10k messages on every blip.

### 9.2 Idempotency

All FC → RT messages idempotent. Re-asserting an assigned tenant or an already-loaded cert is a no-op (`status: "already_loaded"`).

### 9.3 Takeover

A new authenticated connection for an `edge_id` that already has one ⇒ FC accepts the new, closes the old with `4001` `SUPERSEDED_BY_NEW_CONNECTION`. Handles crash-and-respawn.

### 9.4 Edge marked dead

No heartbeat for 90 s ⇒ unhealthy; FC stops issuing JWTs targeting it, removes from DNS pool if pool > 1, keeps accepting reconnects. Unhealthy > 1 h ⇒ alert ops. No auto-decommission.

---

## 10. Versioning

`v: 2`. Subprotocol `fluxbee-edge-control.v2`. Mismatch ⇒ close `1003`. Within v2, additive changes (new optional fields, new msg_types, new reason codes) are backward-compatible; breaking changes require v3. Software versions (`edge_version`, `fc_version`) are observability only.

---

## 11. Close Codes

| Code | Reason | Initiator | When |
|------|--------|-----------|------|
| `1000` | Normal | Either | bootstrap complete, goodbye, reconnect request |
| `1001` | Going away | Either | shutdown |
| `1003` | Unsupported | Either | version mismatch |
| `1008` | Policy | FC | auth failure, bootstrap reject |
| `1009` | Too big | Either | frame > 1 MiB |
| `1011` | Internal | Either | unexpected failure |
| `4001` | Superseded | FC | new connection same edge_id |
| `4002` | Decommissioned | FC | edge_id retired |

Application-level errors ride in `*_ACK { status: "error", reason_code }`; the connection is not closed unless the error is a protocol violation.

---

## 12. Security Model

### 12.1 Two cert lineages

| | edge_control_cert | edge_tls_cert |
|--|-------------------|---------------|
| CA | FC internal (private, pinned) | Public (Let's Encrypt) |
| Purpose | mTLS on the control channel | Terminate user HTTPS |
| Scope | this edge's control identity | `<edge_id>.fluxbee.ai` only (C1) |
| Rotation | §6.4 | §8.5 |
| Compromise impact | impersonate this edge to FC | impersonate this one hostname |

### 12.2 Compromise of one edge

Attacker with that edge's secrets gets: its own control cert, its own per-edge TLS cert (one hostname), the JWT **public** key (cannot forge JWTs), and its tenant assignments. **Not** a wildcard, **not** the JWT private key. Mitigation: revoke that edge's control cert (CRL checked at FC mTLS handshake), reissue its per-edge TLS cert, reassign its tenants elsewhere. Blast radius: one edge.

### 12.3 Compromise of FC.edge-manager (crown jewel)

Whoever breaches FC can mint JWTs, reassign tenants, issue control certs. Mitigations: the JWT private key and the FC internal CA private key live in **SY.vault inside Fluxbee proper**, not on FC.edge-manager; FC calls vault for every sign/issue, producing an audit trail. FC is a hardened, dedicated, monitored host. (Detailed hardening in `edge-security-hardening.md`.)

### 12.4 Public-internet attacker

Connects to `edge-control.fluxbee.ai` but without a valid control cert the mTLS handshake fails before any app data. Bootstrap requires a live ≤15-min token AND a valid HMAC (C2). TLS-handshake floods mitigated at the network layer in front of FC.

### 12.5 Stolen bootstrap token

Window ≤15 min (C2). Token never transmitted in clear (HMAC binding). Even a captured HMAC is bound to one CSR, so an attacker cannot enroll their own keypair with someone else's token+CSR. Rate-limited per IP. Opaque rejects prevent probing.

### 12.6 Replay

mTLS-authenticated channel; unique `msg_id` per message. No strict replay counter in v2 (acceptable under mTLS). A per-direction monotonic sequence can be added in v3 if needed.

### 12.7 Clock (C5)

NTP mandatory. Drift > 30 s ⇒ `CLOCK_DRIFT` error. Drift > 300 s ⇒ edge refuses to terminate user TLS until NTP converges, since it cannot trust cert validity windows.

### 12.8 Secrets on disk

| File | Perms | Contains |
|------|-------|----------|
| `/etc/fluxbee/edge.bootstrap` | `0600` | bootstrap token (pre-bootstrap only) |
| `/etc/fluxbee/edge.control.cert.pem` | `0644` | control cert (public) |
| `/etc/fluxbee/edge.control.key.pem` | `0600` | control cert private key |
| `/etc/fluxbee/edge.ca.pem` | `0644` | pinned FC CA |
| `/var/lib/fluxbee/edge.tls.cert.pem` | `0644` | per-edge TLS cert (public) |
| `/var/lib/fluxbee/edge.tls.key.pem` | `0600` | per-edge TLS private key |
| `/var/lib/fluxbee/edge.jwt_keys.json` | `0644` | JWT public keys |
| `/var/lib/fluxbee/edge.tenants.json` | `0644` | assigned tenants |

---

## 13. Decisions Log

| Decision | Rationale |
|----------|-----------|
| Per-edge cert, not wildcard (C1) | One compromised edge ≠ platform-wide TLS impersonation |
| HMAC-bound, non-transmitted bootstrap token (C2) | Possession proof without exposure; binds token to CSR |
| Opaque bootstrap reject (C2) | No enumeration of tokens/edges |
| Bootstrap TTL ≤15 min + per-IP rate limit (C2) | Small leak window, no brute force |
| Validated EDGE_PUBLIC_IP_CHANGED (C3) | Blocks DNS-repoint MITM |
| EMERGENCY_KEY_REVOKE (C4) | Break-glass for compromised signing key |
| NTP mandatory + CLOCK_DRIFT (C5) | Cert/JWT validity depends on clock |
| Fingerprint-driven resync (C6) | No 10k-message storms; unambiguous |
| WebSocket + JSON | Bidirectional, persistent, schema-free, good Rust support (tokio-tungstenite) |
| RT.edge always initiates | Outbound-only posture |
| mTLS after bootstrap | Strong, revocable auth |
| Pinned CA for FC server | No public-PKI dependency on control plane |
| Idempotent FC→RT | Aggressive retransmit safe |
| JWT/CA private keys in SY.vault, not on FC | Audit trail, blast-radius separation |

---

## 14. NOT in v2

Multi-FC failover (single FC assumed, manual failover); ordering beyond TCP; replay counters; payload encryption beyond TLS; frame compression; cross-edge coordination; hot reload of `edge_id` or pinned CA (restart required); edge pools/groups; control-channel QoS. CRL/OCSP from edge toward FC server cert and TPM sealing are tracked in `edge-security-hardening.md`.

---

## 15. Implementation Checklist

### RT.edge (Rust, tokio-tungstenite)

- [ ] Load `edge.conf`; detect bootstrap mode
- [ ] Generate keypair (Ed25519), persist key `0600`, build CSR
- [ ] Compute `csr_hmac` with bootstrap token; never send token in clear (C2)
- [ ] WSS client with rustls; pinned-CA server validation; mTLS after bootstrap
- [ ] WS ping 15 s; backoff reconnect with jitter
- [ ] Bootstrap state machine; persist control cert; delete bootstrap token
- [ ] EDGE_HELLO with fingerprints (tenants hash, cert fp, jwt key id) (C6)
- [ ] Clock drift check vs `server_ts`; CLOCK_DRIFT; hard ceiling behavior (C5)
- [ ] Heartbeat 30 s
- [ ] TENANT_ASSIGN/REVOKE persist
- [ ] KEY_ROTATE with grace; EMERGENCY_KEY_REVOKE immediate (C4)
- [ ] CERT_ROTATE per-edge; FQDN match check; hot reload (C1)
- [ ] CONFIG_UPDATE; EDGE_SHUTDOWN/RECONNECT_REQUEST
- [ ] EDGE_PUBLIC_IP_CHANGED detection (C3)
- [ ] EDGE_ERROR conditions; EDGE_GOODBYE on SIGTERM
- [ ] Control-cert renewal within 30 days of expiry
- [ ] Idempotent handling of all FC→RT messages
- [ ] Local audit log of control messages

### FC.edge-manager (Rust or Node)

- [ ] WSS server with mTLS; internal CA (issue control certs + CRL)
- [ ] Edge registry persistence (id, ip, status, last_heartbeat, tenants, expected_ip_ranges)
- [ ] Bootstrap token table with ≤15-min TTL; HMAC verify (C2); per-IP rate limit; opaque reject
- [ ] Admin UI: issue bootstrap token, revoke control cert, assign/revoke tenant, list edges
- [ ] DNS client; validate IP changes against expected ranges before update (C3)
- [ ] ACME DNS-01 client issuing **per-edge** certs (C1); CERT_ROTATE push
- [ ] JWT signing via vault; KEY_ROTATE; EMERGENCY_KEY_REVOKE (C4)
- [ ] Fingerprint diffing for delta resync; TENANT_LIST_REQUEST on mismatch (C6)
- [ ] Takeover (4001); heartbeat/dead detection; unhealthy>1h alert
- [ ] NTP on host; include accurate `server_ts` (C5)
- [ ] EDGE_ERROR / EDGE_METRICS to observability
- [ ] Audit log of all control operations (vault-backed)

---

## 16. References

| Topic | Document |
|-------|----------|
| RT.edge implementation | `rt-edge-spec.md` |
| FC.edge-manager implementation | `fc-edge-manager-spec.md` |
| Vault (JWT key, FC CA key) | `sy-vault-spec.md` |
| Identity / tenants | `10-identity-v2.md` |
| WAN (local edge router ↔ motherbee data channel) | `05-conectividad.md` |
| Egress NAT (sibling edge concern) | `edge-egress-nat-spec.md` |
| Deep hardening (CRL, TPM, isolation) | `edge-security-hardening.md` (to be written) |

---

## 17. Open Questions

1. DNS provider (Cloudflare / Route53 / Azure DNS) — affects FC DNS client. Orthogonal to protocol.
2. FC internal CA: in-process (`rcgen`) vs external (step-ca/smallstep).
3. FC single instance for alpha vs active-passive from day one.
4. JWT algorithm: EdDSA recommended.
5. Per-edge ACME account ownership and where the ACME account key lives (vault).
6. Expected-IP-range source for C3 validation: static config per edge, or derived from cloud provider CIDRs.
7. Bootstrap token format: random 64-char ASCII vs structured + signature.
