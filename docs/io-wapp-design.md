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

## 5. Inbound — the **fanout** model (DECIDED)

The hard constraint: **one Meta app = ONE webhook URL** (configured at the App level; Meta delivers
ALL numbers'/WABAs' events to that single URL, tagged with `metadata.phone_number_id` + the WABA id).
There is no per-number webhook. So the public entry is necessarily singular, and inbound must be
demultiplexed by `phone_number_id`.

**Chosen topology (user, 2026-07-24): fanout + per-number self-select.** The single public endpoint
fans out to ALL `IO.wapp` nodes; there is **one io.wapp per number**, and each node processes only the
events for ITS configured `phone_number_id` (self-select). This gives per-number nodes (each a normal
single-number node, like io.slack is single-workspace) sharing the one mandatory webhook — no
front-demux, no multi-number config. It leverages fluxbee's existing router **tap/echo fanout**
primitive (`router-tap`), and it fits WhatsApp's best practice: **ack the webhook 200 immediately and
process async** (the reply goes out-of-band via the Graph API, §7 — so the edge NEVER waits for a node
reply on the POST).

### Separation of concerns (thin edge)

- **Connection handshake → the EDGE.** Meta's one-time **GET verification** (`hub.mode`,
  `hub.verify_token`, `hub.challenge`) is connection-level setup, not app processing. The edge stores
  the `verify_token` on the fanout endpoint registration (as it already stores io.api's bearer secret)
  and answers the challenge itself: on match, reply `200 text/plain` with the raw `hub.challenge`; on
  mismatch, `403`. No fanout for the GET.
- **Per-message app auth → the NODE.** The `X-Hub-Signature-256` HMAC (with the app-level `app_secret`)
  is verified by EACH io.wapp using its own `app_secret` from vault. The edge does NOT know
  `app_secret`, numbers, or the WhatsApp signature scheme — it stays a dumb transport/DMZ frontier.

### POST flow (events)

1. Meta `POST /e/<ich>` (signed). Endpoint `auth_mode=public` (Meta sends no bearer; the real auth is
   the HMAC, verified downstream).
2. Edge **acks Meta `200` immediately** (fire-and-forget; no waiting for node replies — avoids Meta
   retries), then **fans out** the event to the `IO.wapp` family via the tap/echo primitive, preserving
   the **raw body** and the `X-Hub-Signature-256` header (needed for HMAC over exact bytes).
3. Each io.wapp verifies the HMAC (`app_secret` from vault) → default-deny on mismatch; filters by its
   `phone_number_id` (only the matching node continues); dedups by WhatsApp `message id` (Meta may
   retry) — reusing io.slack's dedup infra; relays the message as `text/v1` to `io.dst_node`; replies
   out-of-band via the Graph API (§7).

### New edge capability required: **externalize-as-fanout**

Today's edge endpoint is unicast-to-owner and reply-correlated (io.api). io.wapp needs a new endpoint
**mode**:
- **fanout target** (the `IO.wapp` family / a tap group) instead of a single `owner_l2_name`, wired via
  the router tap primitive.
- **ack-fast, no-reply** POST semantics (edge 200s Meta immediately; does not block on a node reply).
- **verify_token challenge**: the edge answers the GET from the stored token; `GET` is allowed on the
  endpoint (io.api hardcodes POST-only).
- **raw-body forwarding**: the fanned-out message carries the exact bytes + the signature header.

The fanout endpoint is created **once** (not per-node): a designated io.wapp (or the admin) externalizes
it as fanout with the app-level `verify_token`; the other io.wapp nodes just subscribe to the fanout.
This capability is reusable by any future family-fanned public ingress. **Security note**: because the
edge does not verify the HMAC, it fans every public POST to N nodes, each rejecting on cheap HMAC —
amplified ×N but bounded by the edge's existing border rate-limit/backpressure. **Isolation note**:
every node transiently sees every number's inbound before filtering — fine for one company's own
numbers; if numbers ever map to mutually-distrusting tenants, a front-demux (a routing io.wapp that
forwards only the matching event) is the fallback.

## 6. Webhook verification & signature (split by layer)

- **GET challenge → the EDGE** (connection handshake). The edge holds the `verify_token` on the fanout
  endpoint registration; it compares `query["hub.verify_token"]` to it (constant-time) and, on match,
  replies `200 text/plain` with the raw `query["hub.challenge"]`; on mismatch → `403`. The node is not
  involved.
- **POST signature → each NODE** (app auth). Each io.wapp recomputes `HMAC-SHA256(app_secret, RAW body)`
  and constant-time-compares to `X-Hub-Signature-256`; default-deny on mismatch. `app_secret` comes
  from the node's vault secret; the edge never sees it. Requires the edge to forward the **raw body**
  + the signature header on the fanned-out message (the node hashes the exact bytes, not a re-serialized
  JSON). The node then filters by `phone_number_id` and dedups by `message id`.

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

## 10. DECISIONS (locked 2026-07-24)

- **D1**: Inbound = **fanout** (§5): one mandatory webhook fanned out to all `IO.wapp` nodes, one node
  per number, each self-selects by `phone_number_id`. Edge stays thin (connection handshake only). This
  supersedes the earlier "response-envelope" idea — the POST is ack-fast/no-reply, so the edge never
  needs to return a node-supplied HTTP body for events.
- **D2**: Edge forwards the **raw body** + `X-Hub-Signature-256` header on the fanned-out message; each
  node verifies the HMAC with its own `app_secret` (the edge does NOT verify signatures).
- **D3**: `boot=true` degraded (io.slack scheme).
- **D4**: Vault secret `resource_type` = `"whatsapp"` (new canonical) vs reuse `"bearer_token"`
  (io-cloud test uses `wapp_token`/`bearer_token`). Recommend a dedicated `"whatsapp"` type.
- **D5**: Default inbound target `io.dst_node` = `AI.generic@motherbee` (matches the io.slack decision).
- **D6**: Template outbound (outside-24h) — defer to v2? (recommended defer).

## 11. Phased plan (after design OK)

1. **Contract + skeleton** — `io_wapp_adapter_config.rs` + `io-wapp` crate booting degraded (control
   plane, CONFIG_GET/SET, vault_ref resolve + refresh + broadcast). Unit-tested; boots UNCONFIGURED.
   ✅ DONE (426b597).
2. **Edge fanout capability** — a new endpoint mode in `sy_edge.rs` (+ the externalize admin flow):
   fanout target (tap group / `IO.wapp` family) instead of unicast owner; GET verify_token challenge
   answered at the edge; **ack-fast** POST (200 Meta immediately, no reply-wait) + fanout of the RAW
   body + signature header. Backward compatible — io.api/io.blob (unicast reply-correlated) unaffected.
   Tests. Security-sensitive; reviewed. ✅ DONE (ac08c8c edge, 9e6f97b admin).
3. **Node inbound** — subscribe to the fanout; verify `X-Hub-Signature-256` (app_secret from vault);
   filter by `phone_number_id`; dedup by message id; parse messages → text/v1 relay to dst_node; media
   in. One io.wapp per number (single-number contract, unchanged). ✅ DONE (implemented + 16 unit
   tests + adversarial review): `webhook.rs` pure parser + constant-time HMAC verify (default-deny);
   `io_context.rs` `wapp_inbound_io_context` + `extract_wapp_post_target`; `run_wapp_inbound_loop`
   (verify → self-select by `phone_number_id` → dedup → identity resolve/provision → text/v1 relay).
   Media relays a text marker + stashes `media_id` in the `raw.wapp` stub (actual download deferred to §4).
   Interactive replies (`button`/`interactive` list/quick-reply) currently relay as an explicit
   `[unsupported message type: …]` marker (no silent drop) — extracting the tapped button/reply title is
   a §4 follow-up. Empty-text bodies are dropped (never relay a blank turn), mirroring the io.slack peer.
4. **Outbound** — Graph messages + 429 retries; media out.
5. **Packaging** — base-nodes.json + Cargo.toml + publish; firstboot degraded boot.
6. **Live validation** — deploy to the dev hive; boots UNCONFIGURED; then a real webhook round-trip
   once a WABA + token are available.

Nothing is coded until D1–D6 are agreed.
