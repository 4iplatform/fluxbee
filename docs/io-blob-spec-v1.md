# IO.blob / public artifacts - Specification v1

*What comes next for artifacts is in [io-web-v2](io-web-v2.md) §7 (2026-10-23); this spec still
describes the circuit that runs today.*

**Status:** alpha contract implemented and validated on Linux/Proxmox on 2026-07-17: curator,
admin authority, clean ingress install, public replica, `/public` serving and teardown are
operational. What remains is the product smoke with a real AI/IO going through publish/expiry/unpublish.

Read together with `edge-ingress-spec-v6.md` and `blob-annex-spec.md`. This spec defines the
READ-ONLY publication of files generated inside Fluxbee, including self-contained interactive HTML, as
`/public/<key>` URLs served by `SY.edge`.

## 1. Closed decisions

- **`SY.admin` is the authority.** The producer node calls admin directly. Admin keeps the
  `src_l2_name` stamped by the router, resolves its ILK/tenant in Identity and decides whether it publishes.
- **`IO.blob` is a worker, not an authority nor a relay.** It receives an already authorized command from
  `SY.admin`, curates the bytes and keeps the local ledger/refcount. It does not decide tenant, access or URL.
- **Ownership v1 = ownership of the publication.** The current `active/` blob store is shared and the
  `BlobRef` contains no ownership. V1 does NOT claim that it can cryptographically prove that the publisher
  created those bytes; it claims that admin knows which tenant created and controls the publication.
- **First read mode = link-capability.** There is no listing or login at the edge. Anyone who receives
  the link can see it until `expires_at` or `unpublish`. Per-access session/tenant verification is
  the next stage, not a fictitious property of the first release.
- **Self-contained HTML with JavaScript, no network.** JS runs in the browser, never in edge/IO.blob.
  Local interaction (charts, filters, tables, calculations) is allowed inside a sandbox with
  `connect-src 'none'`. Querying APIs or running actions is out of v1.
- **Separate data plane.** The bytes do not travel in mesh frames. Only control and metadata
  travel over RPC; a dedicated `public/` folder replicates bytes one-way to the ingress.

## 2. Actual state of the code at the start

- `BlobToolkit::put_bytes/promote` writes into `blob.path/{staging,active}` and returns an anonymous
  `BlobRef` (`blob_name`, size, mime, filename, spool_day).
- `active/` is a shared store, content-addressed with `hash16`; it has no tenant/owner and today it can
  be replicated `sendreceive` between hives.
- Managed nodes get ILK/tenant through env, but the router only authoritatively stamps the
  `src_l2_name` of the connected process.
- `SY.edge` already serves `/e/<ich>` by forwarding to an IO node. `/b/:ich` is a 501 stub and there is no static
  serving.
- The ingress is provisioned with blob/syncthing disabled.

Therefore, this spec adds a new, bounded circuit; it does not rename as finished something that already
existed.

## 3. Invariants

- **P1 - Producer identity survives.** The first mutating command is `producer -> SY.admin`; it does not go
  through `IO.blob` first. The request does not accept `tenant_id` or `publisher_l2_name` as authority.
- **P2 - Admin owns policy.** Admin resolves `src_l2_name -> ILK -> tenant`, creates `publication_id`,
  defines the expiration/content policy and is the only authority that can order curate/release.
- **P3 - IO.blob accepts one caller.** It only accepts SYSTEM control from `SY.admin@<same-hive>`.
- **P4 - Public folder is curated.** `IO.blob` is the only normal writer of `blob.path/public`.
  Presence in that folder does not authorize a URL.
- **P5 - DMZ never receives `active/`.** The ingress receives only `public/`, through a separate folder
  `fluxbee-blob-public`: motherbee `sendonly`, ingress `receiveonly`.
- **P6 - Allowlist before bytes.** Edge serves only a `BlobRow` pushed/ACKed by admin. An unknown,
  expired or unpublished key returns 404.
- **P7 - Two identifiers.** URL key = random 256-bit capability. Public filename = full lowercase
  SHA-256 of the bytes. The URL never contains the hash of the content.
- **P8 - Data ready before URL ready.** Admin does not return success until edge confirms a valid row,
  and a local file that is present, regular and has the expected SHA-256.
