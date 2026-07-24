# IO.wapp — WhatsApp Cloud API node (design)

Status: **APPROVED** (2026-07-24) — decisions D1–D6 locked (see §10). Implementing per §11.
Author: Claude + user. Scope: add a first-class managed IO node `IO.wapp` that bridges WhatsApp
(Meta **Cloud API**, official) into the fluxbee mesh, following the existing IO-family patterns
(SDK single-config control-plane, vault_ref credentials, degraded boot) — mirroring **io.slack**
(credentials/boot), **io.api** (public inbound via SY.edge), and **io.linkedhelper** (HTTP adapter).

This doc marks every **DECISION** that needs the user's call before coding.

---

## 1. Goal & non-goals

- **Goal**: receive WhatsApp messages (text + media) from customers and relay them into the mesh
  (default target an AI node, exactly like io.slack → AI.chat/AI.generic), and send replies back out.
- **Official channel only**: Meta WhatsApp **Cloud API** (Graph API + webhooks). No unofficial
  clients (whatsapp-web.js / Baileys) — ToS-risky, fragile, and they don't fit "tokens from vault".
- **Non-goals (v1)**: interactive templates authoring, flows/catalog/payments, multi-number routing
  beyond one bound phone number per instance. All spawnable later per the runtime model.

## 2. WhatsApp Cloud API — the shape we must fit

- **Inbound = webhook (Meta → us)**. Meta HTTP-calls a *public* callback URL:
  - **Verification (once, at setup)**: `GET <url>?hub.mode=subscribe&hub.verify_token=<T>&hub.challenge=<N>`.
    We must reply **200** with the **raw** `<N>` as the body (`text/plain`), and only if `<T>` matches
    our configured verify token.
  - **Events**: `POST <url>` with a JSON body (`object:"whatsapp_business_account", entry:[…messages…]`),
    signed `X-Hub-Signature-256: sha256=<hmac-sha256(app_secret, raw_body)>`. We MUST verify the
    signature (default-deny unsigned/mismatched) and reply **200** quickly (Meta retries on non-2xx).
- **Outbound = Graph API (us → Meta)**: `POST https://graph.facebook.com/v<X>/<phone_number_id>/messages`
  with `Authorization: Bearer <access_token>` and a JSON message body.
- **The 24-hour customer-service window**: free-form replies are only allowed within 24h of the
  customer's last inbound message; outside it, only pre-approved **template** messages. v1 handles the
  free-form/in-window case (the AI-reply path); template sending is a later capability. **DECISION D6**.
- **Media**: inbound messages carry a `media_id`; fetch via `GET /<media_id>` (bearer) → a URL → GET
  the bytes (bearer). Outbound media is uploaded to `/<phone_number_id>/media` → `media_id` →
  referenced in the message (or sent by public link).

The consequential difference from io.slack: **io.slack Socket Mode dials OUT** (no public URL);
**io.wapp is webhook-driven and needs a public INBOUND endpoint** — the SY.edge model, like io.api.

## 3. Where it lives (grounded — mirrors io.slack)

| Piece | Location | Reference |
|---|---|---|
| Node crate + binary | `nodes/io/io-wapp/` (bin `io-wapp`); add to `nodes/io/Cargo.toml` members | io-slack |
| Config contract | `nodes/io/common/src/io_wapp_adapter_config.rs` (`IoWappAdapterConfigContract`) | `io_slack_adapter_config.rs` |
| Control-plane wiring | `bootstrap_io_control_plane_state(node_name, &IoWappAdapterConfigContract)` in `main()` | `io_control_plane_bootstrap.rs:88`, io-slack `main.rs:137` |
| Credentials | vault_ref → SY.vault (multi-field object) + refresh loop + `VAULT_SECRET_CHANGED` wake | io-slack `main.rs:716-861,2496-2517` |
| Media | `fluxbee_sdk::blob` + `io_common::text_v1_blob` | io-slack `main.rs:1904-1990,1484-1498` |
| Packaging | `runtimes` entry in `packaging/base-nodes.json`; picked up by build-deb.sh/firstboot | `base-nodes.json:41-82` |

**No central node-kind registry** exists — the runtime name is convention-derived from the binary
name (`io-wapp` → runtime `io.wapp` → instances `IO.wapp@<hive>`; `sy_architect.rs:8388-8442`). The
only enumerations to touch: `nodes/io/Cargo.toml` members + `packaging/base-nodes.json`
(+ optionally a `wapp)` case in `scripts/publish-io-runtime.sh`).

## 4. Config contract (`IoWappAdapterConfigContract`)

