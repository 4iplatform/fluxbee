# Fluxbee — FC.edge-manager Specification

**Status:** v1.0 alpha — implementation design
**Date:** 2026-06-30
**Audience:** FC.edge-manager developer (Rust or Node)
**Related:** `edge-control-protocol-v2.md`, `rt-edge-spec.md`, `sy-vault-spec.md`, `10-identity-v2.md`

---

## 1. Purpose and Role

FC.edge-manager is the **control plane** for all RT.edge instances. It lives inside Fluxbee Cloud (a separate infrastructure from Fluxbee proper). It is the authority that:

- Enrolls new edges (bootstrap).
- Issues and rotates each edge's **per-edge** TLS cert (via ACME DNS-01).
- Distributes the JWT signing public key to edges.
- Assigns and revokes tenants per edge.
- Maintains DNS (`<edge_id>.fluxbee.ai` → current IP), validating IP changes.
- Tracks edge health.
- Performs emergency credential revocation.

It is **not** a Fluxbee node. It speaks the control protocol (`edge-control-protocol-v2.md`) to edges, and acts as an ordinary admin-scoped HTTPS client of an edge's data channel when it needs Fluxbee data.

**Critical property (no endpoint catalog):** FC.edge-manager does **not** maintain a mirror of Fluxbee endpoints. It knows only the *structure* of the public path (`/x/<tenant>/<ilk>`), shared with RT.edge. When it needs actual data (which agents exist, tenant config), it queries SY.admin through an edge's data channel using its own admin JWT. It never duplicates Fluxbee's registry.

---

## 2. Architecture

```
                 Browser (admin + users)
                    │ HTTPS (login, UI)
                    ▼
        ┌────────────────────────────────┐
        │        Fluxbee Cloud           │
        │  ┌──────────────────────────┐  │
        │  │ Web app / auth / JWT mint│  │  (JWT private key in vault)
        │  └──────────────────────────┘  │
        │  ┌──────────────────────────┐  │
        │  │     FC.edge-manager      │  │
        │  │  - WSS control server    │  │
        │  │  - internal CA + CRL     │  │
        │  │  - edge registry (DB)    │  │
        │  │  - DNS client            │  │
        │  │  - ACME (per-edge certs) │  │
        │  │  - vault client          │  │
        │  └──────────┬───────────────┘  │
        └─────────────┼──────────────────┘
                      │ control channel (WSS, mTLS)
                      ▼
                  RT.edge instances
```

---

## 3. Components

| Component | Responsibility |
|-----------|----------------|
| **WSS control server** | Accept edge connections (mTLS), run the protocol state machine per edge |
| **Internal CA** | Issue `edge_control_cert`s, maintain CRL checked at every mTLS handshake |
| **Edge registry (DB)** | Persist edge_id, public_ip, expected_ip_ranges, status, last_heartbeat, assigned_tenants, cert ids |
| **Bootstrap token store** | Tokens with ≤15-min TTL, HMAC verification, consumption tracking, per-IP rate limit |
| **DNS client** | Update `<edge_id>.fluxbee.ai` A records; validate IP changes |
| **ACME client** | Issue/renew **per-edge** TLS certs via DNS-01 |
| **Vault client** | Fetch JWT signing material and FC CA private key from SY.vault (never hold them in process memory long-term) |
| **Admin API/UI** | Issue bootstrap tokens, assign tenants, list edges, revoke certs |

---

## 4. Edge Lifecycle

### 4.1 Enrollment (bootstrap)

1. Operator creates an edge entry in the admin UI: `edge_id`, region, `expected_ip_ranges`. UI issues a bootstrap token (≤15-min TTL, bound to that `edge_id`).
2. Operator installs the token on the edge host (`/etc/fluxbee/edge.bootstrap`).
3. Edge connects, sends `EDGE_BOOTSTRAP` with CSR + HMAC (token not transmitted).
4. FC verifies HMAC, TTL, edge_id, CSR; issues `edge_control_cert`; marks token consumed.
5. Edge reconnects via mTLS.
6. FC issues the edge's per-edge TLS cert (ACME, §6), pushes `CERT_ROTATE`, `KEY_ROTATE`, and `TENANT_ASSIGN`s.

