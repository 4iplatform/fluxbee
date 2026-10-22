# ACME certificates for the public edge (v1)

**Status:** implemented in 0.1.82 (`d2980cf`) and validated on 8.x on 2026-10-21: PROD's edge serves
a Let's Encrypt certificate (lab/DEPLOYMENTS.md, 0.1.82). Design of 2026-10-21, revised after one
review round (protocol and security, fit with the code, operations). **Decisions:** the operator's O6 (2026-10-13), D1–D5 and the naming rules
(2026-10-20). **Findings:** FINDINGS A-78 (new), PENDING-BUGS PB-1.

Each installation's public edge serves its own certificate from an ACME certificate authority, issued
and renewed with no human step, validated with TLS-ALPN-01 on the :443 the edge already opens. The
CA is Let's Encrypt unless configured otherwise. Nothing in the code names a domain or a CA we run:
the names and the CA come from `hive.yaml`, so the open-source package works under any domain.

Out of scope for v1:
- `cloud.<id>` and `archi.<id>`: SNI passthrough (O5a); they will reuse this machinery.
- More than one edge per installation (O7).
- DNS automation and wildcard certificates.
- External Account Binding, for CAs that need it.

---

## 1. Decisions

| ID | Decision |
|---|---|
| O6 | ACME, TLS-ALPN-01 on :443, one certificate per holder. No port 80, no DNS credentials on any host, no wildcard. |
| D1 | The ACME client is SY.admin on the motherbee, its account key in SY.vault. SY.admin is the only node allowed to command the edge (`policy/system.rego` rule (1)). It already holds `admin.public_base_url` and `admin.public_edge_node` and writes the edge's vault secrets, so no new exception to "the admin mediates every action" is needed. The edge serves what it is told and reloads when told. |
| D2 | The edge swaps its certificate without restarting (PB-1), in this pass. |
| D3 | Renew in the window the CA suggests (ARI, RFC 9773), else at two thirds of the lifetime. Alerts are relative to the lifetime; O6's fixed 21-day alert goes. |
| D4 | The CA's default profile unless configured. At Let's Encrypt that is `classic`: 90 days today, 64 from 2027-02-10, 45 from 2028-02-16. Every rule here works with 45-day certificates. |
| D5 | First an order against Let's Encrypt staging with the real PROD name, never installed; then production. |
| Names | The certified name is the host of `admin.public_base_url`, so the URLs the system hands out and the certificate cannot disagree. |
| Egress | The internet probe takes its target from `hive.yaml` (`egress.reachability_url`). There is no default. |
| Examples | Shipped examples use neutral names (`edge_tls`, `example.com`). |

