# SY.admin Internal Command Gateway

**Status:** v1.0
**Date:** 2026-03-14
**Audience:** Developers implementing SY.admin, internal callers (AI/WF/IO nodes), SDK
**Parent specs:** `02-protocolo.md`, `07-operaciones.md`

---

## 1. Purpose

Enable internal nodes to invoke any SY.admin function via socket/WAN messages, with the same behavior as HTTP. SY.admin remains the single gateway for all control operations — internal callers do not bypass it to reach SY.orchestrator or other SY nodes directly.

---

## 2. Design Decisions

1. **SY.admin is the only control gateway.** Both HTTP (external) and socket (internal) enter through the same dispatcher. No fork in logic, no separate code paths.

2. **Every HTTP function is automatically available via socket.** There is no manual catalog of "allowed internal actions." When a new endpoint is added to admin's HTTP handler, it is immediately callable via ADMIN_COMMAND using the same action name and payload shape. The implementation reuses the same internal dispatch function for both entry points.

3. **No ACL for internal commands (v1).** If a message arrives via socket with `msg: ADMIN_COMMAND`, it is accepted and processed. Security hardening with origin/action matrix is deferred.

4. **Monocommand execution.** Admin processes one command at a time, regardless of entry channel. If a command is in progress (HTTP or socket), the next one waits in queue. This simplifies concurrency and avoids race conditions in orchestrator dispatch.

5. **request_id is correlation only.** No persistence, no idempotency enforcement, no deduplication. The caller sends a `request_id`, admin echoes it back in the response for the caller to match. Logging and persistence are deferred.

6. **No bypass in production.** Direct messaging to SY.orchestrator remains restricted to controlled tooling and E2E tests. The recommended path for any productive operation is always through SY.admin.

---

## 3. Architecture

### 3.1 External Flow (Unchanged)

```
HTTP client
  → SY.admin HTTP handler
  → parse action + params
  → dispatch(action, params)        ← shared dispatcher
  → executor (SY.orchestrator, SY.identity, etc.)
  → HTTP response
```

### 3.2 Internal Flow (New)

```
Internal node (AI/WF/IO)
  → ADMIN_COMMAND message to SY.admin@motherbee
  → SY.admin message handler
  → parse action + params from payload
  → dispatch(action, params)        ← same shared dispatcher
  → executor (SY.orchestrator, SY.identity, etc.)
  → ADMIN_COMMAND_RESPONSE message back to caller
```

### 3.3 Shared Dispatcher

The key implementation requirement: both HTTP and socket entry points call the same `dispatch(action, params)` function. This function contains all validation, normalization, routing to executors, timeout handling, and response formatting.

```rust
// Pseudocode — both entry points converge here
async fn dispatch(action: &str, hive_id: Option<&str>, params: Value) -> AdminResult {
    // validate action exists
    // normalize params (same rules as HTTP)
    // acquire command lock (monocommand)
    // route to executor
    // wait for response with timeout
    // release lock
    // return result
}

// HTTP entry point
async fn handle_http(req: HttpRequest) -> HttpResponse {
    let action = extract_action(&req);
    let params = extract_params(&req);
    let hive_id = extract_hive_id(&req);
    let result = dispatch(&action, hive_id.as_deref(), params).await;
    to_http_response(result)
}

// Socket entry point
async fn handle_admin_command(msg: Message) -> Message {
    let action = msg.payload["action"].as_str();
    let params = msg.payload["params"].clone();
    let hive_id = msg.payload["hive_id"].as_str();
    let result = dispatch(action, hive_id, params).await;
    to_admin_command_response(result, &msg)
}
```

### 3.4 Invariant

For the same `action` and equivalent `params`, the functional result is identical regardless of entry channel (HTTP or socket). Only transport metadata differs (HTTP status codes vs message envelope).

---

## 4. Monocommand Execution

Admin processes one command at a time. This applies globally across both channels.

```rust
// Single command lock
let _guard = self.command_lock.lock().await;
// Only one command executes at a time
let result = dispatch(action, hive_id, params).await;
// Lock released when _guard drops
```

