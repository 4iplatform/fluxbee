# Fluxbee — Edge Control Protocol Specification

**Status:** v1.0 draft — implementation-ready after closed design decisions
**Date:** 2026-05-19
**Audience:** RT.edge developer, FC.edge-manager developer
**Related:** `rt-edge-spec.md`, `fc-edge-manager-spec.md`, `sy-vault-spec.md`, `10-identity-v2.md`, `05-conectividad.md`

---

## 1. Purpose

This document specifies the wire protocol used between **RT.edge** instances (running on Fluxbee infrastructure, DMZ hosts with dual NICs) and **FC.edge-manager** (the control plane process running inside Fluxbee Cloud).

The protocol is bidirectional, persistent, and carries **control plane traffic only** — never user data, never application traffic.

The protocol must allow:

- A new RT.edge to bootstrap and authenticate against FC.edge-manager.
- Continuous health monitoring of RT.edges by FC.edge-manager.
- Distribution of credentials (wildcard TLS certificate, JWT signing public key) from FC.edge-manager to RT.edges.
- Assignment and revocation of tenants to/from specific RT.edges based on region or policy.
- Reporting of edge state (public IP, capacity, load) from RT.edges back to FC.edge-manager.
- Graceful shutdown coordination.

The protocol explicitly does **NOT** carry:

- User HTTP traffic. Browsers connect directly to RT.edges using the wildcard cert. RT.edges translate to Fluxbee L2 envelopes and forward via WAN to motherbee.
- Endpoint registration of IO.api or any application node (`EDGE_REGISTER` / `EDGE_UNREGISTER`). This is Fluxbee data plane and travels over the WAN protocol between motherbee and RT.edge as standard L2 messages.
- Admin queries against Fluxbee data (`list agents of tenant X`, `get tenant config`, etc.). FC.edge-manager performs these as a regular HTTPS client of the RT.edge using its own admin-scoped JWT.

---

## 2. Design Principles

**2.1 RT.edge always initiates.** RT.edge connects outbound to FC.edge-manager on a stable, hardcoded URL. FC.edge-manager never connects to RT.edges. This preserves Fluxbee's "everything outbound from the inside" principle.

**2.2 Single persistent connection per edge.** Each RT.edge maintains exactly one active control channel to FC.edge-manager. Duplicate connections from the same `edge_id` are rejected by FC; takeover semantics apply (§9.3).

**2.3 Control plane only.** No user data, no per-request lookups. If a piece of information is needed at HTTP request time, it must be cached locally at the RT.edge. The control channel pushes state asynchronously; it does not respond synchronously to data-plane queries.

**2.4 Cached state outlives the channel.** The data plane (browser → RT.edge → motherbee) must continue functioning when the control channel is down, using last-known state (current wildcard cert, current JWT public key, current tenant assignments). The control channel is for state evolution, not steady-state operation.

**2.5 JSON envelope for evolution.** Messages use a JSON envelope similar in spirit to Fluxbee's L2 message format. This allows additive evolution of message types without breaking existing peers. New fields are ignored by older peers; new message types unknown to a peer are logged and discarded.

**2.6 Versioned protocol.** Every message carries a protocol version. Peers negotiate the version on connection. Backward-incompatible changes require a major version bump and coordinated rollout.

**2.7 Mutual TLS authentication after bootstrap.** The bootstrap token authenticates the very first connection. All subsequent connections use mTLS with a long-lived edge certificate issued by FC.edge-manager's internal CA. The bootstrap token is single-use and discarded immediately after.

**2.8 Stateless re-synchronization on reconnect.** When the WebSocket drops, the RT.edge reconnects with backoff and both peers re-synchronize state from scratch via a fresh `EDGE_HELLO` and a full configuration push from FC.edge-manager. No session continuity is assumed across reconnects.

**2.9 No silent failures.** Every long-running task (cert distribution, tenant assignment, key rotation) produces an explicit acknowledgement message. Either peer can detect missing acknowledgements and retry or escalate.

---

## 3. Terminology

| Term | Meaning |
|------|---------|
| **RT.edge** | The Rust process running on a Fluxbee DMZ host. One per edge instance. |
| **FC.edge-manager** | The control plane process inside Fluxbee Cloud. Manages all RT.edges. |
| **Control channel** | The persistent WebSocket-over-TLS connection between an RT.edge and FC.edge-manager. |
| **Data channel** | The HTTPS path from any client (including FC.edge-manager itself acting as admin) to an RT.edge for application traffic. Out of scope for this document. |
| **edge_id** | Stable identifier of an RT.edge instance (e.g., `eu-edge-1`, `us-edge-2`). Assigned at provisioning, immutable. |
| **edge_bootstrap_token** | Single-use opaque token issued by FC admin UI for first-time edge enrollment. |
| **edge_cert** | Long-lived X.509 certificate issued by FC.edge-manager's internal CA. Used for mTLS authentication of the control channel after bootstrap. Not used for any other purpose. |
| **wildcard_cert** | The public TLS certificate for `*.fluxbee.ai` issued by Let's Encrypt. Used by RT.edges to terminate user HTTPS. Distributed via control channel. |
| **jwt_public_key** | Public key of Fluxbee Cloud's JWT signing keypair. Used by RT.edges to validate user JWTs locally. Distributed via control channel. |
| **tenant_assignment** | A mapping declaring that a given `tenant_id` is served by a specific RT.edge. RT.edge accepts HTTP requests for assigned tenants and rejects others. |

