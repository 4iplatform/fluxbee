# IO.api - Edge-native instanced ingress specification

**Status:** alpha implementation contract

**Updated:** 2026-07-19

**Related:** `edge-ingress-spec-v6.md`, `io-blob-spec-v1.md`, `io-cloud-spec-v1.md`

## 1. Purpose

`IO.api` exposes a bounded JSON ingress from Internet into one Fluxbee tenant. It is a managed,
instanced `IO.*` runtime. It is not an HTTP server and does not own a public network interface.

The only public path is:

```text
Internet HTTPS
  -> SY.edge /e/<ich>                 bearer + method + size gate
  -> Fluxbee message over router      io.api.inbound.v1
  -> IO.api.<instance>@<hive>         Edge/ICH gate + business validation
  -> identity / frontdesk / relay
  -> fixed internal destination
```

This contract replaces the removed direct-HTTP implementation based on Axum, local API keys,
multipart uploads and outbound webhooks.

## 2. Deployment model

- Runtime key: `io.api`.
- Node shape: managed and instanced, like `ai.generic` nodes.
- Example names: `IO.api.orders@worker-1`, `IO.api.partner-a@motherbee`.
- One instance belongs to one Orchestrator-injected tenant.
- One instance owns one stable `api_channel_id` and therefore one ICH/public URL.
- Multiple APIs are multiple managed instances. They may target the same or different Edge nodes.
- The runtime is seeded under `dist/runtimes/io.api/<version>` by install/package flows.
- There is no `io-api.service` singleton and no `/usr/bin/io-api` service lifecycle.

Orchestrator must inject:

- `FLUXBEE_NODE_NAME`;
- `FLUXBEE_NODE_ILK_ID`;
- `FLUXBEE_NODE_TENANT_ID`;
- managed `config.json` and normal node socket paths.

## 3. Identity and publication

### 3.1 Own channel

The instance calls `ensure_own_ich` with:

- `channel_type = api_channel`;
- `address = config.io.api_channel_id`;
- `ilk_id = FLUXBEE_NODE_ILK_ID`;
- `tenant_id = FLUXBEE_NODE_TENANT_ID`.

Identity stamps the node as owner. Admin later verifies that router-stamped caller and ICH owner are
the same before externalizing.

ICH writes target the canonical primary `SY.identity@motherbee`, including when IO.api runs on a
worker. Tenant/ILK reads use the worker's replicated local Identity SHM.

### 3.2 Public bearer

The public bearer is not part of `CONFIG_SET`, managed state or process arguments. `IO.api` invokes
`SY.admin externalize` without a secret. Admin uses the existing ingress mechanism to:

1. mint a 256-bit token;
2. store it in Vault as `edge_channel_secret:<ich>`, dedicated to the selected Edge;
3. send only that `secret_ref` to Edge;
4. return the token to IO.api.

IO.api keeps the returned token only in memory. A newly minted token is emitted once in the
authorized successful `CONFIG_SET` response as `runtime.publication.entry_token`. It is never
returned by `CONFIG_GET` and is not written to the managed config or state snapshot. The deploy
helper deliberately avoids writing the response containing it to its log.

### 3.3 Externalize

The fixed Admin call is:

```json
{
  "action": "externalize",
  "params": {
    "ich": "ich:<uuid>",
    "edge_node": "SY.edge@ingress-1",
    "inbound_family": "io.api.inbound.v1",
    "auth_mode": "shared-secret",
    "methods": ["POST"]
  }
}
```

The runtime reconciles periodically through `list_externalized`. It adopts an existing exact row
without rotating its bearer. If the row disappeared while the process remains alive, IO.api
re-externalizes with the token retained in memory. On first publication Admin mints the token.

When channel or Edge changes, IO.api closes the previous route before opening the new one and
disables the old ICH after `unexternalize`. `edge.publish=false` performs the teardown without
opening a replacement. Normal process restart leaves the Edge row intact, matching IO.cloud and
Edge warm-start behavior.

## 4. Effective configuration

Minimum configuration:

```json
{
  "edge": {
    "node": "SY.edge@ingress-1",
    "publish": true
  },
  "io": {
    "api_channel_id": "orders",
    "dst_node": "AI.orders@worker-1",
    "relay": {
      "window_ms": 0,
      "max_open_sessions": 10000,
      "max_fragments_per_session": 8,
      "max_bytes_per_session": 262144
    }
  },
  "ingress": {
    "subject_mode": "explicit_subject"
  }
}
```

`caller_is_subject` uses a principal fixed by the instance:

```json
{
  "ingress": {
    "subject_mode": "caller_is_subject",
    "caller_identity": {
      "external_user_id": "partner-service",
      "display_name": "Partner service",
      "email": "service@example.com"
    }
  }
}
```

Rules:

- `edge.node` is a fully qualified `SY.edge@<hive>`.
- `edge.publish` defaults to true.
- `io.api_channel_id` is stable and at most 256 bytes.
- `io.dst_node` is Admin-controlled and cannot be overridden per request.
- `ingress.subject_mode` is `explicit_subject` or `caller_is_subject`.
- `caller_is_subject` requires `ingress.caller_identity.external_user_id`.
- Relay config follows the shared `io-common` contract.
- `node.*` and `runtime.*` remain infrastructure sections and may require restart.
- `node.frontdesk_target` (or `IO_API_FRONTDESK_TARGET`) is infrastructure wiring for the
  intermediate registration handoff. It defaults to the canonical singleton
  `SY.frontdesk.gov@motherbee`.

The following old fields are rejected rather than ignored:

- `listen`;
- `auth` / `auth.api_keys`;
- `integrations` / webhook configuration;
- `blob` and multipart/attachment limits;
- Edge credentials (`secret`, `token`, `token_ref`), methods, auth mode, inbound family or upstream.

## 5. Edge data contract

Edge exposes only `POST /e/<ich>`. A request body must fit Edge's canonical 64 KiB message envelope.
Edge validates the bearer, removes `Authorization`, parses JSON when possible and forwards:

```text
meta.msg_type = io.api.inbound.v1
meta.ich      = <instance ICH>
routing.dst   = <resolved IO.api instance name>
context       = {method:"POST", path:"/", query?, headers?}
payload       = <JSON body>
```

IO.api validates again:

- router-stamped `src_l2_name` equals configured `edge.node`;
- `meta.ich` equals the currently published own ICH;
- family is `io.api.inbound.v1`;
- context method is POST and path is `/`;
- lifecycle is CONFIGURED and publication is active;
- payload is a JSON object.

The node replies to the Edge UUID with the same trace and ICH. Edge returns that payload as HTTP
200 JSON. Application failure is represented by `status=error` and `error_code`; transport failures
remain Edge 502/504.

## 6. Request contract

`explicit_subject by_data`:

```json
{
  "request_id": "optional-caller-id",
  "subject": {
    "external_user_id": "customer-42",
    "display_name": "Ada Lovelace",
    "email": "ada@example.com",
    "phone": "+54...",
    "company_name": "Example",
    "attributes": {}
  },
  "message": {
    "text": "Create the report",
    "external_message_id": "msg-42",
    "timestamp": "2026-07-19T12:00:00Z"
  },
  "options": {
    "metadata": {"conversation_id":"case-9"},
    "relay": {"final":true}
  }
}
```

`explicit_subject by_ilk` replaces subject data with an `ilk`. IO.api lists Identity ILKs and
requires an exact `(ilk_id, instance tenant_id)` match. Unknown and cross-tenant ILKs return the
same `subject_not_found` error.

Prohibited request authority fields:

- `subject.tenant_id` and `subject.tenant_hint`;
- `options.routing` and any `dst_node` override;
- attachments or multipart data.

Large/generated files use the Blob toolchain and publication mechanism, not an Edge message frame.

## 7. Internal processing

- Tenant always comes from `FLUXBEE_NODE_TENANT_ID`.
- `caller_is_subject` identity comes from instance config.
- `explicit_subject by_data` resolves/provisions through Identity.
- Temporary/incomplete explicit subjects use the existing Frontdesk structured handoff. Frontdesk
  must be `CONFIGURED`; its canonical `type=error` response is surfaced as
  `frontdesk_unavailable`, not as a malformed response envelope.