Reject path returns an opaque `BOOTSTRAP_REJECTED`; the real cause is logged server-side only. Bootstrap attempts are rate-limited per source IP.

### 4.2 Steady state

Per connected edge, FC maintains a state machine: track heartbeats, answer `EDGE_HELLO` with delta resync (§7), push credential rotations as they happen, validate `EDGE_PUBLIC_IP_CHANGED`.

### 4.3 Health

Heartbeat tracked; no heartbeat 90 s ⇒ mark unhealthy, stop issuing JWTs targeting this edge, remove from DNS pool if pool > 1. Unhealthy > 1 h ⇒ alert ops. No auto-decommission; retirement is an explicit operator action (`4002`).

### 4.4 Decommission

Operator retires an edge: revoke its control cert (CRL), revoke its per-edge TLS cert, reassign its tenants, remove DNS, close connection `4002`.

---

## 5. DNS Management (validated, C3)

FC owns DNS for `*.fluxbee.ai` via a provider API (Cloudflare / Route53 / Azure DNS — open question).

On `EDGE_HELLO` or `EDGE_PUBLIC_IP_CHANGED`:

1. Compare reported `public_ip` against the edge's `expected_ip_ranges`.
2. **In range** ⇒ update `<edge_id>.fluxbee.ai` A record, TTL 60 s; reply `EDGE_DNS_UPDATED`.
3. **Out of range** (different subnet/AS) ⇒ do **not** update; reply `EDGE_PUBLIC_IP_HELD`; alert ops for manual approval.

This is the defense against an attacker with a stolen control cert repointing the edge's hostname to a machine they control (which, combined with a per-edge cert, would otherwise enable MITM of that one hostname).

Low TTL means a legitimate IP change converges within ~60 s with no manual action.

---

## 6. Per-Edge TLS Certificates (C1)

Each edge gets its **own** cert for `<edge_id>.fluxbee.ai`, never a wildcard.

- ACME DNS-01 challenge (FC controls DNS, so it can satisfy the TXT challenge).
- One ACME order per edge hostname.
- Renewal ~30 days before expiry; push via `CERT_ROTATE` (which includes the private key, encrypted in transit by the mTLS control channel).
- The private key MAY be generated by FC (then shipped) or generated by the edge with a CSR-based ACME flow. Leaning FC-generated for operational simplicity in alpha; revisit for stronger key custody later.

Blast radius: a leaked per-edge cert impersonates one hostname, not the platform. Revocation/reissue touches one edge.

ACME account key custody: stored in vault (open question §10).

---

## 7. Tenant Assignment and Delta Resync (C6)

### 7.1 Assignment policy

