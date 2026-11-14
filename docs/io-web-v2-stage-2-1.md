# io.web stage 2.1: exposures and access (build design, decided 2026-11-13)

Base: `daily_onworking_coa` at `74ededeb`, 0.1.144 live. This is the build design of
docs/io-web-v2.md §12.6 step 1. Every claim about existing code carries `file:line`, read on
2026-11-13. Built so far: R1 and R2 (§8), live in 0.1.145 and 0.1.146 (2026-11-13); the rest is not built.

Paths are shortened: `admin` = `src/bin/sy_admin.rs`, `acme` = `src/bin/sy_admin/acme.rs`, `edge` =
`src/bin/sy_edge.rs`, `web/main` and `web/web` = `nodes/io/io-web/src/{main,web}.rs`, `cloud/main` =
`nodes/io/io-cloud/src/main.rs`, `sdk/` = `crates/fluxbee_sdk/src/`.

---

## 0. The operator's decisions (2026-11-13)

Asked with their options and recommendations; the operator took every recommendation:

1. **DNS: one wildcard DNS record per hive**, `*.hive-<id>.fluxbee.ai`, made by hand for now (a DNS
   record, not a wildcard certificate; each host is still its own name on the certificate).
   *"Después veremos cómo hacemos un io.dns para que pueda interactuar con Azure, porque lo necesito
   scopeado a eso.. por ahora a mano."* Every host lands in Certificate Transparency logs, so a label
   never names a customer.
2. **`web:` in hive.yaml is retired.** Archi's host becomes the exposure Cloud publishes with
   `upstream: archi`.
3. **No operator's own close in 2.1.** The brake is `kill_node IO.web@motherbee`. `unpublish_exposure`
   and `list_exposures` are Cloud's.
4. **A renewal is a new enablement with a new secret**, *"lo normal por seguridad, pero con timeout
   suficiente"*. The secret never leaves Fluxbee (§3.1): Cloud stores nothing secret. The times
   that matter, each with its margin: an enable answers only once io.web holds the new key; a retry
   after a lost answer is safe (§6's retries); the previous enablement keeps its sessions to its own
   end, so a renewal before that end overlaps with no cut; a link lasts about 120 s and Cloud asks
   for another when one expires.
5. **The session is a cookie signed with the enablement's key, no table.** *"Si se resuelve con el
   mismo cookie mejor aún el tema del iframe... pero al final va iframe"*: the iframe (2b) is the
   target, and only the cookie's attributes change for it.
6. **Cloud's traffic stored in SY.storage** (§1, FINDINGS A-349): *"los pedidos de Cloud viajan como
   mensajes"*. The fix, no turn for Cloud's channel, goes in R2, the router's release, before Cloud
   uses links outside the gate.
---

## 1. What the code gives, and what it rules out

- **One certificate serves every name.** `route_hello` gives every handshake the one main
  configuration; SNI picks only an armed ACME challenge (edge:2407-2427). The ACME client orders one
  certificate whose names are the main host, then `web:`'s hosts (acme:142-144, 180-199). An
  exposure host is therefore one more name on that certificate, not a certificate of its own.
  §12.1's "certificates per host" is, in the code, one certificate with a name per host.
- **The edge learns its main name from the certificate:** the only name, or the one every other name
  lies under (edge:3734-3750). `host_route` and `validate_web_routes` rely on it
  (edge:3895-3930, 3724-3730). Hosts must therefore be `<label>.<main>`; a third party's own domain
  cannot be an exposure host.
- **Names and routes are fixed at boot.** `WebConfig` (admin:731-732) feeds both the ACME names
  (admin:735-736) and the routes SY.admin pushes every 60 s (admin:906, 1094-1112). A name the
  served leaf misses waits for that leaf's renewal point (acme:1407-1410, 1468-1497), or for the
  operator's forced `POST /acme/renew` (acme:4061-4076).
- **The edge's web routes** take one upstream, `io.web` (edge:3533-3546). It replaces the set whole,
  keeps it in `web_routes.json` and serves it again at boot (edge:1587-1589, 3575-3692). Those
  commands arrive from `SY.admin@motherbee` only, on their own channel (edge:3186-3250).
- **"Cloud only" needs work.** A-62 lets Archi ask for every listed action except `Nobody`'s
  (admin:6321-6326), and the HTTP API and the executor reach every action (admin:6018-6023).
  io.cloud's relay arrives as `AdminOrigin::Mesh(<router-stamped name>)` (admin:4780-4790,
  10733-10764).
- **io.web's config is open to Archi through SY.admin.** `set_node_config` is `WorkflowNode`, which
  Archi gets (admin:6051); `node_control_config_set` is Archi's (admin:6120); `send_node_message`
  sends any system message as SY.admin's own, refusing only `COGNITION_CLEAR` (admin:22590-22634,
  A-160); io.web takes CONFIG_SET from SY.admin (web/main:405-410). So the enablements cannot travel
  in io.web's config, as io_web_adapter_config.rs:12-14 anticipated: they travel in a new push that
  `send_node_message` refuses (§5.1).
- **The vault.** Only SY.admin and Archi write (sy_vault.rs:1029-1033); a read returns the current
  version only (sy_vault.rs:1219-1248); the owner reads its secret (sy_vault.rs:1935-1940). A
  rollback swaps the current and previous values (admin:9317-9322), so it cannot safely follow a put
  whose answer was lost; `publish_cloud_endpoint` rolls back only after a put that answered
  (admin:10353-10363, 8724-8731).
- **io.web serves by method and path alone**, never reading a cookie or a body (web/web:406-439,
  474-495). The edge's proxy passes every client cookie: `Cookie` is in neither
  `DROPPED_REQUEST_HEADERS` nor the hop-by-hop list (edge:4322-4351, 4789-4847); `/e/` drops it
  (edge:122-128, 5773-5793). Both log the path without its query (edge:4487-4515; web/web:417-431).
- **Cloud's traffic rides the `user` family** (edge:5446; admin:8706; cloud/main:115, 587). A
  router copies every `user` message a node sends to its stream, whole (src/router/mod.rs:886-889,
  7752-7759, 7794-7796; sdk/protocol.rs:608-610). SY.storage keeps the payload (sy_storage.rs:2052)
  and reads the ingress' stream too (sy_storage.rs:2913-2915).

