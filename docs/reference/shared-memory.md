# Shared memory: reference

**Status:** the code on 2026-10-17 (0.1.79).
**Audience:** router and node developers who read or write a region, or change a layout.

This page describes the shared-memory regions as the code builds them: names, writers, readers,
layouts, the seqlock, and what happens on restart. Every statement names the code that backs it.
`docs/history/03-shm.md` and `docs/history/06-regiones.md` (Spanish) are older and partly wrong (versions, sizes,
fields); where they disagree with this page, the code and this page win.

Main sources: `src/shm/mod.rs` (layouts, writers, readers), `src/router/mod.rs` (how the router
uses them), `go/sy-opa-rules/main.go` and `distribution.go` (the OPA writer),
`crates/fluxbee_sdk/src/identity.rs` and `go/fluxbee-go-sdk/identity_shm.go` (identity readers),
`src/bin/sy_identity.rs`, `src/bin/sy_cognition.rs`, `src/bin/sy_config_routes.rs` (writers). How
the router uses routes, VPNs and taps is in [routing-and-policy.md](routing-and-policy.md).

---

## 1. The regions

| Region | POSIX name | One per | Writer | Readers |
| --- | --- | --- | --- | --- |
| Router | `/jsr-<router uuid>` | router | that router | sibling routers; SY.orchestrator; SY.opa.rules |
| Config | `/jsr-config-<hive>` | hive | SY.config.routes | the hive's routers; SY.orchestrator |
| LSA | `/jsr-lsa-<hive>` | hive | the hive's gateway router | the hive's routers; SY.admin; SY.orchestrator |
| OPA | `/jsr-opa-<hive>` | hive | SY.opa.rules (Go) | the hive's routers |
| Identity | `/jsr-identity-<hive>` | hive with SY.identity | SY.identity | the router; nodes through the Rust and Go SDKs |
| Memory | `/jsr-memory-<hive>` | hive with SY.cognition | SY.cognition | the hive's routers; SY.cognition |

The shipped `config/hive.yaml` lists SY.identity and SY.cognition for the motherbee and worker
roles only (`system_nodes`), so ingress and egress hives have no identity and no memory region.

| Region | Magic | Version | Size |
| --- | --- | --- | --- |
| Router | `0x4A535352` "JSSR" (`SHM_MAGIC`) | `SHM_VERSION` 2 | 336,128 B |
| Config | `0x4A534343` "JSCC" (`CONFIG_MAGIC`) | `CONFIG_VERSION` 2 | 370,944 B |
| LSA | `0x4A534C41` "JSLA" (`LSA_MAGIC`) | `LSA_VERSION` 3 | 672,000 B |
| OPA | `0x4A534F50` "JSOP" (`OPA_MAGIC`) | `OPA_VERSION` 1 | 4,194,496 B (192 + 4 MiB) |
| Identity | `0x4A534944` "JSID" (`IDENTITY_MAGIC`) | `IDENTITY_VERSION` 6 | by limits; 52,461,760 B with SY.identity's defaults |
| Memory | `0x4A534D4D` "JSMM" (`MEMORY_MAGIC`) | `MEMORY_VERSION` 1 | 4,194,496 B (192 + 4 MiB) |

Sizes follow from the `#[repr(C)]` structs and the `layout_*` functions, which start every section
on a 64-byte boundary (`REGION_ALIGNMENT`). The identity struct sizes are asserted in code
(`const _: () = { assert!(size_of::<…>() == …) }`).

---

## 2. Mechanics common to every region

### 2.1 Names and permissions

- POSIX shared memory (`shm_open`), visible as `/dev/shm/jsr-…`.
- A name has at most 64 bytes (`SHM_NAME_LIMIT`, `validate_name`): `/jsr-identity-` leaves 50
  bytes for the hive id, `/jsr-config-` and `/jsr-memory-` 52.