**Added by the review, and consistent with "no magic automation":**
- ACME replaces automatically only certificates it issued itself.
- Moving a certificate seeded by hand (PROD's wildcard) to ACME, or certifying a new name, is an
  explicit, confirmed `POST /acme/renew`.
- A rollback to a hand-seeded certificate therefore stays in place.

---

## 2. What exists (verified 2026-10-21)

- **SY.edge** (`src/bin/sy_edge.rs`): rustls 0.23.37 with ring, tokio-rustls 0.26.4, hyper 1.8 through
  hyper-util's auto builder.
  - **TLS:** one certificate for every client (`with_single_cert`, 1855-1885), an empty ALPN list, SNI
    ignored.
  - **Accept loop** (1748-1774): it has no handshake timeout.
    `run_frontend(…, None)` serves plaintext (1781-1785). Disk TLS (316-326) and `allow_plaintext`
    ingresses also exist.
  - **Boot:** it reads `{cert, key}` from SY.vault under `edge.tls_vault_key` (2122-2161). Any failure
    exits, so systemd restarts it in a loop (330-345).
    `from_vault_error` folds every verdict into `MaterialInvalid`, retryable ones included
    (2091-2107).
  - **Fingerprint:** today it is the SHA-256 of the whole `cert` PEM, chain included (2151-2158).
  - **PB-2:** `TlsChainDiagnosis` (1808-1853) catches a truncated chain. Boot logs it; a reload refuses
    it (2053).
  - **System messages** are handled one at a time (404-682). `EDGE_PUBLISH_BLOB` waits up to 60 s in
    line; a reload probe can take about 55 s.
  - **On `VAULT_SECRET_CHANGED`** it probes the new secret and exits (465-657).
- **That notice never reaches the edge (A-78).**
  - The router's edge gate (`vpn_allows_between`, `src/router/mod.rs` ~5976) comes before the global
    SYSTEM bypass. It admits only the `EDGE_*` commands and their answers, the IO data path and
    `VAULT_GET` (~5965), and the three broadcast fan-outs apply it.
  - In PROD, SY.vault has sent 146 notices, and ingress1's journals hold none since 2026-08-05.
  - So PB-1's restart never happened: the 2026-07-30 logbook saw the old certificate served after a
    put.
  - Channel secrets are not affected: a rotation reaches the edge through a new `EDGE_OPEN_URL`, with
    its grace window.
- **Edge commands** come only from `SY.admin@motherbee`: `system.rego` rule (1), and
  `authorize_edge_service_command` (sy_edge.rs:1179). A new command touches:
  - the SDK constants (`crates/fluxbee_sdk/src/protocol.rs` 544-553);
  - `PROTECTED_SYSTEM_ACTIONS` and its tests (`src/router/system_policy.rs` 36-62; the count goes from
    27 to 30);
  - the router's edge gates (mod.rs 5910-5946);
  - `edge_service_actions` in `system.rego`, and both wasm files, rebuilt on Linux (the Go
    `compile-file` tool is Linux-only);
  - the edge's handler;
  - `docs/nodes/sy-edge.md`.
- **SY.vault:**
  - Keys use only `[a-z0-9:_-]` (`crates/fluxbee_sdk/src/vault.rs` 571-591).
  - SY.admin may `put` and `rotate` any secret (the admin bypass) but cannot read the value of one
    owned by the edge (`src/bin/sy_vault.rs` 1655-1695).
  - A `put` keeps the previous version and restates the owner. A `rotate` keeps the stored metadata
    as it is (1394-1398). Exactly one previous version is kept.
- **SY.admin** (`src/bin/sy_admin.rs`):
  - reads `admin.public_base_url` and `admin.public_edge_node` once at boot (592-626; env vars
    override);
  - sends edge commands with `send_system_request_with_meta` (15547);
  - runs a periodic sweep (`run_publication_expiry_sweep`, 4648-4659), with its state under
    `state_dir/sy-admin/`;
  - its HTTP client is reqwest with rustls (already in Cargo.lock through the AI SDK).
- **The motherbee does not know the ingress' `edge.tls_vault_key`.** Only the ingress' `hive.yaml`
  holds it.
- **Drift alerts:**
  - only SY.orchestrator writes them (`append_drift_alert`, 13185-13251);
  - they are history rows with no resolved state, read with `GET /drift-alerts`;
  - a new admin action that writes them must be classified for the orchestrator's F8 origin gate
    (`classify_admin_action`).
- **The internet probe** (`check_internet_reachable`, sy_orchestrator.rs 7219-7238) targets
  `EGRESS_PING_TARGET = "fluxbee.ai"`. It runs on the egress, on workers that route through it, and
  on the motherbee. It is the only domain in code, besides a test header.
- **Outbound:** the PROD motherbee reaches Let's Encrypt production and staging. Both directories
  offer `renewalInfo` and the profiles `classic`, `tlsserver` and `shortlived`.

---

## 3. Configuration, and what lives where

The motherbee's `hive.yaml`:

```yaml
admin:
  public_edge_node: "SY.edge@ingress1"
  public_base_url: "https://hive-k3m9x7q2.example.com"   # its host is the certified name
acme:
  terms_agreed: true                    # required: the operator accepts the CA's terms
  directory_url: "https://acme-v02.api.letsencrypt.org/directory"            # default
  staging_directory_url: "https://acme-staging-v02.api.letsencrypt.org/directory"   # default
  profile: "classic"                    # optional: the CA's default when absent
  contact: "mailto:ops@example.com"     # optional (Let's Encrypt no longer emails)
  trust_anchor: "/etc/fluxbee/acme-ca.pem"   # optional, only for a private CA
```

- **Without `acme:`** nothing is ordered. The edge serves a certificate seeded by hand, as today, and
  the expiry checks and alerts of §7 still run.
- **A section that cannot work turns ACME off.** SY.admin keeps running, and `/acme/status` and the
  log say why. That happens when:
  - `terms_agreed` is not `true`;
  - `public_base_url` is missing, uses a port other than 443, or its host is an IP literal, a
    wildcard or has no dot;
  - the profile is not one the directory offers, or it is `shortlived`, whose six-day certificates do
    not fit the 24 h backoff cap.
- **The name** is the host of `public_base_url`, lowercased, with no trailing dot. It is read at boot,
  so a new name takes effect when SY.admin restarts. Replacing the certificate for the new name is
  an explicit `POST /acme/renew`: URLs already handed out keep working until the operator decides.
- **No ingress** (no `public_edge_node` and no connected ingress): `/acme/status` reports
  `not_configured`.

| What | Where | Owner | Notes |
|---|---|---|---|
| The certificate and its private key | SY.vault, under the ingress' `edge.tls_vault_key` (PROD `edge_tls_fluxbee_ai`, new installs `edge_tls`) | `SY.edge@<ingress>` | `{cert, key}` PEM, `resource_type: tls`, root tenant. One previous version for rollback. Only the edge reads it. |
| The ACME account | SY.vault, `acme_account:<16 hex of SHA-256(directory_url)>` | `SY.admin@motherbee` | The library's credentials JSON; the URL in the metadata description. Cloud and the admin API cannot write it. |
| Order state | `state_dir/sy-admin/acme.json` on the motherbee | | The account URL per directory, the order in flight, the last attempt, the last 20 issued certificates (serial, dates, profile), the last edge status seen, the conditions. No key. |
| The name | `admin.public_base_url` (motherbee) | | |
| The edge's secret name | `edge.tls_vault_key` (ingress) | | SY.admin asks the edge for it, and accepts only names that start with `edge_tls`. |

---

## 4. The edge

**TLS mode.** `run_frontend` takes an explicit mode: `plaintext`, `disk`, or `vault {key}`. Only a
`vault` edge serves ACME; the other modes report themselves in `EDGE_TLS_STATUS`, and SY.admin
leaves them alone.

**One acceptor, one choice per connection.** The edge accepts with `tokio_rustls::LazyConfigAcceptor`.
One 10 s timeout covers reading the ClientHello and the handshake, on every path.

| ClientHello | Configuration | Then |
|---|---|---|
| ALPN is exactly `["acme-tls/1"]` (RFC 8737), and a challenge is armed for its SNI | that challenge's: `alpn_protocols = ["acme-tls/1"]` and the challenge certificate | finish the handshake and close; HTTP is never reached |
| ALPN is exactly `["acme-tls/1"]`, nothing armed for its SNI | none | drop |
| anything else, `acme-tls/1` mixed with other protocols included | the current main configuration (empty ALPN list, as today) | today's path, unchanged |

- **The challenge certificate** is minted by the edge, so no private key crosses the mesh:
  - a fresh P-256 key and an empty subject;
  - SAN = the name, lowercase, no trailing dot;
  - the critical `acmeIdentifier` extension with the 32-byte digest, from rcgen
    `CustomExtension::new_acme_identifier`. The hex is validated before it reaches rcgen, which
    asserts the length.
- **An armed challenge** lives in memory under its name until it expires (600 s by default, 1800 s at
  most) or is cleared. A clear names both the name and the digest, so a late clear never removes a
  newer arm.
- **Hot reload (PB-1).** The main configuration sits behind a lock that each new connection reads.
  - A reload re-reads the secret once (no 55 s probing on this path) and checks it with the shared
    verdict below.
  - It swaps only when the check passes; open connections finish on the old configuration.
  - A refused reload keeps the current certificate and answers why.
  - Nothing exits.
- **The verdict, one function in the SDK,** shared by SY.admin's step 5 and the edge's reload. The
  chain must:
  - parse;
  - be complete, a leaf plus at least one intermediate (PB-2);
  - have a key that matches the leaf;
  - have a leaf that is valid now (not expired, not before its start).
- **The fingerprint** reported anywhere is the SHA-256 of the leaf's DER, in hex.
- **Awaiting.** When the vault says the secret is missing or unusable, the edge starts with no main
  configuration.
  - It answers armed `acme-tls/1` handshakes and drops every other TLS handshake. It never serves
    plaintext.
  - It re-reads the secret every 30 s, backing off to 5 min, and leaves this state on its own when a
    good secret appears.
  - Today a missing secret makes the edge exit in a loop, so a new ingress could never answer the
    challenge for its first certificate.
  - The vault's verdict is kept typed: `not_found`, `forbidden`, `invalid_material`, or `transient`
    (the vault did not answer, or answered `IDENTITY_UNAVAILABLE`). A transient verdict keeps
    retrying.
- **Commands run beside the message loop.** The three TLS commands are handled in their own task over
  the shared state, so they never wait behind `EDGE_PUBLISH_BLOB` or a secret resolution.

**New commands**, from `SY.admin@motherbee` only:

| Command | Request | Answer (with `status`) |
|---|---|---|
| `EDGE_TLS_STATUS` | `{}` | `mode`, `tls_vault_key`, `state` (`serving` or `awaiting`), `reason` (the typed verdict), the served leaf as PEM, `fingerprint`, `not_before`, `not_after`, `names`, `issuer`, `chain_len`, `loaded_at` |
| `EDGE_TLS_RELOAD` | `{}` | one re-read and verdict; the STATUS fields plus `changed` and, when refused, `refused` |
| `EDGE_ACME_CHALLENGE` | `{name, digest, ttl_secs?}` to arm; `{name, digest, clear: true}` to clear | `{name, armed, expires_at}` |

**`VAULT_SECRET_CHANGED`** for the edge's own key does what `EDGE_TLS_RELOAD` does. It never exits.

---

## 5. Router, policy and SY.vault

- **The three commands** join:
  - `edge_service_actions`, rule (1): `SY.admin@motherbee` only;
  - `PROTECTED_SYSTEM_ACTIONS`;
  - the router's edge gates.

  Both wasm files are rebuilt in a Linux container, or on the build box, and committed with the
  rego.
- **A-78, without opening the DMZ to the vault's inventory.** The notices are broadcasts that carry
  every stored key's name, tenant and owners, and SY.vault re-announces all of them at boot. The edge
  must not see them.
  - **SY.vault** also sends a *unicast* notice to the owner when the owner is an `SY.edge`. It does so
    on put, rotate, rollback and delete, and in the boot re-announce.
  - **The router's edge gate** admits `VAULT_SECRET_CHANGED` only as a unicast from SY.vault on the
    primary hive to an `SY.edge`. It still drops the broadcast.
- **SY.vault refuses an `SY.edge` reading a general (unowned) secret.** Today that read fails only
  because the edge's identity does not resolve on the motherbee. An explicit rule keeps it closed if
  that ever changes.

---

## 6. SY.admin, the ACME client

**Library:** `instant-acme` 0.8.5, pinned exactly, with features `ring`, `hyper-rustls` and `time`.
- It brings no aws-lc. Its effective MSRV is 1.88; we build with 1.92.
- It adds rcgen 0.14 next to the workspace's 0.13. Porting `mesh_tls.rs` is a separate change.
- The ARI identifier is built with `CertificateIdentifier::new(aki, serial)` from x509-parser 0.16.
- The terms URL comes from a GET of the directory.
- Every call has a timeout of its own: the library sets none, and its order polling gets 5 min.
- The library verifies the CA's TLS against the system store, so the `.deb` now depends on
  `ca-certificates`.

**The account:**
- One per directory URL.
- Created on first use, with `terms_agreed` and `contact`.
- The credentials go to the vault (§3), and `acme.json` records the account URL.
- A vault credential whose account URL differs from that record is refused: a swapped account could
  otherwise reuse an authorization from the last 30 days without the edge.

**One order at a time, for every path.** The check, `POST /acme/renew` and `POST /acme/test` share a
single in-flight slot. A busy slot answers 409.

1. **Look.** `EDGE_TLS_STATUS`: the mode, the secret name, the served leaf.
   - Refuse unless the mode is `vault` and the name starts with `edge_tls`.
   - When the secret exists, its metadata must say `resource_type: tls`, root tenant, owner = this
     edge. That keeps a compromised edge from steering SY.admin's write onto another secret.
   - Confirm the vault answers before any CA call.
2. **Order.** `newOrder` for the name, with the profile.
   - `replaces` is the served leaf's ARI identifier, and only when `acme.json` records that serial as
     issued to this account by this directory.
   - If an error points to `replaces`, the order is retried once without it.
   - The order URL is recorded at once. After a restart the order is resumed (`Account::order`), not
     placed again.
   - An abandoned authorization is deactivated.
3. **Challenge.** `EDGE_ACME_CHALLENGE` arms the TLS-ALPN-01 digest for 600 s. Then the challenge is
   marked ready and the order polled for up to 5 min.
   - An `invalid` result fetches the authorization's error.
   - The challenge is cleared afterwards, whatever happened.
4. **Finalize.** A new P-256 key and a CSR (empty subject, SAN = the name); finalize; fetch the chain.
5. **Check before storing:**
   - the shared verdict (§4);
   - the leaf covers the name;
   - the chain verifies to a trusted root for that name: the platform roots, or `acme.trust_anchor`;
   - `not_after` is more than 2 days away.

   A staging or test-CA certificate fails here, so it is never installed by mistake.
6. **Store.** `put` the edge's secret with explicit metadata (`resource_type: tls`, owner = the edge,
   root tenant). A put keeps the previous version and restates the owner. If the vault does not
   answer, the material is kept in memory and the write retried for 10 min. `acme.json` records the
   certificate as issued and not installed (serial and dates, no key).