---

## 2. The exposure

### 2.1 Identity and host

- `exposure_id`: `exp:<uuid v4>`, minted by SY.admin at publish and never reused.
- `label`: chosen by Cloud, one DNS label: lowercase ASCII letters, digits and hyphens, 1 to 63
  characters, starting and ending with a letter or a digit; taken as given (an uppercase label is
  refused, never folded).
- `host`: `<label>.<main>`, main being `acme::certified_name(admin.public_base_url)` (acme:242-313):
  the derivation `web:` uses for `archi.`, with its 253-character check (admin:953-964). Cloud names
  no domain, and the domain never comes from code (§11.1).
- **One label, one exposure.** The same label and upstream again answers the existing exposure
  (`created: false`), so Cloud may retry after `ADMIN_UNAVAILABLE`; another upstream answers
  `EXPOSURE_EXISTS`. At most 16 exposures (each a name on the shared certificate; Let's Encrypt
  allows 100), and at most one with upstream `archi`.

### 2.2 The upstream: recorded in 2.1, dialled in 2.2 (§12.5's open point)

| `upstream.kind` | Fields | Accepted when |
| --- | --- | --- |
| `archi` | none | Archi on the motherbee's loopback (D4). Its address is io.web's own constant in 2.2 and never comes from Cloud |
| `hive` | `hive`, `port` | `hive` is a valid hive id (sdk/mesh/registry.rs:22). The registry's strict reading gives it the role `worker` (registry.rs:143-). Never the motherbee, an ingress or an egress. In 2.2 its address is the registry's `address` (registry.rs:134-136; sy_orchestrator.rs:24243) |
| `address` | `address`, `port` | An IP literal, never a name (no DNS rebinding), in 10/8, 172.16/12, 192.168/16 or fc00::/7. Never loopback, link-local, unspecified, multicast or broadcast. Never the `address` of a hive of the registry: name the hive instead |

**Fluxbee's own ports are refused for every kind:**

| Port | What | Where it is set |
| --- | --- | --- |
| 22 | SSH | |
| 80, 443 | the edge | |
| 3000 | Archi | sy_architect.rs:92 |
| 8080 | SY.admin | admin:669-673 |
| 8384 | Syncthing API | sy_orchestrator.rs:183 |
| 9000 | WAN | src/config/mod.rs:45; sy_orchestrator.rs:11016-11019 |
| 9100 | identity sync | sy_identity.rs:46 |
| 9200 | io.web | sdk/mesh/mod.rs:22 |
| 21027 | Syncthing discovery | |
| 22000 | Syncthing | sy_orchestrator.rs:104 |

This covers §12.2's "never a Fluxbee service". Managed nodes listen on none of these.

### 2.3 The registry, and how each piece learns it

**The authority is SY.admin's `state_dir/sy-admin/exposures.json`.** It is written as
`publications.json` is: a temp file, mode 0640, dir 0750 (admin:7983-8008). SY.admin loads it at
boot and holds it in `AdminContext` (admin:328) behind an `RwLock`.

```json
{ "schema_version": 1, "generation": 1763121600123,
  "exposures": [ { "exposure_id": "exp:…", "label": "shop", "host": "shop.hive-k3m9x7q2.fluxbee.ai",
      "upstream": {"kind": "hive", "hive": "worker1", "port": 8080},
      "published_at": 1763121600, "published_by": "cloud",
      "enablements": [ {"enablement_id": "enb:…", "from": 1763121700, "until": 1763150500,
                         "slot": "a", "key_version": 4} ] } ] }
```

- **`enablements`** holds the live ones only, newest last: at most two, the current and the one it
  renewed (Q4). An ended one is pruned at the next write; views compute the state from `until`.
- **`generation = max(previous + 1, now in ms)`.** It never goes back, even when the file is lost:
  §7's lesson, "a release switch carries a generation that never goes back".
- **Opening persists before it opens; closing acts before it persists.** Publish and enable write
  the file first, and push nothing if the write fails. Disable and unpublish change memory and push
  first, then write; a failed write is retried every minute, and the answer says `persisted: false`.
- **A corrupt file** is logged as an ERROR and kept for the operator. SY.admin pushes empty sets,
  which fail closed, refuses every exposure write with `RECORD_FAILED` naming the file, and
  `web_status` says why.

| Piece | What it holds | How it learns it | After its restart |
| --- | --- | --- | --- |
| SY.edge | the hosts, as today's `EDGE_WEB_ROUTES` set | at publish and unpublish, then every 60 s (admin:1094-1101) | `web_routes.json` at once (edge:3590-3617) |
| io.web | the set without secrets: hosts, upstreams, enablements as `{id, until, slot, key_version}` | `IO_WEB_EXPOSURES` (§5.5) at each change, then every 60 s | its own `exposures.json`, beside its `config.json`, mode 0600; then it reads the keys |
| SY.vault | per exposure, two keys `web_exposure_key:<uuid>:a` and `:b`, owner IO.web | SY.admin's put at each enable | — |
| SY.admin | everything | its file | pushes at once to the edge and to io.web |

**Why two key slots and no rollback.** Each enable writes the slot the current enablement does not
use, so a failed or unanswered put never touches a live enablement's key, and no rollback is needed.
That matters because a rollback swaps values and cannot follow a lost answer (admin:9317-9322). The
slots keep the vault at two keys per exposure. Secrets are never deleted automatically: they go only
with a tenant's deletion, or by the operator's hand.

---

## 3. Access: the secret, the link, the session

### 3.1 The per-enablement secret

- At enable, SY.admin mints `K`, 32 bytes from `OsRng` as `mint_public_capability` does
  (admin:7942-7946), and stores it as `web_exposure_key:<exposure uuid>:<slot>`: `value {secret: <64
  hex>}`, `resource_type: web_session_key`, the root tenant, `owner_node: IO.web@<motherbee>`, whose
  ILK identity resolves, as for the edge's bearer (admin:10340-10364, 24879-24923). SY.admin then
  forgets `K`.