Same shape as io.slack: the **binding** lives in config, the **secret** is a vault reference. The
node boots **Unconfigured → DEGRADED** with only the orchestrator seed (`{tenant_id}`), and only a
config with real operator content is validated (SDK `managed_control_plane.rs:203-211,266-284`).

```jsonc
{
  "wapp": {
    "auth": { "type": "vault_ref", "resource_type": "whatsapp", "key": "wapp/IO.wapp@motherbee" }
  },
  "io": {
    "phone_number_id": "1234567890",          // the sending number id (binding, like slack workspace_id)
    "waba_id": "9876543210",                  // WhatsApp Business Account id (binding)
    "dst_node": "AI.generic@motherbee",       // where inbound relays (absent → router resolve), like io.slack
    "graph_api_version": "v20.0",             // optional, default pinned
    "blob": { /* io.blob.* media knobs, like io.slack */ }
  }
}
```

- `required_fields`: `wapp.auth.{type,resource_type,key}`, `io.phone_number_id`, `io.waba_id`.
- `validate_and_materialize`: enforce `auth.type=="vault_ref"`, non-empty `resource_type`/`key`,
  non-empty phone_number_id/waba_id. NO inline tokens accepted (mirror io.slack).
- `secret_descriptors`: one `config.wapp.auth.key`, `persistence="vault"`, `value_redacted=false`.
- `redact_effective_config`: no-op (auth.key is a reference, not the secret).

**Vault secret object** (one key, `resource_type="whatsapp"`, resolved + validated like io.slack's
`{app_token,bot_token}`):

```jsonc
{ "access_token": "<System-User permanent token>",
  "app_secret":   "<Meta app secret — verifies X-Hub-Signature-256>",
  "verify_token": "<operator-chosen webhook verify token>" }
```

`resolve_wapp_credentials_from_vault(vault, key)` → 3-state `{Found(creds), Absent, Transient}`
(delete→clear, timeout→keep), refresh loop + broadcast wake — copied verbatim from io.slack.

## 5. Inbound — **the central architectural DECISION (D1)**

WhatsApp needs a public HTTPS endpoint. Two options; both are real in the codebase.

### Option A (RECOMMENDED) — SY.edge `/e/<ich>` + a small, reusable edge enhancement

Reuse io.api's model: io.wapp self-externalizes an ICH (`ensure_own_ich` `provision.rs:130`; admin
`"externalize"` `sy_admin.rs:4218`, owner-only gate `authorize_channel_command:5031`), Meta webhooks
to `https://<edge>/e/<ich>`, the edge forwards to `IO.wapp` as `msg_type=io.wapp.inbound.v1` with the
HTTP `{method,path,query,headers}` in `meta.context` and the JSON body as `payload`
(`sy_edge.rs:2007-2051`). Reply correlates by `trace_id`.

- **Pros**: no open port on the node; SY.edge is the single TLS/DMZ frontier; bearer auth + method
  allowlist + backpressure + owner-only externalize enforced at the edge; cross-hive by name; matches
  the B1 DMZ invariant and the io.api precedent. It's "the fluxbee way".
- **The gap (must be resolved)**: the edge ALWAYS returns **HTTP 200 + `application/json`** wrapping
  `reply.payload` (`sy_edge.rs:2086-2092`) — a node cannot set status/content-type/raw body. Meta's
  GET verification needs the **raw `hub.challenge`** as `text/plain`, and a JSON-quoted `"12345"` will
  not satisfy it.
- **Proposed enhancement (small, general, reusable by io.api/io.blob too)**: teach the edge to honor
  an OPTIONAL response envelope in the node reply, e.g.
  `reply.payload = { "__edge_response": { "status": 200, "content_type": "text/plain", "body": "12345" } }`.
  When present, the edge emits exactly that; when absent, it keeps today's 200/JSON wrapping (fully
  backward compatible). io.wapp uses it only for the GET challenge (and to always 200 the POST fast).
  Also: io.wapp's externalize row must allow **GET+POST** (io.api hardcodes POST-only; we relax
  `methods` + the `validate_edge_http_context` path/method check on our side).

### Option B — own HTTP listener (io.linkedhelper model)

io.wapp runs its own axum listener (`config.listen.{address,port}`, `io-linkedhelper/src/main.rs:713-756`).

- **Pros**: full control of verbs/status/content-type/raw body — Meta's challenge is trivial; no edge
  change.
- **Cons**: you must provision external ingress/TLS/reverse-proxy to reach the node's port (outside
  the edge's DMZ model), and you re-implement auth/backpressure/webhook-signature at the node — exactly
  the things SY.edge centralizes. Diverges from the io.api/io.slack security posture.