7. **Reload.** `EDGE_TLS_RELOAD`, with a 75 s timeout; the edge must answer with the new fingerprint.
   - **The edge refuses:** roll the secret back at once, raise `install_refused`, and order nothing
     until the operator acts. Nothing is ever written while the edge does not serve the last version
     written; this rule keeps the previous version from being pushed out.
   - **No answer:** the certificate stays in the vault, and the next check retries the reload, never a
     new order.
8. **Record** the result in `acme.json`.

The private key exists in SY.admin's memory and in the vault only. It is never logged and never
written to disk.

**The test order (D5).** `POST /acme/test` takes no URL.
- It runs steps 1–5 against `staging_directory_url`, with that directory's account and without
  `replaces`. Step 5 expects an untrusted root (staging) and reports it as such.
- It is refused when `staging_directory_url` equals `directory_url`.
- It never stores or installs anything.
- It is a mutating action, because it arms the edge and creates an account: it needs confirmation,
  answers 202, and shows its progress in `/acme/status`.

---

## 7. When to renew, and the alerts

**The check** runs 2 min after SY.admin starts, then every 6 h plus a random delay of up to 30 min.
It honours the backoff and `rateLimited` times kept in `acme.json`, so a restart never spends an
order. It reads `EDGE_TLS_STATUS`:

| The edge | `acme:` configured | `acme:` absent |
|---|---|---|
| does not answer, or its router blocks the command (an older hive during an upgrade) | no order, no CA backoff; retry every 10 min; `edge_unreachable` after 1 h; expiry judged from the last status seen | the same |
| `awaiting`, `not_found` | order now: there is nothing to replace | `edge_without_certificate` |
| `awaiting`, any other verdict | `EDGE_TLS_RELOAD` first, then alert: no order, because ordering cannot fix a wrong owner or a bad secret | `edge_without_certificate` |
| serves a leaf this account issued; ARI answers | renew at a random point of the suggested window (RFC 9773), honouring Retry-After | n/a |
| serves a leaf this account issued; no ARI answer | renew at two thirds of its lifetime | n/a |
| `acme.json` holds a newer certificate than the served one | `EDGE_TLS_RELOAD`, never a new order | n/a |
| serves a leaf seeded by hand, or for another name | alert only (`foreign_certificate` at its renewal point, `name_not_covered`); `POST /acme/renew` replaces it | expiry checks only |

- **Name coverage:** a wildcard SAN covers one leftmost label. PROD's `*.fluxbee.ai` covers
  `hive-k3m9x7q2.fluxbee.ai`.
- **Failures back off** 1 h, 2 h, 4 h and so on, up to 24 h. The failure count and the next attempt
  are kept in `acme.json`.
  - That stays under Let's Encrypt's limit of 5 failed validations per account and name per hour.
  - A `rateLimited` answer waits until the time the CA sends, or 24 h.