- The router region's name is `/jsr-` plus the router UUID in simple form, 32 hex digits without
  dashes (`build_shm_name`). It would be cut to 16 digits only past 64 bytes, which this prefix
  never reaches. The name is kept in `/var/lib/fluxbee/state/<router L2 name>/identity.yaml`
  (`shm.name`), so it survives restarts, and travels in the IRP HELLO (`shm_name`).
- Regions are created with mode 0600 (`open_or_create_region`; Go `opaPrivateFileMode`): a reader
  must run as the writer's user. The Rust SDK logs once when the identity region exists but cannot
  be opened (`is_permission_lookup_unavailable`).

### 2.2 Create, reuse, validate

A Rust writer opens its region with `open_or_create_region(name, len, init)`:

- a region of at least `len` bytes with a known magic and that magic's current version is
  **reused as it is**: `init` does not run, so the header keeps the values written when the region
  was created (owner UUID and PID, hive, limits);
- a smaller one, or one with a wrong magic or version, is unlinked and created again;
- a new one is sized with `ftruncate`, zeroed and given its header.

Then `normalize_seq_header` turns an odd `seq` left by a writer that died mid-write into the next
even value and logs `recovered stale odd seqlock state on open`. SY.opa.rules does the same in Go
(`openOrCreateOpaRegion`: reuse when magic and version match, `initHeader` only on creation).

Readers map read-only and refuse a region whose magic or version differs (`is_region_valid`,
`is_opa_region_valid`). A layout change therefore means a version bump: an old reader refuses the
new region and a new reader the old one (comment on `CONFIG_VERSION`).

### 2.3 The seqlock

Each region has one writing process and a `seq: AtomicU64` in its header; odd means a write is in
progress.

- **Writer.** `SeqlockWriteGuard` calls `seqlock_begin_write` (`fetch_add(1, Relaxed)`) when made
  and `seqlock_end_write` (`fence(Release)`, then `fetch_add(1, Relaxed)`) when dropped. Each writer
  method (`register_node`, `write_static_routes`, `update_heartbeat`, …) is one write. Go:
  `atomic.AddUint64(seq, 1)` before and after (`writeHeader`, `updateHeartbeat`, `writeStatus`).
- **Reader.** Load `seq` (Acquire); if odd, spin; `fence(Acquire)`; copy out what it needs;
  `fence(Acquire)`; load `seq` again; accept if unchanged, else retry (`read_router_snapshot` and
  the other `read_*` functions).
- **Timeout.** A reader gives up after `SEQLOCK_READ_TIMEOUT_MS` = 5 ms: the snapshot readers return
  `None` or `ShmError::SeqLockTimeout`. The Rust SDK identity reader also waits 5 ms; the Go one
  50 ms (`identitySeqlockTimeoutMs`); SY.opa.rules reads a router header with up to 100 tries 1 ms
  apart (`readRouterStatus`).
- **`seq` is not a content counter.** Every heartbeat is a write, so `seq` moves at least every 5 s.
  For content use `config_version` (config region) or `policy_version` and `wasm_hash` (OPA
  region). SY.cognition keys its agent-ILK cache on the identity `seq` (`agent_identity_of`), so
  that cache lasts at most one heartbeat.

### 2.4 Heartbeat and staleness

- Every writer refreshes `heartbeat` (epoch ms) every 5 s: the router (`heartbeat_interval_ms`),
  SY.config.routes, SY.identity, SY.cognition and SY.opa.rules (`heartbeatLoop`). The gateway
  refreshes the LSA heartbeat with each full write, at least every 10 s (`lsa_broadcast_loop`).
- Stale means older than `HEARTBEAT_STALE_MS` = 30 s. What readers do with a stale region:
  - another router's region: ignored (`discover_peer_regions`);
  - config, LSA, OPA and memory regions, in the router: the reader opens the name again, to find a
    region recreated under it, and keeps using what it reads; a stale region is not discarded
    (`read_config_snapshot_for_routing`, `read_lsa_snapshot_for_routing`,
    `read_opa_header_for_reload`, `read_memory_snapshot_for_delivery`);
  - identity: no check; every read opens the region afresh (`open_read_only_auto`, SDK
    `open_identity_shm_reader`).