---

## 4. Transport

### 4.1 WebSocket over TLS

- URI scheme: `wss://`
- Default port: `443`
- TLS version: `1.3` minimum
- Cipher suites: TLS 1.3 defaults (no further restriction)
- Authentication: mTLS using `edge_cert` (except during bootstrap, see §6)
- WebSocket subprotocol header: `fluxbee-edge-control.v1`

The RT.edge MUST validate FC.edge-manager's server certificate against a pinned CA bundle located at `/etc/fluxbee/edge.ca.pem`. Standard browser trust stores are not used; pinning prevents downgrade attacks if a public CA is compromised.

### 4.2 Endpoint

FC.edge-manager exposes a single, hardcoded endpoint:

```
wss://edge-control.fluxbee.ai/v1/edge
```

This URL is loaded from `/etc/fluxbee/edge.conf` at RT.edge boot. It MUST NOT change without a coordinated upgrade of all RT.edges.

### 4.3 Framing

WebSocket text frames carry JSON-encoded messages, exactly one message per frame. Binary frames are not used in v1. Maximum frame size: 1 MiB. Messages larger than that indicate a protocol error and the connection MUST be closed by either peer with close code `1009`.

### 4.4 Keepalive

Two independent keepalive mechanisms operate in parallel:

| Mechanism | Direction | Interval | Timeout | Action on timeout |
|-----------|-----------|----------|---------|-------------------|
| WebSocket ping | RT.edge → FC | 15 s | 30 s | Close connection, reconnect |
| `EDGE_HEARTBEAT` | RT.edge → FC | 30 s | 90 s | FC marks edge as `unhealthy`, drops outbound queue |

WebSocket-level pings detect dead TCP/TLS connections. Application-level heartbeats detect dead RT.edge processes (e.g., RT.edge crashed but TCP socket is still being half-held by the kernel).

### 4.5 Reconnection

When the connection drops for any reason, the RT.edge reconnects with exponential backoff:

| Parameter | Value |
|-----------|-------|
| Initial delay | 1 s |
| Max delay | 60 s |
| Backoff factor | 2 |
| Jitter | ±20% |

Reconnection is unconditional. RT.edges never give up. The data plane continues to serve traffic during reconnection using cached state.

FC.edge-manager MUST tolerate rapid reconnections (e.g., from a flapping network) without amplifying load — it should rate-limit `EDGE_HELLO` processing per `edge_id` if needed.

---

## 5. Message Envelope

All messages share a common envelope:

```json
{
  "v": 1,
  "msg_id": "550e8400-e29b-41d4-a716-446655440000",
  "msg_type": "EDGE_HELLO",
  "ts": "2026-05-19T10:00:00.000Z",
  "in_reply_to": null,
  "payload": { }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `v` | integer | yes | Protocol version. Always `1` in this spec. |
| `msg_id` | UUID v4 | yes | Unique identifier for this message. Used for correlation. |
| `msg_type` | string | yes | One of the message types defined in §6, §7, §8. |
| `ts` | ISO-8601 string | yes | Sender's wall clock time. Informational; not used for ordering or replay protection. |
| `in_reply_to` | UUID v4 or null | conditional | Required when this message is a response to a previous one. Set to `null` otherwise. |
| `payload` | object | yes | Message-specific content. May be `{}` if no payload. |

### 5.1 Unknown message types

If a peer receives a `msg_type` it does not recognize, it MUST log a warning with `msg_id` and `msg_type`, discard the message, and continue the session. It MUST NOT close the connection.

This rule enables forward compatibility: FC.edge-manager can introduce new messages that older RT.edges silently ignore until upgraded.

### 5.2 Unknown fields

Unknown fields in a `payload` MUST be ignored. Peers MUST NOT echo back unknown fields they do not understand.

---

## 6. Bootstrap Protocol

### 6.1 Pre-conditions

Before bootstrap, the RT.edge has on disk:

| File | Permissions | Content |
|------|-------------|---------|
| `/etc/fluxbee/edge.conf` | `0644` | YAML with `edge_id`, FC control URL, motherbee WAN endpoint |
| `/etc/fluxbee/edge.bootstrap` | `0600` | The `edge_bootstrap_token` as plain ASCII |
| `/etc/fluxbee/edge.ca.pem` | `0644` | PEM-encoded FC internal CA bundle (pinned trust anchor) |

The RT.edge has NO `edge_cert` yet. The first connection uses server-side TLS only (RT.edge validates FC's server cert against the pinned CA, but does not present a client cert).

### 6.2 EDGE_BOOTSTRAP

The first message the RT.edge sends after WebSocket upgrade MUST be `EDGE_BOOTSTRAP`:

```json
{
  "v": 1,
  "msg_id": "<uuid>",
  "msg_type": "EDGE_BOOTSTRAP",
  "ts": "...",
  "in_reply_to": null,
  "payload": {
    "edge_id": "eu-edge-1",
    "bootstrap_token": "<opaque ASCII string, 64 chars>",
    "csr": "<PEM-encoded PKCS#10 Certificate Signing Request>",
    "edge_version": "1.0.0",
    "edge_platform": "linux-x86_64",
    "public_ip": "203.0.113.42"
  }
}
```

The CSR contains the RT.edge's freshly-generated public key. The corresponding private key is generated locally and never leaves the RT.edge host.

### 6.3 EDGE_BOOTSTRAP_ACK

FC.edge-manager validates the request:

1. The `bootstrap_token` exists in FC's pending-tokens table.
2. The token has not been used.
3. The token has not expired (default validity: 24 h from issuance).
4. The `edge_id` in the message matches the `edge_id` the token was issued for.
5. The CSR is well-formed and uses an acceptable algorithm (Ed25519 or P-256).

On success:

```json
{
  "v": 1,
  "msg_id": "<uuid>",
  "msg_type": "EDGE_BOOTSTRAP_ACK",
  "in_reply_to": "<bootstrap msg_id>",
  "payload": {
    "edge_cert": "<PEM-encoded X.509 certificate signed by FC CA>",
    "edge_cert_chain": "<PEM-encoded intermediate(s) to FC CA root>",
    "valid_until": "2027-05-19T10:00:00Z"
  }
}
```

The RT.edge:

1. Persists `edge_cert` to `/etc/fluxbee/edge.cert.pem` (`0644`).
2. Persists the cert chain to `/etc/fluxbee/edge.chain.pem` (`0644`).
3. Persists the private key to `/etc/fluxbee/edge.key.pem` (`0600`).
4. Deletes `/etc/fluxbee/edge.bootstrap`.
5. Closes the current WebSocket connection (close code `1000`, reason `BOOTSTRAP_COMPLETE`).
6. Reconnects immediately, this time presenting `edge_cert` as client certificate (mTLS).

### 6.4 EDGE_BOOTSTRAP_REJECT

If any validation fails:

```json
{
  "v": 1,
  "msg_id": "<uuid>",
  "msg_type": "EDGE_BOOTSTRAP_REJECT",
  "in_reply_to": "<bootstrap msg_id>",
  "payload": {
    "reason_code": "TOKEN_INVALID",
    "message": "Bootstrap token not found or already consumed"
  }
}
```

| `reason_code` | Description |
|---------------|-------------|
| `TOKEN_INVALID` | Token unknown or malformed |
| `TOKEN_EXPIRED` | Token past its expiration timestamp |
| `TOKEN_ALREADY_USED` | Token was already consumed by a previous bootstrap |
| `EDGE_ID_MISMATCH` | Token issued for a different `edge_id` |
| `CSR_INVALID` | CSR malformed or uses unacceptable algorithm |

FC.edge-manager closes the connection with code `1008` (policy violation). The RT.edge logs the error, alerts via local mechanism (syslog, optional webhook to ops), and retries with backoff. It does NOT discard the bootstrap_token on its end — the operator may re-issue a fresh token at FC.

### 6.5 Cert renewal

Edge certs are valid for 1 year. Starting 30 days before expiration, the RT.edge initiates renewal after each successful `HEARTBEAT_ACK`:

```json
{
  "msg_type": "EDGE_CERT_RENEW",
  "payload": { "csr": "<PEM>" }
}
```

FC.edge-manager validates the request comes from the still-valid current cert (via mTLS), then responds:

```json
{
  "msg_type": "EDGE_CERT_RENEW_ACK",
  "in_reply_to": "<renew msg_id>",
  "payload": {
    "edge_cert": "<PEM>",
    "edge_cert_chain": "<PEM>",
    "valid_until": "..."
  }
}
```

The RT.edge persists the new cert atomically (write to temp file, rename) and keeps using the old cert for the current TCP connection. On next reconnect (which may be forced by the RT.edge via `EDGE_RECONNECT_REQUEST` after persisting the new cert), it presents the new cert.

---

## 7. Steady-State Messages: RT.edge → FC.edge-manager

### 7.1 EDGE_HELLO

Sent as the first message after every mTLS-authenticated WebSocket connect (i.e., on initial connection after bootstrap, and on every reconnect). Reports current state.

```json
{
  "msg_type": "EDGE_HELLO",
  "payload": {
    "edge_id": "eu-edge-1",
    "edge_version": "1.0.0",
    "public_ip": "203.0.113.42",
    "region": "eu-west-1",
    "capacity": {
      "max_concurrent_requests": 10000,
      "max_tenants": 1024
    },
    "current_state": {
      "tenants_known": ["tnt:aaa...", "tnt:bbb..."],
      "wildcard_cert_fingerprint": "sha256:abcd...",
      "jwt_key_id": "fc-jwt-key-2026-01",
      "motherbee_connected": true,
      "uptime_seconds": 3600
    }
  }
}
```

`current_state` tells FC.edge-manager what the RT.edge currently has loaded. FC.edge-manager uses this to compute the delta and push only what has changed since the RT.edge last had state from FC (cert rotated, new tenants assigned, JWT key rotated). On a fresh RT.edge boot, all fields are absent or empty and FC pushes everything.

### 7.2 EDGE_HELLO_ACK

FC.edge-manager replies with the authoritative state:

```json
{
  "msg_type": "EDGE_HELLO_ACK",
  "in_reply_to": "<hello msg_id>",
  "payload": {
    "accepted": true,
    "fc_version": "1.0.0",
    "config": {
      "heartbeat_interval_seconds": 30,
      "rate_limit_per_ip_rps": 100,
      "rate_limit_per_tenant_rps": 1000
    }
  }
}
```

If `accepted` is `false`, the message includes `reason_code` and `message` and the connection is closed (e.g., `EDGE_DECOMMISSIONED`, `EDGE_ID_REVOKED`).

Immediately after `EDGE_HELLO_ACK`, FC.edge-manager begins pushing state via `TENANT_ASSIGN`, `KEY_ROTATE`, `CERT_ROTATE` as needed. The RT.edge MUST tolerate this batch arriving in any order and treat each message idempotently.

### 7.3 EDGE_HEARTBEAT

Sent every 30 seconds (configurable via `EDGE_HELLO_ACK.config.heartbeat_interval_seconds`):

```json
{
  "msg_type": "EDGE_HEARTBEAT",
  "payload": {
    "uptime_seconds": 7200,
    "motherbee_connected": true,
    "active_requests": 42,
    "memory_mb": 128,
    "cpu_percent": 12.5
  }
}
```

FC.edge-manager replies with `EDGE_HEARTBEAT_ACK` (which MAY be empty payload) or simply records the heartbeat without reply. The RT.edge does NOT require an acknowledgement for each heartbeat — the WebSocket ping/pong is sufficient for liveness. The `EDGE_HEARTBEAT_ACK` is optional and used by FC to opportunistically piggyback config nudges.

### 7.4 EDGE_PUBLIC_IP_CHANGED

Sent if the RT.edge detects its public IP has changed during runtime (rare but possible with elastic IP reassignment or NAT egress reconfiguration):

```json
{
  "msg_type": "EDGE_PUBLIC_IP_CHANGED",
  "payload": {
    "old_ip": "203.0.113.42",
    "new_ip": "203.0.113.43",
    "detected_at": "..."
  }
}
```

FC.edge-manager responds by updating the DNS A record (`<edge_id>.fluxbee.ai`) via its DNS provider API. Confirmation comes via `EDGE_DNS_UPDATED` (see §8.7).

### 7.5 EDGE_METRICS

Optional, sent every 60 seconds if `metrics_enabled` is true in `EDGE_HELLO_ACK.config`:

```json
{
  "msg_type": "EDGE_METRICS",
  "payload": {
    "window_seconds": 60,
    "requests_total": 12345,
    "requests_by_status": { "2xx": 12000, "4xx": 300, "5xx": 45 },
    "requests_by_tenant": { "tnt:aaa": 8000, "tnt:bbb": 4345 },
    "p50_latency_ms": 35,
    "p99_latency_ms": 220,
    "bytes_in": 12345678,
    "bytes_out": 98765432
  }
}
```

FC.edge-manager forwards these to its observability stack. No reply.

### 7.6 EDGE_ERROR

Sent when the RT.edge encounters an operational anomaly worth reporting:

```json
{
  "msg_type": "EDGE_ERROR",
  "payload": {
    "severity": "warning",
    "code": "MOTHERBEE_DISCONNECTED",
    "message": "WAN connection to motherbee dropped, attempting reconnect",
    "details": {}
  }
}
```

| `severity` | Meaning |
|------------|---------|
| `info` | Notable event, no action needed |
| `warning` | Degraded state, self-recovery in progress |
| `error` | Operational issue, manual attention may be required |
| `critical` | Service-affecting, immediate attention needed |

| `code` | When emitted |
|--------|--------------|
| `MOTHERBEE_DISCONNECTED` | WAN link to motherbee dropped |
| `MOTHERBEE_RECONNECTED` | WAN link to motherbee recovered (severity `info`) |
| `WILDCARD_CERT_EXPIRING` | Wildcard cert within 7 days of expiry and no rotation received |
| `RATE_LIMIT_TRIPPED` | Rate limit triggered abnormally often (potential abuse) |
| `JWT_VALIDATION_FAILURES_HIGH` | High rate of invalid JWTs (potential attack) |
| `INTERNAL_ERROR` | Unexpected runtime error in the edge process |

No reply expected. FC.edge-manager logs and alerts as configured.

### 7.7 EDGE_GOODBYE

Sent on graceful shutdown:

```json
{
  "msg_type": "EDGE_GOODBYE",
  "payload": {
    "reason": "OPERATOR_SHUTDOWN",
    "drain_seconds": 30
  }
}
```

FC.edge-manager:

1. Marks the edge as draining.
2. Stops issuing JWTs that target this edge_url.
3. Updates DNS to remove this edge from the regional pool (if applicable).
4. Waits for the connection to close.

The RT.edge stops accepting new HTTP requests, completes in-flight ones (up to `drain_seconds`), then closes the connection with code `1000`, reason `GOODBYE`.

---

## 8. Steady-State Messages: FC.edge-manager → RT.edge

### 8.1 TENANT_ASSIGN

Assigns a tenant to this RT.edge. Idempotent.

```json
{
  "msg_type": "TENANT_ASSIGN",
  "payload": {
    "tenant_id": "tnt:550e8400-e29b-41d4-a716-446655440000",
    "tenant_name": "Acme Corp",
    "policy": {
      "rate_limit_rps": 1000,
      "max_concurrent_requests": 100
    }
  }
}
```

The RT.edge:

1. Adds `tenant_id` to its accepted-tenants set in memory.
2. Persists the set to disk (`/var/lib/fluxbee/edge.tenants.json`) for survival across restarts.
3. Replies with `TENANT_ASSIGN_ACK`.

```json
{
  "msg_type": "TENANT_ASSIGN_ACK",
  "in_reply_to": "<assign msg_id>",
  "payload": { "tenant_id": "tnt:...", "status": "ok" }
}
```

### 8.2 TENANT_REVOKE

Removes a tenant from this RT.edge.

```json
{
  "msg_type": "TENANT_REVOKE",
  "payload": { "tenant_id": "tnt:...", "reason": "REASSIGNED_TO_OTHER_EDGE" }
}
```

The RT.edge removes the tenant from its set, persists, and replies `TENANT_REVOKE_ACK`. New HTTP requests for that tenant return `404 Tenant not served by this edge`.

### 8.3 KEY_ROTATE

Distributes a new JWT signing public key.

```json
{
  "msg_type": "KEY_ROTATE",
  "payload": {
    "key_id": "fc-jwt-key-2026-06",
    "algorithm": "EdDSA",
    "public_key_pem": "<PEM>",
    "effective_at": "2026-06-01T00:00:00Z",
    "previous_key_id": "fc-jwt-key-2026-01",
    "previous_key_grace_until": "2026-06-08T00:00:00Z"
  }
}
```

The RT.edge stores both keys. JWTs are validated against:

- The new key if `iat >= effective_at`, OR
- The previous key if `iat < effective_at` AND now < `previous_key_grace_until`.

After `previous_key_grace_until`, the previous key is discarded.

Reply: `KEY_ROTATE_ACK { key_id, status: "ok" }`.

### 8.4 CERT_ROTATE

Distributes a new wildcard TLS certificate.

```json
{
  "msg_type": "CERT_ROTATE",
  "payload": {
    "cert_id": "wildcard-fluxbee-ai-2026-05",
    "cert_pem": "<PEM>",
    "chain_pem": "<PEM>",
    "private_key_pem": "<PEM>",
    "valid_until": "2026-08-01T00:00:00Z"
  }
}
```

The RT.edge:

1. Persists cert, chain, and key atomically to disk under `/var/lib/fluxbee/edge.tls.*`.
2. Reloads the TLS configuration of its HTTPS listener (hot reload, no process restart).
3. Replies `CERT_ROTATE_ACK { cert_id, status: "ok" }`.

The private key is treated with the same care as `edge_key.pem` (`0600`, never logged).

### 8.5 CONFIG_UPDATE

Updates RT.edge runtime configuration.

```json
{
  "msg_type": "CONFIG_UPDATE",
  "payload": {
    "rate_limit_per_ip_rps": 200,
    "rate_limit_per_tenant_rps": 2000,
    "heartbeat_interval_seconds": 30,
    "metrics_enabled": true,
    "log_level": "info"
  }
}
```

Only fields present are updated. The RT.edge applies the change immediately and replies `CONFIG_UPDATE_ACK { applied_fields: [...], status: "ok" }`.

### 8.6 EDGE_SHUTDOWN

Requests the RT.edge to gracefully shut down.

```json
{
  "msg_type": "EDGE_SHUTDOWN",
  "payload": { "drain_seconds": 30, "reason": "DECOMMISSIONING" }
}
```

The RT.edge proceeds as if it received `SIGTERM`: stops accepting new requests, completes in-flight requests, then sends `EDGE_GOODBYE` and closes.

### 8.7 EDGE_DNS_UPDATED

Notifies the RT.edge that the DNS record has been updated. Informational.

```json
{
  "msg_type": "EDGE_DNS_UPDATED",
  "payload": {
    "fqdn": "eu-edge-1.fluxbee.ai",
    "ip": "203.0.113.43",
    "ttl_seconds": 60,
    "updated_at": "..."
  }
}
```

No reply expected.

### 8.8 EDGE_RECONNECT_REQUEST

Asks the RT.edge to drop and re-establish its control channel. Used when FC.edge-manager wants to force a state resync.

```json
{
  "msg_type": "EDGE_RECONNECT_REQUEST",
  "payload": { "reason": "STATE_RESYNC" }
}
```

The RT.edge closes the connection with code `1000`, reason `RECONNECT_REQUESTED`, and reconnects immediately (skipping the backoff delay).

---

## 9. Reconnection and Resync

### 9.1 Reconnect flow

1. WebSocket drops (any reason).
2. RT.edge enters reconnect loop with backoff.
3. RT.edge presents `edge_cert` via mTLS, opens new WebSocket.
4. RT.edge sends `EDGE_HELLO` with `current_state` fingerprints.
5. FC.edge-manager replies `EDGE_HELLO_ACK`.
6. FC.edge-manager pushes deltas: missing tenant assignments, new keys/certs if fingerprints differ.
7. Steady state resumes.

### 9.2 Idempotency

All FC → RT messages MUST be idempotent. Receiving `TENANT_ASSIGN` for an already-assigned tenant is a no-op. Receiving `CERT_ROTATE` with the same `cert_id` already loaded is a no-op (reply with `ALREADY_LOADED` status). This allows FC to retransmit aggressively without coordination.

### 9.3 Takeover semantics

If FC.edge-manager has an active connection for `edge_id=X` and receives a new connection claiming the same `edge_id` (authenticated with valid mTLS):

1. FC accepts the new connection.
2. FC closes the old connection with code `4001` (custom), reason `SUPERSEDED_BY_NEW_CONNECTION`.
3. FC treats the new connection as the canonical one.

This handles the case where an RT.edge crashed and a new instance came up before the TCP keepalive expired the old connection on FC's side.

### 9.4 Edge marked dead

If FC.edge-manager does not receive `EDGE_HEARTBEAT` for 90 seconds:

1. The edge is marked `unhealthy`.
2. FC stops issuing JWTs targeting this edge.
3. FC updates DNS to remove this edge from regional pool (if pool > 1).
4. FC continues attempting to accept reconnections from this `edge_id`.

If unhealthy for > 1 hour, FC alerts ops. The edge_id is NOT auto-decommissioned — that requires explicit operator action via admin UI.

---

## 10. Versioning

### 10.1 Protocol version

The `v` field in the envelope is the protocol version. v1 is the only version defined.

### 10.2 Version negotiation

The WebSocket subprotocol header (`fluxbee-edge-control.v1`) and the `v` field together identify the protocol version. Mismatched versions result in connection rejection with close code `1003` (unsupported data).

### 10.3 Backward compatibility within v1

Within v1, the following changes are backward-compatible and do NOT require a version bump:

- Adding new optional fields to existing `payload` objects.
- Adding new `msg_type` values.
- Adding new `reason_code` enum values.

Breaking changes (renaming fields, changing semantics, removing fields) require v2 and a coordinated upgrade.

### 10.4 Software version

`edge_version` (RT.edge) and `fc_version` (FC.edge-manager) are reported in `EDGE_HELLO` / `EDGE_HELLO_ACK` for observability and ops. They do not affect protocol compatibility — semver of the protocol (`v`) is the only contract.

---

## 11. Errors and Close Codes

### 11.1 WebSocket close codes

| Code | Reason | Initiator | When |
|------|--------|-----------|------|
| `1000` | Normal closure | Either | Graceful (bootstrap complete, goodbye, reconnect request) |
| `1001` | Going away | Either | Process shutdown |
| `1003` | Unsupported data | Either | Protocol version mismatch |
| `1008` | Policy violation | FC | Auth failure, bootstrap reject |
| `1009` | Message too big | Either | Frame > 1 MiB |
| `1011` | Internal error | Either | Unexpected failure |
| `4001` | Superseded | FC | New connection for same edge_id (custom) |
| `4002` | Decommissioned | FC | edge_id has been retired (custom) |

### 11.2 Application-level errors

Errors during message processing are reported via the corresponding `*_ACK` message with `status: "error"` and a `reason_code`. The connection is NOT closed for application-level errors unless they indicate a protocol violation.

---

## 12. Security Considerations

### 12.1 Trust model

- **Pinned CA**: RT.edges trust only FC's internal CA (pinned at install time). Public CAs are not trusted for the control channel.
- **Bootstrap token**: a shared secret distributed out-of-band by the operator. Single-use, time-bound.
- **edge_cert**: long-lived but rotatable. Compromise of one edge_cert affects only that edge.
- **FC server cert**: signed by FC's internal CA. RT.edges validate it via the pinned bundle.

### 12.2 What an attacker who obtains an edge_cert can do

- Connect to FC.edge-manager as that specific `edge_id`.
- Receive: wildcard TLS cert (private key!), JWT public key, tenant assignments.
- The wildcard TLS private key is the most sensitive item. Possession allows TLS impersonation of `*.fluxbee.ai`.

**Mitigation**: detect anomalies (duplicate connections from same edge_id from different IPs trigger alerts; FC tracks `public_ip` of every `EDGE_HELLO`). On suspected compromise, operator revokes the edge_cert (its serial is added to a CRL checked by FC at every mTLS handshake) and rotates the wildcard cert immediately.

### 12.3 What an attacker who breaches FC.edge-manager can do

- Issue rogue JWTs (game over for user data plane).
- Issue rogue tenant assignments (steer tenants to compromised edges).
- Issue rogue edge_certs (full edge takeover).

**Mitigation**: FC.edge-manager is the crown jewel. Treat as such. Hardened host, restricted access, dedicated monitoring. The JWT private key and FC internal CA private key live in SY.vault inside Fluxbee proper, not on FC.edge-manager itself — FC.edge-manager calls vault for every sign/issue operation, generating an audit trail.

### 12.4 What an attacker on the public internet can do

- Connect to `edge-control.fluxbee.ai`.
- Without a valid bootstrap_token AND a valid edge_cert, the mTLS handshake fails before any application data is exchanged.
- DoS: floods of TLS handshakes. Mitigated by upstream rate limits at the network layer (cloud LB / WAF if used in front of FC.edge-manager).

### 12.5 What an attacker on Fluxbee's internal LAN can do

- Out of scope for the control channel (no LAN-internal communication uses it).
- RT.edge ↔ motherbee uses WAN/TLS with mTLS (separate cert lineage, see `05-conectividad.md`).

### 12.6 Replay attacks

Each message has a unique `msg_id` (UUID v4). Replays of valid messages are detectable. However, the protocol does NOT enforce strict replay protection in v1 (acceptable because the channel is mTLS-authenticated end-to-end). If replay protection becomes important in v2, a monotonic sequence number per direction can be added.

### 12.7 Secret handling on disk

| File | Permissions | Contains |
|------|-------------|----------|
| `/etc/fluxbee/edge.bootstrap` | `0600` | Bootstrap token (pre-bootstrap only) |
| `/etc/fluxbee/edge.cert.pem` | `0644` | edge_cert (public) |
| `/etc/fluxbee/edge.chain.pem` | `0644` | Cert chain (public) |
| `/etc/fluxbee/edge.key.pem` | `0600` | edge_cert private key |
| `/etc/fluxbee/edge.ca.pem` | `0644` | Pinned FC CA bundle (public) |
| `/var/lib/fluxbee/edge.tls.cert.pem` | `0644` | Wildcard cert (public) |
| `/var/lib/fluxbee/edge.tls.key.pem` | `0600` | Wildcard private key |
| `/var/lib/fluxbee/edge.tls.chain.pem` | `0644` | Wildcard chain (public) |
| `/var/lib/fluxbee/edge.jwt_keys.json` | `0644` | JWT public keys (public material) |
| `/var/lib/fluxbee/edge.tenants.json` | `0644` | Assigned tenants (not sensitive) |

All `0600` files are owned by the user running the RT.edge process. The host is the trust boundary; OS-level access control (and disk encryption if applicable) is the line of defense.

---

## 13. Decisions Log

| Decision | Rationale |
|----------|-----------|
| WebSocket over TLS as transport | Bidirectional, persistent, well-supported in Rust (tokio-tungstenite); JSON payload is schema-free |
| RT.edge always initiates | Preserves outbound-only semantics of Fluxbee infrastructure |
| Single connection per edge_id | Simpler state model; takeover semantics handle crashes |
| mTLS after bootstrap | Standard strong authentication; edge_cert is revocable |
| Bootstrap token single-use | Limits blast radius if token leaks |
| Pinned CA for FC server | Avoids dependency on public PKI for internal control plane |
| Idempotent FC → RT messages | Allows aggressive retransmit without coordination |
| State resync on every reconnect | Stateless reconnection; no replay/sequence machinery in v1 |
| 1-year edge_cert validity | Long enough to be operationally bearable, short enough to bound exposure |
| JWT public key in payload (PEM) | No JWKS endpoint; control channel IS the distribution mechanism |
| Wildcard TLS distributed via control channel | Single cert covers all current and future edges; no per-edge ACME |
| Heartbeat at app level + WS ping | Detects both dead socket and dead process |
| 1 MiB max frame | Generous for control plane; protects against memory blowups |
| Forward-compatible unknown msg_types | Enables additive evolution |
| `v` in every envelope | Explicit version per message; trivial to parse |

---

## 14. What is NOT in v1

- Multi-region or multi-FC failover (single FC.edge-manager assumed).
- Message ordering guarantees beyond per-direction TCP ordering.
- Replay protection beyond mTLS channel security.
- Encryption of message payloads beyond TLS (TLS is sufficient).
- Compression of WebSocket frames.
- Cross-edge coordination (edges do not talk to each other through this channel).
- Hot reload of `edge_id` (immutable for the lifetime of the install).
- Hot reload of the pinned CA bundle (requires process restart).
- Edge groups or pools (edges are individually assigned tenants).
- Bandwidth limits or QoS in the control channel.

---

## 15. Implementation Checklist

### RT.edge (Rust, tokio-tungstenite)

- [ ] Load `/etc/fluxbee/edge.conf` at boot
- [ ] Detect bootstrap mode (presence of `edge.bootstrap`, absence of `edge.cert.pem`)
- [ ] Generate keypair on first boot (Ed25519 preferred), persist private key `0600`
- [ ] Generate CSR for bootstrap
- [ ] Implement WebSocket-over-TLS client with rustls
- [ ] Server cert validation against pinned CA bundle
- [ ] mTLS client cert presentation after bootstrap
- [ ] WebSocket ping every 15 s, fail-fast on missing pong
- [ ] Reconnect with exponential backoff + jitter
- [ ] Bootstrap state machine (send EDGE_BOOTSTRAP, handle ACK/REJECT)
- [ ] Persist edge_cert and chain atomically
- [ ] Delete bootstrap token after success
- [ ] EDGE_HELLO with state fingerprints on every connect
- [ ] EDGE_HEARTBEAT every 30 s (configurable)
- [ ] Handle TENANT_ASSIGN / TENANT_REVOKE, persist set
- [ ] Handle KEY_ROTATE with grace period for previous key
- [ ] Handle CERT_ROTATE with hot reload of TLS listener
- [ ] Handle CONFIG_UPDATE
- [ ] Handle EDGE_SHUTDOWN / EDGE_RECONNECT_REQUEST
- [ ] EDGE_PUBLIC_IP_CHANGED detection (periodic check of own egress IP)
- [ ] EDGE_ERROR emission for monitored conditions
- [ ] EDGE_GOODBYE on graceful shutdown (SIGTERM)
- [ ] EDGE_CERT_RENEW within 30 days of expiry
- [ ] Idempotent handling of all FC → RT messages
- [ ] Local audit log of all control messages

### FC.edge-manager (Rust or Node)

- [ ] WebSocket-over-TLS server with mTLS support
- [ ] Internal CA: keypair, cert issuance for edge_certs, CRL
- [ ] Persistence: edge registry (edge_id, public_ip, status, last_heartbeat, assigned_tenants)
- [ ] Bootstrap token table: token, edge_id, issued_at, expires_at, consumed_at
- [ ] Admin UI endpoint: issue bootstrap token (requires admin auth)
- [ ] Admin UI endpoint: revoke edge_cert
- [ ] Admin UI endpoint: assign / revoke tenant from edge
- [ ] Admin UI endpoint: list edges and their state
- [ ] DNS provider client (Cloudflare/Route53/Azure DNS API), update A records on EDGE_HELLO / EDGE_PUBLIC_IP_CHANGED
- [ ] ACME client (DNS-01) for wildcard cert renewal
- [ ] JWT signing: vault integration for private key, key rotation logic
- [ ] CERT_ROTATE push when wildcard rotates
- [ ] KEY_ROTATE push when JWT signing key rotates
- [ ] Connection takeover handling (4001)
- [ ] Heartbeat tracking, dead detection at 90 s
- [ ] Alert on unhealthy edges > 1 hour
- [ ] EDGE_ERROR forwarding to observability stack
- [ ] EDGE_METRICS forwarding to observability stack
- [ ] Graceful shutdown coordination (EDGE_SHUTDOWN to all edges)
- [ ] Audit log of all control plane operations (issued tokens, certs, assignments)

---

## 16. References

| Topic | Document |
|-------|----------|
| RT.edge implementation details | `rt-edge-spec.md` (to be written) |
| FC.edge-manager implementation details | `fc-edge-manager-spec.md` (to be written) |
| SY.vault (JWT signing key, FC CA private key storage) | `sy-vault-spec.md` |
| Identity model (tenants, ILKs) | `10-identity-v2.md` |
| WAN protocol (RT.edge ↔ motherbee data channel) | `05-conectividad.md` |
| Overall architecture | `01-arquitectura.md` |

---

## 17. Open Questions for Implementation

These are intentionally left open and require team decision before coding starts:

1. **DNS provider API choice**: Cloudflare, Route53, Azure DNS? Affects FC.edge-manager DNS client implementation. Decision orthogonal to protocol.
2. **CA implementation in FC.edge-manager**: in-process (Rust `rcgen` library) or via an external CA tool (step-ca, smallstep)? Affects operational complexity.
3. **FC.edge-manager process model**: single instance for v1 (acceptable, with manual failover) or active-passive from day one? Affects state persistence requirements.
4. **JWT algorithm**: EdDSA recommended (smaller, faster, fewer footguns than RSA). Confirm before coding.
5. **Wildcard cert ACME account**: who owns the Let's Encrypt account? Where is the ACME private key stored? (Vault, presumably.)
6. **Bootstrap token format**: random ASCII 64 chars (suggested), or structured (edge_id-embedded + signature)? Affects how operator hands it to the host install script.

These questions are tracked separately and resolved before §15 checklist items are scheduled.
