# Smoke: the public artifacts circuit (io.blob) — publish / expiry / unpublish

E2E test of the full circuit: a producer node (AI) generates an HTML page, **publishes** it
as a read-only public URL, it is **served** by `SY.edge`, it **expires** by itself via TTL, and the
operator can **revoke** it. It closes the pending "product smoke" in
`docs/io-blob-spec-v1.md`.

Real topology (prod): **motherbee** runs `SY.admin` + `IO.blob` + `AI.chat`;
**ingress** runs `SY.edge`. The `public/` bytes replicate motherbee→ingress over syncthing;
the control travels over mesh RPC. `publish_artifact` is **mesh-only** (the producer
calls it over the socket; admin resolves its tenant from the router-stamped `src_l2_name`,
never asserted). The operator can NOT publish over HTTP —only revoke.

## 0. Prerequisites

- New binaries deployed on the VMs: `sy_admin`, `sy_edge`, `io-blob`, `ai_node_runner`
  (a rebuild of the `.deb` or a hot-swap of these four). Rust toolchain **1.92** (ethnum breaks on 1.97).
- motherbee + ingress up and joined (add_ingress done, the ingress's `info.yaml` at
  `status: connected`).
- `AI.chat@motherbee` with its identity resolved (vault present) — only then is the
  `publish_html_page` tool registered. And with an LLM provider configured (so that it decides to call the tool).

## 1. Config (Step 3)

Admin needs to know which edge to push the row to and with which base to build the public URL.
Two ways (env wins over hive.yaml):

**Via env — systemd drop-in for `sy-admin` (motherbee):**
```ini
# /etc/systemd/system/sy-admin.service.d/public-artifacts.conf
[Service]
Environment=SY_ADMIN_PUBLIC_EDGE_NODE=SY.edge@<hive-ingress>
Environment=SY_ADMIN_PUBLIC_BASE_URL=https://<ingress-ip-or-dns>:8443
```
> If `SY_ADMIN_PUBLIC_EDGE_NODE` is omitted, admin auto-discovers the `connected` ingress from
> `hives/*/info.yaml` (`resolve_public_edge_node`). `SY_ADMIN_PUBLIC_BASE_URL`, on the other hand, is worth
> setting: without it the reply comes with `public_url: null` and the tool falls back to the relative path
> `/public/<key>`.

**The edge (ingress) already listens** on its public listener (`SY_EDGE_HTTP_LISTEN` or
`edge.listen` in hive.yaml, gated on role=ingress). With TLS: `/public/<key>` on `:8443`.

`systemctl daemon-reload && systemctl restart sy-admin` after the drop-in.

## 2. Publish (the producer leg — the only one that creates the URL)

Send `AI.chat` a message that asks it to publish. The model calls `publish_html_page`
(which validates the HTML, writes the blob, promotes it, and does `publish_artifact` to admin over the socket):

> «Generá una página HTML simple autocontenida que diga "hola fluxbee" y **publicala**
>  como URL pública.» (Generate a simple, self-contained HTML page that says "hola fluxbee" and
>  **publish it** as a public URL.)

The tool returns `{status:"ok", url, publication_id, expires_at}` to the model; the model must
return the URL to the user in its reply.

**Verify the authoritative ledger (motherbee):**
```
GET  http://127.0.0.1:<admin>/publications        # or whatever listing admin exposes
# the publication must be status="published", with edge_node, tenant_id, expires_at
```

**Serve — from a machine that reaches the ingress:**
```
curl -k https://<ingress>:8443/public/<key>        # 200 + the HTML; content-type from the row
```
> `<key>` = the tail of the `url`/`public_url` that the tool returned. It is an opaque 128-bit
> capability token, NOT the sha256.

## 3. Expiry (admin-side GC, `run_publication_expiry_sweep`)

Publish with a short TTL so as not to wait (minimum 60s):
- Repeat step 2 asking for a page with a short expiration, or call the tool with
  a low `expires_in_secs` if a test producer is instrumented.

Admin's reaper runs every **600s**; to force the window, wait for
`expires_at <= now` and for the next tick. Verify:
```
curl -k https://<ingress>:8443/public/<key>        # 404 (the edge rejects expires_at<=now
                                                    #      when serving AND its local reaper drops the row)
# admin ledger: status="expired", released_at set
```
> Defense in depth: the edge drops expired rows on its own (on load and in its
> reaper); admin's sweep ALSO releases the `public/` bytes (`MSG_BLOB_RELEASE` to io.blob)
> and retires the authoritative ledger. The `MSG_EDGE_UNPUBLISH_BLOB` is idempotent (a row already
> dropped by the edge answers `removed:false` → admin treats it as ok, it does not get stuck).

## 4. Unpublish (operator revocation — D2)

Any tenant, from admin's localhost HTTP (motherbee):
```
curl -sS -X POST http://127.0.0.1:<admin>/artifacts/unpublish \
  -H 'content-type: application/json' \
  -d '{"publication_id":"pub:<uuid>"}'
# -> {"status":"ok","payload":{"publication_id":"pub:...","unpublished":true}}
```
Verify: `curl` to `/public/<key>` → 404; admin ledger `status="unpublished"`, `released_at` set.
Idempotent: repeating it returns `unpublished:false` (it already was).

## 5. Success criteria

- [ ] publish via the tool returns a URL and the `curl` serves it (200 + correct HTML).
- [ ] the tenant served is the producer node's (not asserted in the request).
- [ ] expiry: after the TTL, `curl` 404 and ledger `expired` + bytes released.
- [ ] operator unpublish: 404 + ledger `unpublished`; idempotent.
- [ ] admin's command-log (`GET /hives/<hive>/commands`) shows the
      `publish_artifact` / `unpublish_artifact` entries with the correct `origin`.