- **The cap:** at most one issuance per third of the current lifetime, unless the order is forced. A
  clock far ahead or a bug cannot burn the weekly limit.

**Operator actions:**

| Action | Effect |
|---|---|
| `GET /acme/status` | the configuration and why it is off, if it is; the terms URL; the accounts; the edge's live status; the order in flight and its progress; the last attempt; the next check and the ARI window; the last issued certificates; the conditions open now |
| `POST /acme/renew` | an order now: replaces a hand-seeded or foreign certificate, certifies a new name. Needs confirmation; answers 202. |
| `POST /acme/test` | the staging order of §6. Needs confirmation; answers 202. |
| `POST /acme/reload` | `EDGE_TLS_RELOAD`, after a manual change or a vault rollback |

**Conditions and alerts.** Each check evaluates the conditions; `/acme/status` shows the open ones.
A condition writes a drift alert when it opens, and an `info` row when it closes. A standing condition
does not write every 6 h.

| Condition | Severity | When |
|---|---|---|
| `renewal_failed` | warning | the last attempt failed |
| `renewal_overdue` | warning | 48 h past the renewal point, and the edge still serves the old leaf |
| `expiring` | critical | less than a sixth of the leaf's lifetime is left |
| `name_not_covered` | warning | the served leaf does not cover the name |
| `foreign_certificate` | warning | a hand-seeded leaf has passed its renewal point while `acme:` is set |
| `install_refused` | critical | the edge refused a certificate SY.admin issued |
| `edge_without_certificate` | critical | the edge is `awaiting` |
| `edge_unreachable` | warning | no status for 1 h |

