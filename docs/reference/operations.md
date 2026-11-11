# Operations: deploy and lifecycle

How a Fluxbee mesh is installed, grown, upgraded and checked. Every statement points to the code
that does it; the operator's own routine on the lab is `lab/ops.py`, used here as an example.

The ground rules, from the code:

- Only the **motherbee** is installed from the `.deb`. Worker, ingress and egress hives
  ("spokes") are bootstrapped from the motherbee by `add_hive` and updated by it afterwards
  (`add_hive_flow`, `sync_core_to_worker` in `src/bin/sy_orchestrator.rs`).
- Every operator action goes through **SY.admin**, the HTTP API on the motherbee at
  `127.0.0.1:8080` (`admin.listen`; `JSR_ADMIN_LISTEN` overrides it). SY.admin passes node and hive
  actions to the motherbee's `SY.orchestrator`, which runs them or forwards them to the target
  hive's orchestrator (`build_admin_request`, `action_routes_via_local_orchestrator` in
  `src/bin/sy_admin.rs`; `forward_system_action_to_hive` in `src/bin/sy_orchestrator.rs`).
- Software reaches the spokes through Syncthing folders the motherbee serves; there is no SSH
  delivery after a hive has joined.

## Roles and their services

`/etc/fluxbee/hive.yaml` names the hive (`hive_id`) and its `role`, and holds its own paths and
ports. What each role runs is the release's, in the code (`src/release_settings.rs`,
[hive-config-update-v1](../hive-config-update-v1.md) §3): the system nodes to start in order, the
ones the boot waits for, and the blob's and dist's topology. They reach every hive with the core
update, and a rollback takes them back with the binaries. A hive.yaml an earlier release wrote
still loads; the keys it holds for these (`system_nodes`, `blob.enabled`,
`blob.sync.enabled|public_enabled|tool`, `dist.sync.enabled|tool`) are ignored, and the
orchestrator names them in one warning at its start. Keep them while the hive may go back to
0.1.116 or earlier, which reads them; a hive joined by 0.1.117 or later has none, and goes back
below 0.1.117 only after they are put back by hand. The table's tests hold each list to start with
`SY.config.routes`, only the motherbee to run `SY.vault`, and every vault consumer to come before
it (see [SY.vault](../nodes/sy-vault.md)):

| Role | System nodes (start order) |
| --- | --- |
| motherbee | SY.config.routes, SY.identity, SY.opa.rules, SY.admin, SY.architect, SY.storage, SY.cognition, SY.timer, SY.wf-rules, SY.frontdesk.gov, SY.vault |
| worker | SY.config.routes, SY.identity, SY.opa.rules, SY.cognition, SY.timer, SY.wf-rules |
| egress | SY.config.routes, SY.opa.rules |
| ingress | SY.config.routes, SY.opa.rules, SY.edge |

Every role also runs `rt-gateway` (started first) and `sy-orchestrator`. The node pages:
[rt-gateway](../nodes/rt-gateway.md), [SY.config.routes](../nodes/sy-config-routes.md),
[SY.identity](../nodes/sy-identity.md), [SY.opa.rules](../nodes/sy-opa-rules.md),
[SY.vault](../nodes/sy-vault.md), [SY.storage](../nodes/sy-storage.md),
[SY.cognition](../nodes/sy-cognition.md), [SY.timer](../nodes/sy-timer.md),
[SY.wf-rules](../nodes/sy-wf-rules.md).

## Install the motherbee

1. **Build** the package: `packaging/build-deb.sh <version>` writes
   `dist/fluxbee_<version>_amd64.deb`. It builds every Rust and Go binary, stages them in
   `/usr/bin` and `/var/lib/fluxbee/dist/core/bin` with a `dist/core/manifest.json` that holds
   their sha256, seeds the base runtimes of `packaging/base-nodes.json` under
   `dist/runtimes/<runtime>/<version>/`, and refuses to build without a vendored Syncthing unless
   `FLUXBEE_ALLOW_NO_SYNCTHING=1`.
2. **Publish** it to an apt repository: `scripts/apt-repo-publish.sh --deb <file> [--serve]`
   keeps a flat, unsigned repository (clients use `deb [trusted=yes] http://<host>:8900 ./`); every
   version stays listed.
3. **Install**: `apt-get install fluxbee` (it depends on `postgresql`, `openssl`, `curl`,
   `python3`). The maintainer scripts (`packaging/deb-preinst`, `deb-postinst`):
   - create the `fluxbee` system user and the directories under `/var/lib/fluxbee`;
   - register the base runtimes into the live runtimes manifest (`fluxbee-seed-runtimes`, below);
   - copy `/etc/fluxbee/hive.yaml.example` to `hive.yaml` if there is none;
   - enable `sy-orchestrator`. A fresh install starts nothing; it prints
     `Edit /etc/fluxbee/hive.yaml, then run: sudo fluxbee-firstboot`.
