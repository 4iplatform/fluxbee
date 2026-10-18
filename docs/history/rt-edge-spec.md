# Fluxbee — RT.edge Specification

**Status:** v1.0 alpha — implementation design
**Date:** 2026-06-30
**Audience:** RT.edge developer (Rust)
**Related:** `edge-control-protocol-v2.md`, `fc-edge-manager-spec.md`, `05-conectividad.md`, `02-protocolo.md`, `10-identity-v2.md`, `sy-vault-spec.md`

---

## 1. Purpose and Role

RT.edge is the **public ingress runtime** of Fluxbee. It is the only component with a public-internet-facing interface, running on a Fluxbee DMZ host. It terminates user HTTPS, authenticates requests, translates them into Fluxbee messages, and injects them into the **local edge router** over the normal node socket. The local router / `RT.gateway` on the private interface owns WAN connectivity to motherbee. RT.edge is not itself a WAN peer.

RT.edge is **stateless and reconstructible**: all durable state (certs, keys, tenant assignments) is pushed to it by FC.edge-manager over the control channel and cached locally. A wiped RT.edge re-bootstraps and re-syncs.

What RT.edge does:

- Terminates user HTTPS using its **per-edge** TLS cert (`<edge_id>.fluxbee.ai`).
- Validates user JWTs locally (stateless) using the JWT public key.
- Publishes `https://<edge_id>.fluxbee.ai/x/<tenant>/<ilk>[/extra]` as an external L3 endpoint for an allowed `(tenant_id, ilk)`.
- Translates HTTP ⇄ Fluxbee L2 (`http.req` / `http.res` envelopes).
- Maintains the control channel to FC.edge-manager (`edge-control-protocol-v2.md`).
- Connects as a specialized node to the local edge router over the Fluxbee router socket. The router handles WAN inward to motherbee.
- Enforces per-IP and per-tenant rate limits.
- Holds the dynamic publication registry (`tenant_id + ilk` → externally published policy) populated by `EDGE_REGISTER` from IO nodes.

What RT.edge does NOT do:

- Serve HTML or any UI (that is Fluxbee Cloud).
- Hold any private signing key (JWT private key lives in Fluxbee Cloud / vault).
- Make tenant-assignment or credential-issuance decisions (that is FC.edge-manager).
- Run `SY.identity` or require identity SHM on the edge host. L3 identity resolution happens after traffic enters Fluxbee core.
- Resolve ILKs to handler L2 names locally in the default security posture.
- Talk to other edges.

---

## 2. Architecture

```
                         Internet
                            │
                            ▼ HTTPS :443 (per-edge cert)
        ┌───────────────────────────────────────────┐
        │                 RT.edge                    │
        │                                            │
        │  ┌──────────────────────────────────────┐  │
        │  │ HTTPS frontend (axum + rustls)       │  │
        │  │  - TLS termination (per-edge cert)   │  │
        │  │  - JWT validation (tower middleware) │  │
        │  │  - rate limit (tower-governor)       │  │
        │  │  - path parser → tenant + ilk        │  │
        │  └──────────────┬───────────────────────┘  │
        │                 │ http.req / http.res        │
        │  ┌──────────────▼───────────────────────┐  │
        │  │ L3 publication registry + translator │  │
        │  │  - tenant+ilk → published policy     │  │
        │  │  - conn_id correlation               │  │
        │  └──────────────┬───────────────────────┘  │
        │                 │ L2 envelope                │
        │  ┌──────────────▼───────────────────────┐  │
        │  │ Router socket client                 │  │
        │  │  - fluxbee_sdk / RouterDispatcher    │  │
        │  └──────────────────────────────────────┘  │
        │                                            │
        │  ┌──────────────────────────────────────┐  │
        │  │ Control channel client               │  │
        │  │  (tokio-tungstenite → FC.edge-mgr)   │  │
        │  │  - cert/key/tenant state             │  │
        │  └──────────────────────────────────────┘  │
        └───────────────────────────────────────────┘
                            │ Unix socket
                            ▼
                    Local edge router
                            │ private NIC, WAN/TLS
                            ▼
                         Motherbee
```

Four concurrent subsystems sharing in-memory state via `Arc<RwLock<...>>` (or `arc-swap` for read-mostly hot paths like the cert and the registry):