- io.web reads it as owner with its own ILK, as io.slack reads its key (io-slack main.rs:91, 169-171;
  sdk/vault.rs:778-780, 862), and checks `resource_type` and that the version is the enablement's
  `key_version`. It derives `K_link = HMAC-SHA256(K, "fluxbee io.web link v1")` and `K_sess =
  HMAC-SHA256(K, "fluxbee io.web session v1")` once per enablement: the domain separation means a
  link never verifies as a cookie.
- Nobody else reads `K`: the admin API refuses `vault_get` and `vault_assign` of the prefix (§5.6),
  and Cloud's `put_token` may not write it (sdk/vault.rs:613-629).

### 3.2 The one-time link (§12.3: about 120 s, single use, bound to the host, logged)

- **io.web mints it** when SY.admin asks with `IO_WEB_ACCESS_LINK`, for the current enablement only:

  ```text
  https://<host>/.fluxbee/enter?code=L1.<x>.<e>.<jti>.<iat>.<tag>
  x, e: the exposure's and enablement's uuids, 32 hex each
  jti: 16 random bytes, hex
  iat: unix seconds
  tag: hex(HMAC-SHA256(K_link, "L1|" + host + "|" + x + "|" + e + "|" + jti + "|" + iat))
  ```

  The host is inside the MAC and not in the code, so a code redeemed on another host fails.
- **The code rides the query**, so neither the edge's journal nor io.web's ever holds it
  (edge:4487-4515; web/web:417-431).
- **A GET of `/.fluxbee/enter` redeems it**, checking in order: (1) the host is an exposure's; (2) the
  code parses strictly; (3) `x` is that exposure; (4) `e` is a live enablement of it whose key io.web
  holds; (5) the tag verifies in constant time (`hmac::verify_slice`); (6) `iat ≥` io.web's boot time
  and `now ≤ iat + 120`; (7) `jti` is not in the redeemed set, then is added. Any failure answers the
  same fixed 401 page, the reason in the log alone. A HEAD never redeems.