- The synchronous Frontdesk RPC accepts only a correlated `user` reply as success and classifies
  `SYSTEM/UNREACHABLE` and `SYSTEM/TTL_EXCEEDED` as transport errors.
- Message payload is canonical `text/v1`.
- Conversation thread uses `PersistentChannel(api_channel_id, conversation_id)`.
- Relay uses shared `io-common`; request routing override is always `None`.
- Final outbound webhook delivery does not exist in this version.

Accepted response example:

```json
{
  "status": "accepted",
  "accepted": true,
  "request_id": "req_...",
  "trace_id": "...",
  "relay": "flushed_immediately",
  "subject_ilk": "ilk:<uuid>",
  "handled_by": "IO.api.orders@worker-1"
}
```

Error example:

```json
{
  "status": "error",
  "error_code": "routing_override_forbidden",
  "error_detail": "options.routing is not accepted; destination belongs to instance configuration"
}
```

## 8. Control plane and status

Only router-stamped configured Admin or Orchestrator origins may invoke `CONFIG_GET/SET`. The Router
also treats the effective `CONFIG_GET` and `CONFIG_SET` verbs as protected SYSTEM actions.

Config replace is transactional: an invalid candidate does not destroy the last working config.
Valid destination, relay, ingress and publication changes hot-apply. `node.*`/`runtime.*` changes are
reported as restart-required.

`CONFIG_GET` and successful `CONFIG_SET` include:

```json
{
  "runtime": {
    "transport": "router_socket",
    "public_frontier": "SY.edge",
    "inbound_family": "io.api.inbound.v1",
    "publication": {
      "status": "published",
      "ich": "ich:<uuid>",
      "edge_node": "SY.edge@ingress-1",
      "url": "/e/ich:<uuid>",
      "credential_pending": false,
      "last_error": null
    }
  }
}
```

Publication states are `unconfigured`, `published`, `disabled` or `error`.

When Admin minted a new bearer, the successful `CONFIG_SET` response contains it once:

```json
{"runtime":{"publication":{"entry_token":"<bearer>","entry_token_one_time":true}}}
```

The caller must retain it for the external client. Later `CONFIG_GET` calls expose only
`credential_pending`, never the bearer value.

## 9. Security invariants

1. No TCP listener or direct Internet route exists on IO.api.
2. Edge is the only public auth and size frontier.
3. Edge origin and own ICH are checked again in IO.api.
4. Tenant comes only from managed instance identity.
5. An explicit ILK must belong to that tenant.
6. Public request data cannot choose the internal destination.
7. Config contains neither a bearer nor a Vault reference; Admin owns credential creation.
8. Only the owning IO node can externalize its ICH through Admin.
9. One instance/ICH can be rotated or removed without changing another API instance.
10. Multipart, arbitrary HTTP headers/status forwarding and outbound callback HTTP are absent.

## 10. Operational flow

The canonical helper is `scripts/deploy-io-api.sh`. It performs:

1. publish `io.api` and update the runtime manifest;
2. spawn/restart the managed instance through Admin/Orchestrator;
3. send the typed `CONFIG_SET` and capture a newly issued one-time bearer, when present;
4. poll `control/config-get` until `runtime.publication.status=published`.

No local `/schema` or HTTP port probe is valid for this runtime.

Current Edge v6 placement is constrained by `EDGE-H4`: ingress forwarding resolves one WAN hop.
An IO.api instance must therefore run in a hive directly adjacent to its configured ingress Edge.
Publishing a worker instance behind motherbee is valid control-plane state, but requests return
`HANDLER_UNREACHABLE` until that worker has direct ingress adjacency or WAN multihop is implemented.

Permanent removal is an ordered operation: apply `edge.publish=false`, stop the managed node with
`DELETE /hives/{hive}/nodes/{node}`, wait until it is no longer router-visible, then call
`DELETE /hives/{hive}/nodes/{node}/instance`. Killing a node alone intentionally preserves its
instance and publication intent for restart recovery.