1. **HTTPS frontend** — accepts user traffic.
2. **L3 publication registry + translator** — validates published `(tenant_id, ilk)` endpoints and builds Fluxbee messages.
3. **Router socket client** — injects messages into the local edge router; the router / `RT.gateway` handles WAN.
4. **Control channel client** — credential/state sync with FC.

Security boundary note: the edge host does not need to run the full SY set. In particular, it should not require `SY.identity` for the alpha ingress path. RT.edge carries L3 metadata inward; Fluxbee core resolves it where identity and policy live.

---

## 3. Crate Selection

| Concern | Crate |
|---------|-------|
| Async runtime | `tokio` |
| HTTP server | `axum` |
| TLS | `rustls` + `tokio-rustls` (pure Rust, no OpenSSL) |
| Middleware | `tower`, `tower-http` |
| Rate limiting | `tower-governor` |
| WebSocket (control) | `tokio-tungstenite` |
| JWT | `jsonwebtoken` (EdDSA support) |
| Router socket | `fluxbee_sdk::RouterDispatcher` |
| Hot-swap state | `arc-swap` |
| Serialization | `serde`, `serde_json` |
| Logging/metrics | `tracing`, `tracing-subscriber`, `metrics` |

Rationale matches `edge-control-protocol-v2.md` §13: rustls avoids the OpenSSL chain; axum+tower give composable middleware; tokio-tungstenite is the cleanest WS for the control channel.

---

## 4. HTTP → L2 Envelope

### 4.1 Request envelope (`http.req`)

Built by RT.edge after TLS termination, JWT validation, path parsing, and local publication checks. Sent inward as a Fluxbee message with `meta.msg = "http.req"` and L3 metadata:

- `meta.dst_ilk = <ilk from the URL>`
- `meta.src_ilk = <principal.user_ilk>` when the JWT has a user ILK
- `routing.dst = <core_ingress_l2>` in the default alpha posture, for example `RT.edge.ingress@motherbee`

RT.edge does **not** put the ILK directly in `routing.dst`. In the security posture where `SY.identity` does not run on the edge host, RT.edge also does **not** emit `Destination::Resolve` locally, because the local edge router would need identity/OPA data to resolve it. Instead, RT.edge sends the request to a configured core ingress L2 target inside Fluxbee proper. That core-side ingress/resolver performs L3 → L2 resolution using identity/OPA and returns `http.res` to RT.edge over the same Fluxbee path.

If a future edge deployment deliberately runs a minimal identity/OPA projection, this can be relaxed additively to allow local `Destination::Resolve`; it is not the default v1 alpha path.

```json
{
  "v": 1,
  "kind": "http.req",
  "conn_id": "<uuid generated by the edge for correlation>",
  "ts": "2026-06-30T10:00:00Z",
  "method": "POST",
  "path": "/extra/sub/path",
  "query": { "k": "v" },
  "headers": { "content-type": "application/json", "...": "filtered" },
  "principal": {
    "tenant_id": "tnt:...",
    "user_ilk": "ilk:...",
    "scopes": ["..."]
  },
  "body_inline": "<string or null>",
  "body_blob": null
}
```

Rules:

- `path` is what follows `/x/<tenant>/<ilk>`, so the destination node never sees the mount prefix.
- `headers` are filtered: hop-by-hop headers and the raw `Authorization` are stripped. The validated identity is in `principal`, not in a raw token.
- `principal` is present only if a valid JWT was supplied. For an explicitly public endpoint, `principal` is `null`. There is no "unauthenticated but claims an identity" state — either the JWT validated or there is no principal.
- Body handling per §4.3.

### 4.2 Response envelope (`http.res`)

The destination node replies with `meta.msg = "http.res"`, correlated by `conn_id`.

```json
{
  "v": 1,
  "kind": "http.res",
  "conn_id": "<same as request>",
  "status": 200,
  "headers": { "content-type": "application/json" },
  "body_inline": "<string or null>",
  "body_blob": null
}
```

RT.edge maps this back to an HTTP response on the still-open client connection identified by `conn_id`.

### 4.3 Body and blob handling

**PENDING REVISION before implementation.**

The current Fluxbee router frame limit is 128 KiB, and the existing SDK auto-spill path is tied to `text/v1` payloads with a lower effective inline threshold. `http.req` / `http.res` is a new payload family, so the exact body contract must be reviewed before coding.

Implementation must decide:

- the safe inline ceiling after accounting for the full Fluxbee envelope, not just raw body size;
- whether non-UTF-8 bodies are always blob-backed or base64 inline is allowed;
- whether `body_blob` uses the existing canonical `BlobRef` shape (`type`, `blob_name`, `size`, `mime`, `filename_original`, `spool_day`) instead of a new `{ ref, size, ctype }` shape;
- how blob availability is guaranteed across the edge hive and motherbee before the target node reads it.

Alpha-safe default: cap inline bodies conservatively and reject or explicitly mark large/body-binary paths as not implemented until this revision is closed.

### 4.4 Sync model and timeouts

HTTP is request/response with a client waiting on an open connection. RT.edge holds the client connection open, keyed by `conn_id`, until the `http.res` arrives or a timeout fires.

| Timeout | Default | On expiry |
|---------|---------|-----------|
| Inward response | 30 s | `504 Gateway Timeout` to client, drop the pending `conn_id` |
| Idle client | 60 s | close connection |

Streaming, SSE, and WebSocket upgrade are **out of scope in v1** (Fluxbee has no streaming inter-node contract yet). Requests that ask for an upgrade get `501 Not Implemented`. This is declared explicitly so it does not surface later as a bug.

---

## 5. Path Routing

```
https://<edge_id>.fluxbee.ai/x/<tenant>/<ilk>[/extra/path...]
```

- `/x/` is the external-traffic prefix.
- `<tenant>` is the tenant id (also present in the JWT — they must match, §6).
- `<ilk>` identifies the L3 destination being exposed. RT.edge only verifies that this `(tenant, ilk)` is published on this edge. It does not resolve the ILK to a handler node locally.
- `[/extra/path...]` becomes `path` in the envelope.

Routing steps:

1. Parse `tenant`, `ilk`, `extra`.
2. Confirm `tenant` is in the edge's accepted-tenant set (else `404 Tenant not served by this edge`).
3. Confirm JWT `tenant` claim matches path `tenant` (else `403`).
4. Look up `(tenant, ilk)` in the L3 publication registry (§7). If absent / not published ⇒ `404` or `503` depending on policy.
5. Build `http.req` with `meta.dst_ilk = ilk` and send it to configured `core_ingress_l2` through the local router socket.
6. The core ingress/resolver inside Fluxbee proper resolves L3 using identity/OPA and returns `http.res`; RT.edge awaits it by `conn_id` / trace correlation.

---

## 6. JWT Validation

Stateless, no network call at request time.

1. Extract `Authorization: Bearer <jwt>`. Absent ⇒ `principal = null` (continue only if the endpoint is public; otherwise `401`).
2. Verify signature against the current JWT public key (and the previous key if within grace, per `KEY_ROTATE`). A key under `EMERGENCY_KEY_REVOKE` is rejected immediately.
3. Verify claims: `exp` not passed, `iat` consistent with key `effective_at`, `aud` includes this edge / `fluxbee.edge`, `tenant` present.
4. Cross-check `jwt.tenant == path.tenant` (defense in depth; §5 step 3).
5. Populate `principal { tenant_id, user_ilk, scopes }` into the envelope.

The edge holds JWT **public** keys only. It can verify, never mint.

---

## 7. L3 Publication Registry (data plane, EDGE_REGISTER)

This is **not** part of the control channel. It travels over the Fluxbee data plane, because it is public endpoint publication, not edge administration.

The registry is intentionally **L3-only** on the edge:

- key: `(tenant_id, ilk[, optional_subpath])`
- value: publish policy (`protected/public`, method/path policy, limits, lease)
- no local `handler_node` / L2 routing decision is required

This keeps `SY.identity` out of the edge host. RT.edge only answers "is this L3 endpoint allowed to receive internet traffic on this edge?" The actual L3 → L2 target resolution happens inside Fluxbee core after the request crosses the local edge router / WAN boundary.

### 7.1 Registration

An IO node (or a core-side publication manager acting on behalf of an IO node) that needs a public endpoint sends, over normal Fluxbee routing, to this RT.edge instance:

```json
{
  "meta": { "type": "system", "msg": "EDGE_REGISTER" },
  "payload": {
    "ilk": "ilk:...",
    "tenant_id": "tnt:...",
    "optional_subpath": "webhooks/stripe",
    "auth": { "mode": "jwt_required" },
    "methods": ["POST"],
    "lease_seconds": 300
  }
}
```

RT.edge validates that `tenant_id` is assigned to this edge by the control plane, records the L3 publication lease, and replies (correlated by `trace_id`, mirroring the `CONFIG_CHANGED` / `CONFIG_RESPONSE` pattern):