If a second command arrives (HTTP or socket) while one is in progress, it waits for the lock. The caller experiences this as latency, not as an error.

**Timeout:** Each action has a maximum execution time (same as HTTP timeouts). If the executor does not respond within the timeout, admin returns `error` with `error_code: TIMEOUT` and releases the lock.

---

## 5. Message Contract

### 5.1 ADMIN_COMMAND (Request)

```json
{
  "routing": {
    "src": "WF.architect@motherbee",
    "dst": "SY.admin@motherbee",
    "ttl": 16,
    "trace_id": "5a7f2f4f-bf34-4b15-bde6-7f7fe7345f3d"
  },
  "meta": {
    "type": "admin",
    "msg": "ADMIN_COMMAND"
  },
  "payload": {
    "action": "run_node",
    "hive_id": "worker-220",
    "params": {
      "node_name": "AI.soporte.l1",
      "runtime": "ai.soporte",
      "runtime_version": "current",
      "config": {
        "api_provider": "openai",
        "api_key": "sk-...",
        "model": "gpt-4",
        "system_prompt": "..."
      }
    },
    "request_id": "req-20260314-001"
  }
}
```

**Payload fields:**

| Field | Required | Type | Description |
|-------|----------|------|-------------|
| `action` | Yes | string | Action name, same as the HTTP handler dispatch key |
| `hive_id` | No | string | Target hive when the action is hive-scoped. Omit for global actions |
| `params` | No | object | Action parameters, same shape as HTTP request body |
| `request_id` | No | string | Correlation ID, echoed back in response |

### 5.2 ADMIN_COMMAND_RESPONSE (Response)

```json
{
  "routing": {
    "src": "SY.admin@motherbee",
    "dst": "WF.architect@motherbee",
    "ttl": 16,
    "trace_id": "5a7f2f4f-bf34-4b15-bde6-7f7fe7345f3d"
  },
  "meta": {
    "type": "admin",
    "msg": "ADMIN_COMMAND_RESPONSE"
  },
  "payload": {
    "status": "ok",
    "action": "run_node",
    "payload": {
      "node_name": "AI.soporte.l1@worker-220",
      "runtime": "ai.soporte",
      "version": "1.2.0"
    },
    "error_code": null,
    "error_detail": null,
    "request_id": "req-20260314-001"
  }
}
```

**Response payload fields:**

| Field | Always present | Description |
|-------|---------------|-------------|
| `status` | Yes | `ok`, `sync_pending`, or `error` |
| `action` | Yes | Echo of the requested action |
| `payload` | Yes (nullable) | Domain-specific result, same as HTTP response body |
| `error_code` | Yes (nullable) | Error code when status=error, same codes as HTTP |
| `error_detail` | Yes (nullable) | Human-readable error detail |
| `request_id` | If sent | Echo of the caller's request_id |

### 5.3 Correlation

- Primary: `routing.trace_id` (always present, system-generated if not provided).
- Caller-side: `payload.request_id` (optional, echoed back for caller matching).

---

## 6. Action Mapping

Every HTTP endpoint maps to an `action` string. The mapping is defined by the HTTP handler dispatch table in SY.admin — there is no separate catalog for internal commands.

**Current actions (as of v1.16+, non-exhaustive):**