- So after a writer recreates a region, a router can go on reading the old, unlinked mapping until
  its heartbeat is 30 s old (tests `*_reader_reopens_after_shm_recreation_when_old_reader_is_stale`
  in `src/router/mod.rs`).

---

## 3. Router region `/jsr-<router uuid>`

`ShmHeader` (248 B) at 0; `NodeEntry[1024]` (328 B each) from 256 (`layout_router`).

| Header field | Meaning |
| --- | --- |
| `router_uuid`, `router_name`, `hive_id`, `is_gateway` | the router, set at creation |
| `owner_pid`, `owner_start_time`, `created_at`, `generation` (1) | set at creation |
| `seq`, `heartbeat`, `updated_at` | §2.3, §2.4 |
| `node_count`, `node_max` (1024) | active entries, capacity |
| `opa_policy_version`, `opa_load_status` | the user policy this router holds: 0 ok, 1 error, 2 loading (`update_opa_status`) |

`NodeEntry`: `uuid`, `name` (256 B) and `name_len`, `vpn_id`, `flags` (`FLAG_ACTIVE`, or
`FLAG_DELETED` once the node left), `connected_at`.

- **Writes.** `register_node` at HELLO reuses the active entry with the same UUID, else takes the
  first free slot. A full table (`SlotFull`) or a name over 256 bytes fails the HELLO.
  `unregister_node` when the connection ends leaves a `FLAG_DELETED` hole; readers skip holes and
  stop after `node_count` active entries. `update_node_vpn` when VPNs are reassigned;
  `update_opa_status` on every heartbeat and after every policy load, only when it changed.
- **Readers.**
  - Sibling routers, every 5 s: the nodes become `PeerNode` entries of their FIB, and
    `is_gateway` tells them which sibling is the gateway (`discover_peer_regions`). The router never
    reads its own region; it routes from its in-memory node table.
  - SY.orchestrator reads the gateway router's region (`load_router_snapshot`): boot readiness by
    heartbeat (≤ 30 s), whether the lifecycle nodes are connected, and the watchdog's list of
    connected `AI.*`, `WF.*` and `IO.*` nodes.
  - SY.opa.rules reads each header for `get_status` (`listRouterStatuses`, `readRouterStatus`),
    finding routers by their socket names in `/var/run/fluxbee/routers`. Its Go `RouterHeader`
    mirrors `ShmHeader`, with `routerVersion = 2`.
- **Restart.** A restarted router keeps its UUID, so it reuses its region (§2.2). Nothing clears
  the node table at start: an entry is rewritten when its node reconnects, and the entry of a node
  that never comes back stays active. The router routes from its in-memory table, but the readers
  above see that node until the region is recreated.

---

## 4. Config region `/jsr-config-<hive>`

| Section | Entry | Offset | Count |
| --- | --- | --- | --- |
| header | `ConfigHeader` (200 B) | 0 | 1 |
| routes | `StaticRouteEntry` (320 B) | 256 | `MAX_STATIC_ROUTES` 256 |
| VPN rules | `VpnAssignment` (288 B) | 82,176 | `MAX_VPN_ASSIGNMENTS` 256 |
| taps | `TapEntry` (840 B) | 155,904 | `MAX_TAP_ENTRIES` 256 |