**Recommendation**: **Option A + the edge response-envelope enhancement.** It keeps io.wapp behind the
single hardened frontier and the enhancement is a net win for every edge-served node. Option B is the
lower-friction *only if* we refuse to touch the edge. → **User's call.**

## 6. Webhook verification & signature (in the node)

- **GET challenge**: compare `context.query["hub.verify_token"]` to the vault `verify_token`
  (constant-time); on match reply the `hub.challenge` via the edge response-envelope (Option A) or
  raw (Option B); on mismatch → 403. Default-deny.
- **POST signature**: recompute `HMAC-SHA256(app_secret, raw_body)` and constant-time compare to
  `X-Hub-Signature-256`. **Caveat (D2)**: the edge currently forwards the *parsed* JSON `payload`, not
  the exact raw bytes — HMAC needs the RAW body. So Option A also needs the edge to pass the raw body
  (e.g. keep `body_base64`/a raw field alongside the parsed payload) OR io.wapp re-serializes
  canonically (fragile). **DECISION D2**: forward raw body for signed webhooks.

## 7. Outbound (Graph API) & the 24h window

- `POST /<phone_number_id>/messages` (bearer). Reply to the sender's `wa_id`; free-form text within
  the 24h window (v1). Rate-limit/429 with Retry-After retries (reuse the io.slack `slack_send_with_retry`
  shape). Outside the window → requires a template (D6, deferred).
- Inbound relay: build a `text/v1` payload (content + attachments) and route to `io.dst_node`
  (absent → `Destination::Resolve`), stamping a raw-context stub (sender wa_id, phone_number_id,
  message id) so a reply can be correlated back — exactly io.slack's inbound shape.

## 8. Media (reuse, don't reinvent)

Mirror io.slack: inbound → fetch media via Graph media URL (bearer) → `BlobToolkit::put_bytes` →
`blob_ref` embedded in the router message (allowed-mimes / max-bytes from `io.blob.*`). Outbound →
`resolve_text_v1_for_outbound` → upload to `/media` → send referencing the `media_id`.

## 9. Packaging & boot

- `nodes/io/Cargo.toml` += `io-wapp`; `packaging/base-nodes.json` runtimes +=
  `{"runtime":"io.wapp","crate":"io-wapp","bin":"io-wapp","workspace":"nodes/io","boot":<D3>,"instance":"IO.wapp@motherbee"}`.
- **DECISION D3 — boot=true vs false**: user leaned **`boot=true` degraded** (io.slack scheme:
  firstboot auto-spawns `IO.wapp@motherbee`, degraded until creds+config load). io.linkedhelper
  precedent is `boot=false` (spawn-on-demand per tenant). Recommend **boot=true degraded** per the
  user's steer; per-tenant instances still spawnable via run_node.
- Publish via `scripts/publish-io-runtime.sh` (+ a `wapp)` case) or the generic `publish-runtime.sh`.

## 10. Open DECISIONS (need your call)

- **D1**: Inbound model — **Option A (edge + response-envelope enhancement)** [recommended] vs Option B
  (own listener).
- **D2**: If Option A — extend the edge to forward the **raw body** (for HMAC signature verification).
- **D3**: `boot=true` degraded (io.slack scheme) [recommended] vs `boot=false`.
- **D4**: Vault secret `resource_type` = `"whatsapp"` (new canonical) vs reuse `"bearer_token"`
  (io-cloud test uses `wapp_token`/`bearer_token`). Recommend a dedicated `"whatsapp"` type.
- **D5**: Default inbound target `io.dst_node` = `AI.generic@motherbee` (matches the io.slack decision).
- **D6**: Template outbound (outside-24h) — defer to v2? (recommended defer).

## 11. Phased plan (after design OK)

1. **Contract + skeleton** — `io_wapp_adapter_config.rs` + `io-wapp` crate booting degraded (control
   plane, CONFIG_GET/SET, vault_ref resolve + refresh + broadcast). Unit-tested; boots UNCONFIGURED.
2. **Edge enhancement (if D1=A)** — optional response-envelope + raw-body forwarding in `sy_edge.rs`
   (backward compatible), with tests; io.api/io.blob unaffected.
3. **Inbound** — self-externalize (GET+POST), verify challenge + signature, parse messages → text/v1
   relay to dst_node; media in.
4. **Outbound** — Graph messages + 429 retries; media out.
5. **Packaging** — base-nodes.json + Cargo.toml + publish; firstboot degraded boot.
6. **Live validation** — deploy to the dev hive; boots UNCONFIGURED; then a real webhook round-trip
   once a WABA + token are available.

Nothing is coded until D1–D6 are agreed.