- **Then io.web mints a session (§3.3)** and answers 200 with `Set-Cookie`, `Cache-Control:
  no-store`, `Referrer-Policy: no-referrer` and a page whose `<meta http-equiv="refresh"
  content="0; url=/">` moves on. Not a 303: a `SameSite=Strict` cookie is withheld from a redirect
  inside a navigation that began on another site (Cloud's), and is sent on a navigation the
  exposure's own page starts.
- **State.** The redeemed set holds `jti → iat + 120` in memory, pruned on insert; it is bounded by
  the redemptions of the last 120 s. The rule `iat ≥ boot` makes a restart safe: links issued
  before it are refused, and Cloud asks again.
- **Logs.** `link issued` and `link redeemed` or `refused` carry `link_id` (the first 16 hex of
  SHA-256(jti)), the exposure, the enablement, `expires_at` and the edge's `X-Request-Id` and
  `X-Forwarded-For`; never the code. SY.admin's command log keeps params, status and error code
  only (admin:1666-1712), so the URL never reaches it.

### 3.3 The session (§12.3, §11.3)

- **The cookie.**

  ```text
  __Host-fluxbee_session=S1.<x>.<e>.<sid>.<issued>.<active>.<tag>
  tag = hex(HMAC-SHA256(K_sess, "S1|" + host + "|" + x + "|" + e + "|" + sid + "|" + issued + "|" + active))
  ```

  It is set with `Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=min(1800, until(e) − now)`
  and no `Domain`, so it is host-only, as `__Host-` requires.
- **It is valid while** the tag verifies over the request's host, `e` is a live enablement whose key
  io.web holds, `now − active ≤ 1800` (30 min idle) and `now < until(e)` (the enablement's end at
  most). More than one session cookie is refused.
- **Sliding idle.** When `now − active ≥ 60`, the answer carries the cookie again with
  `active = now`. In 2.1 that is io.web's own answers; in 2.2 it is appended to proxied answers.
- **Without a valid session** every path but `/.fluxbee/*` answers the fixed 401 page, and clears an
  invalid cookie with `Max-Age=0`. With one, 2.1 serves io.web's placeholder page (web/web:443-455).
  The 401 comes from the headers alone, and no body is ever read (§11.3).
- **For 2b**, only the attributes change (`SameSite=None; Partitioned`) and the Origin check
  arrives. The token is the same.

### 3.4 Revocation

| Event | Effect | Bound |
| --- | --- | --- |
| `disable_exposure` | io.web drops the exposure's enablements and keys when the push arrives: every link and session fails at the next request | SY.admin waits for io.web's answer (5 s), then re-pushes every 60 s; the enablement's `until` bounds even a partitioned io.web |
| The enablement ends | io.web's own clock, against the `until` it holds | — |
| A renewal, Q4 (a) | a new enablement, `K` and slot. The previous enablement's sessions run to its `until`; new links come from the new one only | ≤ 8 h |
| `unpublish_exposure` | the same as a disable; the edge's route goes, and the door answers 404 | — |
| A stolen cookie | valid until idle or the enablement's end; Cloud's remedy is a disable and a new enable (new `K`). A deny-list keyed by `sid` is the 2.2 logout's hook | 30 min idle, 8 h |
| io.web restarts | sessions survive: both slots are re-read. Links issued before the restart are refused | — |
| io.web purged and launched again | new ILK, so the old keys are unreadable: `key: unavailable`, which fails closed, until Cloud enables again | — |

---

## 4. Who checks what

| At the door (SY.edge): generic, no secret in the DMZ (§3's placement) | In io.web (the manager, §1) |
| --- | --- |
| TLS with the shared certificate | Host → exposure; an unknown host answers 404 |
| Host routing: only published exposure hosts reach io.web; any other host is 404 (edge:3895-3930) | Whether the exposure is enabled, from its own copy and clock |
| GET and HEAD only, no body, no upgrade (edge:4721-4760), unchanged in 2.1 | The link: MAC, host binding, expiry, single use, issued after boot |
| Pace, in-flight caps per client and in all, fixed 502 and 503 (edge:4415-4547) | The cookie: MAC, idle, enablement end, attributes |
| The forwarding headers it sets itself, and the hop-by-hop strip (edge:4789-4847) | 401 from the headers; logs without codes, cookies or queries |
| **New:** only `__Host-` cookies cross to io.web; the main host still gets none | Origin checks for unsafe methods (2.2) |

The edge neither knows nor holds anything of an enablement, so its routes change at publish and
unpublish only. A closed exposure still reaches io.web, which answers 401.

---

## 5. The legs

### 5.1 SY.admin

**Actions.** Each is a `Command` route in `INTERNAL_ACTION_REGISTRY` (admin:5275), with the version
going from 26 to 27 (admin:5271), and an explicit arm in `dispatch_internal_admin_command` next to
`publish_cloud_endpoint`'s (admin:10846-10852), audited with `append_internal_dispatch_log`.

| Action | Params | Who | Answer `payload` |
| --- | --- | --- | --- |
| `publish_exposure` | `label`, `upstream` | IO.cloud alone | `{exposure, created}` |
| `enable_exposure` | `exposure_id`, `duration_s` (60…28 800) | IO.cloud alone | `{exposure, renewed}` |
| `disable_exposure` | `exposure_id` | IO.cloud alone (Q3) | `{exposure, was_enabled, persisted}` |
| `unpublish_exposure` | `exposure_id` | IO.cloud alone | `{exposure_id, host, removed, route, io_web, persisted}` |
| `issue_exposure_link` | `exposure_id` | IO.cloud alone | `{exposure_id, url, expires_at, link_id}` |
| `list_exposures` | none | IO.cloud and Archi; HTTP `GET /web/exposures` | `{exposures: [view], count, max: 16}` |

The `exposure` view is in §6.

**"IO.cloud alone" has three parts.** (1) A new access class, `MeshAccess::Cloud`
(admin:6025-6044, 6049-6192): `authorize_mesh_caller` admits Archi for every access but `Nobody` and
`Cloud` (admin:6321-6326), and IO.cloud keeps its row, `CLOUD_EXPOSED_ACTIONS`. (2) The handler
refuses every origin but `Mesh("IO.cloud@<motherbee>")` with 403 `UNAUTHORIZED`, the HTTP API and
the executor included: the arms take `origin`, not only `caller_l2_name` (admin:10703-10731). (3) No
HTTP route for the writes; `enforce_cloud_relay_content` (admin:11599-) gains one arm per action:
only its fields, and no `tenant_id`. `list_exposures` is `MeshAccess::Archi` and Cloud-exposed.

**Help.** Each action gets entries in `admin_action_summary`, `_request_notes`, `_path_patterns`,
`_example_scmd`, `_is_read_only` (true for the list only) and `_timeout`, which defaults to 30 s;
each action bounds its own calls at 5 s (admin:16474, 16545, 16607, 16900, 18674, 19027, 27226).

**`web_status`** (admin:1419-1437) gains `exposures`: the views, plus io.web's
`{pushed_generation, acknowledged_generation, last_answer, since}`. `web.hosts` gives way to the
exposures' hosts. `certificate` becomes `acme.web_certificate(<exposure hosts>)` (acme:4304).
Archi reads all of it through its existing `GET /web/status` (sy_architect.rs:13882-13887).

**Order of operations**, after the origin check, the field checks and the lock:

- **The lock.** `exposure_lock`, a `try_lock`. Taken, it answers 409 `BUSY` with
  `payload.retryable: true` and nothing read, as `cloud_door_busy` does (admin:8677-8679,
  8780-8791).
- **publish.** (1) Derive the host; no main name answers 409 `NO_MAIN_HOST`. (2) The label rules:
  same label and upstream, another upstream, the cap. (3) Write the file; a failed write answers
  500 `RECORD_FAILED`, nothing pushed. (4) Push `EDGE_WEB_ROUTES` and `IO_WEB_EXPOSURES` (5 s each).
  (5) Wake the ACME client (`names_changed`, §5.2). (6) Answer; an unanswered push comes back
  `pending`, and the 60 s loops push again.
- **enable.** (1) Check `duration_s`. (2) io.web's ILK must resolve from identity, which the vault
  put's owner needs (admin:24899-24920), else 409 `IO_WEB_ABSENT` ("run_node IO.web@motherbee in the
  root tenant first"). (3) Mint `K`, put it in the free slot (5 s); refused or unanswered answers 502
  `VAULT_UNAVAILABLE`, nothing changed: at most the idle slot changed, which holds no live key or
  only the overlapped one's. (4) Write the file with the new enablement, the current one kept as
  previous, an older one dropped; a failed write answers 500 `RECORD_FAILED`, the new key unused.
  (5) Push `IO_WEB_EXPOSURES` (5 s); io.web answers once it holds `K`. (6) Answer `enabled_until`
  with `io_web: acknowledged` or `pending`.
- **disable.** Clear the enablements in memory, push `IO_WEB_EXPOSURES` (5 s), write the file,
  answer. An exposure already closed answers ok with `was_enabled: false`.
- **unpublish.** Remove the exposure in memory, push `EDGE_WEB_ROUTES` and `IO_WEB_EXPOSURES`,
  write the file, answer. The name stays on the certificate until its renewal: nothing is ordered,
  as for a `web:` removal (§11.1 item 2).
- **issue_exposure_link.** The exposure must be enabled (`now < until`), else 409
  `EXPOSURE_CLOSED`. Ask `IO_WEB_ACCESS_LINK {exposure_id, enablement_id, generation}` (5 s); on
  `ENABLEMENT_UNKNOWN` (io.web's set is older) push the set and ask once more. No answer or
  `KEY_UNAVAILABLE` answers 503 `IO_WEB_UNAVAILABLE`, retryable.
- **The loops.** `WebRoutes::payload` reads the registry, not `WebConfig` (admin:1104-1112).
  `push_once` takes the 5 s bound from the handlers and keeps its 20 s in the loop
  (admin:908, 1116-1184). A second 60 s loop pushes `IO_WEB_EXPOSURES` and records io.web's
  answer, logging changes only, as `log_change` does (admin:1215-1271).
- **`send_node_message`** refuses `IO_WEB_EXPOSURES` and `IO_WEB_ACCESS_LINK`, as it refuses
  `COGNITION_CLEAR` (admin:22626-22634). The router's gate cannot help here: it sees SY.admin as the
  sender either way.
- **Q2 (a).** `WebConfig`, `WEB_HOST_LABELS` and `HiveFile.web` go (admin:193, 903-993). A `web:`
  key left in hive.yaml logs one warning at boot. `HiveFile` does not deny unknown fields
  (admin:176-194), so the key is otherwise ignored.

**Tests**, each of which fails on `74ededeb`:

- `exposure_writes_are_iocloud_s_alone`: IO.cloud@motherbee admitted; SY.architect@motherbee,
  IO.cloud@worker1, IO.api.x@motherbee refused; the `Http` and `Executor` origins answered 403. And
  `archi_on_the_motherbee_may_ask_for_every_listed_action`, updated: `Cloud` joins `Nobody`
  (admin:28477-28493).
- `publish_exposure_derives_the_host_under_the_main_name`;
  `publish_exposure_refuses_a_bad_label_and_never_folds_it`;
  `publish_exposure_refuses_fluxbee_upstreams` (§2.2's table: the motherbee, an ingress, an
  unregistered hive, a name for an IP, 127.0.0.1, 169.254.x, a hive's address, each refused port);
  `publish_exposure_is_idempotent_for_the_same_upstream_and_refuses_another`;
  `publish_exposure_stops_at_16`.
- `enable_puts_a_new_key_owned_by_io_web_in_the_free_slot_then_records_then_pushes` (the mock mesh
  sees `owner_node`, `resource_type`, the slot and the root tenant, then the push);
  `enable_answers_vault_unavailable_and_records_nothing_when_the_put_is_unanswered`;
  `enable_refuses_more_than_8_hours`.
- `disable_pushes_a_set_without_the_enablement_before_writing`;
  `disable_answers_persisted_false_when_the_write_fails`.
- `issue_link_is_refused_while_closed`; `issue_link_answers_io_web_s_url`;
  `issue_link_pushes_once_on_enablement_unknown`.
- `the_web_routes_carry_every_published_host`;
  `the_generation_never_goes_back_even_after_the_file_is_lost`;
  `send_node_message_never_carries_io_web_commands`;
  `a_busy_exposure_lock_answers_busy_and_reads_nothing`.

### 5.2 SY.admin's ACME client (acme.rs)

- **The names come from a live source.** `AcmeService` gets a dependency `names: Fn() -> Vec<String>`
  (the published hosts, sorted), and `settings()` (acme:2233-2238) returns `[main] ++ live`, so every
  reader sees the live set: `decide` (acme:1411-1505), `name_not_covered` (acme:1646-1663),
  `Run::every_name` (acme:2080), the renew answer (acme:4071-4076). `acme.json` keeps parsing: new
  fields take `serde(default)`, `STATE_SCHEMA_VERSION` stays 1 (acme:68), as §11.1 requires.
- **A name-change order.** A name the leaf this account issued does not cover, and whose DNS answers
  the main name's addresses, is due now: outside the cap of one issuance per third of the lifetime
  (`cap_until`, acme:1578-1583); inside `install_refused`, the CA's rate limit and the backoff
  (acme:1552-1577); within a budget of its own, one order per 15 min (the names published meanwhile
  ride it) and five per rolling 7 days, recorded in `acme.json`. Past the budget the host is `held`
  with `retry_at`; the operator's `POST /acme/renew` still overrides (acme:4061-4076).
- **DNS first.** A host whose name does not resolve to the main host's addresses is left out of the
  order, so it never fails the order for every name, and it is `waiting_dns`. SY.admin resolves it
  again every 5 min. The order's names are the main one, every name the leaf covers, and the new
  names that resolve.
- **`names_changed()`** wakes the loop, which otherwise sleeps until `next` (acme:2319-2326). The
  check runs within 60 s.
- **A dropped name orders nothing.** The leaf keeps it until its renewal.
- **`host_certificate(host)`** returns `{state, detail, retry_at}`, from the edge's last status
  (`last_status.leaf.names`), the last attempt's `failed_names` (acme:842-856) and the DNS cache.
  The states are `covered`, `pending`, `waiting_dns`, `failed`, `held` and `off` (ACME absent or
  off: the operator certifies by hand).
- **Tests:** `a_published_host_missing_from_the_leaf_is_ordered_outside_the_cap`;
  `a_host_without_dns_is_left_out_and_waits`;
  `name_change_orders_coalesce_and_stop_at_the_weekly_budget`; `a_dropped_name_orders_nothing`;
  `acme_json_of_0_1_144_keeps_parsing`.

### 5.3 io.cloud and Cloud's catalog

- **The SDK, `cloud.rs`.** Six rows in `CLOUD_OP_ACTIONS` under the same names (sdk/cloud.rs:19-59):
  `publish_exposure`, `enable_exposure`, `disable_exposure`, `unpublish_exposure`,
  `issue_exposure_link`, `list_exposures`; the same six in `CLOUD_EXPOSED_ACTIONS`
  (sdk/cloud.rs:65-86); one catalog summary each, stating no envelope tenant, the states and the
  codes (sdk/cloud.rs:132-185); the pinned lists of its tests updated (sdk/cloud.rs:191-226).
  SY.admin's `list_cloud_actions` follows by itself (admin:15989-16011).
- **io.cloud.** `translate_cloud_op` (cloud/main:1168-1599) gets six pure arms. Each copies only its
  own fields, `duration_s` a whole number and `upstream` an object SY.admin checks. An envelope
  `tenant_id` is refused ("an exposure is the installation's, not a tenant's"), not ignored as for
  `list_tenants` (cloud/main:1514-1523), in line with io.cloud's "refused rather than ignored"
  (cloud/main:508-513). `cloud_view` passes the answers through (cloud/main:1811). The 20 s timeout
  fits every op, the slowest about 15 s (cloud/main:1690-1693).
- **Tests:** `translate_exposure_ops_copy_only_their_fields_and_refuse_a_tenant`; the unchanged
  `translate_cloud_op_actions_match_the_shared_sdk_vocabulary` covers the rows once they land
  (cloud/main:2661-2698); in SY.admin, `iocloud_on_the_motherbee_relays_only_the_cloud_surface` and
  the exposed-in-registry guard (admin:28545-28573, 30283-30291).

### 5.4 SY.edge

1. **Cookies.** In `io_web_request_headers` (edge:4789-4847), `Cookie` is rebuilt with only the
   pairs whose name begins with `__Host-`, and dropped when none is left. `/e/` already drops cookies
   (edge:5773-5793).
2. **A cap on routes.** `validate_web_routes` (edge:3694-3732) refuses a set of more than
   `MAX_WEB_ROUTES = 64` hosts. The served set stays.
3. **Nothing else.** The method policy, the headers and the certificate are unchanged, and no
   session logic comes to the DMZ.

**Tests:** `the_proxy_passes_only_host_prefixed_cookies` (`a=1; __Host-fluxbee_session=x; b=2`
arrives as `__Host-fluxbee_session=x`, `a=1` alone as no header);
`a_set_over_the_cap_is_refused_and_the_served_set_stays`.

### 5.5 io.web

- **Two commands** on a channel of their own, `exposures`, served by their own task, as the edge's
  `tls` channel is (edge:3186-3211). A vault read then never holds NODE_STATUS_GET or CONFIG
  (web/main:188-199, 246-267). Both are admitted only from the router-stamped
  `SY.admin@motherbee` (`admin_target`, web/main:126-127), as system messages, never `via_tap`.
  Anything else answers `UNAUTHORIZED` and is logged.
- **`IO_WEB_EXPOSURES`** carries the whole set, `{generation, exposures: [...]}` as in §2.3. A lower
  generation than the one held answers `STALE_GENERATION`; an equal one is acknowledged with no
  read. A higher one: io.web reads the key (3 s) of each live enablement it does not hold at the same
  slot and version, and checks it (§3.1); one that cannot be read or does not match is `key:
  unavailable`, retried every 30 s, and fails closed. The new set replaces the old in memory, so
  what is gone is dropped at once (the revocation), then the file is written. The answer is
  `{status, generation, changed, enablements: [{enablement_id, key: ready | unavailable, detail}]}`.
  At boot io.web loads its file, then the keys, before it opens the port (web/web:199-215).
- **`IO_WEB_ACCESS_LINK`** checks the exposure and that the enablement is the current one with its
  key held, then answers `{url, expires_at, link_id}` (§3.2). Otherwise it answers
  `EXPOSURE_UNKNOWN`, `ENABLEMENT_UNKNOWN` or `KEY_UNAVAILABLE`.
- **Serving.** `respond(method, path)` (web/web:474-495) becomes `respond(request, now)`, reading the
  host, the session cookie and the path. `/.fluxbee/` is reserved on every host and never reaches an
  upstream in 2.2: `/.fluxbee/enter` redeems a link, and `/.fluxbee/healthz` replaces `/healthz`
  (web/web:481), which in 2.2 would shadow the upstream's own. Every other path checks the session
  (§3.3): the placeholder with one, the 401 page without. An unknown host answers 404.
  `SECURITY_HEADERS` and the CSP are unchanged (web/web:460-470).
- **Status.** `extensions.web.exposures` holds `{generation, exposures, enabled, keys_unavailable,
  links_issued, links_redeemed, links_refused, sessions_minted}` (web/main:367-393).
- **Config.** The contract still has no field. The note "the sessions of stage 2 bring the first
  one, by vault_ref" (io_web_adapter_config.rs:49-53) is replaced by "the keys come by SY.admin's
  push, never by config (§12.2: Cloud alone)".
- **Dependencies.** `hmac` and `sha2` come from the workspace (nodes/io/Cargo.toml:34, 47).
  Randomness comes from `uuid` v4 (nodes/io/Cargo.toml:57).
- **Tests:**
  - link: `a_link_redeems_once_and_sets_the_host_cookie` (every attribute);
    `a_link_is_refused_on_another_host`, `_after_120_s`, `_tampered`, `_from_before_the_boot`,
    `_of_an_ended_enablement`; `head_never_redeems`;
  - session: `a_session_serves_the_page_and_slides_after_60_s`; `a_session_ends_after_30_min_idle`,
    `_at_the_enablement_s_end`; `a_session_of_one_exposure_fails_on_another`;
    `a_disable_push_ends_every_session_at_once`;
  - push: `pushes_are_taken_only_from_sy_admin_at_the_motherbee_and_never_via_tap`;
    `a_stale_generation_is_refused`; `a_key_whose_version_differs_is_unavailable`;
    `the_set_reloads_from_its_file_at_boot`; and `healthz_moved_under_the_reserved_prefix`.

### 5.6 Vault and identity

- **SY.vault does not change.**
- **The SDK.** `web_exposure_key:` joins `CLOUD_RESERVED_VAULT_KEY_PREFIXES` (sdk/vault.rs:613-619).
  That refuses it in io.cloud's `put_token` (cloud/main:1198) and in SY.admin's re-check
  (admin:11624-11631).
- **SY.admin.** `EDGE_ONLY_VAULT_KEY_PREFIXES` (admin:24287-24288) is renamed
  `NODE_ONLY_VAULT_KEY_PREFIXES` and gains the prefix, so `vault_get` and `vault_assign` answer 403
  `RESERVED_KEY` (admin:24298-). Otherwise an assign to SY.admin, followed by a get, would read it
  back (A-316).
- **The resource type** is `web_session_key`.
- **Identity.** It needs io.web's ILK, registered when io.web is launched. Nothing is new.
- **Tests:** `vault_get_and_assign_refuse_web_exposure_keys`, and in the SDK
  `web_exposure_keys_are_reserved`.

### 5.7 The orchestrator

**No change.** Placement stays the motherbee and the root tenant, `boot: false`, port 9200
(sy_orchestrator.rs:23594; packaging/base-nodes.json:75-82). A purge of io.web removes its
`exposures.json`, which SY.admin pushes back within 60 s; its new ILK makes the old keys unreadable
(§3.4). io.web's removal is never refused for live exposures.

### 5.8 Router and policy

- `IO_WEB_EXPOSURES` and `IO_WEB_ACCESS_LINK` join `PROTECTED_SYSTEM_ACTIONS`
  (src/router/system_policy.rs:37-80). Their pinned count goes from 33 to 35
  (system_policy.rs:866-913).
- `policy/system.rego` gains `web_service_actions` beside `edge_service_actions`
  (policy/system.rego:44-51), with the same rule: SY.admin on the primary hive.
- Both wasm files are rebuilt with the compiler, and CI compares them byte for byte
  (policy/system.rego:7-16).
- This adds the delivery gate (src/router/mod.rs:8073-8108) to io.web's own check, as the edge's
  commands and CONFIG_SET have both. The messages are delivered without it, so this release may come
  in any order.
- **Test:** the truth table admits SY.admin@motherbee and refuses SY.admin@worker1, SY.architect,
  IO.cloud and a `via_tap` copy.

### 5.9 Archi and its handbook

- **No Archi binary change with Q3 (a).** Archi reads the exposures through `web_status`.
- **The handbook** is a file in the `.deb` (packaging/build-deb.sh:247; sy_architect.rs:369-371). Its
  new section on exposures: Cloud alone publishes, enables, disables, unpublishes and issues links,
  and Archi gets `UNAUTHORIZED` for each, says so and never retries; how to read the states (closed
  or enabled, `enabled_until`, the certificate state) and what each certificate state asks of the
  operator (DNS, `POST /acme/renew` while `held`, ACME `off`); `IO_WEB_ABSENT` asks for `run_node
  IO.web@motherbee`; the brake is `kill_node IO.web@motherbee`.

### 5.10 ops.py and observability

- **`lab/ops.py`'s `web` line** (lab/ops.py:303-327) becomes `web exposures=N enabled=M
  cert=covered:a,pending:b route=… io.web=…(ack gen)`. It never fails the health check.
- **The journals**, every line with `exposure_id` and `host`, never a code, a cookie or `K`:
  SY.admin logs `exposure published`, `enabled` (until), `disabled`, `unpublished`, `link issued`
  (link_id) and changes in io.web's answers; io.web logs the set applied (generation), keys ready or
  unavailable, links issued, redeemed and refused with the reason, and sessions minted (sid's first
  8 hex); the edge logs proxied requests as today, path only. `ops timeline` reads them for the
  gate's timings.

### 5.11 Docs and findings

- **Docs:** docs/io-cloud-api.md §5.8 "Exposures" (the shapes of §6, the retries in its §6, the
  0.1.14x entry in its §9); docs/io/io-web.md (what it serves, sessions, exposures, status, logs);
  docs/nodes/sy-admin.md (the actions, `web_status`, `web:` retired); docs/nodes/sy-edge.md
  (cookies, the cap); docs/acme-edge-certificates-v1.md (names at runtime, name-change orders, DNS
  first); docs/control-path-hardening-v1.md (`MeshAccess::Cloud` in the A-62 table);
  docs/io-web-v2.md (§12's status; §12.1's "certificates per host" corrected to "one certificate, a
  name per host").
- **FINDINGS, pending, verified in code:** Cloud's requests and io.cloud's answers are `user`
  messages, which routers copy whole to their stream, and SY.storage keeps the payload in `turns`
  (§1): `put_token`'s credential values today, the link from 2.1 on. The fix is the router's, no
  turn for the Cloud channel, in its own release (Q6).

---

## 6. Answers for Cloud

**The envelope** is io.cloud's (docs/io-cloud-api.md §4): `{status, op, request_id, handled_by, ich,
result | error_code, error_detail}`. `result` is SY.admin's `payload`.

**The exposure view**, in every op's answer:

```json
{ "exposure_id": "exp:6f1c…", "label": "shop", "host": "shop.hive-k3m9x7q2.fluxbee.ai",
  "url": "https://shop.hive-k3m9x7q2.fluxbee.ai/",
  "upstream": {"kind": "hive", "hive": "worker1", "port": 8080},
  "state": "enabled", "enabled_until": "2026-11-14T18:00:00Z", "published_at": "2026-11-14T10:00:00Z",
  "certificate": {"state": "covered", "detail": null, "retry_at": null},
  "route": {"state": "acknowledged"}, "io_web": {"state": "acknowledged"} }
```

| Field | Values |
| --- | --- |
| `state` | `closed`, `enabled` (then `enabled_until` is set) |
| `certificate.state` | `covered`, `pending`, `waiting_dns`, `failed`, `held`, `off` |
| `route.state` | `acknowledged`, `pending`, `no_edge` |
| `io_web.state` | `acknowledged`, `pending`, `absent` |

**Each op's `result`:**

| Op | `result` |
| --- | --- |
| `publish_exposure` | `{exposure, created}` |
| `enable_exposure` | `{exposure, renewed}` |
| `disable_exposure` | `{exposure, was_enabled, persisted}` |
| `unpublish_exposure` | `{exposure_id, host, removed: true, route, io_web, persisted}` |
| `issue_exposure_link` | `{exposure_id, url, expires_at, link_id}`. `url` is opened top-level, in Cloud's own tab, within 120 s, once. Cloud never logs it |
| `list_exposures` | `{exposures: [view], count, max: 16}` |

**Errors:**

| `error_code` | Op | Meaning | Retry |
| --- | --- | --- | --- |
| `INVALID_REQUEST` | any | a field, the label, the upstream, `duration_s`, an envelope tenant | no |
| `UNAUTHORIZED` | any | the hive's re-check; a bug, never a client error | no |
| `EXPOSURE_NOT_FOUND` | all but publish and list | an unknown `exposure_id`; after an unknown unpublish it means done | no |
| `EXPOSURE_EXISTS` | publish | the label is published with another upstream, or a second `archi` | no |
| `EXPOSURE_LIMIT` | publish | 16 published | no |
| `NO_MAIN_HOST` | publish | the hive's `admin.public_base_url` gives no host | the operator |
| `EXPOSURE_CLOSED` | link | not enabled | enable first |
| `IO_WEB_ABSENT` | enable | IO.web@motherbee was never launched | the operator |
| `BUSY` | writes | another exposure write is running (`retryable`) | yes |
| `VAULT_UNAVAILABLE` | enable | nothing changed | yes |
| `RECORD_FAILED` | writes | nothing opened | yes |
| `IO_WEB_UNAVAILABLE` | link | io.web did not answer, or holds no key yet (`retryable`) | yes |

**Retries after `ADMIN_UNAVAILABLE`** are safe: publish answers the same exposure again; enable makes
one more enablement, which is harmless; disable answers `was_enabled: false`; unpublish answers
`EXPOSURE_NOT_FOUND`; a new link leaves the unused one to expire.

**The flow:** publish → `list_exposures` until `certificate.state` is `covered` → enable with the
duration → link → open it → renew before `enabled_until` → disable.

---

## 7. The gate on 8.x (after R4)

Cloud's path is the real door: the edge's `/e/<ich>`, with the Cloud bearer read by the gate tool
from its 0600 file by path, never printed. Each step's time goes live in the script, and afterwards
`ops timeline` reads them.

**The baseline, on 0.1.144 before R3:** `GET https://archi.<id>/` (200, the placeholder, its
time); `/acme/status` (the main host and archi); the `web` line of `ops.py health`; the round trip
of `list_cloud_actions` through the door; io.web's restart to serving, from its journal.

**The steps:**

0. **Smoke.** Launch a throwaway node and read a stopped one (the run_node smoke).
   `GET https://archi.<id>/` answers 404 at the door under Q2 (a). `web_status` shows
   `exposures: []`, io.web acknowledged.
1. **Catalog.** `list_cloud_actions` names the six ops; `list_exposures` is empty.
2. **Archi's exposure.** `publish_exposure {label: archi, upstream: {kind: archi}}` answers
   `created`, `certificate: covered` (the name is already on the leaf), `route: acknowledged`.
   `https://archi.<id>/` now answers io.web's 401. The same publish again answers `created: false`.
3. **A new host.** `publish_exposure {label: gate21, upstream: {kind: hive, hive: worker1, port:
   8080}}`, which needs Q1's DNS. Time it to `covered`. `/acme/status` lists the host, and the
   served leaf carries it.
4. **Refusals.** Port 9000, `hive: ingress1`, `address: 127.0.0.1`, label `Gate21` and an envelope
   tenant are each `INVALID_REQUEST`. A link while closed is `EXPOSURE_CLOSED`.
5. **Enable for 600 s.** The answer is `enabled_until` with io.web acknowledged. `vault_get_metadata`
   of the slot names IO.web as owner, and the version.
6. **The link, with a cookie jar.** Redeeming answers 200 with `Set-Cookie: __Host-fluxbee_session=…;
   Path=/; Secure; HttpOnly; SameSite=Strict`, and `/` with the cookie serves the placeholder. A
   second redemption answers 401, as do the same code on `archi.<id>` and a link after 125 s. The
   edge's and io.web's journals hold `link_id`, never the code. For Q6, look for the link in
   `turns` and record what is found.
7. **The enablement's end.** Enable for 60 s, take a session, wait: the session answers 401 at
   `until`.
8. **Renewal.** A second enable moves the slot. The old session lives until the old `until`, and the
   new link works.
9. **Disable.** io.web acknowledges, and the live session answers 401 at once.
10. **Restarts.** io.web while enabled: the session survives, and a link issued before the restart
    is refused. SY.admin: `list_exposures` is unchanged, its first pushes out within 1 s. The edge:
    both hosts are routed at once.
11. **Who may not.** An executor plan step `publish_exposure` answers 403 `UNAUTHORIZED`, and the
    HTTP API has no route for it. `send_node_message` with `IO_WEB_EXPOSURES` answers 400. A
    throwaway mesh client on worker1 sending `IO_WEB_EXPOSURES` to IO.web is dropped by the router
    (R2) and logged.
12. **Cleanup.** Unpublish `gate21`: its host answers 404 at the door. Archi's exposure is kept,
    closed, as product state.

**Left to the tests:** the 30 min idle and the 60 s slide (with a clock); MAC tampering, malformed
codes and cookies; the generation order and the key version check; the full label and upstream
table; the ACME budget and DNS decisions; the edge's cookie filter; the A-62 rows and the
translations.

---

## 8. Releases and branches (METHOD §9: one branch per binary; shared code alone)

| Release | Branches | What | Why in this order |
| --- | --- | --- | --- |
| R1, Rust SDK alone | `coa/web21-sdk-protocol` | `MSG_IO_WEB_EXPOSURES` and `MSG_IO_WEB_ACCESS_LINK` with their responses (sdk/protocol.rs); the reserved prefix (§5.6) | SY.admin and io.web compile against the constants |
| R2, router alone | `coa/web21-router` | the protected actions, rego, both wasm (§5.8); A-349's fix, no turn for Cloud's channel (§0, decision 6) | independent: before or after R3, always before the gate |
| R3, binaries | `coa/web21-admin` (SY.admin and acme.rs), `coa/web21-edge`, `coa/web21-io-web`, `coa/web21-io-cloud` | §5.1, §5.2, §5.4, §5.5, §5.3's translation; each with its docs | each is green alone. io.cloud's new arms pass while `CLOUD_OP_ACTIONS` lacks the rows (cloud/main:2678-2692 loops over the rows) |
| R4, Rust SDK alone | `coa/web21-sdk-cloud` | `cloud.rs`'s six rows and catalog (§5.3) | it needs SY.admin's registry (admin:30285-30291) and io.cloud's arms (cloud/main:2684-2685) green first. Only then can Cloud reach the ops |

- **Totals:** seven branches, four releases.
- **Gates.** R1's: a `put_token` of `web_exposure_key:x` is refused. R3's: §7's step 0, plus the
  refusal of step 11. §7 runs whole after R4.
- **Files.** ops.py, FINDINGS, the logbook and the ledger are written on `daily_onworking_coa` at
  integration. The handbook rides in R3's SY.admin branch.
