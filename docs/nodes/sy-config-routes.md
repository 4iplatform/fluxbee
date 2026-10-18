# SY.config.routes

SY.config.routes owns a hive's routing configuration: static routes, VPN assignments and
router taps. It keeps them in a YAML file and writes them into the hive's config shared-memory
region, which the routers of that hive read. It is the first system node every hive starts.

Source: `src/bin/sy_config_routes.rs`. Region writer: `ConfigRegionWriter` in
`src/shm/mod.rs`. Reader: `refresh_config` in `src/router/mod.rs`.

| | |
| --- | --- |
| Binary | `/usr/bin/sy-config-routes` (`packaging/build-deb.sh`, `RUST_BINS` entry `sy-config-routes:sy_config_routes`) |
| Node name | `SY.config.routes@<hive_id>` |
| Role | every role: motherbee, worker, ingress, egress |
| systemd unit | `sy-config-routes.service` (`Restart=always`, `RestartSec=5`) |
| Listens on | the local router socket only; no TCP port |
| Writes | POSIX shared memory `/jsr-config-<hive_id>` |
| State | `/etc/fluxbee/sy-config-routes.yaml` |

## Where it runs and why it starts first

Every role's `system_nodes.<role>.nodes` list in `/etc/fluxbee/hive.yaml` starts with
`SY.config.routes`, and `validate_system_nodes` in `src/bin/sy_orchestrator.rs` refuses a list
that does not ("writer of the routing SHM; required by every other SY service"). The orchestrator
starts `rt-gateway` first, then the list in order (`bootstrap_local`). The node is not gated by
role in its unit or in the binary.

## How it starts

`main` in `src/bin/sy_config_routes.rs`:

1. Exits on non-Linux targets. Log level from `JSR_LOG_LEVEL` (default `info`).
2. Reads `hive_id` from `/etc/fluxbee/hive.yaml` and computes its own ILK deterministically
   (`deterministic_system_ilk_id("SY.config.routes@<hive>")`); it does not wait for identity.
3. Loads `/etc/fluxbee/sy-config-routes.yaml` (`load_config`). A missing file is created with
   `version: 1` and empty `routes`, `vpns` and `taps`.
4. Opens or creates the region `/jsr-config-<hive_id>`, owned by a UUID kept in
   `/var/lib/fluxbee/state/nodes/SY.config.routes.uuid` (`load_or_create_uuid`).
5. Writes the whole configuration into the region (`apply_config`).
6. Connects to the router (`RouterDispatcher::connect_with_retry`, retry every 1 s). If
   `JSR_ROUTER_NAME` is set, it uses that router's socket, found through
   `/var/lib/fluxbee/state/<router>/identity.yaml`.
7. Loops: every 5 s it refreshes the region heartbeat (`update_heartbeat`); otherwise it serves
   messages.

## The configuration

`SyConfigFile` (`version`, `updated_at`, `routes`, `vpns`, `taps`):

| Section | Fields | Notes |
| --- | --- | --- |
| route (`RouteConfig`) | `prefix`, `match_kind` (`EXACT`, `PREFIX`, `GLOB`; default `PREFIX`), `action` (`FORWARD` or `DROP`), `next_hop_hive`, `metric` (default 0), `priority` (default 100) | natural key: `prefix` |
| VPN (`VpnConfig`) | `pattern`, `match_kind` (default `PREFIX`), `vpn_id`, `priority` (default 100) | natural key: `pattern` |
| tap (`TapConfig`) | `match_src`, `match_dst`, `target`, `mode` (only `best_effort`), `enabled` (default `true`) | natural key: the triple `(match_src, match_dst, target)` |

Every change bumps `version` and sets `updated_at` (epoch ms), rewrites the region and then the
YAML file (`apply_config`, `write_config`).

### What goes into the region

`apply_config` writes three tables, each replaced whole under the region's seqlock:

- the static routes, plus two frozen markers added on every write
  (`frozen_system_authority_routes`): `SY.orchestrator@*` (glob) and `SY.admin@<hive>` (exact),
  flagged `FLAG_ACTIVE | FLAG_FROZEN`. They show, for inspection only, which origins the
  router's system policy authorizes. The router skips frozen entries when it builds its
  forwarding table (`refresh_config`), and they are not in the YAML file nor in `list_routes`;
- the VPN assignments;
- the taps. This last write bumps the region's `config_version`, which is what the routers
  watch.

Limits (`src/shm/mod.rs`): 256 route slots (`MAX_STATIC_ROUTES`, two of them used by the frozen
markers), 256 VPN assignments (`MAX_VPN_ASSIGNMENTS`), 256 taps (`MAX_TAP_ENTRIES`). A write over
a limit fails with `ValueTooLong`. Prefixes, patterns and tap names are stored in 256-byte
fields; `next_hop_hive` in 32 bytes (`empty_static_route`, `empty_tap_entry`).

### How the routers use it

- A router re-reads the region on each frame from one of its local nodes and on its heartbeat
  tick (5 s, `DEFAULT_HEARTBEAT_INTERVAL_MS` in `src/config/mod.rs`); the re-read is a no-op
  unless `config_version` changed (`refresh_config`).
- On a change it reloads routes (sorted by `priority`, then `metric`), VPN rules and taps,
  reassigns VPNs and rebuilds its forwarding table (`reassign_vpns`, `rebuild_fib`).
- It advertises its routes, VPNs and enabled taps to WAN peers in its LSA
  (`build_local_lsa_payload`).
- If the region's heartbeat is older than 30 s (`HEARTBEAT_STALE_MS`) the router reopens the
  region; it keeps using the last tables written (`read_config_snapshot_for_routing`). The
  region outlives the process, so routing continues while SY.config.routes restarts.

Routing semantics (what a route or a tap does to a message) belong to the router; see
[rt-gateway](rt-gateway.md).

## Messages

### Admin actions (`msg_type: "admin"`, action in `meta.action`)

Handled in `handle_admin_action`. Answers are `admin` messages with the same `meta.action` and
`{status, action, payload}` or `{status: "error", action, error_code, error_detail}`.

| Action | Payload | Result / errors |
| --- | --- | --- |
| `list_routes` | none | `{config_version, routes}` |
| `add_route` | a `RouteConfig` | `{config_version, route}`; `DUPLICATE_PREFIX` |
| `delete_route` | `{prefix}` | `{config_version}`; `NOT_FOUND` |
| `list_vpns` | none | `{config_version, vpns}` |
| `add_vpn` | a `VpnConfig` | `{config_version, vpn}`; `DUPLICATE_PATTERN` |
| `delete_vpn` | `{pattern}` | `{config_version}`; `NOT_FOUND` |
| `list_taps` | none | `{config_version, taps}` |
| `add_tap` | a `TapConfig` | `{config_version, tap}`; `DUPLICATE_TAP` |
| `delete_tap` | `{match_src, match_dst, target}` | `{config_version}`; `NOT_FOUND` |
| anything else | | `UNKNOWN_ACTION` |

Who may send them: the six mutations (`admin_action_mutates`) are accepted only when the
router-stamped `routing.src_l2_name` is `SY.admin@motherbee` (`is_primary_admin_origin`, using
`PRIMARY_HIVE_ID`); any other origin gets `FORBIDDEN` and a warning
`blocked admin action from unauthorized origin`. The three lists are not gated.

SY.admin addresses these actions to `SY.config.routes@<hive in the URL>`
(`build_admin_request` in `src/bin/sy_admin.rs`), so one admin manages every hive's routes.

### Node config (`SYSTEM` messages)

- `CONFIG_GET` → the effective config `{routes, vpns, taps, updated_at, hive}`, `state`
  (`empty` or `configured`), `config_version` and a `contract` block (`config_schema:
  sy_config_routes_v2`, apply mode `replace` only) (`build_node_config_get_payload`).