| HTTP Endpoint | Action | Hive-scoped |
|---------------|--------|-------------|
| `GET /hive/status` | `hive_status` | No |
| `GET /hives` | `list_hives` | No |
| `POST /hives` | `add_hive` | No |
| `GET /hives/{id}` | `get_hive` | Yes |
| `DELETE /hives/{id}` | `remove_hive` | Yes |
| `POST /hives/{id}/update` | `update` | Yes |
| `POST /hives/{id}/sync-hint` | `sync_hint` | Yes |
| `GET /hives/{id}/versions` | `get_versions` | Yes |
| `GET /hives/{id}/nodes` | `list_nodes` | Yes |
| `POST /hives/{id}/nodes` | `run_node` | Yes |
| `DELETE /hives/{id}/nodes/{name}` | `kill_node` | Yes |
| `GET /hives/{id}/nodes/{name}/status` | `get_node_status` | Yes |
| `GET /hives/{id}/nodes/{name}/config` | `get_node_config` | Yes |
| `PUT /hives/{id}/nodes/{name}/config` | `set_node_config` | Yes |
| `GET /hives/{id}/nodes/{name}/state` | `get_node_state` | Yes |
| `GET /hives/{id}/routers` | `list_routers` | Yes |
| `POST /hives/{id}/routers` | `run_router` | Yes |
| `DELETE /hives/{id}/routers/{name}` | `kill_router` | Yes |
| `GET /inventory` | `get_inventory` | No |
| `GET /inventory/{id}` | `get_inventory_hive` | Yes |
| `GET /inventory/summary` | `get_inventory_summary` | No |
| `GET/POST/DELETE /routes` | `list_routes`, `add_route`, `delete_route` | No |
| `GET/POST/DELETE /vpns` | `list_vpns`, `add_vpn`, `delete_vpn` | No |
| `GET/PUT /config/storage` | `get_config_storage`, `set_config_storage` | No |
| `POST /identity/tenants` | `create_tenant` | No |
| `GET /identity/tenants` | `list_tenants` | No |
| `GET /identity/tenants/{id}` | `get_tenant` | No |
| `PUT /identity/tenants/{id}` | `update_tenant` | No |
| `POST /identity/tenants/{id}/approve` | `approve_tenant` | No |
| `GET /identity/ilks` | `list_ilks` | No |
| `GET /identity/ilks/{id}` | `get_ilk` | No |
| `DELETE /identity/ilks/{id}` | `delete_ilk` | No |
| `GET /identity/vocabulary` | `list_vocabulary` | No |
| `POST /identity/vocabulary` | `add_vocabulary` | No |
| `DELETE /identity/vocabulary/{tag}` | `deprecate_vocabulary` | No |
| `GET /identity/metrics` | `get_identity_metrics` | No |
| `POST /opa/policy` | `opa_policy` | No |
| `POST /opa/policy/compile` | `opa_compile` | No |
| `POST /opa/policy/apply` | `opa_apply` | No |
| `POST /opa/policy/rollback` | `opa_rollback` | No |
| `GET /opa/status` | `opa_status` | No |

**Rule for new endpoints:** When a new HTTP endpoint is added to SY.admin, the developer must ensure the handler goes through the shared `dispatch()` function. This automatically makes it available via ADMIN_COMMAND with no additional work.

---

## 7. Implementation Requirements

### 7.1 In SY.admin

1. Add message handler for `ADMIN_COMMAND` that extracts `action`, `hive_id`, `params` and calls `dispatch()`.
2. Format the dispatch result as `ADMIN_COMMAND_RESPONSE` and send back to caller.
3. Apply the same param normalization as HTTP (field name mapping, defaults, etc.).
4. Apply the same error code mapping as HTTP.
5. Apply monocommand lock before dispatch, release after.
6. Apply same per-action timeouts as HTTP.

### 7.2 In SDK (fluxbee_sdk)

Provide a helper for internal callers:

```rust
pub async fn admin_command(
    action: &str,
    hive_id: Option<&str>,
    params: Value,
    timeout: Duration,
) -> Result<AdminCommandResponse> {
    let request_id = generate_request_id();
    let msg = build_admin_command(action, hive_id, params, request_id);
    let response = send_and_wait(msg, timeout).await?;
    parse_admin_command_response(response)
}
```

Usage:

```rust
let result = admin_command(
    "run_node",
    Some("worker-220"),
    json!({
        "node_name": "AI.soporte.l1",
        "runtime": "ai.soporte",
        "runtime_version": "current",
        "config": { ... }
    }),
    Duration::from_secs(30),
).await?;

match result.status {
    "ok" => println!("Node spawned: {}", result.payload),
    "error" => eprintln!("Failed: {} - {}", result.error_code, result.error_detail),
    _ => {}
}
```