- **P9 - Dedup needs refcount.** Several publications can point to the same SHA. Release deletes the
  file only when no publication is left in the ledger.
- **P10 - Read-only.** There is no upload, PUT, DELETE or external write API.

## 4. v1 flow

```text
AI/IO/WF producer (tenant T)
   |  ADMIN_COMMAND publish_artifact {blob_ref, presentation, expires_in_secs?}
   v
SY.admin (authority)
   |  router-stamped src_l2 -> Identity -> tenant T
   |  policy clamp + publication_id
   |  SYSTEM BLOB_CURATE {publication_id, tenant_id, publisher_l2_name, blob_ref}
   v
IO.blob@motherbee (worker)
   |  validate BlobRef -> resolve active file -> regular/no symlink
   |  stream SHA-256 -> atomic copy/hardlink policy -> public/<sha256>
   |  durable ledger + refcount
   v
public/ (sendonly) === Syncthing ===> ingress public/ (receiveonly)
   |                                      |
   | admin retries EDGE_PUBLISH_BLOB      | edge checks file + hash
   +-------------------------------------> SY.edge
                                              |
                                              v
                                  https://content-host/public/<key>
```

### 4.1 Publish request to admin

Producer request:

```json
{
  "action": "publish_artifact",
  "params": {
    "blob_ref": {
      "type": "blob_ref",
      "blob_name": "report_ab12cd34ef56ab78.html",
      "size": 12345,
      "mime": "text/html",
      "filename_original": "report.html",
      "spool_day": "2026-07-17"
    },
    "presentation": "inline",
    "expires_in_secs": 86400
  }
}
```

Forbidden in params: `tenant_id`, `publisher_l2_name`, path, public key, sha256 filename, raw CSP,
raw headers or edge row. Admin derives/mints all of that.

**AI producer surface (D1):** publishing is an **action invoked by the model**, not an automatic
hook — not every generated HTML ends up published. `ai.generic` exposes the tool
`publish_html_page {filename, content}` (registered only when the node has a resolved identity):
it validates the HTML with the same gate as `generate_html_artifact`, writes the blob (`put_bytes` +
`promote`) and runs `publish_artifact` against `SY.admin` over the mesh socket, returning the capability
URL to the model so that it delivers it in its answer. The node's journal names the publication's id,
never the URL (FINDINGS A-95). What the model is told about the tool is its card, in
[reference/agent-tools.md](reference/agent-tools.md). An IO/WF producer would do the analogous thing with its
own trigger; the admin contract is identical.

### 4.2 Admin -> IO.blob worker RPC

SYSTEM commands:

- `BLOB_CURATE` -> `BLOB_CURATE_RESPONSE`
- `BLOB_RELEASE` -> `BLOB_RELEASE_RESPONSE`
- `BLOB_STATUS_GET` -> `BLOB_STATUS_GET_RESPONSE`

`BLOB_CURATE` carries facts already resolved by admin. `IO.blob` validates the exact caller but does not
re-decide the authorization. Successful response:

```json
{
  "status": "ok",
  "publication_id": "pub:<uuid>",
  "public_name": "<64 lowercase hex>",
  "sha256": "<64 lowercase hex>",
  "size": 12345,
  "created": true,
  "ref_count": 1
}
```

Idempotency: repeating the same `publication_id` with the same facts returns success without incrementing
the refcount. Repeating it with different facts returns conflict.

### 4.3 Admin -> edge row

`EDGE_PUBLISH_BLOB` contains only normalized facts:

```json
{
  "key": "<64 hex random capability>",
  "publication_id": "pub:<uuid>",
  "public_name": "<sha256>",
  "sha256": "<sha256>",
  "size": 12345,
  "content_type": "text/html; charset=utf-8",
  "presentation": "inline",
  "expires_at": 1784400000,
  "content_policy": "sandboxed-html-v1"
}
```

Edge stores this row in a registry separate from the ICH map. The key is never concatenated as a path; the
lookup produces the validated `public_name`.

## 5. Access v1 and the next tenant stage

### 5.1 V1: link-capability

`GET /public/<key>` does not require a second bearer. The random key is the capability. Properties:

- 256 random bits, exact lookup, no listing;
- revocable by unpublish;
- `expires_at` applied by edge;
- `X-Robots-Tag: noindex, nofollow, noarchive`;
- `Referrer-Policy: no-referrer`;
- it does not prove that the viewer belongs to the tenant.