4. **Edit** `/etc/fluxbee/hive.yaml` (`hive_id: motherbee`, `role: motherbee`, the `ai` provider,
   `wan.listen`). The orchestrator's first start writes its `ai` as version 1 of the installation
   file, which every hive's readers read from then on and which reaches every hive
   ([hive-config-update-v1](../hive-config-update-v1.md) §4); after that, this section is
   ignored, and the installation's `ai` changes with `set_installation_ai`
   ([The installation's AI settings](#the-installations-ai-settings)).
5. **First boot**: `sudo fluxbee-firstboot` (`packaging/fluxbee-firstboot`):
   1. starts PostgreSQL, creates the role (`FLUXBEE_DB_USER`/`FLUXBEE_DB_PASSWORD`, default
      `fluxbee`/`fluxbee`) and the databases `fluxbee`, `fluxbee_identity`, `fluxbee_storage`;
   2. starts `sy-orchestrator`, which starts the router and the system nodes and keeps
      restarting until the database secret exists;
   3. waits for SY.admin and stores the secret `storage_postgres_url` (resource type `postgres`,
      root tenant `tnt:00000000-0000-0000-0000-000000000001`) with
      `POST /hives/<hive>/vault/secrets`;
   4. restarts `sy-vault`, then `sy-storage` and `sy-identity` so they read it, then the
      orchestrator, until `GET /hives` answers `status: ok` (up to six attempts);
   5. launches one instance of each `boot: true` runtime of `base-nodes.json` in the root
      tenant: `IO.blob@motherbee`, `CONFIGURED` on its defaults, and `IO.cloud@motherbee`,
      `UNCONFIGURED` and `DEGRADED` (`missing_edge_node`) until it trusts the ingress' edge,
      which `publish_cloud_endpoint` records in its config and applies to it at once (FINDINGS
      A-91, A-255);
   6. prints what is still missing: the AI provider key for the core (`openai` or `anthropic`
      in the root tenant), the per-tenant secrets of IO nodes, and IO.cloud's edge (the publish).

On a development host, `scripts/install.sh [<version>]` does steps 1 and 3 from the local checkout:
it builds the package with `packaging/build-deb.sh` and installs it with
`apt-get install --reinstall`. Steps 4 and 5 stay the operator's.

## Ports and posture

| Port | Who | Bound where | Source |
| --- | --- | --- | --- |
| 8080/tcp | SY.admin HTTP | `127.0.0.1` (`admin.listen`) | `src/bin/sy_admin.rs` |
| 3000/tcp | SY.architect UI/API | `127.0.0.1` (`architect.listen`) | `packaging/hive.yaml.example` |
| 9000/tcp | WAN between hives (gateway router) | `wan.listen`; set on the motherbee only | [rt-gateway](../nodes/rt-gateway.md) |
| 9100/tcp | identity sync | `0.0.0.0`, motherbee only | [SY.identity](../nodes/sy-identity.md) |
| 9200/tcp | IO.web, for the ingress' edge only (mutual TLS, a connected ingress) | `0.0.0.0`, motherbee only, once the operator starts it | [IO.web](../io/io-web.md) |
| 22000/tcp | Syncthing | the motherbee; spokes dial it and listen on `127.0.0.1` | `SYNCTHING_SYNC_PORT_TCP` in `src/bin/sy_orchestrator.rs` |
| 8384/tcp | Syncthing REST | `127.0.0.1` | Syncthing unit written by the orchestrator |
| 5432/tcp | PostgreSQL | the distribution's default; `fluxbee-firstboot` uses `127.0.0.1` | `packaging/fluxbee-firstboot` |
| `edge.listen` (e.g. 443) | SY.edge, the public door | ingress hives only | `packaging/hive.yaml.example` |

The orchestrator writes no rule to `ufw` or `firewalld`. An active one is a `posture` drift alert,
`host_firewall_active` (at boot and about every minute), since it may close 9000, 9100, 9200, 22000
or 443 without anyone seeing it. A spoke records it in its own drift alerts, which the
motherbee does not read yet (FINDINGS A-175). SSH on the DMZ hosts is the product's own
([control-path-hardening-v1.md](../control-path-hardening-v1.md) §6):

- **The ingress** accepts SSH (the ports `sshd -T` reports, IPv4 and IPv6) only from loopback, the
  motherbee's address in its `wan.uplinks` and `ingress.ssh_allow_from`, and drops all other SSH;
  nothing else is filtered (`table inet fluxbee_ingress`, reconciled at boot and about every
  minute, loaded at boot by `fluxbee-ingress-nft.service`). Only a complete input set changes it.
  An input that fails (hive.yaml, `sshd -T`, an uplink that does not resolve), a bad entry or an
  empty allow set keeps the rules in force and raises `ingress_ssh_input_failed` (a warning),
  `ingress_ssh_rules_failed` or `ingress_ssh_allow_set_empty`; a failed load raises
  `ingress_ssh_rules_failed`. Recovery: the hypervisor console, or `ingress.ssh_allow_from` in the
  ingress' hive.yaml and a restart of `sy-orchestrator`.
- **The egress** keeps SSH on its WAN interface by default (`egress.ssh_from_wan: true`, D31); with
  `false` its `fluxbee_egress` table drops SSH arriving on `wan_iface`, on the ports `sshd -T`
  reports, else the ones it already drops.

Admin and Archi have no authentication and must stay on loopback; the orchestrator checks the bound
sockets about every minute and reports a service bound off loopback (report only). The full
exposure model is in [docs/host-posture-and-exposure-spec-v1.md](../host-posture-and-exposure-spec-v1.md).

## Join a hive (`add_hive`)

`POST /hives` → action `add_hive` → `SY.orchestrator@motherbee` (`handle_hive_paths`,
`accept_add_hive`). Body:

| Field | |
| --- | --- |
| `hive_id` | 1 to 64 characters `[A-Za-z0-9_-]` |
| `address` | the host to join |
| `ssh_user` | required, `^[a-z_][a-z0-9_-]*$` |
| `ssh_password` or `ssh_key` | how to install the motherbee's key; `ssh_key` is an unencrypted PEM private key |
| `role` | `worker` (default), `egress` or `ingress` (`resolve_add_hive_role`) |
| `ssh_allow_from` | only with `role: ingress`: the networks besides the motherbee that may reach SSH on the ingress host, each an IPv4 or IPv6 address or a CIDR with its host bits clear; written into its hive.yaml as `ingress.ssh_allow_from`. SY.admin refuses it on another role or malformed (`INVALID_REQUEST`, `add_hive_ssh_allow_from_error`) |
| `egress.ssh_from_wan` | only with `role: egress`: `false` drops SSH arriving on the egress's `wan_iface` (default `true`); written into its hive.yaml |
| `harden_ssh`, `restrict_ssh` | SSH hardening after the join (`restrict_ssh` follows `harden_ssh`). With `ssh_password` and no `ssh_key`, `harden_ssh` is required: `true` turns password logins off at the join's end, `false` leaves them on (A-165). Otherwise both default to false |
| `require_dist_sync`, `dist_sync_probe_timeout_secs` | fail the join if the dist folders do not sync (probe default 45 s) |

Checks before anything starts: `INVALID_HIVE_ID`, `INVALID_ADDRESS`, `MISSING_WAN_LISTEN` (the
motherbee needs `wan.listen`), `JOIN_IN_PROGRESS`, `HIVE_EXISTS`. The old `ssh_access` field is
refused: SSH exists only inside the join (`reject_removed_add_hive_fields`). So is a password join
that does not choose `harden_ssh` (`require_password_logins_choice`, A-165). No list in hive.yaml
says which hives may join: adding a hive is what registers it, and the motherbee's router admits
only registered hives (FINDINGS A-48).

The answer is `{"status": "accepted", "join_id", "poll": "GET /hives/<id>"}` (HTTP 202): the join
runs in the background and its progress is in `/var/lib/fluxbee/hives/<id>/info.yaml`, read by
`GET /hives/<id>` (`status` joining, connected, pending, failed or interrupted; a `join` block
with phase and error). The `join` block survives every rewrite of the entry until the join ends
with `phase: done`; an orchestrator restart mid-join ends it at the next boot with
`JOIN_INTERRUPTED`, keeping the status the join had reached (`joining` becomes `interrupted`).
Posting the same request again resumes a pending, interrupted or failed join, under the role its
entry records: an entry keeps its role until the hive is removed (FINDINGS A-281, since 0.1.133).
A request with another role, or with none for a hive recorded as an ingress or an egress, is
refused with `HIVE_EXISTS` (409) before anything is written, its `role` and `message` naming the
recorded role: remove the hive first (`remove_hive`), then add it with the role it should have. An
attempt that failed may have linked the box into its role's folders, a worker's into the
runtimes, and the runtimes' use across the hives asks no hive recorded as an ingress or an egress
([below](#publish-runtimes)).

What a worker join does (`add_hive_flow`):

1. Issues the hive's certificate from the motherbee's mesh CA (`issue_hive_tls`).
2. Gets SSH access with the motherbee key `/var/lib/fluxbee/ssh/motherbee.key` (installing it
   with the given password or key) and waits for the host to settle, up to 900 s
   (`wait_for_host_to_settle`; `HOST_NOT_SETTLED` is retryable).
3. Copies the role's core binaries and `manifest.json` from the motherbee's
   `/var/lib/fluxbee/dist/core/` (`sync_core_to_worker`), the vendored Syncthing
   (`sync_vendor_to_worker`) and the installation file (`sync_installation_to_worker`, mode 0644,
   at the same place in the spoke's dist), so the spoke's readers read it from their first start;
   a motherbee that holds none has nothing to copy, and the file arrives through Syncthing once it
   does.
4. Writes the spoke's `hive.yaml`: uplink to the motherbee's `wan.listen`, `wan.mtls: required`,
   `identity.sync.upstream` with `auth: required`, the motherbee's blob and dist paths, and
   Syncthing's port and home (`render_worker_hive_yaml`). It writes nothing the release defines
   (the role's SY nodes, the blob's and dist's topology): the spoke's orchestrator, of the same
   release, has them. Nor `ai`: the installation file holds it. Nor, since 0.1.127, the blob GC:
   it runs on the motherbee only (FINDINGS A-188); since 0.1.133 a worker cleans its own
   `blob/staging` every hour with no key at all (A-241). Pushes the TLS files to
   `/var/lib/fluxbee/tls/<hive>/`; on a worker, creates and copies the identity sync key
   (`distribute_hive_identity_key_inner`).
5. Writes one systemd unit per core service (`core_service_unit_contents`), starts `rt-gateway`
   and `sy-orchestrator`, waits for the WAN link and for the spoke's orchestrator to appear.
6. Sends `ADD_HIVE_FINALIZE` over the mesh, which pairs the two Syncthing instances
   (`add_hive_finalize_via_socket`). The spoke checks what step 3 copied against the manifests,
   restarting nothing when nothing is new; a vendor still on its way does not stop it, and is
   reported in `vendor.awaiting_update` until it arrives. Its answer comes within the
   motherbee's wait (45 to 75 s, by the probe's timeout), with `timings_ms` for each step.
7. Marks the hive `connected` and revokes its own SSH access: removes the motherbee key from
   `authorized_keys` and the temporary sudoers file (`revoke_bootstrap_ssh_access`).

Egress and ingress joins (`add_egress_hive_flow`, `add_ingress_hive_flow`) follow the same steps
without the identity key; an ingress join also needs `ingress.listen` and writes the `edge`
section. An egress join reads the box before it installs anything: `edge_ip`, when the request
omits it, is the single address `lan_iface` carries inside `lan_cidr` (none or several fail the
join, asking for an explicit one), an explicit one must be on `lan_iface`, and both interfaces
must exist (`EGRESS_LAN_LEG_INVALID`; `EGRESS_LAN_LEG_NOT_VERIFIED` when the box cannot be
read). Once the egress' orchestrator is on the mesh it reads
again that the `inet fluxbee_egress` table is loaded, forwarding is on, IPv6 is disabled and the
LAN leg is in place, and reports it (`egress_nat_applied`, `egress_ipv4_forwarding`,
`egress_ipv6_blocked`, `egress_lan_leg_ok`); any of them missing fails the join with
`EGRESS_NAT_NOT_READY` (`EGRESS_NAT_NOT_VERIFIED` when the box cannot be read). The interfaces and their addresses are the operator's: the orchestrator
never configures them, and the egress fails its own boot on any of these.

**SSH checks before the install.** An ingress join reads the motherbee's source address over the
bootstrap session (`SSH_CONNECTION`) and stops with `SSH_SOURCE_NOT_ALLOWED` when the ingress' SSH
rules would not admit it: the message names the address to add through `ssh_allow_from`. Its later
SSH steps (hardening, key revoke) therefore come from an allowed address. An egress join with
`egress.ssh_from_wan: false` stops with `SSH_FROM_WAN_LOCKOUT` when the box routes the session's
client through `wan_iface` (`ip -o route get <client>`), where the join's packets come in; join it
through another interface. Either check answers `SSH_SOURCE_NOT_VERIFIED` when it cannot read the
session.

**Internet probe.** The egress, a worker that routes through it and the motherbee check the
internet with an HTTPS GET of their own `egress.reachability_url` (the egress at boot and every
drift tick, the others at boot); a failure is a warning in that hive's journal, and on the egress
also an `internet_unreachable` drift alert in its own file, which the motherbee does not read yet
(FINDINGS A-175). There is no
default: without the URL the probe does not run and logs `internet_reachable=not_configured`. An
egress join takes `egress.reachability_url` from the request, else the motherbee's, and refuses
anything but an `https://` URL; a worker join copies the motherbee's with `gateway_ip`. A worker
joined before the motherbee declared its egress gets the section, the URL with it, through
`set_hive_egress` (below); an egress joined before gets the URL by adding the line to its hive.yaml
and restarting `sy-orchestrator`. A failed probe says why, in curl's words (no DNS, no connection,
a timeout, an HTTP error, TLS: FINDINGS A-225).

**A join over a password chooses what happens to password logins (A-165).** Since D32 the join
revokes its own key and sudoers entry at its end, and nothing else closes password logins on the
box. So a request with `ssh_password` and no `ssh_key` must carry `harden_ssh`, or it is refused
with `INVALID_REQUEST` before anything touches the box (next to an `ssh_key` the password only
feeds `sudo`):

- `harden_ssh: true` turns password logins off at the join's end. Afterwards only the hypervisor's
  console, or a key of the operator's own in the user's `authorized_keys`, gets in.
- `harden_ssh: false` leaves them on.

A join re-run after one that failed late (the worker's orchestrator already answers) touches no
SSH, so it applies no `harden_ssh`; when its request carries a password, its `warnings` say so.
A successful join's result (`join.result` in `GET /hives/<id>`) carries `warnings`. A join that
logged in with `ssh_password` (its key-first probe failed and the password seeded the motherbee
key) with `harden_ssh: false` says there that password logins stay enabled on the box, and how to
close them: `PasswordAuthentication no` in
`/etc/ssh/sshd_config.d/00-fluxbee-hardening.conf` and a restart of ssh, which is what
`harden_ssh: true` does.

**Remove a hive:** `DELETE /hives/{hive}` (`remove_hive_flow`) sends `REMOVE_HIVE_CLEANUP` to the
spoke over the mesh (8 s). The spoke answers once its cleanup is scheduled as a transient unit of
its own (`fluxbee-remove-hive-cleanup-<ms>`), which deletes its TLS material and identity key,
stops the Fluxbee services and the managed nodes and deletes its node state, and runs to its end
after the spoke's orchestrator has stopped; an ingress then drops its SSH table
(`inet fluxbee_ingress`) and `fluxbee-ingress-nft.service`, an egress its NAT table and files, and a
worker its IPv6 block (`/etc/sysctl.d/99-fluxbee-egress.conf`) and its egress switch's record
(FINDINGS A-225): IPv6, and the default route its own network configuration gives it, come back at
its next boot. An egress or ingress the cleanup did not reach keeps them, and the answer's
`warning` says what to remove from its console. The motherbee then unpairs Syncthing (the hive
leaves every `fluxbee-dist*` folder and both blob folders, whatever the blob's flags), deletes
`hives/<id>/` and the hive's identity key under `/var/lib/fluxbee/identity/keys/`
(`identity_key_cleanup` in the answer). When the spoke did not answer, the motherbee restarts its
`sy-identity` to close any session the removed hive still holds (`identity_restart`). The
motherbee itself cannot be removed. The registry is the authority: at boot the motherbee deletes
the identity key of every hive with no `hives/<id>/`. Public pages never refuse the removal
(FINDINGS A-242): the ok answer names in `pages_left_up` the pages a node of the hive published
and the pages its edge serves, which stay up until they expire. The cleanup stops the hive's edge
(FINDINGS A-236), so the latter are served no more once it ran; taking one down then releases only
its copy (`edge_skipped`), and the expiry sweep retires it the same way.

**The removed hive's system ILKs** (FINDINGS A-191): once the hive is gone, SY.admin asks
SY.identity to retire its `SY.*@<hive>` identities (`ILK_RETIRE_HIVE`): marked, reversibly, out of
every lookup and of the identity SHM, never purged. The ok answer names the result in
`system_ilks_retired` (`{count, ilk_ids, already_retired}`). A retire that fails leaves the removal
ok: `system_ilks_retired` is `{count: null, error_code, error_detail}` and `warning` says so; then
run `retire_hive_ilks` (`POST /hives/<id>/system-ilks/retire`), never the removal again, which
would only answer `NOT_FOUND`. The same action retires the ILKs of a hive removed before this
release (worker2, the throwaway gate workers); it is refused for the motherbee and for a hive the
registry still lists (`409 HIVE_NOT_REMOVED`). The removed hive's own SY.identity, when it
outlives the removal, cannot take the marks off: the motherbee's SY.identity drops every publish of
a hive the registry no longer lists. A hive joined again under the same name gets its system ILKs
back unmarked from its own SY.identity at its start. A retired system ILK is purged
(`purge_ilk`) only on the operator's word, and only when it made no history (no LLM usage, no
message, no conversation under it); one that made history stays retired. The system ILKs of the SY
nodes a release no longer runs on a hive (SY.policy's, dropped in 0.1.79) are retired by
SY.identity itself at its start.

## A worker behind the egress (`set_hive_egress`, `clear_hive_egress`)

A worker that joins after the motherbee declares `egress.gateway_ip` gets the motherbee's `egress`
section, and its boot applies it: its default route goes to the gateway, IPv6 off, a probe
(docs/edge-egress-nat-spec.md §7). A worker that joined before keeps its own route until the
operator puts it behind the egress (FINDINGS A-219, §15 of the spec):

- **`PUT /hives/{hive}/egress`** (`set_hive_egress`), body `{}` or `{"access_from": [...]}`:
  the worker's hive.yaml gets the section the join would have written, and its route goes to the
  gateway. A worker behind another gateway moves to the declared one the same way.
- **`DELETE /hives/{hive}/egress`** (`clear_hive_egress`): the default routes the worker had
  before it first went behind the egress (its record keeps them; a worker with no record names
  `gateway_ip`), the section out of hive.yaml, and its IPv6 values back. The way around an egress
  outage while the egress is fixed.

Both are the operator's: on SY.admin's loopback API, or staged by Archi and confirmed. Nothing on the
mesh may ask for them. The motherbee itself is refused (`NOT_A_WORKER`): Fluxbee never rewrites its
route (PB-8).

What runs:

1. **SY.admin** checks that the body names no other hive than the path's (`target`, `hive_id`:
   `INVALID_REQUEST`), that the hive is a worker of the registry whose join is done
   (`NOT_A_WORKER`), that no other egress action runs, on any worker (`EGRESS_SWITCH_OPEN`: one at a
   time), and that no installation update run is under way, nor a task of the update acting on that
   worker (`INSTALLATION_UPDATE_RUNNING`).
2. **The motherbee's orchestrator** checks its own `egress`: `gateway_ip` (`NO_EGRESS_DECLARED`),
   `reachability_url` (`NO_REACHABILITY_URL`: the switch is judged by a probe of it), a declaration
   the join would refuse (`EGRESS_DECLARATION_INVALID`). It holds the hive's topology lock for the
   whole action (`JOIN_IN_PROGRESS`), and sends `SYSTEM_EGRESS_ROUTE` to the worker
   (`HIVE_UNREACHABLE` when its orchestrator is not on the mesh).
3. **The worker** runs the guards, reads that only read (`ip -4 route get fibmatch`, `sshd -T`,
   `ss`, 5 s in all from the request on), and refuses before anything changes:
   - `GATEWAY_NOT_ON_LINK`: the gateway is not on a subnet the worker is connected to, or is
     reached through a multipath route.
   - `UPLINK_ON_DEFAULT_ROUTE`: it reaches the motherbee (`wan.uplinks`, `identity.sync.upstream`)
     through its default route, by a host name, over IPv6, or through a tunnel (tun, WireGuard,
     GRE, IPIP, PPP, tap, VXLAN...), whose own packets may take the default route.
   - `ACCESS_ON_DEFAULT_ROUTE`: a live SSH session, or an `access_from` address or network, reaches
     it through its default route. Keep such a network with a static route at the OS level (netplan)
     through the current gateway, which the guard then sees as a more specific route; Fluxbee adds
     no route of its own. A session through a tunnel passes this guard, though the tunnel's own
     packets may take the default route: the console stays.
   - `DEFAULT_ROUTE_MULTIPATH`: the worker's default route is a multipath route, or goes through a
     nexthop object, which a revert could not put back. Give it a default route through one
     gateway first.
   - `PROBE_UNAVAILABLE`: no `curl`. `EGRESS_GUARD_UNREADABLE`: a lookup, sshd's ports or a
     device's type could not be read in time. sshd's ports are read with `sshd -T` once more after
     creating `/run/sshd`, which a socket-activated host lacks until its first SSH connection.
4. It records the way back (`/var/lib/fluxbee/orchestrator/egress-switch.json`: the default routes,
   the four IPv6 sysctls, the sysctl file), pauses its watchdog's egress step, switches
   (`ip route replace default via <gateway_ip>`, the IPv6 block judged by what the kernel reads
   back), probes `reachability_url` (twice at most) and reads its route to the motherbee again, then
   answers `pending_confirm`.
5. The motherbee sends the confirm. The worker keeps the switch only once it arrives, within 15 s:
   it writes hive.yaml (atomically, only `egress` changed), then its in-memory section. Otherwise,
   or when a step failed, it puts the routes, the IPv6 values and the sysctl file back:
   `EGRESS_PROBE_FAILED` (with curl's reason), `EGRESS_IPV6_UNMANAGED`, `UPLINK_ROUTE_CHANGED`,
   `EGRESS_SWITCH_FAILED`, `EGRESS_COMMIT_FAILED`, `EGRESS_CONFIRM_MISSED`. The revert does every
   step it can, and `revert_problems` names each one it could not do.

While the switch is open (at most a minute), the worker's orchestrator refuses a core update or a
core rollback sent to it, which would restart it (`EGRESS_SWITCH_OPEN`; `update`,
`update_installation`, `core_rollback`): run them again once the switch ends.

A clear's probe is reported and decides nothing: the way back may have no internet by design. An
orchestrator that stops mid-switch finishes it at its next start: a commit hive.yaml holds is kept,
anything else is undone from the record (`last_switch.state: interrupted`, and a drift alert in the
worker's own file).

The answer: `status` (`ok`, or `error` with `error_code` and `step`), `outcome` (`switched`,
`cleared`, `unchanged`, `refused`, `reverted`, `not_answered`; a refusal of SY.admin's says it in
its `payload`), `gateway_ip`, `previous_gateway`, `ipv6_blocked`, `internet_reachable` and `probe`
(`url`, `attempts`, `reason`), `ssh_reachable_from`, `switch_id`, `timings_ms` per step.
`EGRESS_NOT_ANSWERED` (504) never means "not applied", whatever its `step`: an orchestrator on an
older release ignores the message (bring the worker to the motherbee's release first,
`update_installation`), a worker whose answer was lost reverts by itself unless it got the
confirm, and one that took the confirm and did not answer it in time (`step: commit`) may still
have committed. An answer SY.admin did not get (`TIMEOUT`) says `not_answered` too.

`GET /versions` and `GET /hives/{hive}/versions` say where each worker stands: `egress.section`, the
default gateway it sees, `ipv6_blocked`, `last_switch` (`state`, `step`, `error_code`, `reason`),
and the motherbee's comparison, `egress.state`: `behind`, `not_behind`, `other_gateway` (the
declaration changed since) or `drifted` (the section is right, the route is not). Connections open at
the switch through the default route break once: run it while the worker's agents are idle.

## Publish runtimes

Runtimes are versioned artifacts under `/var/lib/fluxbee/dist/runtimes/<runtime>/<version>/`,
listed in `dist/runtimes/manifest.json` with `available` versions and a `current` pointer.

- `scripts/publish-runtime.sh --runtime <name> --version <v> --binary <path> [--start-script
  <path>] [--file <path>]... [--set-current] [--prune-missing] [--dist-root <dir>]` copies a
  binary, writes `bin/start.sh` (or installs the one given, which must run the binary with the
  line `exec "$SCRIPT_DIR/<binary>" "$@"`), installs each `--file` next to `bin/`, and merges the
  manifest last. It checks every input before it writes anything, and the manifest never names a
  version that is not all there: the orchestrator moves running nodes onto it on its next pass
  (FINDINGS A-110). A live manifest that is there but cannot be read, does not parse, or is no
  manifest (a JSON object whose `runtimes` is an object) is refused before anything is written,
  since merging over it would drop every other runtime; only a manifest that does not exist starts
  empty. The manifest is written whole or not at all: a temp file beside it, renamed over it. The `publish-*-runtime.sh` scripts build on it with their own start script;
  `publish-ia-runtime.sh` also hands it ai.generic's `tools.json`, the catalog of
  `docs/reference/agent-tools.json` (or `--catalog <path>`) with the version in it.
  `fluxbee-publish <package dir> [--deploy <hive>]` (`src/bin/fluxbee-publish.rs`) publishes a
  package directory and always sets it current.
- Over HTTP: `POST /admin/runtime-packages/publish` (`publish_runtime_package`, motherbee only)
  with an `inline_package` or `bundle_upload` source; optional `sync_to[]` and `update_to[]`
  follow-ups.
- **U-3, the seed merge.** The `.deb` does not ship `manifest.json`: the manifest is operator
  state. `deb-preinst` snapshots it on upgrade; `fluxbee-seed-runtimes` (run by `deb-postinst`)
  registers each base runtime into the live manifest through `publish-runtime.sh`, leaves a
  hot-published `current` alone when its directory still exists, restores the snapshot only if
  the live manifest is missing or unreadable, and fails if it registered nothing. Repair:
  `sudo fluxbee-seed-runtimes && sudo systemctl restart sy-orchestrator`.
- **The way back from a hand publish (A-122).** The seed records the version the package brought
  as each base runtime's `package_version`, which `list_runtimes` and `get_runtime` show. A
  `current` that differs is a hand publish, kept through every upgrade (the seed's NOTE says so in
  dpkg's output). `reset_base_runtime` (`POST /hives/motherbee/runtimes/<runtime>/reset`, or Archi
  behind the operator's CONFIRM) makes `package_version` current again; the orchestrators move the
  runtime's nodes within their next pass. `remove_runtime_version` can then delete the hand
  publish, except on a base that packages are built on (`RUNTIME_HAS_DEPENDENTS`: it stays,
  unused). It never deletes `package_version` itself (`RUNTIME_PACKAGE_VERSION`), and
  `publish-runtime.sh` refuses a hand publish under that version.
- **A version that may be in use stays (FINDINGS A-281, since 0.1.133).** `remove_runtime_version`
  (`DELETE /hives/motherbee/runtimes/<runtime>/versions/<version>`) first reads the version's use
  across the hives, the motherbee's `usage_global_visible`, which
  `GET /hives/motherbee/runtimes/<runtime>` shows for all of a runtime's versions. The motherbee's
  orchestrator asks each hive of its registry for its nodes, but an ingress or an egress, all at
  once, each with 10 s to answer. A worker reads its nodes from their configs whatever its mirror
  of `dist/runtimes` lists, so one whose mirror does not list the runtime yet (10-60 s after a
  workflow's first deploy) is counted by its nodes:
  - `running_nodes`: each node pinned to the version whose unit may run, on the motherbee and on
    each hive that answered, with its `hive` and `lifecycle` (`RUNNING`; `STARTING`, its unit runs
    and the router does not see it yet; `UNKNOWN`, systemd did not answer for its unit). A node
    whose unit does not run (stopped, failed, between two restarts of a crash loop) is not
    listed: one relaunched later fails to start once its version is gone, so stop it
    (`kill_node`) or move it first;
  - `skipped_hives`: each hive whose use is unknown, with its `reason` and the `message` that came
    with it: `TIMEOUT` (no answer within 10 s: its orchestrator stopped or restarting while the
    LSA still shows it), `UNREACHABLE` (the request never reached it: down, or stale in the LSA),
    `USAGE_UNREADABLE` (its own read of its nodes failed: its nodes' folder could not be read, or
    a node's `config.json` cannot be read or parsed while its unit runs or systemd gives no
    answer for it, so that node's runtime is unknown; the `message` names it: fix its config or
    stop the node), or the error a worker before 0.1.133 answered, which reads its mirror first
    (`RUNTIME_NOT_FOUND` while its mirror does not list the runtime yet, `RUNTIME_MANIFEST_MISSING`
    without one). A worker, and a hive whose registry entry gives no role (a join being accepted,
    an entry that does not parse), are asked;
  - `hives_without_runtimes`: each ingress and egress, by the role the hive registry records,
    whatever the state of its join; an entry keeps its role until the hive is removed
    ([add_hive](#join-a-hive-add_hive)). The runtimes never reach them (`fluxbee-dist-runtimes`
    goes to the workers only, [below](#how-dist-reaches-the-other-hives)), so no managed node runs
    there: they are not asked, and one that is down holds no removal up;
  - `in_use`: true while a node is listed, false only once every hive asked was counted, and
    `null` (unknown) while none is listed and a hive is in `skipped_hives`.

  A registry the motherbee cannot list makes the whole use an error (`status: error`), and so
  does a motherbee node whose `config.json` cannot be read while it may run. With a worker that
  does not answer, the read ends in about 10 s, within SY.admin's 30 s for it, so the refusal
  names that worker, as long as the read starts at once: the motherbee runs 4 such reads at a
  time (removals, and reads of `GET /hives/motherbee/runtimes/<runtime>`), each up to 10 s while
  a worker does not answer, so a fifth asked meanwhile waits up to 10 s and a ninth up to 20 s,
  and its refusal may then name the motherbee (`TIMEOUT`); `GET /versions` and Fluxbee Cloud's
  node lists hold none of those 4. SY.admin refuses the removal, with nothing removed:
  `409 RUNTIME_IN_USE` while a node is listed, its detail naming each node with its hive and
  lifecycle; `409 RUNTIME_USAGE_UNKNOWN` while the use is unknown (an error, or a hive in
  `skipped_hives`), naming each hive not read and why in `payload.unanswered_hives` (a join not
  done with its `status` and `join_phase`; the motherbee alone when the read failed there, so no
  hive was counted), with `payload.retryable`, whether asking again, nothing changed first, may
  pass: false while a join that ended (failed or interrupted) is not read, and when the motherbee
  could not read its own nodes or its registry (`USAGE_READ_FAILED`: stop the node whose
  `config.json` it names, or fix that config, or make the folder or the registry readable,
  first). Ask again once each hive answers: bring a worker back, finish or remove a join left
  behind, and remove a hive gone for good with `remove_hive` first. A workflow's delete whose
  purge meets the refusal is done all the same, and names that version and those after it in
  `payload.packages_left` (below). Before 0.1.133 a hive not counted read as one with no node: a
  version a node of a restarting worker ran on was removed, and the worker's mirror deleted it
  when it came back. And every ingress and egress was in `skipped_hives` on every read, for its
  `RUNTIME_MANIFEST_MISSING`, and the hives were asked one after another, 45 s each, so one that
  did not answer held the read past SY.admin's 30 s
  ([SY.orchestrator](../nodes/sy-orchestrator.md#managed-nodes)).
- **A workflow's delete can leave its versions in dist (FINDINGS A-281).** SY.wf-rules removes them
  last, after the workflow, its node and its state, through `remove_runtime_version`, each waited
  for up to 35 s within what is left of the request's 60 s, and stops at the first one refused or
  not answered. Since 0.1.133 the delete answers ok all the same: `payload.packages_left` names
  each version left, with why, `retryable` and the hives not read, and `payload.warning` says so.
  A workflow created again under that name numbers its versions above every version the dist
  holds, and its deploy's purge removes those left; to remove them sooner,
  `remove_runtime_version` each (`GET /hives/motherbee/runtimes/wf.<name>` lists them) once every
  hive answers, the runtime's current last. One entry with no version, `DIST_UNREADABLE`, says the
  hive's `dist/runtimes/wf.<name>` could not be listed: until it can, a compile of the name that
  names no version is refused, so fix it first. `fluxbee-factory-reset` prints them under each
  deleted workflow and counts them in its log line (`packages_left=`); its next run no longer
  lists the workflow.
- **One writer of the manifest at a time (FINDINGS A-171).** Every writer holds an `flock` on
  `dist/runtimes-manifest.lock` from its read of the manifest to its write: SY.admin's
  `publish_runtime_package`, `remove_runtime_version` (across its read of the version's use, up to
  30 s) and `reset_base_runtime`, `fluxbee-publish`, `publish-runtime.sh` (and every
  `publish-*-runtime.sh` on it), and the seed for its whole run: its restore of the snapshot, and
  each base runtime's read of `current` with the publish that read decides, whose
  `publish-runtime.sh` takes the lock on the seed's own descriptor (`FLUXBEE_MANIFEST_LOCK_FD`; a
  descriptor of any other file is refused). A writer waits for another up to 60 s, then fails
  with the manifest untouched; before, a hand publish and an admin write that read the manifest
  at the same moment lost one of the two writes. The lock file sits in the dist root, beside the
  `runtimes` folder and never inside it, since Syncthing shares that folder with the workers; it
  stays (removing it would let a writer lock a new one beside one still held). The package does
  not ship it, nor the manifest: each writer creates it where it is missing, so dpkg never owns,
  replaces or removes it (`packaging/build-deb.sh` refuses a .deb that carries either).

### How dist reaches the other hives

The orchestrator declares one Syncthing folder per kind of content (`dist_sync_folders_for_role`),
`sendonly` on the motherbee and receive-only on spokes:

| Folder | Motherbee path | Spoke path | Roles |
| --- | --- | --- | --- |
| `fluxbee-dist-core-<role>` | `dist/core/<role>` | `dist/core` | every spoke role |
| `fluxbee-dist-vendor` | `dist/vendor` | `dist/vendor` | every spoke role |
| `fluxbee-dist-policy` | `dist/policy` | `dist/policy` | every spoke role |
| `fluxbee-dist-installation` | `dist/installation` | `dist/installation` | every spoke role |
| `fluxbee-dist-runtimes` | `dist/runtimes` | `dist/runtimes` | workers only |
| `fluxbee-dist-agent-assets` | `blob/agent-assets` | `blob/agent-assets` | workers only |

The motherbee rebuilds `dist/core/<role>` at every start of its orchestrator
(`materialize_role_core_trees`). A spoke's receive-only copy is never edited locally: the
orchestrator reverts local changes through Syncthing's `/rest/db/revert`, once the folder's
source has been connected and sharing it for 60 s, so its index is in (FINDINGS A-190). Until then
they stay: a new spoke's folders are filled by its join before the motherbee's peer is linked, and
a revert before the source's index arrives deletes them. With the index in, the copies that match
it stop counting as local changes.

`dist/installation` holds the installation file (`installation.json`): the installation's `ai`,
one version on every hive, written only by the motherbee's orchestrator. It reaches the hives that
joined before it at the next start of each side's orchestrator, which shares the folder with
vendor's members, and a new spoke gets it from its join
([SY.orchestrator](../nodes/sy-orchestrator.md#the-installation-file),
[hive-config-update-v1](../hive-config-update-v1.md) §4). A new version, the operator's, arrives
through the same folder ([The installation's AI settings](#the-installations-ai-settings)).

**Announcing a package to workers.** There is no announce message: the motherbee's Syncthing
announces a change after its own scan. To make a worker pull a package now, scan the folder **on
the motherbee**: `POST /hives/motherbee/sync-hint {"channel":"dist","folder_id":"fluxbee-dist-runtimes","wait_for_idle":true}`
(`SYSTEM_SYNC_HINT`, which calls Syncthing's `/rest/db/scan`). SY.wf-rules does exactly this
before deploying a WF node on a worker (`announceRuntimes` in `go/sy-wf-rules/node/deploy.go`,
whose comment notes that a scan on the worker does not make it pull).

`POST /hives/{hive}/update {"category":"runtime","manifest_version":<n>,"manifest_hash":"<sha256>"}`
sends `SYSTEM_UPDATE` to that hive's orchestrator: it answers `sync_pending` (HTTP 202) until its
copy of the manifest has that hash and the artifacts are present, then reloads the manifest and
applies retention; it does not restart nodes. `run_node` reads the manifest from disk.

A running node that follows `current` moves to a new `current` on its own: the orchestrator's
reconcile relaunches it on that version within a minute of the version's files reaching its hive,
with or without the update. A pinned node keeps its version; a package on a base runtime (a WF
node on `wf.engine`, a config-only AI package on `ai.generic`) still moves to its base's new
`current`, pinned or not
([SY.orchestrator](../nodes/sy-orchestrator.md#a-running-node-and-a-new-runtime-version)).

## Upgrade

### The motherbee

`apt-get install fluxbee=<version>`. On upgrade (`deb-prerm`, `deb-postinst`):

- only the writers of the runtimes manifest stop: `sy-orchestrator`, `sy-admin`, `sy-wf-rules`;
  the router, identity, vault and the rest keep serving on the old binaries while dpkg unpacks;
- the seed runs, `/var/lib/fluxbee/orchestrator/core-installed.json` is removed, and the
  orchestrator starts again;
- its boot (`bootstrap_local`) restarts every core service whose running binary was replaced
  (`/proc/<pid>/exe` ends in ` (deleted)`), and every one of its role's whose
  `systemctl show -p MainPID` gives no answer, or whose `/proc/<pid>/exe` link cannot be read
  for another reason than a process gone, asked again (a restart also starts a stopped one;
  FINDINGS A-282, since 0.1.136), in start order, records it as the last core update when one of
  them ran a replaced binary (`via: package`; `updated` the replaced ones, `unanswered` the
  others, apart), rebuilds the per-role core trees for the spokes, and moves managed nodes that
  follow `current` onto the new version (`reconcile_persisted_custom_nodes`, the pass its 60 s
  loop repeats).

### The spokes (core update)

Since 0.1.123 one call brings every spoke to the motherbee's release, core then config, and SY.admin
keeps the update until each is on it: `POST /installation/update`
([The installation's update](#the-installations-update)). It sends each spoke the per-hive core
update below, which stays the operator's tool for one hive (a rolled-back one included), and the
only way for a release before 0.1.123.

`POST /hives/{hive}/update {"category":"core","manifest_version":0,"manifest_hash":"<sha256 of the motherbee's dist/core/manifest.json>"}`:

- The spoke checks that its synced `dist/core/manifest.json` has that hash; otherwise
  `sync_pending`.
- `prepare_core_update` checks each core binary of the role that `dist/core/bin` holds against
  that manifest and fails the update (`UPDATE_FAILED`) on the first that does not match, so it
  never installs one; past that check it closes `core.awaiting_update` (FINDINGS A-186). A node
  of its role's list that dist holds no binary of, one the new release dropped from the role, is
  left out as installed (`left_out`), but only by an update that replaces `sy-orchestrator`;
  another update refuses it. It backs up the binaries that differ to
  `/var/lib/fluxbee/orchestrator/core-bin.prev.local/`, swaps them into `/usr/bin` and re-renders
  the units (`regen_local_core_units`).
- It answers first (`phase: restarting`), then restarts `rt-gateway` and the system services in
  order but the left-out ones, checking each; a failed check rolls the binaries back. With no
  binary and no unit new it restarts nothing and answers `ok` at once (FINDINGS A-190). A replaced
  `sy-orchestrator` restarts itself through a transient `systemd-run` timer. The outcome is in
  `core-update-last.json`; what is installed, in `core-installed.json`.
- The update runs in the spoke's previous orchestrator, which knows only its own release's list.
  The new orchestrator finishes it at its start, with its own list
  ([hive-config-update-v1](../hive-config-update-v1.md) §3): it stops, disables and removes a
  node the release dropped, unit and binary, kept in the update's generation; it installs a node
  the release added once dist holds its binary. `core-update-last.json` gets `settled`; the
  generation, `settled_at_ms`.
- The spoke's managed nodes that follow `current`, and its package nodes whose base runtime
  moved, move to the new runtime versions within a minute of those versions reaching it, through
  the orchestrator's 60 s reconcile, whether or not the update restarted its orchestrator
  (FINDINGS A-109).
- `POST /hives/{hive}/core/rollback {"generation":"latest"}` (or `{"to_manifest_hash"}`, with
  `dry_run`) goes back to a backed-up generation: a node its update or its settle added is
  stopped, then removed; a node its release dropped comes back, unit and binary, and the
  previous orchestrator, restored with them, starts it.
- `GET /hives/{hive}/versions` (and `GET /versions` for every connected hive) compares core,
  vendor and runtimes; for core the installed hash wins over the synced one. A hive whose start
  found core binaries in dist that do not match the manifest lists them in
  `core.awaiting_update`, with why each. It runs what `/usr/bin` holds until a core update
  installs the binaries. The check every minute only closes the report once dist is consistent
  ([SY.orchestrator](../nodes/sy-orchestrator.md#updates)). Each hive's `config` is its copy of
  the installation file: `version`, `sha256`, `ai` and `state` against the motherbee's file
  (`current`, `behind`, `pending`, `edited_by_hand`), the motherbee's own included, with
  `applied`, the config update the hive applied last; the motherbee's lists `versions`
  ([SY.orchestrator](../nodes/sy-orchestrator.md#the-installation-file)).

The lab routine `lab/ops.py deploy <version>` does all of this in order (`cmd_deploy`): snapshots
every VM, installs the version on the motherbee, waits for its core to report it, waits until each
spoke's orchestrator answers, then:

- from 0.1.123, calls `POST /installation/update` with `{"reason": "deploy <version>"}` and reads
  `GET /installation/update` every 3 s until the update is done or held, or until nothing moves a
  hive any more (900 s at most; a hive that did not answer still moves, for its automatic step). It
  follows only this call's update, and prints each hive's transitions in this run as they happen
  (answered, installed, settled, config applied, or once "on target" for a hive the run did not
  move, which includes one the background task moved, `by: background`, and one a read found
  there, `seen`), with the seconds since the call and the record's own, and each hive left out with
  what moves it.
  A held update passes with a warning naming the hives rolled back by the operator;
- below 0.1.123 (deploying an older release again, the rollback), sends the core update to every
  spoke and waits until each spoke reports the motherbee's hash and settles;

then runs `health`, and prints the phases and each spoke's own records.

## The installation's update

One update brings every hive to what the motherbee holds, its core release and its installation
file ([hive-config-update-v1](../hive-config-update-v1.md) §6, "As built (stage 4)";
[SY.admin](../nodes/sy-admin.md#the-installations-update)). SY.admin keeps it in
`/var/lib/fluxbee/state/sy-admin/installation-update.json`, one at a time, until every hive is on it.
On the motherbee:

1. **Run it** after a release is installed on the motherbee (`apt-get install fluxbee=<version>`),
   or to finish an open update; or ask Archi, which stages it as a plan you confirm:

   ```bash
   curl -sS -X POST http://127.0.0.1:8080/installation/update \
     -H 'content-type: application/json' -d '{"reason":"deploy 0.1.123"}'
   ```

   Each hive gets its core first (`SYSTEM_UPDATE` category core), then its config. It answers once
   every core update answered: `ok` when every hive is on the target, or when the update is held
   (below); `partial` (202) while the update stays open, with `pending`. A core `restarting` is
   followed by SY.admin to its settle, then its config goes. With every hive on the target and no
   update open, `nothing_to_do`. It refuses, changing nothing: `409 INSTALLATION_UPDATE_RUNNING`
   while another run (or `PUT /installation/ai`) is under way; `503 HIVE_REGISTRY_UNREADABLE` when
   SY.admin cannot read `/var/lib/fluxbee/hives`; `409 MOTHERBEE_ROLLED_BACK` when the motherbee's
   own core is rolled back (install a release on it first).
2. **Read it**: `curl -s http://127.0.0.1:8080/installation/update` (add `?history=true` for the
   closed ones). Its `state` is `open`, `held`, `done` or `superseded`. Each hive's `core` and
   `config` have a `state` (`on_target`, `pending`, `in_flight`, `excluded`, `unknown`), the last
   `outcome` with its times, and `next`, what moves it. A hive found on the target where its last
   outcome said it was not (moved by its per-hive update, say, or by an apply whose answer never
   came) shows the read that found it there as its outcome: `settled` for a core, `ok` for a
   config, marked `seen: true`, `by` whoever read it, its `detail` naming the outcome it replaces.
   `GET /versions` carries the summary (`installation_update`). A read makes SY.admin look again at
   the hives off the target it has not looked at for 30 s.
3. **What it leaves as it is**, with why: a hive rolled back with `core_rollback` keeps its core
   (its config still goes) unless the run names it, `{"include_rolled_back": ["egress1"]}`; a copy
   edited by hand gets no config (restore it first); a hive before 0.1.122 gets its core first.
   `POST /hives/{hive}/update` moves any one hive, and SY.admin looks at it at once.
4. **Held**: when the only hives off the target are those rolled back with `core_rollback` (their
   config on the target), the update is `held` and names them: the one allowed divergence. The
   per-hive update that moves one back closes it `done`.
5. **Finishing it**: a hive that did not answer (`not_answered`) gets the update finished by
   SY.admin, once, when it answers again (the automatic step), never a rolled-back hive, one ahead
   of the motherbee or one with a hand-edited copy. An error, or a copy that did not arrive in
   time, waits for you: fix what its `next` says, then run the update again (the same target
   resumes the open update) or update that hive. A core that did not settle stays pending until
   its record changes: run the update again, which sends its core update again.
6. **The installation's rollback**: install the previous release on the motherbee, then run the
   update (a release before 0.1.123 has none: `ops.py deploy` sends the per-spoke core update). An
   update left open for the other release says `stale_target`, and nothing acts on it until the
   run replaces it.

Every outcome is one line of SY.admin's journal, `installation update: outcome`, which
`ops.py timeline` shows.

## The installation's AI settings

The installation's `ai`, its default provider and each provider's model, is one file,
`dist/installation/installation.json`, the same version on every hive, written only by the
motherbee's orchestrator ([hive-config-update-v1](../hive-config-update-v1.md) §4;
[SY.admin](../nodes/sy-admin.md#the-installations-ai)). The motherbee's hive.yaml `ai` is only its
first version. On the motherbee:

1. **Read it**: `curl -s http://127.0.0.1:8080/installation` (`get_installation`): `motherbee.ai`,
   `expected_version`, the newest 20 versions recorded with their values (`versions`, with
   `versions_total`; `?versions_before=<versions_next_before>` lists older ones), and each hive's
   copy (`hives`).
2. **Change it** with the whole new `ai` and the version you read, or ask Archi, which stages it as
   a plan you confirm:

   ```bash
   curl -sS -X PUT http://127.0.0.1:8080/installation/ai \
     -H 'content-type: application/json' \
     -d '{"ai":{"default_provider":"anthropic","providers":{"openai":{"model":"gpt-5.5"},"anthropic":{"model":"claude-sonnet-5-5"}}},"expected_version":1}'
   ```

   A new default provider needs its key (`resource_type` `openai` or `anthropic`) wherever a
   reader of the default reads one, before the change: in the root tenant for the core (SY.admin's
   executor, Archi, SY.cognition, SY.frontdesk.gov), and in each tenant with an AI node that names
   no provider of its own. Check with the vault's listing (names only), the same one SY.admin
   checks before it writes:

   ```bash
   curl -s 'http://127.0.0.1:8080/hives/<motherbee>/vault/secrets?tenant_id=<tenant>&resource_type=anthropic'
   ```

3. **What happens**: SY.admin checks the AI nodes when the change switches the default provider or
   drops a provider (A-11), and, when it switches the default provider, that every reader of the
   default finds a key of the new one; the motherbee's orchestrator writes the next version;
   Syncthing takes it to every hive; SY.admin applies it on every hive at once. A hive restarts only
   what reads `ai` at its start, and only when its engine (the default provider with its model)
   changed, or when the hive cannot vouch for what it runs: at the first change it takes, when
   nothing recorded it yet, and for a reader restarted since its last change. Those readers are
   `sy-cognition` and `sy-frontdesk-gov` on the motherbee, `sy-cognition` on a worker. AI nodes take
   it at their next request, SY.admin's executor and Archi at their next use. Nothing else restarts.
4. **The answer**: `ok` (200) when every hive applied it; `partial` (202) names the hives that have
   not yet, each `sync_pending` (the file had not arrived within 90 s), `not_answered`, `error` or
   `left_to_task` (another task of the installation's update held it until too little of the
   change's time was left to apply it, named in `detail`, so nothing was sent to it; one that lets
   it go sooner is waited for), each with `next`, what finishes it. The version is written either
   way. `skipped` names each hive whose join did not finish: finish its join or remove it, then
   update it as below. `GET /versions` shows each hive's copy (`config.state`, `config.applied`),
   and this finishes one hive:

   ```bash
   curl -sS -X POST http://127.0.0.1:8080/hives/worker1/update \
     -H 'content-type: application/json' \
     -d '{"category":"config","manifest_version":2,"manifest_hash":"<its sha256, from GET /installation>"}'
   ```

5. **Undo** a change by setting a previous version's values (from `versions`) the same way: it
   becomes the next version. A core rollback never touches the file.
6. **Refusals**, with nothing written: `409 NODES_LEFT_WITHOUT_ENGINE` (the named nodes would lose
   their engine: give each its `behavior.provider` and `behavior.model` first, or set
   SY.architect's own `config.ai.model` to null), `409 JOIN_IN_PROGRESS`, `409 VERSION_CONFLICT`
   (read it again), `409 INSTALLATION_EDITED_BY_HAND` (restore the file, or add
   `"replace_hand_edit": true` to make the change over the hand edit), `409 INSTALLATION_UNCHANGED`,
   `409 INSTALLATION_NOT_WRITTEN` (the motherbee holds no file: fix its hive.yaml `ai` and restart
   `sy-orchestrator`, which writes version 1), `409 PROVIDER_KEYS_MISSING` (the named readers, each
   with its tenant, would find no key of the new default provider: put it in SY.vault first, or give
   the AI node its own provider), `422 INVALID_AI_SETTINGS`, `503 HIVES_NOT_CHECKED`, `503
   KEYS_NOT_CHECKED` (SY.vault did not list a tenant's keys) and `503 HIVE_REGISTRY_UNREADABLE`
   (SY.admin cannot read `/var/lib/fluxbee/hives`, so it names no hive to apply on).
7. **No answer to the write** (`504 TIMEOUT`) is not "nothing written": the write may still land.
   Read `GET /installation`: when `expected_version` moved past the one you sent, the version was
   written and applied on no hive, so update every hive as in step 4; when it did not, nothing was
   written.

## Node lifecycle

Managed nodes (`AI.*`, `IO.*`, `WF.*`) run as transient `systemd-run` units named
`fluxbee-node-<name>` with `Restart=always`, and their state lives in
`/var/lib/fluxbee/nodes/<KIND>/<node name>/config.json` (`_system` block written by the
orchestrator). SY nodes and the router are not managed this way and cannot be launched with
`run_node`.

| Action | Route | Notes |
| --- | --- | --- |
| launch | `POST /hives/{hive}/nodes {node_name, runtime, runtime_version, tenant_id, config}` | `runtime_version` defaults to `current`; registers the node's ILK (type `agent`) with SY.identity, writes `config.json`, starts the unit with `FLUXBEE_NODE_NAME`, `FLUXBEE_NODE_ILK_ID`, `FLUXBEE_NODE_TENANT_ID`; while systemd does not answer whether that unit runs, the launch is undone, its config and identity included (`SERVICE_FAILED`; FINDINGS A-271) |
| start / restart | `POST /hives/{hive}/nodes/{name}/start`, `.../restart` | need an existing config (`NODE_CONFIG_NOT_FOUND`); refuse a version whose files, its binary included, are not on the hive yet (`RUNTIME_NOT_PRESENT`), with nothing stopped; a launch that fails (`START_FAILED`, `RESTART_FAILED`) is tried again by the reconcile every 60 s until the node is stopped; a start of a node the router does not see while systemd does not answer whether it runs starts nothing (`SERVICE_FAILED`; FINDINGS A-271) |
| stop | `DELETE /hives/{hive}/nodes/{name}` with `{"purge_instance": false}` | there is no separate stop action; the node is stopped and not relaunched at boot; its config, ILK, secrets and timers stay; refused, with nothing stopped, while systemd does not answer whether it runs (`SERVICE_FAILED`) |
| remove | `DELETE /hives/{hive}/nodes/{name}` with `{"purge_instance": true}` | full teardown, below; refused like a stop while systemd does not answer (`SERVICE_FAILED`) |
| remove a stopped instance | `DELETE /hives/{hive}/nodes/{name}/instance` | refused while running (`NODE_INSTANCE_RUNNING`), and while systemd does not answer whether it runs (`SERVICE_FAILED`, nothing removed; FINDINGS A-271) |
| config, state, status | `GET`/`PUT /hives/{hive}/nodes/{name}/config`, `GET .../state`, `GET .../status` | for an SY node or the router, `status` reads its packaged unit (`sy-<name>`, `rt-gateway`) and the router's LSA, with `config` and `state` not managed; a core node this hive's role does not run answers `NODE_NOT_FOUND`. A status read writes nothing, for any node, and has no `status_version` since 0.1.133 (FINDINGS A-240): `observed_at` dates it. `lifecycle_state` is `RUNNING`, `STARTING` (its unit runs, the router does not see it yet), `STOPPED`, `FAILED` or `UNKNOWN`: systemd gave no full answer for its unit, whether it runs or, when it does not run, whether it failed (FINDINGS A-271, since 0.1.133; read it again, never take it for `STOPPED`), or a managed node whose unit does not run has no config or state left (every time) |

`run_node` refuses: `SY.*`/`RT.*` names; any node in the root tenant except the core IO runtimes
`io.cloud`, `io.blob`, `io.web` (`TENANT_ROOT_NOT_ALLOWED`, `genesis_refusal`); a core IO runtime
under any name but its own, `IO.cloud@motherbee`, `IO.blob@motherbee`, `IO.web@motherbee`, or one of
those names on another runtime (`CORE_IO_NAME_MISMATCH`), and a core IO node outside the root
tenant (`TENANT_ROOT_REQUIRED`; both `core_io_refusal`); a tenant that is not active; a `WF.*` node
whose runtime is not a workflow package (`WF_RUNTIME_PACKAGE_REQUIRED`). Start, restart and the
boot relaunch refuse the same root-tenant cases.

Each `SERVICE_FAILED` above that says systemd did not answer (asked again for up to a second)
carries `retryable: true` in the orchestrator's payload: nothing was done, and the same action
asked again is answered once systemd answers (FINDINGS A-271, since 0.1.133).

**Full teardown** (`kill_node` with `purge_instance`, plus `force` to send SIGKILL first). It is
never refused for the node's public pages (FINDINGS A-242): they stay up until they expire (24 h
after their publish by default, 30 days at most, never renewed), and SY.admin's ok answer names
the ones up in `payload.pages_left_up` (`{count, pages: [{publication_id, expires_at, ...}], more}`,
never a link's key). Take any of them down sooner with `POST /artifacts/unpublish
{"publication_id": ...}`; `GET /artifacts` lists every page that is up. A publish of the node under
way is not waited for: its page may still go up within 110 s. The same holds for removing a
stopped instance, `teardown_node` and `purge_ilk` of the node's ILK. In order:

1. stops the unit and clears `relaunch_on_boot`, so whatever fails after, the reconcile does not
   launch the node again;
2. `TIMER_PURGE_OWNER` to the hive's SY.timer, 3 s (a failure stops the purge there:
   `TIMER_PURGE_FAILED`, with `relaunch_on_boot: false`, and the node stays stopped with its files
   and identity until the purge runs again);
3. deletes the node directory, its UUID file and its identity-map entry;
4. one `teardown_node {node_name, ilk_id}` to `SY.admin@motherbee`, which checks that the node is
   on the caller's hive and the ILK is the node's, deletes the secrets dedicated to the ILK, each
   in its own tenant, then marks and purges the ILK, and reports each step (`ilk_deleted`,
   `ilk_delete`, `ilk_delete_error`, `vault_secrets_purged`; an error answer never reads `ok`;
   [SY.admin](../nodes/sy-admin.md#node-teardown)). The orchestrator waits 120 s for it (150 s for a
   spoke's purge); with no answer the outcome is `unknown` (`TEARDOWN_UNKNOWN`, no counts) and names
   the ILK to check, since SY.admin may still finish. The node's identity-map entry is already gone
   then, so a failed or unknown step is not finished by purging again: once the node is gone from
   the mesh, run `teardown_node` again with the reported `ilk_id`;
5. reports routes, VPNs and taps that still name the node (`routing_references`); it does not
   remove them.

Keeping nodes alive: every 60 s the orchestrator relaunches a managed node that should run
(`relaunch_on_boot`), whose unit is inactive and that the router does not see
(`MANAGED_RECONCILE_INTERVAL_SECS`); every 5 s its watchdog starts `rt-gateway` and any system
service that is not active (`watchdog_tick`).

Moving to a new runtime: the same 60 s pass relaunches a running node on what its runtime names
now, once that is ready on its hive: the version the pointer names, for a node that follows
`current`, and the start script that version runs, which for a package on a base runtime is its
base's `current` one, pinned or not. What a unit runs is read from systemd (its ExecStart, and the
version its launch wrote in its environment), so a node whose config only lagged gets its config
written, without a restart. Until the target is ready the node stays as it is, logged once per
target. A pinned node keeps its version until `set_node_config` rebinds it, which the pass then
applies (a package node launched before 0.1.88 takes it at its next launch). Every launch writes
the version into `_system.runtime_version` before it launches the node, so `status` and
`NODE_HELP` report the version the node runs
([SY.orchestrator](../nodes/sy-orchestrator.md#a-running-node-and-a-new-runtime-version)).

## Orchestrator boot

`bootstrap_local` then `finish_bootstrap` in `src/bin/sy_orchestrator.rs`:

1. reads the core manifest (fatal when missing or invalid); spokes re-render their units; the
   motherbee checks its SSH key and mesh TLS, rebuilds the per-role core trees (each binary
   checked against the manifest before it is copied) and deletes the identity keys of hives the
   registry does not have;
2. starts `rt-gateway` and waits up to 30 s for it (`wait_for_router_ready`);
3. the host firewall report, the ingress' SSH table (never fatal), egress NAT (fatal on an egress
   hive, as is a missing LAN leg, forwarding off or IPv6 not disabled), Syncthing; then the dist
   check: each core binary of the role in `dist/core/bin` against the manifest. One that does not
   match is a warning and `core.awaiting_update` in `/versions`, never fatal: the hive starts on
   what `/usr/bin` holds, and dist is checked again every minute until it matches (FINDINGS
   A-186). Syncthing starts the same way: while `dist/vendor` does not hold the Syncthing its
   manifest describes, the Syncthing already installed runs, reported as
   `vendor.awaiting_update` (FINDINGS A-189); `update category=vendor` and the motherbee's setup
   for an ingress join refuse such a vendor;
4. on a spoke, finishes the core update that installed it (`settle_core_update`): a node its
   role's list no longer names is stopped, disabled and removed, unit and binary, both kept in
   that update's generation; a node the list added is installed from dist. Then it starts the
   role's SY nodes in order and waits up to 60 s for its `wait_for` list
   (`LIFECYCLE_NODES_BOOTSTRAP_TIMEOUT_SECS`; a timeout only warns);
5. on the motherbee, waits for `sy-storage` (30 s) and for its database to answer
   `STORAGE_METRICS_GET` (30 s); failing either ends the orchestrator, which systemd restarts;
6. relaunches persisted nodes, moves the running ones that follow a moved `current`, and hands
   over the blob folders.

Until step 6 ends, admin actions get `ORCHESTRATOR_BOOTING` (HTTP 500 through
`error_code_to_http_status`); retry. On SIGTERM the orchestrator exits without stopping the hive.

## Factory reset

`sudo fluxbee-factory-reset` on the motherbee (`packaging/fluxbee-factory-reset`) is a dry run
that lists what it would remove; `--yes` executes after you type the `hive_id` (or pass
`--confirm-hive <id>`). It needs every hive alive and works through SY.admin: without SY.admin's
answer it stops at its start, when it lists the hives, before it changes anything.

1. takes down every published page that is up (FINDINGS A-212), first: pages are what users
   made, though no removal waits for one (FINDINGS A-242). SY.admin lists them (`GET /artifacts`,
   `list_published_artifacts`, each by its `publication_id`, never by its link's key) and the
   reset takes each down (`POST /artifacts/unpublish`): the edge stops serving its
   `/public/<key>` link, and IO.blob releases its copy in `blob/public/`. It lists again until
   none is left, so a page published meanwhile goes too. When SY.admin answers but does not list
   them (an older SY.admin without the route answers 404, or its ledger cannot be read), the reset
   goes on, counts it as a problem, and names the pages that stay public from SY.admin's ledger
   (`state/sy-admin/publications.json`, read only); they go down when they expire, 30 days at
   most. The dry run lists the pages;
2. deletes every workflow with `force` on motherbee and worker hives; a delete whose purge left
   versions of its package in the motherbee's dist is done, and the reset names each (`left in
   the motherbee's dist: ...`), which a re-run does not remove;
3. kills and purges every node not in the system set (`SY.*`, `RT.*` and the `boot: true`
   instances of `base-nodes.json`), on every hive, each once;
4. marks and purges every non-system ILK, deletes every vault secret except
   `storage_postgres_url`, `edge_channel_secret:*` and `edge_tls*`, marks and purges every tenant
   but the root;
5. clears the user OPA policy on every hive;
6. has every hive that runs SY.cognition drop its state through SY.admin's `cognition_clear`, the
   motherbee's included (FINDINGS A-160); truncates every table of `fluxbee_storage` with
   SY.storage and the frontdesk held down (masked for the boot, then started again); removes the
   nodes' conversation state; and clears Archi's sessions. The dry run lists the cognition hives,
   and each answer is printed;
7. takes down the pages published after step 1, once a node went in steps 2 to 4 (a workflow's
   WF node, a user node, or the ILK of a node that died on its own): a publish under way when its
   node went is not stopped, and its page goes up within SY.admin's 110 s for one publish. The
   reset waits until 120 s after its last removal, then takes down every page that is up, as in
   step 1 (`take_down_late_pages`); a page it cannot take down is a counted problem. With no node
   removed, it waits for nothing.

It keeps the installation, `/etc/fluxbee`, the vault master key, TLS, the spokes (with their SSH
and NAT tables, which their orchestrators keep applying), routes, VPNs, taps and the history,
SY.admin's publication ledger included: its rows say how each page went down, for 30 days after
it did (FINDINGS A-231). It never touches the blob store: the blobs the nodes wrote stay in
`blob/active/` and `blob/staging/`; only the pages' copies in `blob/public/` go with their pages
(IO.blob removes a copy once no page shares it). Re-running is safe; a summary line goes to
`/var/lib/fluxbee/state/factory-reset.log`, with `packages_left=<n>` beside `problems=<n>`.

## Certificates (ACME)

The public edge's certificate comes from an ACME certificate authority, Let's Encrypt unless
configured otherwise, validated with TLS-ALPN-01 on the :443 the edge already opens. SY.admin on
the motherbee orders and renews it, and the edge serves it from SY.vault and reloads it without a
restart ([SY.admin](../nodes/sy-admin.md#the-acme-client), [SY.edge](../nodes/sy-edge.md); design:
[docs/acme-edge-certificates-v1.md](../acme-edge-certificates-v1.md)). The certified name is the
host of `admin.public_base_url`; the CA comes from the `acme:` section. Nothing in the code names
a domain or a CA.

**Set up a fresh installation:**

1. **DNS.** An A record for the name to the ingress' public address, and an AAAA record if the
   ingress has a public IPv6 address. A record that points elsewhere makes the CA validate the
   wrong host.
2. **Port 443/tcp** open on the ingress to the whole internet: the CA validates from several
   places it does not publish. Port 80 is not used.
3. **The ingress' secret name.** Its `edge.tls_vault_key` must start with `edge_tls` (new
   installations use `edge_tls`); SY.admin writes no other secret.
4. **The motherbee's hive.yaml:**

   ```yaml
   admin:
     public_base_url: "https://hive-k3m9x7q2.example.com"   # port 443, a DNS name with a dot
     public_edge_node: "SY.edge@ingress1"   # needed when more than one ingress is connected
   acme:
     terms_agreed: true                     # you accept the CA's terms
     # optional: directory_url, staging_directory_url, profile, contact, trust_anchor
   ```

5. **Restart SY.admin** when it is idle (`sudo systemctl restart sy-admin`): a restart cuts
   executor plans and relayed Cloud commands in flight.
6. **Read `GET /acme/status`.** `config.state` must be `on`; when it is `off`, `config.reason`
   says why (the same line is in `journalctl -u sy-admin`). `config.names` are the certified
   names, `terms_url` is the terms you agreed to, and `edge.live` is what the edge serves.
7. **Optionally, `POST /acme/test`** (it creates the staging account and arms the edge, so confirm
   it first): a staging order with the real names, issued and checked but never installed. Its
   result is `last_test` in the status; a staging root is reported as untrusted, as expected.
8. The first check runs 2 min after SY.admin starts. An edge with no certificate gets one ordered
   then; `last_attempt` and `issued` in the status show the result, and `renewal.renew_at` when
   the next renewal is due. A failure shows in `last_attempt` and backs off 1 h, 2 h, 4 h and so
   on, up to 24 h.

**Move a hand-seeded certificate to ACME.** The check never replaces a certificate it did not
issue: it alerts (`foreign_certificate` once the leaf passes two thirds of its lifetime,
`name_not_covered` when it does not cover the name) and leaves it. `POST /acme/renew`, confirmed,
orders now: the new certificate goes into the edge's secret, the hand-seeded one stays in the
vault as the previous version, and the edge reloads. From then on the check renews it by itself.
The same call certifies a new name after `public_base_url` changed and SY.admin restarted.

**Add a web host to the certificate** (Archi's, [io-web-v2](../io-web-v2.md) §11.1). One
certificate carries the main name and every host `web:` declares, each a label before the main
name:

1. **DNS.** A record for `archi.<main name>` pointing at the ingress, like the main name's (a CNAME
   of it works), made by hand.
2. **The motherbee's hive.yaml:** `web: {archi: true}`. A bare `web:` declares nothing.
3. **Restart SY.admin** when it is idle. Its log names the declared host, and the edge gets the
   host's route within a minute. The served certificate does not cover the host yet: the check
   raises `name_not_covered` (warning) and orders nothing, since the leaf is not due and the cap
   holds automatic orders.
4. **`POST /acme/test`**, confirmed: the staging order carries every name, and `last_test` says
   which one the CA did not validate, if any.
5. **`POST /acme/renew`**, confirmed (forced, past the cap). Then `config.names` lists every name,
   `edge.names_missing` is empty, and `name_not_covered` closes.

When the CA validates the main name but not the added one, an order goes on with the main name
alone only when the edge would lose nothing it serves, or cannot wait: no certificate for the
main name, one that already lacks the added name, or one in the last sixth of its lifetime;
`last_attempt.detail` says why. Otherwise the attempt fails (`last_attempt.failed_names` names
the host) and the backoff retries every name, so a certificate for both names is kept; 48 h past
its renewal point, after such a failure, the main name is renewed alone. A `POST /acme/renew`
while the main name is served and not due just fails: fix the host's DNS and renew again.
Removing `web:` (and restarting) takes the host's route off the edge within a minute and its name
off the certificate at the next renewal.

**Roll back.** `POST /hives/motherbee/vault/secrets/<the edge's secret>/rollback` puts the previous
version back, then `POST /acme/reload` makes the edge serve it. The check then sees a hand-seeded
leaf and leaves it. Roll the vault back before downgrading to a version that cannot renew. The
vault keeps one previous version, so after two renewals the hand-seeded certificate is no longer
there: keep its files. A downgrade below 0.1.84 cannot read the `acme.json` 0.1.84 writes: it sets
the file aside and judges the served leaf hand-seeded until a `POST /acme/renew`.

**When the edge refuses a certificate SY.admin issued**, SY.admin rolls the secret back at once,
raises `install_refused` (critical) and orders nothing until you act: find the cause in
`install_refused.detail` and the edge's journal, then `POST /acme/renew`. A refused first
certificate has no previous version to go back to: delete the secret
(`DELETE /hives/motherbee/vault/secrets/<the edge's secret>`) before renewing. A refusal because
the edge could not read SY.vault (`transient`) is not a refusal: the certificate stays and the
next check reloads it.

**"The vault holds a version the edge does not serve."** SY.admin writes the edge's secret only
over the write the edge serves (version and `updated_at`), after one reload. When a renewal stops
with this, the vault holds something the edge refused or never loaded: `POST /acme/reload` to see
why the edge keeps its certificate, then roll the secret back (or delete one the edge never
served) and renew.

**An edge that answers `status: error`** (no public listener, a `disk` or `plaintext` edge asked
to reload) is a configuration error, shown in `edge.config_error`: nothing is ordered and no
`edge_unreachable` is raised. `acme.trust_anchor`, when set, is trusted for every connection to
the CA as well as for the issued chain.

**Alerts.** The certificate conditions (`renewal_failed`, `renewal_overdue`, `expiring`,
`name_not_covered`, `foreign_certificate`, `install_refused`, `edge_without_certificate`,
`edge_unreachable`) are in `GET /acme/status` while open, and in `GET /drift-alerts` with
`category: certificate`: one row when a condition opens and an `info` row when it closes. The
expiry conditions run without `acme:` too.

**When the motherbee is down,** nothing renews and nothing alerts. An external monitor of the
public name's certificate expiry, run from outside the installation, covers that.

## The web door

The public edge serves each host `web:` declares only through io.web
([io-web-v2](../io-web-v2.md) §11): the edge on the ingress takes the request and proxies it to
`IO.web@motherbee`. `GET /web/status` on the motherbee shows the four pieces of the door, each a
state, never an error ([SY.admin](../nodes/sy-admin.md#the-web-doors-status)):

| Piece | Ready | Otherwise |
| --- | --- | --- |
| `web`: the host | `state: declared`, the host in `hosts` | `absent`: add `web: {archi: true}` to the motherbee's hive.yaml and restart SY.admin; `invalid`: `reason` says why |
| `certificate` | `hosts_missing: []` | `POST /acme/test`, then `POST /acme/renew` ("Add a web host to the certificate" in [Certificates](#certificates-acme)). `served` is the leaf at the ACME client's last look; `GET /acme/status` asks the edge afresh |
| `routes`: the edge's answer to the last push | `state: acknowledged`, `acknowledged_at` within the last minutes (SY.admin pushes every 60 s), and `upstream.state: ready`, the edge's way to io.web | `no_edge`: no `admin.public_edge_node` and not exactly one connected ingress (`detail` says which); `no_answer`: the edge is down or older than 0.1.84; `refused`: the edge's journal says why; `upstream.state: unavailable`: the edge has no way to io.web and answers every web host 502, `upstream.reason` says what is missing on the ingress (its mesh TLS material, `hive_id`, `wan.uplinks`), then restart `sy-edge` there. `upstream.last_failure` is the last request io.web did not answer (`at`, `kind`); the edge's journal says why (`io.web did not answer proxied requests`) |
| `io_web` | `state: running`, `answer.health_state: HEALTHY` | `absent`: io.web starts only by an explicit launch, once ([Node lifecycle](#node-lifecycle)): `POST /hives/motherbee/nodes` with `node_name: IO.web@motherbee`, `runtime: io.web` and the root tenant as `tenant_id`; `stopped`: `start_node`; `failed` or `silent`: `journalctl -u fluxbee-node-IO.web-motherbee`, then `restart_node`; `unknown`: the orchestrator did not answer (`view_error`), or its `lifecycle_state` is `UNKNOWN` (systemd did not answer for io.web's unit), retry |

With all four ready, the edge's upstream included, `https://archi.<main name>/` answers io.web's
page from outside, with a valid certificate. A recent `routes.upstream.last_failure` with
everything else ready means io.web refused the edge or did not answer it: the edge's journal on the
ingress and io.web's on the motherbee say why.

## Health checks

| Check | What it tells |
| --- | --- |
| `GET /health` | SY.admin answers; nothing else |
| `GET /hives` | the join records in `hives/*/info.yaml`; `status: ok` only means the motherbee's orchestrator answered |
| `GET /hives/{hive}` | one hive's join record (`status`, `join`) |
| `GET /inventory`, `/inventory/summary`, `/inventory/{hive}` | the live view from the router's LSA; answers while the orchestrator boots |
| `GET /hives/{hive}/nodes`, `.../nodes/{name}/status` | nodes and one node's lifecycle and health |
| `GET /hives/{hive}/versions`, `GET /versions` | core, vendor and runtimes per hive; `core.awaiting_update` names the core binaries a hive's dist does not hold as its manifest says, while it runs what `/usr/bin` holds, and `vendor.awaiting_update` the vendored Syncthing, while it runs the one already installed; `config` the hive's installation file, with its `state` against the motherbee's (`current`, `behind`, `pending`, `edited_by_hand`) and `applied`, the config update it applied last |
| `GET /installation` | the installation's `ai`: the motherbee's file, its versions and each hive's copy ([The installation's AI settings](#the-installations-ai-settings)) |
| `GET /hives/{hive}/opa/status` | user policy in sync, routers loaded |
| `GET /hives/{hive}/streams` | the router streams' backlog and sinks |
| `GET /drift-alerts`, `GET /hives/{hive}/drift-alerts` | the history the motherbee's orchestrator records (`category` `core`, `egress`, `posture`, `syncthing`, about the motherbee itself, apart from a spoke's `core` `post_sync_mismatch`; `syncthing` `api_unanswered` and `api_unanswered_restart` when Syncthing's REST API gives no answer and when the watchdog restarts Syncthing for it, FINDINGS A-283), and `certificate`: SY.admin's certificate conditions (`record_drift_alert`), a row with `state: open` when one opens and an `info` row with `state: closed` when it closes. A spoke's own checks stay in the spoke's file until the system's log (FINDINGS A-175), and the answer's `note` says so |
| `GET /acme/status` | the edge's certificate: ACME's configuration, the names, the open conditions ([Certificates](#certificates-acme)) |
| `GET /web/status` | the web door: the declared host, the certificate's names, the edge's answer to the web routes with its way to io.web, io.web ([The web door](#the-web-door)) |
| `GET /config/storage/metrics` | SY.storage's inbox |
| `systemctl --failed`, `journalctl -u <unit>` | per host |

`lab/ops.py health [--since HH:MM]` runs on every host and passes only when all of `failed`,
`denials`, `srcdrops`, `streamlost`, `usagelost`, `usagerej` and `wandrops` are 0 (`wandrops`
since every hive runs 0.1.135; what each counts:
[rt-gateway](../nodes/rt-gateway.md#what-it-logs-that-labopspy-health-counts)). Two lines from
SY.admin follow:

```text
edge-cert  acme=on names=hive-k3m9x7q2.fluxbee.ai,archi.hive-k3m9x7q2.fluxbee.ai conditions=none
web        host=archi.hive-k3m9x7q2.fluxbee.ai route=acknowledged upstream=ready io.web=running
```

- `edge-cert` (`GET /acme/status`): a `critical` condition fails the check.
- `web` (`GET /web/status`): the declared host (`none` without `web:`, `invalid` for a section
  that cannot work), the edge's answer to the routes (`routes.state`), the edge's way to io.web
  (`routes.upstream.state`: `ready`, `unavailable`, or `unknown` when the last push was not
  acknowledged), with the kind and time of its last failure when there is one
  (`upstream=ready(head@2026-10-23T14:02:11Z)`), and io.web's state, with its health when it
  reports one other than `HEALTHY` (`io.web=running(DEGRADED)`). It never fails the check: io.web
  runs only once the operator starts it, and the edge answers again within a minute of a restart.

`lab/ops.py versions` and `lab/ops.py opa-status` print the version and policy view per hive.

### Long lists and the mesh frame

Over HTTP on the motherbee an answer has no size limit. Over the mesh (Archi, IO.cloud, any
node's `ADMIN_COMMAND`) an answer is one frame, at most 128 KiB (FINDINGS A-198):

- **A list that grows with the installation answers in pages** when asked to: `/inventory`,
  `/inventory/{hive}`, `/hives/{hive}/status`, `/hives/{hive}/nodes`,
  `/hives/{hive}/identity/tenants/{tenant_id}/nodes`, `/versions` (a page of hives),
  `/hives/{hive}/identity/ilks`, `/hives/{hive}/identity/tenants`, `/hives/{hive}/commands`,
  `/hives/{hive}/vault/secrets` and `/hives/{hive}/timer/timers` take `?limit=&cursor=` (an empty
  `cursor` is none). The answer has `next_cursor` while rows follow (pass it back as `cursor`) and
  `total`. Without `limit` every row comes, as before, but for three lists that never gave every
  row: `/hives/{hive}/vault/secrets` gives its `limit`, 200 by default, `/hives/{hive}/commands` 50
  by default, and `/hives/{hive}/timer/timers` 100 timers by default (at most 1000), fewer when
  one frame fills first. A cursor the list did not give is refused, `INVALID_CURSOR` (400). The
  timer list is SY.timer's page as SY.timer answers it ([SY.timer §7.6](../sy-timer.md)): it has
  no `total`, a cursor that is not one is refused `TIMER_INVALID_PAYLOAD` (400), and a timer whose
  fire time moves between two pages (a recurring timer's fire) can come twice or not at all; since
  0.1.133 a page its `limit` cuts has `next_cursor` too (FINDINGS A-266). The installation's
  versions in `/installation` and `/hives/motherbee/versions` are the newest 20 (`versions_total`;
  `?versions_before=<versions_next_before>` lists older ones).
- **A list is no snapshot.** SY.admin gathers a long list from the node behind it page by page,
  within the time the action gives one request: a row written or removed meanwhile may be in it or
  not, though never twice, and a list not gathered in that time answers `TIMEOUT` (read it in
  pages).
- **An answer past the frame is never lost**: the caller gets `REPLY_TOO_LARGE` at once, with
  the answer's size, how to page it and how many rows fit (`error_detail.suggested_limit`), or,
  for a change, that it ran and its outcome. The node that sent it logs `a mesh reply does not
  fit one frame: REPLY_TOO_LARGE sent in its place` (SY.admin), or the same of an identity,
  vault or orchestrator answer, with the action, the caller and the size. An executor plan's
  answer is cut only as far as the frame needs: the largest step outputs first
  (`output_truncated`), then every output, each step keeping its status and ids, then each step's
  last event; past all of it `REPLY_TOO_LARGE` says the plan ran and keeps its `execution_id` and
  `log_path`. The run's full log is on the motherbee, and each step's outcome is in the command
  log (`/hives/motherbee/commands`, origin `executor`)
  ([SY.admin](../nodes/sy-admin.md#replies-that-do-not-fit-a-frame)).

## Links

- [docs/host-posture-and-exposure-spec-v1.md](../host-posture-and-exposure-spec-v1.md): ports,
  exposure and what is still open.
- `packaging/base-nodes.json`: the base node set (which runtimes ship, which boot).
- `lab/ops.py`: the operator's routine on the lab.