---

## 8. Examples

### 8.1 Spawn a Node (Internal)

```json
{
  "payload": {
    "action": "run_node",
    "hive_id": "worker-220",
    "params": {
      "node_name": "AI.soporte.l1",
      "runtime": "ai.soporte",
      "runtime_version": "current",
      "config": { "api_key": "sk-...", "model": "gpt-4" }
    }
  }
}
```

### 8.2 Kill a Node (Internal)

```json
{
  "payload": {
    "action": "kill_node",
    "hive_id": "worker-220",
    "params": {
      "node_name": "AI.soporte.l1",
      "force": false
    }
  }
}
```

### 8.3 Get Inventory (Internal)

```json
{
  "payload": {
    "action": "get_inventory",
    "params": {}
  }
}
```

### 8.4 Create Tenant (Internal)

```json
{
  "payload": {
    "action": "create_tenant",
    "params": {
      "name": "Acme Corp",
      "domain": "acme.com",
      "status": "pending"
    }
  }
}
```

### 8.5 Update Node Config (Internal)

```json
{
  "payload": {
    "action": "set_node_config",
    "hive_id": "worker-220",
    "params": {
      "node_name": "AI.soporte.l1",
      "system_prompt": "Updated prompt...",
      "temperature": 0.5
    }
  }
}
```

### 8.6 Error Response

```json
{
  "payload": {
    "status": "error",
    "action": "run_node",
    "payload": null,
    "error_code": "RUNTIME_NOT_PRESENT",
    "error_detail": {
      "runtime": "ai.soporte",
      "version": "1.2.0",
      "expected_path": "/var/lib/fluxbee/dist/runtimes/ai.soporte/1.2.0/bin/start.sh",
      "hint": "Run SYSTEM_UPDATE to materialize this runtime on the worker"
    },
    "request_id": "req-20260314-001"
  }
}
```

---

## 9. Implementation Tasks

- [ ] FR9-T1. Add `ADMIN_COMMAND` / `ADMIN_COMMAND_RESPONSE` message handler in SY.admin.
- [ ] FR9-T2. Wire handler to shared `dispatch()` function (same as HTTP).
- [ ] FR9-T3. Implement monocommand lock (single command in progress across HTTP + socket).
- [ ] FR9-T4. Implement SDK helper `admin_command()` in `fluxbee_sdk`.
- [ ] FR9-T5. E2E: `run_node` via ADMIN_COMMAND produces same result as HTTP POST.
- [ ] FR9-T6. E2E: `kill_node` via ADMIN_COMMAND produces same result as HTTP DELETE.
- [ ] FR9-T7. E2E: `get_inventory` via ADMIN_COMMAND returns valid inventory.
- [ ] FR9-T8. E2E: monocommand — concurrent commands wait, don't error.
- [ ] FR9-T9. Update `02-protocolo.md` with ADMIN_COMMAND message definition.
- [ ] FR9-T10. Update `07-operaciones.md` with internal command gateway documentation.

---

## 10. Not In Scope (v1)

- ACL by origin or action (deferred, implement as guard in dispatch when needed).
- Audit logging to database (deferred, log to structured stdout for now).
- Idempotency enforcement via request_id (deferred).
- Replacing HTTP (HTTP remains the external entry point).
- Batch commands (one action per ADMIN_COMMAND message).

---

## 11. Relationship to Other Specs

| Spec | Relationship |
|------|-------------|
| `02-protocolo.md` | Add ADMIN_COMMAND / ADMIN_COMMAND_RESPONSE message definitions |
| `07-operaciones.md` | Document internal command gateway and SDK usage |
| `node-spawn-config-spec.md` | `run_node` action creates config.json via the same dispatch path |
| `node-status-contract.md` | `get_node_status` action available via internal command |
| `system-inventory-spec.md` | `get_inventory` action available via internal command |
| `runtime-lifecycle-spec.md` | `update`, `sync_hint`, `get_versions` available via internal command |
| `10-identity-v2.md` | Identity management actions available via internal command |