Cloud must deliver the link only to the right tenant, but whoever forwards it shares access. This is
an explicit product decision for the first release.

### 5.2 Next stage: tenant-gated access

Cloud validates its OAuth/tenant session and asks admin for a short capability for a publication. The
edge validates signature/expiration, without learning OAuth or accepting a free tenant from the browser. This adds
a separate `mint_artifact_access`; it does not change ownership or the data plane. Until it is implemented, the
spec does not call a link "tenant-private".

## 6. Self-contained interactive HTML

The HTML generated by AI is untrusted code. Edge/IO.blob do not run it; the browser runs it.
V1 allows local JavaScript with a fixed policy, which the publisher cannot control:

```text
Content-Security-Policy:
  sandbox allow-scripts;
  default-src 'none';
  script-src 'unsafe-inline';
  style-src 'unsafe-inline';
  img-src data: blob:;
  font-src data:;
  connect-src 'none';
  form-action 'none';
  object-src 'none';
  base-uri 'none'
```

Also:

- `X-Content-Type-Options: nosniff`;
- a content host without cookies and separate from the Cloud/API origin;
- Cloud embeds it with `iframe sandbox="allow-scripts"` when it is part of its UI;
- no forms, popups, parent navigation, service workers or network requests;
- self-contained HTML/CSS/JS/data in a single file for v1.

For reporting, a versioned and trusted JS renderer that consumes JSON generated by AI is preferred.
Arbitrary HTML+JS is still allowed inside the sandbox; it gets no Fluxbee APIs.

## 7. IO.blob worker

`IO.blob` is a motherbee-only managed runtime and does not externalize ICH. Config:

- `IO_BLOB_NODE_NAME` default `IO.blob`;
- `IO_BLOB_ADMIN_HIVE` default local hive;
- `IO_BLOB_BLOB_ROOT` default `/var/lib/fluxbee/blob`;
- `IO_BLOB_PUBLIC_ROOT` default `<blob_root>/public`;
- `IO_BLOB_LEDGER_PATH` default `/var/lib/fluxbee/state/io-blob/publications.json`;
- `IO_BLOB_MAX_BYTES` default to the canonical Blob SDK limit.

It is part of `system_nodes.motherbee` as a packaged lifecycle node: the Orchestrator starts,
waits for and supervises `io-blob.service` together with the base topology. This inclusion does not change its
identity boundary: it is still an `IO.*` worker, visible in the inventory of managed nodes,
and `SY.identity` does not create a deterministic ILK of type `system` for it. `SY.vault` also excludes
non-`SY.*` entries when it builds its local allowlist of known authorities, so being in
this lifecycle does not grant access to the secret pool of the root tenant.

Responsibilities:

- exact caller gate `SY.admin@<hive>`;
- validate schema/BlobRef/publication facts;
- resolve only through `BlobToolkit`, never accept paths;
- reject symlink/non-regular and size mismatch;
- full SHA-256 computed by streaming;
- atomic write by descriptor inside `blob/public` (exclusive temporary file and `linkat`), with the
  owner of that folder and `0640` permissions (FINDINGS A-56);
- atomic ledger, idempotency and refcount;
- safe release and operational status;
- no Identity SHM, no tenant decision, no edge call, no public HTTP.

## 8. Edge static serving

`SY.edge` adds a bounded local data path for `/public/:key`, replacing the `/b/` stub. It is a
deliberate exception to the principle "edge only forwards": large bytes do not fit in the envelope and must
not go back into the mesh.

Requirements:

- typed registry separate from the ICH registry;
- only `GET` and `HEAD`; `Range`/conditional GET recommended for PDF/media;
- semaphore separate from the `/e/` RPC, with the permit held during the whole stream;
- key -> row -> `public_name`; never key -> path;
- `public_name` exactly 64 lowercase hex;
- open no-follow, regular file, canonical root and size/hash readiness at publish;
- headers derived from a fixed policy, never arbitrary headers from the producer;
- unknown/expired/missing/unready -> 404 fail-closed.

Edge configuration generated by `add_ingress`:

- `edge.publications_path`: `/var/lib/fluxbee/state/sy-edge/publications.json`;
- `edge.blob_public_root`: `<blob.path>/public`;
- `edge.public_max_inflight`: default `128`;
- `edge.public_ready_timeout_ms`: default `60000`.