- **Header.** `owner_uuid` (SY.config.routes' node UUID), `owner_pid`, `owner_start_time`,
  `heartbeat`, `seq`, `static_route_count`, `vpn_assignment_count`, `tap_count`, `config_version`
  (1 at creation), `hive_id`, `created_at`, `updated_at`.
- **`StaticRouteEntry`.** `prefix` (256 B) and `prefix_len`, `match_kind` (0 EXACT, 1 PREFIX,
  2 GLOB), `action` (0 FORWARD, 1 DROP), `next_hop_hive` (32 B) and `next_hop_hive_len`, `metric`,
  `priority`, `flags` (`FLAG_ACTIVE`; `FLAG_ACTIVE | FLAG_FROZEN` for the two system markers),
  `installed_at`.
- **`VpnAssignment`.** `pattern` (256 B) and `pattern_len`, `match_kind`, `vpn_id`, `priority`,
  `flags`.
- **`TapEntry`.** `match_src`, `match_dst` and `target` (256 B each, with lengths), `mode`
  (0 = best effort, the only mode), `enabled`, `flags`, `installed_at`.

- **Writes** (`apply_config` in `src/bin/sy_config_routes.rs`), at start and after every change:
  `write_static_routes(routes + 2 markers, false)`, `write_vpn_assignments(vpns, false)`,
  `write_taps(taps, true)`. Each replaces its whole section and zeroes the unused slots; only the
  last one adds 1 to `config_version`, so a reader that acts on the version sees all three. A list
  over its section's capacity fails the write (`ValueTooLong`).
- **Readers.**
  - The hive's routers, on every frame from one of their nodes and every 5 s: they act only when
    `config_version` differs from the last one they applied; they skip `FLAG_FROZEN` routes and
    take routes and VPN rules whose `flags` are 0 or carry `FLAG_ACTIVE` (`refresh_config`,
    `assign_vpn`).
  - SY.orchestrator: which routes, VPN rules and taps name a node (`enumerate_routing_references_to`).
- **Restart.** SY.config.routes rewrites the three sections from `/etc/fluxbee/sy-config-routes.yaml`
  at start (creating the file empty if it is missing) and bumps `config_version`. While it is down
  the routers keep using the region as it is. With no region at all, a router routes without
  routes, VPN rules or taps and puts every node in VPN 0.

---

## 5. LSA region `/jsr-lsa-<hive>`

| Section | Entry | Offset | Count |
| --- | --- | --- | --- |
| header | `LsaHeader` (200 B) | 0 | 1 |
| hives | `RemoteHiveEntry` (192 B) | 256 | `MAX_REMOTE_HIVES` 16 |
| nodes | `RemoteNodeEntry` (292 B) | 3,328 | `MAX_REMOTE_NODES` 1,024 |
| routes | `RemoteRouteEntry` (320 B) | 302,336 | `MAX_REMOTE_ROUTES` 256 |
| VPN rules | `RemoteVpnEntry` (288 B) | 384,256 | `MAX_REMOTE_VPNS` 256 |
| taps | `RemoteTapEntry` (836 B) | 457,984 | `MAX_REMOTE_TAPS` 256 |

- **Header.** `gateway_uuid`, `owner_pid`, `owner_start_time`, `heartbeat`, `seq`, `hive_count`,
  `total_node_count`, `total_route_count`, `total_vpn_count`, `total_tap_count`, `created_at`,
  `updated_at`, `local_hive_id`.
- **`RemoteHiveEntry`.** `hive_id`, `router_uuid` and `router_name` of the advertising router,
  `last_lsa_seq`, `last_updated`, `flags`, and the per-hive counts.
- **Node, route, VPN and tap entries** carry the fields of the config region (nodes: `uuid`,
  `name`, `vpn_id`) without `installed_at`, plus `hive_index`, the position of their hive in the
  hive section, and `flags` = `FLAG_ACTIVE`. Route and VPN `priority` is always 0: the LSA does not
  carry it.

- **Content** (`write_lsa_state`, which rewrites the whole region each time):
  - hive entry 0 is the gateway's own hive, flagged `FLAG_ACTIVE | HIVE_FLAG_SELF`, with the nodes of
    every router of the hive (its own and its siblings'), its routes, VPN rules and taps;
  - then every hive from which an LSA was accepted, sorted by hive id, flagged `FLAG_STALE` once no
    LSA came for 40 s (`dead_interval_ms`, checked every 20 s by `lsa_stale_loop`).
- **When it is written:** at gateway start, every 10 s (`lsa_broadcast_loop`), after each accepted
  LSA (`apply_lsa_payload`), and when a hive turns stale or fresh again.
- **Limits.** 16 hive entries including the gateway's own (15 others), 1,024 nodes across all of
  them, 256 routes, 256 VPN rules, 256 taps. A write over any limit fails as a whole (`ValueTooLong`,
  logged as `lsa snapshot write failed`) and the region keeps its previous content.
- **Readers.**
  - The hive's routers, every 5 s and on every frame from a node (`refresh_lsa`). The FIB and the
    UUID lookups skip `HIVE_FLAG_SELF` and `FLAG_STALE` hives (`rebuild_fib`, `find_remote_node`);
    the tap fan-out reads the taps of every hive; nobody routes with the VPN rules in this region.
  - SY.admin (`stream_nodes_from_lsa`, `topology_hives_from_lsa`) and SY.orchestrator
    (`load_lsa_snapshot`).
- **Restart.** The gateway's in-memory LSA table starts empty, so its first write leaves only its
  own hive. The other hives come back as their LSAs arrive: each side of a WAN link sends one right
  after the handshake (`handle_wan_connection` → `broadcast_lsa`).

---

## 6. OPA region `/jsr-opa-<hive>`

`OpaHeader` (192 B) at 0; the policy wasm from 192, up to `OPA_MAX_WASM_SIZE` = 4 MiB. The Go writer
addresses fields by offset, so they are listed:

| Offset | Field | Meaning |
| --- | --- | --- |
| 0, 4 | `magic`, `version` | §1 |
| 8 | `seq` | §2.3 |
| 16 | `policy_version` | 0 = no user policy |
| 24 | `wasm_size` | bytes of wasm after the header |
| 32 | `wasm_hash` | sha256 of the wasm; all zeros when there is none |
| 64, 72 | `updated_at`, `heartbeat` | epoch ms |
| 80 | `status` | 0 ok, 1 error, 2 loading (`OPA_STATUS_*`) |
| 82, 146 | `entrypoint` (64 B), `entrypoint_len` | for example `router/target` |
| 148, 164 | `owner_uuid`, `owner_pid` | SY.opa.rules |

- **Writer:** SY.opa.rules (`writeHeader`, `writePolicy`, `go/sy-opa-rules/main.go`; the Rust
  `OpaRegionWriter` is used only by tests).
  - `writePolicy` is two writes: status loading, then header and wasm with status ok, or with no
    wasm and status error when the wasm is over 4 MiB.
  - A cleared policy is version 0, no wasm, zero hash (`clearPolicy`).
  - After writing, SY.opa.rules broadcasts `OPA_RELOAD {version, hash}` with `ttl: 2`
    (`broadcastOpaReload`).
- **Readers:** the hive's routers.
  - On `OPA_RELOAD`: they load the region's wasm if its hash equals the notice's (`apply_opa_reload`).
  - Every heartbeat: they load when `policy_version` differs from theirs and `status` is ok; unload
    on version 0, size 0, status ok; wait while loading or in error (`maybe_refresh_opa_from_shm`).
  - Each router reports what it holds in its own region (§3).
- **Restart.** SY.opa.rules writes the region from `/var/lib/fluxbee/opa/current/` at start
  (`loadCurrentPolicy`), and its sync loop rewrites it whenever the region's hash differs from the
  installed policy (`ensureRegionMatchesCurrent`, every 5 s). Routers keep the policy they loaded
  while SY.opa.rules is down; a restarted router loads it from the region on its first heartbeat.

---

## 7. Identity region `/jsr-identity-<hive>`

Sections follow the header in this order, each on a 64-byte boundary (`layout_identity`):

| Section | Entry | Count | Offset with the defaults |
| --- | --- | --- | --- |
| header | `IdentityHeader` (176 B) | 1 | 0 |
| tenants | `TenantEntry` (320 B) | `max_tenants` | 192 |
| ILKs | `IlkEntry` (1,160 B) | `max_ilks` | 327,872 |
| channels (ICH) | `IchEntry` (488 B) | `max_ichs` = 4 × `max_ilks` | 9,830,592 |
| ICH mappings | `IchMappingEntry` (384 B) | `max_ich_mappings` = 2 × `max_ichs` | 25,821,376 |
| aliases | `IlkAliasEntry` (64 B) | `max_ilk_aliases` | 50,987,200 |
| vocabulary | `VocabularyEntry` (232 B) | `max_vocabulary` | 51,511,488 |

- **Limits.** SY.identity reads them from `hive.yaml` (`identity.max_ilks`, `max_tenants`,
  `max_vocabulary`, `max_ilk_aliases`); defaults 8,192, 1,024, 4,096 and `max_ilks`
  (`identity_region_limits`, `DEFAULT_IDENTITY_SHM_MAX_*` in `src/bin/sy_identity.rs`). The
  `DEFAULT_IDENTITY_MAX_*` constants in `src/shm/mod.rs` (1,000,000 ILKs, …) only feed
  `IdentityRegionLimits::default()`, which no production code uses.
- **The header records the limits** (`max_*`), and readers build the layout from them
  (`open_read_only_auto`; SDK `open_identity_shm_reader`; Go reads `max_*` at fixed offsets).
  Because a larger existing region is reused with its old header (§2.2), lowering the limits
  leaves the header, which the readers follow, with the old ones while the writer lays the
  sections out with the new ones.
- **Header counts:** `tenant_count`, `ilk_count`, `ich_count`, `ich_mapping_count`,
  `vocabulary_count`, `ilk_alias_count`; also `owner_uuid`, `owner_pid`, `is_primary`, `hive_id`.

**Entries** (UUIDs as 16 raw bytes; enum codes as SY.identity writes them, `src/bin/sy_identity.rs`):

- `TenantEntry`: `tenant_id`, `name`, `domain` (128 B each), `status` (0 pending, 1 active,
  2 suspended), `flags`, `max_ilks`, `sponsor_tenant_id`, `created_at`, `updated_at`. Tenants
  marked deleted are left out.
- `IlkEntry`: `ilk_id`; `ilk_type` (0 human, 1 agent, 2 system); `registration_status`
  (0 temporary, 1 partial, 2 complete); `flags`; `tenant_id`, the tenant the ILK is charged to;
  `display_name`; `handler_node`, the serving node's L2 name (the ILK's `node_name`); `ich_offset`
  and `ich_count`, its channels, contiguous in the channel section; `role_hash`,
  `skill_hashes[16]` and `skill_count`, `handbook_hashes[8]` and `handbook_count`,
  `personality_hash` (sha256; all zeros = none); `created_at`, `updated_at`.
- `IchEntry`: `ich_id`, `ilk_id`, `tenant_id` (the channel's scope), `channel_type` (32 B) and
  `address` (256 B), both trimmed and lowercased, `flags`, `owner_l2_name`, `is_primary` (the first
  channel), `enabled`, `added_at`.
- `IchMappingEntry`: a hash table from `(channel_type, address, tenant_id)` to `(ich_id, ilk_id)`.
  - Hash: FNV-1a 64 over the channel type, byte `0xff`, the address, byte `0xfe`, the 16 tenant
    bytes (`compute_ich_hash`, mirrored by the SDK and by Go `identityIchHash`).
  - Open addressing: start at `hash % table size`, probe linearly. `flags` is `OCCUPIED` (0x1) or
    `TOMBSTONE` (0x2); an empty slot ends a lookup; an insert reuses the first tombstone it passed.
- `IlkAliasEntry`: `old_ilk_id` → `canonical_ilk_id` until `expires_at` (epoch ms), `flags`.
- `VocabularyEntry`: `tag`, `category`, `flags`, `description`, `created_at`, `deprecated_at`.
  SY.identity writes this section empty (`sync_identity_shm_mappings`).

**Writes** (SY.identity):

- **Full rebuild** (`sync_identity_shm_mappings`), at start and for delete, restore and purge
  (`is_lifecycle_action`). `write_snapshot_entries` replaces tenants, ILKs, channels, aliases and
  vocabulary in one write and empties the mapping table; then each mapping goes in with its own
  write (`upsert_ich_mapping`). Between the two, a lookup by channel misses.
- **Incremental** (`apply_identity_shm_deltas`): upserts and removals of one tenant, ILK, alias or
  channel set, each its own write. A failed incremental apply falls back to the full rebuild.
- **Provisioning** (`provision_temporary_ilk`): the ILK, its channels and their mappings in one
  write.

**Readers:**

- The router reads a full snapshot on every Resolve that carries an `src_ilk` and on every
  user-policy load (`read_identity_snapshot`): registration status, aliases, `data.identity`.
- The Rust SDK (`crates/fluxbee_sdk/src/identity.rs`): channel to ILK (`resolve_ilk_from_*`,
  `resolve_identity_option_from_*`; inputs trimmed and lowercased, tenant required), ILK and tenant
  existence and status, ILK by handler node, the region's `seq`. Its users include the IO nodes,
  the AI node runners, SY.admin, SY.cognition, SY.orchestrator, SY.storage and SY.vault. A region
  that exists but cannot be opened (EACCES, EPERM) reads as "not found", except in the `_strict`
  variants.
- The Go SDK (`go/fluxbee-go-sdk/identity_shm.go`): `LookupIlkByChannel`, `LookupIlkByHandlerNode`,
  `WaitForSelfSystemIlkID`. It hard-codes the struct sizes and field offsets.

**Restart.** SY.identity rebuilds the whole region at start from its store: the database on the
primary, the full sync on a replica. Until then readers read what the region holds.

---

## 8. Memory region `/jsr-memory-<hive>`

`MemoryHeader` (160 B) at 0: `seq`, `data_size`, `thread_count`, `updated_at`, `heartbeat`,
`owner_uuid`, `owner_pid`, `hive_id`. From offset 192, `data_size` bytes of JSON, up to
`MEMORY_MAX_DATA_SIZE` = 4 MiB; the rest is zeroed on each write (`MemoryRegionWriter::write_snapshot`).

```json
{
  "schema_version": 2,
  "updated_at": 1760000000000,
  "threads": [
    {
      "thread_id": "…",
      "agent_ilk": "ilk:<uuid>",
      "agent_node": "AI.sales@worker1",
      "package": { "package_version": 2, "thread_id": "…" }
    }
  ]
}
```

- **Key.** One entry per communication `(thread_id, agent_ilk)` (`MemoryShmThreadEntry`;
  `CommunicationKey` in SY.cognition). `agent_node` is where that agent's router delivers.
- **Lookup.** Delivering a turn with `meta.thread_id` to an `AI.*` node, the router takes the first
  entry whose `thread_id` equals it and whose `agent_node` equals the destination node's name
  (`package_for`, `src/router/mod.rs`). An entry without `agent_ilk` and `agent_node`, written
  before 0.1.77, matches nothing.
- **Two versions.** `MEMORY_VERSION` (1) is the header layout; `schema_version` (2, written by
  SY.cognition) is inside the JSON and nobody checks it.
- **Writer.** SY.cognition (`sync_memory_shm`), after each live turn and after its start-up reload.
  It ranks the communications it holds (the one this turn updated first, then recency, an active
  scope, live entities, turn count, key), keeps those whose JSON still fits in 4 MiB, then carries
  over the entries of communications it has not loaded yet (`build_memory_hot_set_snapshot`). A
  JSON over 4 MiB is refused (`ValueTooLong`).
- **Readers.**
  - The hive's routers at delivery: the whole JSON is read and parsed once per delivery that needs
    it (`read_memory_snapshot_for_delivery`). Without a readable region they deliver with no
    package.
  - SY.cognition itself (carried entries, start-up reload).
  - `src/bin/cognition_shm_dump.rs` (`--hive <hive> [--thread-id <id>]`) prints it.
- **Restart.** SY.cognition reuses the region, reloads from SY.storage the communications listed in
  it, and keeps the entries it has not reloaded (`reload_communications_from_shared_memory`).

---

## 9. Taps in shared memory

- The hive's own taps are `TapEntry` rows in the config region (§4).
- Every hive's taps, the gateway's own hive included, are `RemoteTapEntry` rows in the LSA region,
  each with its `hive_index` (§5).
- The router reads both when a unicast message enters from one of its nodes, matches
  `(match_src, match_dst)` by exact name, skips disabled rows (and LSA rows without `FLAG_ACTIVE`)
  and deduplicates the targets (`fanout_taps_for_unicast`). Semantics:
  [routing-and-policy.md](routing-and-policy.md) §8.3.

---

## 10. What survives what

| Region | Writer restarts | Writer down |
| --- | --- | --- |
| Router | region and node table reused as they were (§3) | the router is gone too; siblings drop the region after 30 s |
| Config | rewritten from the YAML, `config_version` bumped | routers keep the last content |
| LSA | rewritten with the own hive only; others return with their LSAs | non-gateway routers keep the last content |
| OPA | rewritten from `opa/current/`; repaired every 5 s | routers keep the loaded policy |
| Identity | rebuilt from the store | readers keep the last content |
| Memory | reused; reloaded entries refreshed, the rest kept | routers keep attaching the last packages |

No service removes a region for good; a writer unlinks one only to create it again (§2.2).
`scripts/install.sh` (`cleanup_volatile_runtime_artifacts`) and `scripts/fluxbee_cleanall.sh`
delete every `jsr-*` region, the router ones through their `identity.yaml`;
`packaging/fluxbee-factory-reset` deletes the memory region.

---

## 11. Changing a layout

- Bump the region's version constant (§2.2).
- Identity: update the `const _` size asserts in `src/shm/mod.rs`, the mirror structs and constants
  in `crates/fluxbee_sdk/src/identity.rs` (`IDENTITY_VERSION`, layout), and the sizes and offsets in
  `go/fluxbee-go-sdk/identity_shm.go`, in step.
- OPA and router header: `go/sy-opa-rules/main.go` hard-codes `opaHeaderSize = 192`, the offsets in
  `writeHeader`, the `RouterHeader` struct and `routerVersion = 2`.
- Keep `compute_ich_hash` identical in Rust, the Rust SDK and Go.

---

## 12. Constants (`src/shm/mod.rs`)

| Constant | Value |
| --- | --- |
| `MAX_NODES` | 1,024 |
| `MAX_STATIC_ROUTES`, `MAX_VPN_ASSIGNMENTS`, `MAX_TAP_ENTRIES` | 256 each |
| `MAX_REMOTE_HIVES` | 16 |
| `MAX_REMOTE_NODES` | 1,024 |
| `MAX_REMOTE_ROUTES`, `MAX_REMOTE_VPNS`, `MAX_REMOTE_TAPS` | 256 each |
| `OPA_MAX_WASM_SIZE`, `MEMORY_MAX_DATA_SIZE` | 4 MiB each |
| `IDENTITY_DEFINITION_MAX_SKILLS`, `IDENTITY_DEFINITION_MAX_HANDBOOKS` | 16, 8 |
| `ICH_CHANNEL_TYPE_MAX_LEN`, `ICH_ADDRESS_MAX_LEN` | 32, 256 |
| `MATCH_EXACT`, `MATCH_PREFIX`, `MATCH_GLOB` | 0, 1, 2 |
| `ACTION_FORWARD`, `ACTION_DROP` | 0, 1 |
| `FLAG_ACTIVE`, `FLAG_DELETED`, `FLAG_STALE`, `FLAG_FROZEN`, `HIVE_FLAG_SELF` | 0x1, 0x2, 0x4, 0x8, 0x10 |
| `ICH_MAP_FLAG_OCCUPIED`, `ICH_MAP_FLAG_TOMBSTONE` | 0x1, 0x2 |
| `OPA_STATUS_OK`, `OPA_STATUS_ERROR`, `OPA_STATUS_LOADING` | 0, 1, 2 |
| `HEARTBEAT_INTERVAL_MS`, `HEARTBEAT_STALE_MS` | 5,000, 30,000 |
| `SHM_NAME_MAX_LEN` (`SHM_NAME_LIMIT`) | 64 |
| `SEQLOCK_READ_TIMEOUT_MS` (private) | 5 |