Tenants are assigned to edges by policy: by region (tenant's primary region → nearest edge), by load, or manually. Each `TENANT_ASSIGN` carries a per-tenant policy (rate limits, concurrency).

### 7.2 Fingerprint-driven resync

On `EDGE_HELLO`, the edge reports `tenants_fingerprint` (hash over its sorted assigned-tenant set) and `tenants_count`.

1. FC computes the same fingerprint over its authoritative set for that edge.
2. **Match** ⇒ no tenant messages needed.
3. **Mismatch** ⇒ FC requests the edge's full list once (`TENANT_LIST_REQUEST`), diffs, and sends only the missing `TENANT_ASSIGN` / surplus `TENANT_REVOKE`.

This avoids streaming thousands of assignments on every reconnect (the v1 ambiguity), while staying convergent.

---

## 8. JWT Signing Key Distribution

- The JWT signing **keypair** is owned by Fluxbee Cloud's auth component; the **private** key lives in SY.vault. FC.edge-manager fetches signing operations or the public key from vault; it does not hold the private key long-term in memory.
- On key rotation, FC pushes `KEY_ROTATE` to all edges (new public key, `effective_at`, previous key grace).
- On suspected compromise, FC pushes `EMERGENCY_KEY_REVOKE` (no grace) to all edges and rotates immediately (C4).
- Edges hold public keys only; they verify, never mint.

---

## 9. Security Model (FC side)

### 9.1 Crown jewel

FC.edge-manager can mint nothing by itself if keys are in vault, but it orchestrates everything: it can issue control certs (full edge takeover), reassign tenants, trigger JWT minting. Therefore:

- JWT private key and FC internal CA private key in **SY.vault**, not in FC process memory. Every sign/issue is a vault call ⇒ audit trail.
- Dedicated, hardened host; not co-tenant with general workloads.
- Admin actions (issue token, revoke cert, assign tenant) behind MFA admin auth.
- Full audit log of every control-plane operation.

### 9.2 Bootstrap defense (C2)

≤15-min token TTL; HMAC-bound CSR (token never transmitted); per-IP rate limit; opaque client rejects; detailed server-side logs.

### 9.3 mTLS + CRL

Every edge connection authenticated by `edge_control_cert`. CRL checked at handshake; a revoked cert cannot connect even with valid TLS.

### 9.4 IP-change validation (C3)

Per §5. Large changes held for human approval.

### 9.5 NTP (C5)

FC host runs NTP; includes an accurate `server_ts` in `EDGE_HELLO_ACK` so edges can self-check drift.

---

## 10. Open Questions

1. **DNS provider**: Cloudflare / Route53 / Azure DNS. Affects the DNS client. (Azure is already in the company for some things; Cloudflare is cheapest/best at this. Decision pending.)
2. **Internal CA**: in-process (`rcgen` if Rust) vs external (step-ca / smallstep). Trade-off: operational complexity vs control.
3. **FC instance model**: single instance for alpha (manual failover) vs active-passive from day one. Edges keep serving on cached state if FC is down, so single instance is tolerable for alpha but must be documented.
4. **ACME account key custody**: vault. Confirm the flow for DNS-01 TXT record creation against the chosen provider.
5. **Per-edge key custody**: FC-generated-and-shipped vs edge-generated CSR. Alpha leans FC-generated; production may want edge-held keys (cert never leaves the edge).
6. **Language**: Rust (single static binary, rock-solid once running — Cesar's stated preference) vs Node (if it shares the Fluxbee Cloud stack and that stack is already validated). Protocol is language-agnostic.
7. **Cache/invalidation for UI data** (deferred): when FC serves dashboards, how it caches Fluxbee data and receives `CACHE_INVALIDATE`. Explicitly out of this spec; the control channel will gain one additive message later.

---

## 11. Implementation Checklist

- [ ] WSS server with mTLS; per-edge protocol state machine
- [ ] Internal CA: issue edge_control_certs; CRL; check CRL at handshake
- [ ] Bootstrap token store: ≤15-min TTL, HMAC verify, consumption, per-IP rate limit, opaque reject
- [ ] Edge registry DB: id, ip, expected_ip_ranges, status, last_heartbeat, tenants, cert ids
- [ ] Admin API/UI: issue token, list edges, assign/revoke tenant, revoke cert, retire edge
- [ ] DNS client: A-record update, IP-range validation, hold-and-alert on out-of-range
- [ ] ACME DNS-01: per-edge cert issuance + renewal; CERT_ROTATE push
- [ ] Vault client: JWT signing material, FC CA private key
- [ ] KEY_ROTATE on rotation; EMERGENCY_KEY_REVOKE on compromise
- [ ] Fingerprint diff + TENANT_LIST_REQUEST for delta resync
- [ ] Heartbeat tracking; dead detection 90 s; unhealthy>1h alert
- [ ] Takeover (4001); decommission (4002)
- [ ] NTP; accurate server_ts
- [ ] EDGE_ERROR / EDGE_METRICS to observability
- [ ] Full audit log (vault-backed) of control-plane ops
- [ ] Graceful shutdown coordination across edges

---

## 12. References

| Topic | Document |
|-------|----------|
| Control protocol | `edge-control-protocol-v2.md` |
| Edge side | `rt-edge-spec.md` |
| Vault (JWT key, CA key) | `sy-vault-spec.md` |
| Identity, tenants | `10-identity-v2.md` |
| Deep hardening | `edge-security-hardening.md` (to be written) |