`GET`/`HEAD` support ETag and a single byte range. The semaphore permit lives during the whole
stream. HTML gets the fixed CSP of section 6; no header comes from the producer.

## 9. Data replication

- New folder id: `fluxbee-blob-public`.
- Source: motherbee `<blob.path>/public`, `sendonly`.
- Destination: ingress `edge.blob_public_root`, `receiveonly`. If the replica has local changes,
  the ingress orchestrator's watchdog logs them and reverts them (`/rest/db/revert`): the folder
  goes back to the motherbee's copy (A-16).
- Point-to-point peer; no automatic discovery/relay/NAT traversal.
- Never add the ingress as a device of the `fluxbee-blob` folder (`active/`).
- A publish does not finish until edge can open/verify the file.
- Unpublish: remove the row first; release/refcount after that; delete the replica in the background.
- `add_ingress` copies the Syncthing vendor, runs `ADD_HIVE_FINALIZE`, exchanges device IDs and
  persists the ingress ID so that `remove_hive` unhooks the peer.
- The public peer profile is exclusive: when it links an ingress, it does not add that device to
  `fluxbee-blob` or to `fluxbee-dist`.
- The change applies automatically to new additions. An ingress created with the previous profile, without
  `blob.sync.public_enabled`, must be reprovisioned through remove/add; this version does not silently
  rewrite the `hive.yaml` of an existing ingress.

## 10. Honest threat boundaries

- A leaked capability link allows reading until expiry/unpublish.
- `active/` has no strong ownership. A publisher that knows a BlobRef can ask to publish those
  bytes; admin controls who creates the publication, not who created the content.
- Managed nodes run today without Linux isolation per tenant. Strong ownership of bytes requires a
  mediated writer and non-root/per-tenant workloads; it is out of this vertical.
- The DMZ contains the plaintext of the published artifacts. Compromising the edge allows reading that set,
  but not the `active/` corpus.
- Arbitrary JavaScript is still hostile content; sandbox + no-network is a mandatory part of the
  contract, not an option of the publisher.

## 11. Implementation order

1. **[IMPLEMENTED LOCALLY]** `IO.blob` curator worker: admin-only RPC, full SHA-256, atomic public copy,
   ledger/refcount, and packaging as a managed runtime.
2. **[IMPLEMENTED LOCALLY]** `SY.admin publish_artifact/unpublish_artifact`: direct producer gate,
   Identity tenant resolve, command to IO.blob, durable publication ledger and mesh-only help in the
   Admin catalog (outside the HTTP executor).
3. **[IMPLEMENTED LOCALLY]** One-way Syncthing `fluxbee-blob-public` on motherbee/ingress.
4. **[IMPLEMENTED LOCALLY]** `SY.edge /public/:key` + BlobRow + readiness/hash ACK + headers/sandbox.
5. **[VALIDATED IN LAB]** Infra E2E: clean `add_ingress`, TCP peer connected, one-way replica of
   interactive HTML, restart, `GET`/`HEAD`/range/ETag/CSP and `remove_hive` with no Syncthing drift.
6. **[IMPLEMENTED]** Producer surface + full lifecycle (the code compiles, live smoke
   pending): tool `publish_html_page` in `ai.generic` (§4.1); admin-side expiration GC
   `run_publication_expiry_sweep` (every 600s: `MSG_EDGE_UNPUBLISH_BLOB` + `MSG_BLOB_RELEASE`,
   ledger→`expired`, idempotent); operator route `POST /artifacts/unpublish` (revokes any
   tenant, D2); and durable audit of `publish_artifact`/`unpublish_artifact` in the command-log.
7. **[PENDING PRODUCER SMOKE]** A real AI/IO generates HTML and goes through `publish_artifact`, expiry and
   `unpublish_artifact` over the installed mesh. Runbook: `lab/blob-public-smoke.md`.
8. **[FUTURE]** `mint_artifact_access` for per-access tenant validation through Cloud.

## 12. Out of v1

- Upload or writes from the internet.
- API calls from published HTML.
- Multi-file sites, service workers or full applications.
- Public listing/directory.
- Strong ownership of bytes prior to publication.
- OAuth/cookie session interpreted directly by edge.