- **Where the alerts go:** SY.orchestrator records them through a new admin action,
  `record_drift_alert`.
  - It is classified as a write for the F8 gate and accepted only from `SY.admin@motherbee`.
  - It takes only the category `certificate` and the kinds above.
- **The lab's `ops.py health`** reads `/acme/status` and fails on a critical condition.
- **When the motherbee itself is down,** nothing renews and nothing alerts. An external monitor of the
  public name's expiry covers that, and `docs/reference/operations.md` says so.
- **For a 45-day certificate:** renewal at day 30, `renewal_overdue` at day 32, `expiring` at
  day 37.5. The 90- and 64-day lifetimes scale the same way.

---

## 8. Open source and naming

- **`egress.reachability_url`** goes in the `egress` section of each hive that runs the probe:
  - the egress itself;
  - the workers that route through it, rendered from the motherbee's section at join;
  - the motherbee.

  `add_hive` checks that it is an https URL. Without it, the probe does not run and reports
  `internet_reachable: null` (`not_configured`); it never sends a ping or a GET to a target the code
  picked. `internet_reachable` becomes optional, and `EGRESS_PING_TARGET` goes.
- **Examples:** `packaging/hive.yaml.example` names the edge's secret `edge_tls` and documents
  `acme:`. The test header becomes `edge.example.com`.