```json
{
  "meta": { "type": "system", "msg": "EDGE_REGISTER_ACK" },
  "payload": {
    "ilk": "ilk:...",
    "tenant_id": "tnt:...",
    "url": "https://eu-edge-1.fluxbee.ai/x/tnt:.../ilk:...",
    "lease_until": "2026-06-30T10:05:00Z"
  }
}
```

The router-stamped `source_l2_name` is useful for audit and abuse debugging, but RT.edge does not use it as the runtime routing target for internet requests.

### 7.2 Lifecycle

- The endpoint is alive while its registration lease is fresh and its tenant assignment is still active on this edge.
- The publisher refreshes before `lease_seconds` expires. Missing refresh or explicit `EDGE_UNREGISTER` removes the endpoint after a grace period.
- If the internal target behind an ILK is gone, core-side routing returns an `http.res` error / transport error; RT.edge maps that to `503` or `504`.
- Re-registering the same `(tenant_id, ilk, optional_subpath)` is idempotent and preserves the same URL.

### 7.3 Registry vs control channel vs identity

The registry is local to RT.edge and rebuilt from `EDGE_REGISTER` traffic. It is **not** synced to FC.edge-manager. FC knows tenants (control channel) but not individual endpoints (data plane). `SY.identity` remains inside Fluxbee proper and is not required on the edge host. Clean plane separation.

---

## 8. Rate Limiting

`tower-governor`, two layers, defaults pushed via `EDGE_HELLO_ACK` / `CONFIG_UPDATE`:

| Layer | Default | Key |
|-------|---------|-----|
| Per source IP | 100 rps | client IP |
| Per tenant | 1000 rps | path/JWT tenant |

Abnormal tripping emits `EDGE_ERROR { code: RATE_LIMIT_TRIPPED }` to FC. Heavy DDoS is expected to be handled upstream (cloud LB/WAF) in production; the in-edge limiter protects against trivial abuse without that.

---

## 9. Process Lifecycle

### 9.1 Boot

1. Load `/etc/fluxbee/edge.conf` (`edge_id`, FC control URL, local router socket, core ingress L2 target, expected public IP range).
2. If `edge.bootstrap` present and no control cert ⇒ bootstrap (`edge-control-protocol-v2.md` §6).
3. Open control channel; `EDGE_HELLO` with fingerprints; receive cert/keys/tenants via delta resync.
4. Only once a valid per-edge TLS cert and JWT public key are loaded does the HTTPS listener bind `:443`. Before that, the edge serves nothing (fail closed).
5. Connect to the local edge router socket as a Fluxbee node.
6. Begin accepting traffic.

### 9.2 Steady state

Four subsystems run concurrently. The HTTPS frontend reads cert and registry via `arc-swap` for lock-free hot paths. Control-channel pushes swap the cert/keys/tenants atomically.

### 9.3 Degraded modes

| Condition | Behavior |
|-----------|----------|
| Control channel down | Keep serving with cached state; reconnect with backoff |
| Local router socket down | New requests get `502 Bad Gateway`; emit `EDGE_ERROR ROUTER_DISCONNECTED`; reconnect to router |
| Local edge router's WAN path to motherbee down | New requests get `502 Bad Gateway` or `504 Gateway Timeout` depending on whether the router returns transport error or the request times out |
| Clock drift > 300 s | Refuse to terminate user TLS until NTP converges (cert validity untrustworthy) |
| TLS cert expiring, no rotation received | Emit `EDGE_ERROR EDGE_TLS_CERT_EXPIRING`; keep serving with current cert until it actually expires |

### 9.4 Shutdown (SIGTERM or EDGE_SHUTDOWN)

Stop accepting new requests; drain in-flight up to `drain_seconds`; send `EDGE_GOODBYE`; close control channel and router socket; exit.

---

## 10. HTTP Error Mapping

| Situation | HTTP status |
|-----------|-------------|
| No/invalid JWT on a protected endpoint | 401 |
| JWT tenant ≠ path tenant | 403 |
| Tenant not served by this edge | 404 |
| `(tenant, ilk)` not published on this edge | 404 or 503 per publication policy |
| Local router socket down | 502 |
| Core ingress / inward route unavailable | 502 |
| Inward response timeout | 504 |
| Rate limit exceeded | 429 |
| Streaming/upgrade requested | 501 |
| Body too large for the supported alpha body mode | 413 |
| Malformed path | 400 |
| Internal edge error | 500 |