- `CONFIG_SET` replaces the sections it names (`routes`, `vpns`, `taps`; at least one), with
  `apply_mode: replace` only (`apply_node_config_set_request`). The new `version` is the
  request's `config_version`, or the current one plus 1 when the request sends 0. Errors come
  back as `INVALID_CONFIG_SET`.
- The node does not check the origin of `CONFIG_GET`/`CONFIG_SET` itself. Both are protected
  system actions (`PROTECTED_SYSTEM_ACTIONS` in `src/router/system_policy.rs`): the router
  delivers them only from `SY.admin@motherbee` (rule 2 of `policy/system.rego`), and drops
  others with `router dropped protected SYSTEM action from unauthorized origin`
  (`prepare_local_delivery` in `src/router/mod.rs`).
- `NODE_STATUS_GET` is answered by the SDK default handler (`try_handle_default_node_status`).

## SY.admin HTTP routes

On the motherbee, `127.0.0.1:8080` (`src/bin/sy_admin.rs`):

| Route | Action |
| --- | --- |
| `GET /hives/{hive}/routes` | `list_routes` |
| `POST /hives/{hive}/routes` body `RouteConfig` | `add_route` |
| `DELETE /hives/{hive}/routes/{prefix}` | `delete_route` |
| `GET /hives/{hive}/vpns` | `list_vpns` |
| `POST /hives/{hive}/vpns` body `VpnConfig` | `add_vpn` |
| `DELETE /hives/{hive}/vpns/{pattern}` | `delete_vpn` |
| `GET /hives/{hive}/taps` | `list_taps` |
| `POST /hives/{hive}/taps` body `TapConfig` | `add_tap` |
| `DELETE /hives/{hive}/taps?match_src=&match_dst=&target=` | `delete_tap` |

`CONFIG_GET`/`CONFIG_SET` go through the admin's generic node-control routes:
`POST /hives/{hive}/nodes/SY.config.routes@{hive}/control/config-get` and `.../control/config-set`
(actions `node_control_config_get`, `node_control_config_set`).

Example, a route that sends every name starting with `support.` to `worker1`:

```bash
curl -s -X POST 127.0.0.1:8080/hives/motherbee/routes \
  -H 'content-type: application/json' \
  -d '{"prefix":"support.","action":"FORWARD","next_hop_hive":"worker1"}'
```

## Interaction with node teardown

When the orchestrator purges a node, its reply lists the routes, VPNs and taps of its hive that
still name that node (`routing_references`, from `enumerate_routing_references_to` in
`src/bin/sy_orchestrator.rs`). It reports them; it does not delete them.
`fluxbee-factory-reset` also leaves routes, VPNs and taps alone (its docstring, "KEPT").

## Failure behavior

- A YAML or region error at startup ends the process; systemd restarts it every 5 s.
- If the router channel closes, the main loop returns and the process exits.
- An error inside an admin action or `CONFIG_SET` (bad payload, a table over its limit) is
  logged as `admin action error: ...` or `node config set error: ...`. For admin actions the
  handler stops before answering, so the caller sees a timeout.
- While the process is down the routers keep the last tables written to the region.

## Health

- `systemctl status sy-config-routes`; `journalctl -u sy-config-routes` shows
  `config region written` (with the counts), one `vpn rule loaded` / `tap rule loaded` per
  entry, and `connected to router`.
- `curl -s 127.0.0.1:8080/hives/<hive>/routes` answers from the node itself.
- The router logs `config snapshot updated` with `config_version` and the counts after each
  change it picks up (`refresh_config`).
- `ls /dev/shm/jsr-config-<hive_id>` on the hive shows the region.

## Links

- [rt-gateway](rt-gateway.md): how routes, VPNs and taps act on messages.
- [SY.opa.rules](sy-opa-rules.md): the other per-hive input of the router (user policy).
- [Operations](../reference/operations.md).