- **Reserved keys:** `acme_account` joins the vault prefixes that Cloud and the admin API may not write.
- **Our brand stays in our configuration:** `fluxbee.ai` remains in PROD's config, the README's project
  link and the docs that describe our installation.

---

## 9. PROD

The secret `edge_tls_fluxbee_ai` holds the Sectigo wildcard, valid until 2027-02-12. Each manual step
below needs the operator's approval and goes in the ledger.

1. **Before the deploy:** add `egress.reachability_url: "https://fluxbee.ai"` to the motherbee's and
   egress1's `hive.yaml`. Older binaries ignore the key. worker1 has no egress section.
2. **Deploy 0.1.82 without `acme:`.** The edge serves the wildcard through the new acceptor.
   `/acme/status` shows it, matched by its wildcard SAN. No condition is expected: `foreign_certificate`
   needs `acme:`.
3. **A-78:** put and delete a throwaway secret owned by the edge. The edge's journal shows both
   notices, and nothing reloads.
4. **Tell the Cloud team** that the issuer changes. The Cloud contract pins nothing.
5. **`POST /acme/test`:** Let's Encrypt staging issues a certificate for the real name, and nothing is
   installed.
6. **Add `acme:` to the motherbee's `hive.yaml`** (`terms_agreed`, production, `classic`), and restart
   SY.admin when it is idle: a restart cuts in-flight executor plans and relayed Cloud commands. The
   check orders nothing, because the wildcard is a hand-seeded certificate.
7. **`POST /acme/renew`,** confirmed. It orders, puts (the wildcard stays as the previous version) and
   reloads the edge.
   - Verify `/acme/status`.
   - Verify from outside: the issuer is Let's Encrypt and the SAN is the name.
   - Repeat the strict chain check of 2026-07-30 (Python `ssl`, no AIA fetching).
8. **Rollback:** `POST /hives/motherbee/vault/secrets/edge_tls_fluxbee_ai/rollback`, then
   `POST /acme/reload`. The check then sees a hand-seeded leaf and leaves it.
   - Before downgrading the code to 0.1.81, which cannot renew, roll the vault back first.
   - The wildcard's original files stay with the operator. After two renewals the vault no longer
     holds it.

**DNS:** `fluxbee.ai` has no CAA record. If one is added, it should be
`issue "letsencrypt.org; validationmethods=tls-alpn-01; accounturi=<the account URL>"` (RFC 8657),
plus `sectigo.com` only to reissue the wildcard.

---

## 10. Tests

- **Edge:**
  - the challenge certificate: critical extension, its value, the SAN;
  - every row of the acceptor table;
  - the 10 s timeout on both the ClientHello and the handshake;
  - hot swap, with an old connection kept;
  - the shared verdict: truncated, expired or not yet valid, key mismatch;
  - `awaiting` with each verdict, and its re-reads;
  - a challenge's expiry and its clear-by-digest;
  - the TLS modes in STATUS;
  - the command origin check;
  - commands answered while the loop is busy.
- **Router, policy, vault:**
  - the truth table for the three commands;
  - A-78's gate: the unicast from SY.vault passes, the broadcast and every other SYSTEM frame do not;
  - SY.vault's unicast to an edge owner;
  - the refused general read by an `SY.edge`.
- **SY.admin:** the ACME calls sit behind a small trait, and these run against a fake:
  - the order flow: success, a challenge failure, a refused install that rolls back, a reload with no
    answer, a vault outage after finalize, the resumed order, the retry without `replaces`, the
    single-flight 409;
  - the decision table of §7, the backoff, the cap, the condition transitions;
  - the config checks, the `edge_tls` and metadata checks, the account URL check, the reserved
    prefix.
- **Live gate (8.x):** §9.