L2-level failures returned by the destination node (e.g. `OPA_NO_TARGET`, node error) map to a sensible HTTP status carried in `http.res.status`; RT.edge passes the node's chosen status through.

---

## 11. Metrics

Emitted via `metrics` crate, scraped or shipped to Fluxbee observability, and summarized to FC via `EDGE_METRICS`:

- Requests total, by status class, by tenant.
- Latency p50 / p99 (edge-internal and end-to-end inward).
- Active `conn_id` count (in-flight requests).
- JWT validation failures (rate; spikes ⇒ `JWT_VALIDATION_FAILURES_HIGH`).
- Rate-limit trips.
- Router socket reconnects, WAN transport errors observed via L2 responses, control-channel reconnects.
- Bytes in/out.

---

## 12. Local Configuration

`/etc/fluxbee/edge.conf` (the only hand-set file; everything else arrives over the control channel):

```yaml
edge_id: "eu-edge-1"
control_url: "wss://edge-control.fluxbee.ai/v2/edge"
router_socket: "/var/run/fluxbee/routers/rt-edge.sock"
core_ingress_l2: "RT.edge.ingress@motherbee"
expected_public_ip_cidr: "203.0.113.0/24"   # for EDGE_PUBLIC_IP_CHANGED self-check
listen_https: "0.0.0.0:443"
log_level: "info"
```

Secrets (bootstrap token, certs, keys) live in their own files per `edge-control-protocol-v2.md` §12.8, not here.

---

## 13. Open Questions

1. **HTTP body/blob contract - PENDING REVISION**: exact inline ceiling, binary handling, canonical `BlobRef` shape, blob sync/availability, and large response behavior must be closed before implementation.
2. **`/x/` prefix**: confirm vs `/api/`, `/n/`, `/io/`. Cosmetic but frozen once chosen.
3. **WF as ilk target**: confirm WFs are addressable by ilk identically to nodes, so the edge needs no special-casing.
4. **conn_id lifetime store**: in-memory map of `conn_id → pending client connection`. Bound its size; define eviction on timeout to avoid leak under load.
5. **Public endpoints**: how does the edge know an endpoint is public (no JWT required)? Flag in `EDGE_REGISTER`, or default-all-protected with explicit opt-in. Leaning default-protected.
6. **Header filtering allowlist**: exact set of headers passed inward vs stripped.
7. **Core ingress resolver L2 name**: final canonical name for the internal node that receives edge `http.req` and performs L3 resolution inside Fluxbee core.

---

## 14. Implementation Checklist

- [ ] Config loader (`edge.conf`)
- [ ] Control-channel client (per `edge-control-protocol-v2.md` checklist)
- [ ] rustls HTTPS listener, per-edge cert, hot reload via arc-swap
- [ ] axum router for `/x/<tenant>/<ilk>/*`
- [ ] JWT validation middleware (current + grace key, emergency revoke)
- [ ] tenant-match enforcement (path vs JWT)
- [ ] tower-governor rate limiting (per-IP, per-tenant)
- [ ] http.req builder (header filtering, principal injection)
- [ ] Body handling: pending revision; do not implement large/body-binary path until §4.3 is closed
- [ ] Router socket client to local edge router (`fluxbee_sdk::RouterDispatcher`)
- [ ] conn_id correlation map with timeout eviction
- [ ] http.res → HTTP response mapping
- [ ] L3 publication registry: EDGE_REGISTER / EDGE_UNREGISTER / ACK over L2
- [ ] Registry lifecycle (lease refresh, grace eviction, idempotent re-register)
- [ ] Error mapping table (§10)
- [ ] Metrics emission + EDGE_METRICS
- [ ] Degraded-mode behaviors (§9.3)
- [ ] Graceful shutdown / drain
- [ ] 501 for streaming/upgrade

---

## 15. References

| Topic | Document |
|-------|----------|
| Control protocol (edge ↔ FC) | `edge-control-protocol-v2.md` |
| FC side | `fc-edge-manager-spec.md` |
| WAN protocol (local edge router ↔ motherbee) | `05-conectividad.md` |
| L2 message format, framing | `02-protocolo.md` |
| Identity, tenants, ilk (resolved inside Fluxbee core, not on edge) | `10-identity-v2.md` |
| Blob SDK | `sy-vault-spec.md` / blob docs |
| Egress sibling | `edge-egress-nat-spec.md` |
