# DEPLOYMENTS — record of the versions deployed to PROD (audit)

> **Append-only** ledger of every version that TOUCHES production (`pve` @ `192.168.8.207`). It is the
> audit record: **which** version, **when**, **which commit**, **what changed**, **how it was
> verified** and **how it is rolled back**. One row in the table + one detailed entry per version.
>
> **Rule (HANDBOOK §12): no `apt install` in prod — and no core-update to a spoke — without an
> entry here.** Without an entry, the deploy is not done.
>
> **Snapshots: one per VM (HANDBOOK §12; three from 2026-09-25, one since 2026-10-29).** The "by
> snapshot" rollback of an older entry no longer exists: only the last release's snapshot is kept.
> The path that remains is `apt install fluxbee=<previous>`, as far back as the repo goes: it keeps
> the newest 10 versions (since 2026-10-29).
>
> Siblings: [`logbook/HANDBOOK.md`](logbook/HANDBOOK.md) (deploy recipes), [`logbook/METHOD.md`](logbook/METHOD.md)
> (how the infra is operated), `logbook/YYYY-MM-DD.md` (narrative logbook), [`logbook/FINDINGS.md`](logbook/FINDINGS.md)
> (findings/bugs). This doc is ONLY the ledger of the versions in prod. Dates in **ART (−03)**;
> the prod host's journal is in EDT (see METHOD §1).

## Quick table (audit)

| Version | Date (ART) | Commit | Scope | Status | Rollback |
|---|---|---|---|---|---|
| **0.1.136** | 2026-11-12 | `ba2addb2` | motherbee + spokes (core), the AI SDK alone: A-257 (the AI runtime reads every message while its 4 workers run LLM turns: a status, help or config request runs at once beside them on a direct lane of 64 places, and so does a handoff in the shape the frontdesk parses, on at most 8 of them and within 30 s; the turns start on the 4 workers in the order they came, with up to 128 waiting, and the next one is answered `ai_runtime_busy` at once, never run); its review's texts: an AI node answers status and help at once however busy only on ai.generic 0.1.136 or later, and ai.generic running its model for a handoff on the direct lane is a known limit | ✅ live, checked on 8.x | snap `pre-aisdk-0-1-136` (all 4 VMs) · `apt install fluxbee=0.1.135` + 0.1.135's `update_installation` (nothing one-way; A-257 comes back) |
| **0.1.135** | 2026-11-11 | `9a6ffb6d` | motherbee + spokes (core), the router alone: A-272 (split horizon: a broadcast that came over the WAN goes on to every WAN peer but the one it came from, so no router sends a broadcast back to its peer, and over IRP a router that is not the gateway hands none back to the gateway), A-268 (two FIB builds at once are stored in the order they began, so a build that read before a change never undoes that change's rebuild); `ops.py health` checks `wandrops=0` from then on | ✅ replaced by 0.1.136 | snap `pre-router-0-1-135` (deleted 2026-11-12) · `apt install fluxbee=0.1.134` + 0.1.134's `update_installation` (nothing one-way; the WAN echoes come back, and now that `HEALTH_OK` checks `wandrops=0` the hives that drop them fail health: the motherbee whenever a node broadcasts over the WAN, a spoke when its own nodes do) |
| **0.1.134** | 2026-11-11 | `8539bc64` | motherbee + spokes (core), the orchestrator alone, a hotfix: A-314 (a node unit systemd does not have reads as not running: on the hives' systemd 255 `systemctl is-active` exits 4 for it, which 0.1.133 read as no answer, so no node could be launched and a stopped node whose unit was gone could be neither started nor purged and read `UNKNOWN`); its review: `kill_node` and `remove_node_instance` refuse an `SY.*` or `RT.*` name (`INVALID_REQUEST`) before anything is done, as `run_node` does | ✅ replaced by 0.1.135 | snap `pre-orch-0-1-134` (deleted 2026-11-11) · `apt install fluxbee=0.1.133` + 0.1.133's `update_installation` (that brings A-314 back); to 0.1.132 by apt (nothing one-way found) |
| **0.1.133** | 2026-11-11 | `45c736e4` | motherbee + spokes (core), one branch per binary, the sweep's end: SY.identity and SY.admin (A-191: a system ILK is retired, never deleted, at SY.identity's start for an SY node its release no longer runs, and by `retire_hive_ilks` or `remove_hive` for a removed hive; A-232: SY.identity's listings refuse a cursor not theirs without repeating it), SY.admin (A-255: `publish_cloud_endpoint` applies the edge to the running IO.cloud; A-260 and A-274: the expiry sweep and a take-down call the edge and IO.blob outside the publication lock; A-281: no version removed while some hive's use of it was not read; A-285's texts), SY.timer 1.6 (A-258: its refusals logged, bounded; A-259: the heap holds the pending timers only; A-266: a page its `limit` cuts answers `next_cursor`), the orchestrator (A-240: a status read writes nothing, `status_version` gone from the answer; A-241: a worker cleans its own blob staging; A-267 and A-271: a check with no answer acts on nothing; A-281's use across the hives, `hives_without_runtimes`), SY.wf-rules (A-270: no delete without force of a WF node that is not `RUNNING`; A-281: a delete names the versions it left), SY.architect (A-279 with A-288: Archi's SCMD reads decode their query and keep it) | ✅ replaced by 0.1.134 (deployed 22:35Z, replaced 23:32Z; its gate found A-314) | snap `pre-sweep-0-1-133` (deleted 2026-11-11) · `apt install fluxbee=0.1.132` + 0.1.132's `update_installation` (nothing one-way found: the system ILKs 0.1.133 retired stay retired, harmless to 0.1.132) |
| **0.1.132** | 2026-11-11 | `36135e52` | motherbee + spokes (core), the router alone: A-245 (a far router's `UNREACHABLE` or `TTL_EXCEEDED` reaches a sender on another spoke: the hub hands it on toward the spoke whose LSA lists the sender), A-195 (no WAN frame arrives before what names its sender: a new link's first frames are the LSA and the hub's reachability; the hub tells every spoke of a hive's changed nodes as its LSA comes; A-261, A-262 with it), A-265 (the hub rebuilds its FIB as it applies an LSA, and as a hive goes stale), A-246 (a broadcast answered once, by the router it entered), A-253 (a stream record too big for a batch named in the router's journal, its ids quoted, escaped and cut, at most 10 between two alarm lines), A-208 with A-247 (the several-routers mode, unsupported: no wedge, answers over the link a frame came from); `ops.py health` counts the router's lines only when the pattern opens the message | ✅ replaced by 0.1.133 | snap `pre-router-0-1-132` (deleted 2026-11-11) · `apt install fluxbee=0.1.131` + 0.1.131's `update_installation` (nothing one-way; the wire is the same) |
| **0.1.131** | 2026-11-11 | `0f6b54f1` | motherbee + spokes (core), one branch per binary, from the sweep: SY.admin (A-242: no removal refused for published pages, each ok answer names them in `pages_left_up`; A-249 closed with it; A-251: SY.timer's refusals answered by their code; A-250 (a): a short read while a hive restarts, asked again at once), SY.timer 1.5 (A-238: a canceled timer aged by its cancel, schema 4; A-244: the lock map drops a timer's entry), IO (A-91: io.blob CONFIGURED on its defaults, io.cloud UNCONFIGURED `missing_edge_node` without an edge; A-226; A-223's spec), WF (A-254: a timer SY.timer does not take goes again until it does, the store at v7; the 366-day bound in wf.engine and SY.wf-rules), the orchestrator (A-252: a new spoke switches Syncthing's posture at its join's peer link) | ✅ replaced by 0.1.132 | snap `pre-sweep-0-1-131` (deleted 2026-11-11) · `apt install fluxbee=0.1.130` + 0.1.130's `update_installation` (the older SY.timer and wf.engine read schema 4 and store v7; A-233's and A-212's page refusals come back) |
| **0.1.130** | 2026-11-10 | `8604debf` | motherbee + spokes (core), the Go SDK alone: A-228 (an RPC error for a router notice carries `reason`, `original_dst`, `last_hop`), A-239 (`TimerInfo.canceled_reason`; SY.timer serializes the SDK's `TimerInfo`), A-218's ECHO leftovers (`MSGEcho`/`MSGEchoReply` gone; SY.timer and wf-generic take the AI nodes' notice rule; SY.timer answers an ECHO `TIMER_UNKNOWN_VERB`) | ✅ replaced by 0.1.131 | snap `pre-gosdk-0-1-130` (deleted 2026-11-11) · `apt install fluxbee=0.1.129` + 0.1.129's `update_installation` (nothing one-way; the wire is the same) |
| **0.1.129** | 2026-11-10 | `9b396e3e` | motherbee + spokes (core), the router alone: A-222 (a bounded queue per link, 1,024 frames or 8 MiB counted as heap, the senders' share 960 / 7 MiB; a message past it refused `LINK_BUSY` with an answer, the node never disconnected), A-209 (no router answers a tap copy; the router clears a `via_tap` a node sets), A-221 (an agent's turn goes to cognition only once its link took it) | ✅ replaced by 0.1.130 | snap `pre-router-0-1-129` (deleted 2026-11-10) · `apt install fluxbee=0.1.128` + 0.1.128's `update_installation` (nothing one-way) |
| **0.1.128** | 2026-11-10 | `89fd1fd8` | motherbee + spokes (core), the Rust SDK alone: A-218 (`TNT_APPROVE`, seven dead action names, ECHO/ECHO_REPLY/TIME_SYNC gone; SY.identity's migration v5 drops `identity_vocabulary` and `identity_tenants.approved_by`; the identity shared memory exactly as it was), A-237 (an intermediate answer marked `accepted`), A-248 (an AI node used as a tap target answers nothing, its copies counted) | ✅ replaced by 0.1.129 | snap `pre-sdk-0-1-128` (deleted 2026-11-10) · `apt install fluxbee=0.1.127` + 0.1.127's `update_installation` (SY.identity of 0.1.127 ignores migration row 5, never names the dropped column, and recreates the empty vocabulary table) |
| **0.1.127** | 2026-11-10 | `d2b344bb` | motherbee + spokes (core), one branch per binary: SY.orchestrator (A-199's step 1: reads apart, at most 4 waiting on one hive, a command that panics answered; hive-config stage 5: the blob folders follow their dist folder, and the blob GC runs only on the motherbee (A-188); A-196's restart marker; A-232's refusals); SY.admin (A-233: no purge while a node has a page up or a publish in flight; A-231, the ledger keeps a page down 30 days; A-234, the HTTP head and body caps; A-232; A-171, the manifest's lock file); SY.vault (A-200, one pass at the boot's announcement); SY.timer (A-220, canceled with a reason, schema v3; `LINK_BUSY` retried; A-232) | ✅ replaced by 0.1.128 | snap `pre-reads-0-1-127` (deleted 2026-11-10) · `apt install fluxbee=0.1.126` + 0.1.126's `update_installation` (SY.timer of 0.1.126 reads the v3 table) |
| **0.1.126** | 2026-11-10 | `cf005771` | motherbee + spokes (core): A-219: an existing worker goes behind the egress, and back, by an explicit action (`set_hive_egress` / `clear_hive_egress`, the operator's word or Archi's plan with CONFIRM; `SYSTEM_EGRESS_ROUTE` between the orchestrators, protected; guards, a record, a probe, the motherbee's confirm, commit or revert, the start's resume; `/versions`' egress state); with A-225 (a removed worker's IPv6 file, the boot judging IPv6 by the kernel, the probe's reason) | ✅ replaced by 0.1.127 | snap `pre-egress-0-1-126` (deleted 2026-11-10) · first `clear_hive_egress` of every worker behind the egress (a release before 0.1.126 cannot), then `apt install fluxbee=0.1.125` + 0.1.125's `update_installation` |
| **0.1.125** | 2026-11-10 | `f39e2bb0` | motherbee + spokes (core): the sweep's calls: published pages listed (`GET /artifacts`), taken down by the factory reset, and a tenant's purge refused while one is up (A-212); no default tenant and two different tenants refused (A-213); every fixed text in English and the frontdesk answering in the person's language (A-214); a WF forward to another IO node names no one (A-215); the frontdesk retrying a transient failure (A-216); wf-generic staying up on a failed store (A-217); one-time conversions removed (A-210, mostly); hive.yaml written atomically by `set_storage` (A-224); SY.admin's HTTP answering a failed handler (A-229); Archi asking for a node's tenant (A-230); Archi's runtime redeploy keeping each node's tenant (A-235); a removed ingress's SY.edge stopped (A-236) | ✅ replaced by 0.1.126 | snap `pre-clean-0-1-125` (deleted 2026-11-10) · `apt install fluxbee=0.1.124` + 0.1.124's `update_installation` (SY.identity of 0.1.124 still reads every record 0.1.125 writes) |
| **0.1.124** | 2026-11-10 | `bffaf4f2` | motherbee + spokes (core): the router's release: a WAN session runs its cleanup however it ends and a send into a dead link is answered (A-203, the links between sibling routers and to nodes too); a WAN peer silent for the dead interval is cut (A-207), and a WAN handshake has a deadline; a sender's message the router's additions take over the frame is answered `FRAME_TOO_LARGE`, a broadcast everywhere or nowhere (A-202); the router no longer reads hive.yaml's `ai` and blob keys; with SY.admin's installation update record (A-204, A-205, and its review round's fixes), SY.timer refusing a `TZ=`/`CRON_TZ=`/`@` cron spec (A-206), wf-generic out of the core (A-211) and A-181's leftover texts | ✅ replaced by 0.1.125 | snap `pre-router-0-1-124` (deleted 2026-11-10) · `apt install fluxbee=0.1.123` + 0.1.123's `update_installation` (the installation file v7 stays; 0.1.123's router needs hive.yaml's `ai` and blob keys valid, as they are on 8.x) |
| **0.1.123** | 2026-11-09 | `e6f6c93f` | motherbee + spokes (core): hive-config-update-v1's stage 4: SY.admin's installation update (`update_installation`, the operator's word; `get_installation_update`, Archi's read) brings every hive to the motherbee's core and installation file, core then config, kept in SY.admin's state dir and resumed after a restart; what it leaves alone, with why (a hive rolled back with `core_rollback` keeps its core and the update is `held`; a hand-edited copy gets no config); the automatic step, once per hive and target, for a hive that did not answer and comes back; `set_installation_ai` opens it, Archi follows it, `ops.py deploy` runs it; with A-198: a mesh reply that does not fit the frame answers `REPLY_TOO_LARGE` with its outcome, optional `limit`/`cursor` pages on the listings that grow (every row without them, as before), an executor run's answer cut to fit; and A-201: Archi reads the admin command log | ✅ replaced by 0.1.124 | snap `pre-inst-0-1-123` (deleted 2026-11-10) · `apt install fluxbee=0.1.122` + `ops deploy 0.1.122`'s per-spoke core update (SY.admin's `installation-update.json` stays, ignored by 0.1.122) |
| **0.1.122** | 2026-11-09 | `6c6d963d` | motherbee + spokes (core): hive-config-update-v1's stage 3c: the operator changes the installation's `ai` through SY.admin (`get_installation`, Archi's read; `set_installation_ai`, the operator's word, through Archi's confirmed plan or loopback), A-11's checks, the motherbee's orchestrator writes the next version and every hive applies it (`SYSTEM_UPDATE` category `config`), restarting only the start readers whose engine changed; SY.admin's executor and Archi take it at their next use; with A-197: Archi reads SY.admin's catalog in its compact view over the mesh | ✅ replaced by 0.1.123 | snap `pre-inst-0-1-122` (deleted 2026-11-09) · `apt install fluxbee=0.1.121` + core-update of the spokes (the installation file stays at its version, which 0.1.121's readers read the same) |
| **0.1.121** | 2026-11-08 | `84d123ae` | motherbee + spokes (core): hive-config-update-v1's stage 3b: the motherbee writes the installation file's version 1 from its hive.yaml `ai` and distributes it (`fluxbee-dist-installation`, following vendor's members); the join copies it and writes no `ai`; SY.admin, SY.architect, SY.cognition, SY.frontdesk.gov and ai.generic read it; `/versions` shows each hive's `config` | ✅ replaced by 0.1.122 | snap `pre-inst-0-1-121` (deleted 2026-11-09) · `apt install fluxbee=0.1.120` + core-update of the spokes (keep each hive.yaml `ai`; a worker joined by 0.1.121 needs it put back, HANDBOOK) |
| **0.1.120** | 2026-11-08 | `0f516fdd` | motherbee + spokes (core): the AI SDK alone, hive-config-update-v1's stage 3a: the installation file's type and reader (`fluxbee_ai_sdk::installation`), used by no reader yet | ✅ replaced by 0.1.121 | snap `pre-aisdk-0-1-120` (deleted 2026-11-09) · `apt install fluxbee=0.1.119` + core-update of the spokes |
| **0.1.119** | 2026-11-08 | `b7f20c52` | motherbee + spokes (core): the orchestrator alone: the Syncthing watchdog leaves a hive's Syncthing to a join in flight (the motherbee's during an add_hive or remove_hive, a spoke's during its finalize), and asks again right before it restarts it (A-194) | ✅ replaced by 0.1.120 | snap `pre-watch-0-1-119` (deleted 2026-11-08) · `apt install fluxbee=0.1.118` + core-update of the spokes |
| **0.1.118** | 2026-11-08 | `9ea2f812` | motherbee + spokes (core): a fresh worker joins again (A-190): a receive-only folder is reverted only once its source has been connected and sharing it for 60 s; the join's finalize goes on while its vendor is still arriving, fits its probe in the motherbee's wait and answers each step's time; a core update with nothing new restarts nothing; `ops timeline` and `ops deploy`'s phase times | ✅ replaced by 0.1.119 | snap `pre-join-0-1-118` (deleted 2026-11-08) · `apt install fluxbee=0.1.117` + core-update of the spokes |
| **0.1.117** | 2026-11-08 | `e1c5e5ab` | motherbee + spokes (core): hive-config-update-v1's stage 2: the release's settings live in the code, by role (`src/release_settings.rs`): each role's SY nodes, their order and waits, and the blob's and dist's topology, with today's values; the orchestrator, SY.identity and SY.vault read the table, the joins stop writing those keys, an old hive.yaml's are ignored and named once; a node a release drops or adds is settled by the new orchestrator's start, and `core_rollback` undoes it; `ops deploy` waits for each spoke's settle (A-181) | ✅ replaced by 0.1.118 | snap `pre-relkeys-0-1-117` (deleted 2026-11-08) · `apt install fluxbee=0.1.116` + core-update of the spokes (the 8.x hives keep their old keys, which 0.1.116 needs) |
| **0.1.116** | 2026-11-08 | `9616278d` | motherbee + spokes (core): the orchestrator alone, hive-config-update-v1's stage 1: the start never stops on a dist mismatch, it reports it in /versions (`core.awaiting_update`) and checks again every minute (A-186); a hive whose vendored Syncthing does not match its manifest runs the one installed and reports it (`vendor.awaiting_update`, A-189) | ✅ replaced by 0.1.117 | snap `pre-start-0-1-116` (deleted 2026-11-08) · `apt install fluxbee=0.1.115` + core-update of the spokes |
| **0.1.115** | 2026-11-08 | `02731d26` | motherbee + spokes (core): the AI SDK alone: a provider's error is typed, OpenAI retries like Anthropic and reads the status first, a refusal, a missing key or an answer with nothing to send are their own errors, cognition counts them apart from invalid outputs, and the person gets one fixed English text (A-184) | ✅ replaced by 0.1.116 | snap `pre-aisdk-0-1-115` (deleted 2026-11-08) · `apt install fluxbee=0.1.114` + core-update of the spokes |
| **0.1.114** | 2026-11-08 | `f8425df5` | motherbee + spokes (core): the Go SDK alone: a Go node says HELLO once at start, and a 5 s deadline bounds every handshake (A-183) | ✅ replaced by 0.1.115 | snap `pre-gosdk-0-1-114` (deleted 2026-11-08) · `apt install fluxbee=0.1.113` + core-update of the spokes |
| **0.1.113** | 2026-11-08 | `f72ca82e` | motherbee + spokes (core): the Rust SDK alone: a node's handshake with its router is bounded by 10 s, where a router that never answered held it forever (A-182) | ✅ replaced by 0.1.114 | snap `pre-rustsdk-0-1-113` (deleted 2026-11-08) · `apt install fluxbee=0.1.112` + core-update of the spokes |
| **0.1.112** | 2026-11-08 | `1dc2eb40` | motherbee + spokes (core): the sprint's sixth release: the first agent on a thread takes what people said there before it, rebuilt on its review (A-173, SY.cognition); a second sy-orchestrator started by hand refuses and exits (A-180), and a drift alert read for another hive says what it holds, which Archi passes on (A-175's interim) | ✅ replaced by 0.1.113; its gate found A-185 | snap `pre-take-0-1-112` (deleted 2026-11-08) · `apt install fluxbee=0.1.111` + core-update of the spokes |
| **0.1.111** | 2026-11-08 | `cc01f86f` | motherbee + spokes (core): the sprint's fifth release, the router alone: a second rt-gateway started by hand refuses and exits, leaving the live one's sockets, shared memory and stream alone (A-177); `wan.mtls` fails closed (A-178); the baked system policy is checked against its Rego in CI, the policy compile made deterministic for it (A-179, with sy-opa-rules) | ✅ replaced by 0.1.112 | snap `pre-router-0-1-111` (deleted 2026-11-08) · `apt install fluxbee=0.1.110` + core-update of the spokes |
| **0.1.110** | 2026-11-07 | `eb972451` | motherbee + spokes (core): the sprint's fourth release, each item on its own binary: Archi removes runtime versions and hives, deletes and restores tenants (A-172 and its family, SY.admin); three texts (A-176: SY.timer, SY.wf-rules, SY.architect); the egress's internet alert and a worker's IPv6 read back (A-174, SY.orchestrator) | ✅ replaced by 0.1.111 | snap `pre-family-0-1-110` (deleted 2026-11-08) · `apt install fluxbee=0.1.109` + core-update of the spokes |
| **0.1.109** | 2026-11-07 | `3f292500` | motherbee + spokes (core): the sprint's third release, on SY.admin, the orchestrator and the seed (one branch): a base runtime goes back to its package's version through Archi (A-122); a join over a password chooses `harden_ssh` (A-165); the vault help's types (A-164); an identity read that names a spoke goes to the primary (A-169); three small ones (A-170) | ✅ replaced by 0.1.110 | snap `pre-reset-0-1-109` (deleted 2026-11-07) · `apt install fluxbee=0.1.108` + core-update of the spokes |
| **0.1.108** | 2026-11-07 | `7f18a610` | motherbee + spokes (core): the provider holds an agent's answer to its response contract inside the tool loop too (A-150; the shared AI SDK, so alone) | ✅ replaced by 0.1.109 | snap `pre-contract-0-1-108` (deleted 2026-11-07) · `apt install fluxbee=0.1.107` + core-update of the spokes |
| **0.1.107** | 2026-11-07 | `6c107a9b` | motherbee + spokes (core): the sprint's second release: the narrator says "the agent" only of the AI agent (A-167); SY.admin's `cognition_clear` has a hive's SY.cognition drop everything it keeps, behind the operator's CONFIRM, and the factory reset clears every hive's cognition with it (A-160) | ✅ replaced by 0.1.108 | snap `pre-train2-0-1-107` (deleted 2026-11-07) · `apt install fluxbee=0.1.106` + core-update of the spokes |
| **0.1.106** | 2026-11-07 | `9f19bcc5` | motherbee + spokes (core): the sprint's first release, four changes on different binaries ([METHOD](logbook/METHOD.md) §9): the episode gate says why it turns a proposal down (A-163); Archi takes a page down, behind the operator's CONFIRM (A-120); a restore minds the email only in its own tenant (A-80); the tool cards say the code's values (A-103) | ✅ replaced by 0.1.107 | snap `pre-train1-0-1-106` (deleted 2026-11-07) · `apt install fluxbee=0.1.105` + core-update of the spokes |
| **0.1.105** | 2026-11-07 | `f4d1c0f4` | motherbee + spokes (core): a line a WF relays between two people names where it went and, when its message names them, whom (A-161, [cognition-hands-v1](../docs/cognition-hands-v1.md) §12.9): the agent reads it as not said to it, and cognition's tagger and narrator read the same; an answer with no text says its `stop_reason` (A-162) | ✅ replaced by 0.1.106 | snap `pre-relayto-0-1-105` (deleted 2026-11-07) · `apt install fluxbee=0.1.104` + core-update of the spokes |
| **0.1.104** | 2026-11-06 | `1a3fe3ce` | motherbee + spokes (core): the rest of A-137 ([cognition-hands-v1](../docs/cognition-hands-v1.md) §12): the lines a WF relays between people reach the agent that takes the conversation back, cognition analyzes people's turns with no agent and their writers pay, episodes are public news on every hive and the agent's package carries them, and SY.wf-rules keeps a solution on one hive | ✅ replaced by 0.1.105 | snap `pre-news-0-1-104` (deleted 2026-11-07) · `apt install fluxbee=0.1.103` + core-update of the spokes |
| **0.1.103** | 2026-11-06 | `8af07c3d` | motherbee + spokes (core): SY.cognition keeps its state in its own file on its hive (A-158; [cognition-hands-v1](../docs/cognition-hands-v1.md) §11): closed items leave it, SY.storage drops the cognition tables, and the router's stream and the SDK lose `storage.cognition.*` | ✅ replaced by 0.1.104 | snap `pre-cogstate-0-1-103` (deleted 2026-11-06) · `apt install fluxbee=0.1.102` + core-update of the spokes |
| **0.1.102** | 2026-11-06 | `476bf231` | motherbee + spokes (core): A-137's feelings, each person's and fading with their own turns, read by the agent (A-155; [cognition-hands-v1](../docs/cognition-hands-v1.md) §10), the tagger's affect signals (A-157), the averages and `last_seen_at` (two of A-156's three), and a person with no name | ✅ replaced by 0.1.103 | snap `pre-feelings-0-1-102` (deleted 2026-11-06) · `apt install fluxbee=0.1.101` + core-update of the spokes |
| **0.1.101** | 2026-11-05 | `abf2a8d4` | motherbee + spokes (core): A-137's first part, who wrote each message and which turns cognition analyzes ([cognition-hands-v1](../docs/cognition-hands-v1.md)), at most 10 timers in a WF definition (A-153), a quiet WF conversation that closes on its own (A-154), and the empty `wf: {}` block | ✅ replaced by 0.1.102 | snap `pre-writer-0-1-101` (deleted 2026-11-06) · `apt install fluxbee=0.1.100` + core-update of the spokes |
| **0.1.100** | 2026-11-05 | `b052722b` | motherbee + spokes (core): the sprint's block 3, the message trace (A-138) with the frontdesk's identity note, SY.timer 1.3's `TIMER_LIST` in pages (A-151), Archi's confirm sentence once (A-152) | ✅ replaced by 0.1.101 | snap `pre-trace-0-1-100` (deleted 2026-11-05) · `apt install fluxbee=0.1.99` + core-update of the spokes |
| **0.1.99** | 2026-11-04 | `34cff128` | motherbee + spokes (core): the sprint's blocks 1 and 2, SY.timer 1.2 (A-115), Archi's staged turn (A-147), the router's VPN warning and protected verbs (A-146, A-100), `bundle_upload` (A-84), io.slack's download (A-85), io.web's handshakes and the edge's proxy (A-89, A-90) | ✅ replaced by 0.1.100 | snap `pre-sprint-0-1-99` (deleted 2026-11-05) · `apt install fluxbee=0.1.98` + core-update of the spokes |
| **0.1.98** | 2026-11-03 | `b8f41f1c` | motherbee + spokes (core): a workflow's name follows the runtime naming policy (A-145) | ✅ replaced by 0.1.99 | snap `pre-namefix-0-1-98` (deleted 2026-11-04) · `apt install fluxbee=0.1.97` + core-update of the spokes |
| **0.1.97** | 2026-11-03 | `b91260a5` | motherbee + spokes (core): SY.admin's executor dispatches a fixed step through the tool's own dispatch (A-144) | ✅ replaced by 0.1.98 (gate in course) | snap `pre-dispatchfix-0-1-97` (deleted 2026-11-03) · `apt install fluxbee=0.1.96` + core-update of the spokes |
| **0.1.96** | 2026-11-03 | `3726dca1` | motherbee + spokes (core): two fixes from WF Case 5's gate, SY.admin's executor and identity writes (A-144, A-143), and Archi's chat CONFIRM | ✅ replaced by 0.1.97 (gate in course) | snap `pre-gatefix-0-1-96` (deleted 2026-11-03) · `apt install fluxbee=0.1.95` + core-update of the spokes |
| **0.1.95** | 2026-11-03 | `a864b0aa` | motherbee + spokes (core); SY.architect's own model `claude-opus-5-5` (`ai.model`, CONFIG_SET); the motherbee's `hive.yaml` back on `claude-sonnet-5-5`; the old gate (WF gate0192, its agents and roles) removed | ✅ replaced by 0.1.96 (gate in course) | snap `pre-wfcase5-0-1-95` (deleted 2026-11-03) · `apt install fluxbee=0.1.94` + core-update of the spokes; SY.architect's `config.json` back to `{"ai":{"model":null}}` |
| **0.1.94** | 2026-11-02 | `9ee729a6` | motherbee + spokes (core); the gate's WF redeployed; `AI.pruebas@worker1`'s config set again without `runtime.immediate_memory`; later the motherbee's AI on `claude-opus-5-5` (Archi) | ✅ replaced by 0.1.95 | snap `pre-wfcase4-0-1-94` (deleted 2026-11-03) · `apt install fluxbee=0.1.93` + core-update of the spokes, then the gate's WF and AI.pruebas back on their 0.1.93 definition and config |
| **0.1.93** | 2026-10-30 | `c70ad89` | motherbee + spokes (core) | ✅ replaced by 0.1.94 | snap `pre-cogturns-0-1-93` (deleted 2026-11-02) · `apt install fluxbee=0.1.92` + core-update of the spokes |
| **0.1.92** | 2026-10-30 | `4e20b88` | motherbee + spokes (core) + the gate's `IO.slack.gate0192@motherbee` and its vault secret (tenant `pruebas`) | ✅ replaced by 0.1.93 | snap `pre-wfcase3-0-1-92` (deleted 2026-10-30) · `apt install fluxbee=0.1.91` + core-update of the spokes |
| **0.1.91** | 2026-10-30 | `75a6786` | motherbee + spokes (core) | ✅ replaced by 0.1.92 | snap `pre-wfcase1-0-1-91` (deleted 2026-10-30) · `apt install fluxbee=0.1.90` + core-update of the spokes |
| **0.1.90** | 2026-10-29 | `e6b07fc` | motherbee + spokes (core) | ✅ replaced by 0.1.91 | snap `pre-wfcase0-0-1-90` (deleted 2026-10-30) · `apt install fluxbee=0.1.89` + core-update of the spokes |
| **0.1.89** | 2026-10-28 | `694becc` | motherbee + spokes (core); the gate's ai.generic `0.1.88-gate1` deleted on the motherbee before the deploy (A-122) | ✅ replaced by 0.1.90 | snap `pre-tools2-0-1-89` (all 4 VMs) · `apt install fluxbee=0.1.88` + core-update of the spokes |
| **0.1.88** | 2026-10-27 | `d01ba1d` | motherbee + spokes (core) + ai.generic `0.1.88-gate1` published as `current` (gate) | ✅ replaced by 0.1.89 | snap `pre-reconcile-0-1-88` (all 4 VMs) · `apt install fluxbee=0.1.87` + core-update of the spokes |
| **0.1.87** | 2026-10-27 | `6772df4` | motherbee + spokes (core) | ✅ replaced by 0.1.88 | snap `pre-tools1-0-1-87` (all 4 VMs) · `apt install fluxbee=0.1.86` + core-update of the spokes; an AI node without `behavior.model` needs one first (see the entry) |
| **0.1.86** | 2026-10-27 | `89848d8` | motherbee + spokes (core) | ✅ replaced by 0.1.87 | snap `pre-tools-0-1-86` (all 4 VMs) · `apt install fluxbee=0.1.85` + core-update of the spokes, then each AI node's config back to the 0.1.85 shape (see the entry) |
| **0.1.85** | 2026-10-24 | `95bacd3` | motherbee + spokes (core) + `IO.web@motherbee` started (root tenant) | ✅ replaced by 0.1.86 | snap `pre-web-0-1-85` (all 4 VMs) · kill + purge `IO.web@motherbee`, then `apt install fluxbee=0.1.84` + core-update of the spokes |
| **0.1.84** | 2026-10-23 | `5be9e7b` | motherbee + spokes (core) + motherbee `hive.yaml` (`web:`) + the edge's certificate (two names) | ✅ replaced by 0.1.85 | snap `pre-door-0-1-84` (all 4 VMs) · `apt install fluxbee=0.1.83` + core-update of the spokes (`acme.json` and the edge's publications: see the entry) |
| **0.1.83** | 2026-10-22 | `5fb8dab` | motherbee + spokes (core) | ✅ replaced by 0.1.84 | snap `pre-hardening-0-1-83` (all 4 VMs) · `apt install fluxbee=0.1.82` + core-update of the spokes (the ingress keeps its SSH table: see the entry) |
| **0.1.82** | 2026-10-21 | `d2980cf` | motherbee + spokes (core) + `hive.yaml` (motherbee `acme:` + `reachability_url`, egress1 `reachability_url`) + the edge's certificate | ✅ replaced by 0.1.83 | snap `pre-acme-0-1-82` (all 4 VMs) · vault rollback of `edge_tls_fluxbee_ai` (the wildcard) · `apt install fluxbee=0.1.81` + core-update of the spokes |
| **0.1.81** | 2026-10-20 | `99b0d2a` | motherbee + spokes (core) + egress1 `hive.yaml` + DB | ✅ replaced by 0.1.82 | snap `pre-batch-0-1-81` (all 4 VMs) · `apt install fluxbee=0.1.80` + core-update of the spokes |
| **0.1.80** | 2026-10-17 | `58db83a` | motherbee + spokes (core) | ✅ replaced by 0.1.81 | snap `pre-batch-0-1-80` (all 4 VMs) · `apt install fluxbee=0.1.79` + core-update of the spokes |
| **0.1.79** | 2026-10-17 | `0bc85e3` | motherbee + spokes (core) | ✅ replaced by 0.1.80 | snap `pre-stream3-0-1-79` (all 4 VMs) · `apt install fluxbee=0.1.78` + core-update of the spokes |
| **0.1.78** | 2026-10-17 | `104fa65` | motherbee + spokes (core) | ✅ replaced by 0.1.79 | snap `pre-stream2b-0-1-78` (all 4 VMs) · `apt install fluxbee=0.1.77` + core-update of the spokes |
| **0.1.77** | 2026-10-17 | `377945a` | motherbee + spokes (core) | ✅ replaced by 0.1.78 | snap `pre-stream2a-0-1-77` (all 4 VMs); `apt` alone is not enough (see the entry) |
| **0.1.76** | 2026-10-16 | `e24e7d3` | motherbee + spokes (core) | ✅ replaced by 0.1.77 | snap `pre-stream1b-0-1-76` (all 4 VMs); `apt install fluxbee=0.1.75` loses the purged inbox (see the entry) |
| **0.1.75** | 2026-10-16 | `1c00389` | motherbee + spokes (core) | ✅ replaced by 0.1.76 | snap `pre-stream1a-0-1-75` (all 4 VMs); `apt` alone is not enough (see the entry) |
| **0.1.74** | 2026-10-16 | `0d69329` | motherbee + spokes (core) | ✅ replaced by 0.1.75 | snap `pre-sdk-0-1-74` (all 4 VMs) · `apt install fluxbee=0.1.73` |
| **0.1.73** | 2026-10-16 | `822f237` | motherbee + spokes (core) + `hive.yaml` (AI: Claude) | ✅ replaced by 0.1.74 | snap `pre-cognition-0-1-73` (all 4 VMs) · `apt install fluxbee=0.1.72` · `hive.yaml.pre-claude-0-1-72` |
| **0.1.72** | 2026-10-16 | `17e1edd` | motherbee + spokes (core) | ✅ replaced by 0.1.73 | snap `pre-sync-0-1-72` (all 4 VMs) · `apt install fluxbee=0.1.71` |
| **0.1.71** | 2026-10-16 | `73f7132` | motherbee + spokes (core) | ✅ replaced by 0.1.72 | snap `pre-gate-0-1-71` (all 4 VMs) · `apt install fluxbee=0.1.70` |
| **0.1.70** | 2026-10-16 | `c289bb3` | motherbee + spokes (core) | ✅ replaced by 0.1.71 | snap `pre-wf-0-1-70` (all 4 VMs) · `apt install fluxbee=0.1.69` |
| **0.1.69** | 2026-10-15 | `6f03310` | motherbee + spokes (core) | ✅ replaced by 0.1.70 | snap `pre-teardown-0-1-69` (all 4 VMs) · `apt install fluxbee=0.1.68` |
| **0.1.68** | 2026-10-15 | `1b47c3e` | motherbee + spokes (core) | ✅ replaced by 0.1.69 | snap `pre-cloud-0-1-68` (all 4 VMs) · `apt install fluxbee=0.1.67` |
| **0.1.67** | 2026-10-15 | `e85041d` | motherbee + spokes (core) | ✅ live | snap `pre-tenant-0-1-67` (all 4 VMs) · `apt install fluxbee=0.1.66` (the v4 migration is not undone; see the entry) |
| **0.1.66** | 2026-10-14 | `96c8d77` | motherbee + spokes (core) | ✅ live | snap `pre-secrets-0-1-66` (all 4 VMs) · `apt install fluxbee=0.1.65` |
| **0.1.65** | 2026-10-14 | `8b3e951` | motherbee + spokes (core) | ✅ live | snap `pre-scope-0-1-65` (all 4 VMs) · `apt install fluxbee=0.1.64` (the v3 migration is not undone; see the entry) |
| **0.1.64** | 2026-10-13 | `f8c73c5` | motherbee + spokes (core) | ✅ live | snap `pre-cloud-0-1-64` (all 4 VMs) · `apt install fluxbee=0.1.63` |
| **0.1.63** | 2026-10-06 | `aab0e3e` | motherbee + spokes (core) | ✅ live | snap `pre-a58-0-1-63` (all 4 VMs) · `apt install fluxbee=0.1.62` (HANDBOOK, rollback under 0.1.63) |
| **0.1.62** | 2026-10-06 | `b4868e8` | motherbee + spokes (core) | ✅ live | snap `pre-identity-0-1-62` (all 4 VMs) · `apt install fluxbee=0.1.61` (spokes first) |
| **0.1.61** | 2026-10-06 | `e0f2d04` | motherbee + spokes (core) | ✅ live | snap `pre-ssh-0-1-61` (all 4 VMs) · `apt install fluxbee=0.1.60` |
| **0.1.60** | 2026-10-06 | `0af5469` | motherbee + spokes (core) | ✅ live | snap `pre-hardening-0-1-60` (all 4 VMs) · `apt install fluxbee=0.1.59` |
| **0.1.59** | 2026-10-06 | `f60eb92` | motherbee + spokes (core) | ✅ live | snap `pre-stage3-0-1-59` (all 4 VMs) · `apt install fluxbee=0.1.58` |
| **0.1.58** | 2026-10-06 | `9d9391b` | motherbee + spokes (core) | ✅ live | snap `pre-stage2-0-1-58` (all 4 VMs) · `apt install fluxbee=0.1.57` |
| **0.1.57** | 2026-10-06 | `a703b89` | motherbee + spokes (core) | ✅ live | snap `pre-posture-0-1-57` (all 4 VMs) · `apt install fluxbee=0.1.56` |
| **0.1.56** | 2026-10-03 | `715143a` | motherbee + spokes (core) | ✅ live | snap `pre-root-io-0-1-56` (all 4 VMs) · `apt install fluxbee=0.1.55` |
| **0.1.55** | 2026-10-02 | `975fd34` | motherbee + spokes (core) | ✅ live | snap `pre-root-tenant-0-1-55` (all 4 VMs) · `apt install fluxbee=0.1.54` |
| **0.1.54** | 2026-10-02 | `6aa4ee4` | motherbee + spokes (core) | ✅ live | snap `pre-gov-0-1-54` (all 4 VMs) · `apt install fluxbee=0.1.53` |
| **0.1.53** | 2026-10-02 | `26edcd5` | motherbee + spokes (core) | ✅ live | snap `pre-ai-generic-0-1-53` (all 4 VMs) · `apt install fluxbee=0.1.52` |
| **0.1.52** | 2026-10-02 | `0670184` | motherbee + spokes (core) | ✅ live | snap `pre-cleanup-0-1-52` (all 4 VMs) · `apt install fluxbee=0.1.51` |
| **0.1.51** | 2026-10-01 | `f7c2520` | motherbee + spokes (core) | ✅ live | snap `pre-query-decode-0-1-51` (all 4 VMs) · `apt install fluxbee=0.1.50` |
| **0.1.50** | 2026-10-01 | `aa206ac` | motherbee + spokes (core) | ✅ live | snap `pre-config-routes-0-1-50` (all 4 VMs) · `apt install fluxbee=0.1.49` |
| **0.1.49** | 2026-10-01 | `c921d1a` | motherbee + spokes (core) | ✅ live | snap `pre-architect-fixes-0-1-49` (all 4 VMs) · `apt install fluxbee=0.1.48` |
| **0.1.48** | 2026-10-01 | `1486807` | motherbee + spokes (core) | ✅ live | snap `pre-architect-opa-0-1-48` (all 4 VMs) · `apt install fluxbee=0.1.47` |
| **0.1.47** | 2026-10-01 | `652a9c9` | motherbee + spokes (core) | ✅ live | snap `pre-opa-blockB-0-1-47` (all 4 VMs) · `apt install fluxbee=0.1.46` |
| **0.1.46** | 2026-10-01 | `7371ca1` | motherbee + spokes (core) | ✅ live | snap `pre-opa-blockA-0-1-46` (all 4 VMs) · `apt install fluxbee=0.1.45` |
| **0.1.45** | 2026-10-01 | `d6505ff` | motherbee + spokes (core) | ✅ live | snap `pre-rule3-0-1-45` (all 4 VMs) · `apt install fluxbee=0.1.44` |
| **0.1.44** | 2026-10-01 | `005d0ad` | motherbee + spokes (core) | ✅ live | snap `pre-src-binding-0-1-44` (all 4 VMs) · `apt install fluxbee=0.1.43` |
| **0.1.43** | 2026-09-30 | `2b3abb2` | motherbee + spokes (core) | ✅ live | snap `pre-sync-hint-0-1-43` (all 4 VMs) · `apt install fluxbee=0.1.42` |
| **0.1.42** | 2026-09-30 | `ac301d9` | motherbee + spokes (core) | ⚠️ superseded (scan rejected: 0.1.43) | snap `pre-opa-fixes-0-1-42` (all 4 VMs) · `apt install fluxbee=0.1.41` |
| **0.1.41** | 2026-09-30 | `72891be` | motherbee + spokes (core) + `hive.yaml` | ✅ live | snap `pre-opa-global-0-1-41` (all 4 VMs) · `apt install fluxbee=0.1.40` + `hive.yaml.pre-opa-0-1-41` |
| **0.1.40** | 2026-09-30 | `43dd174` | motherbee + spokes (core) | ✅ live | snap `pre-purge-order-0-1-40` (all 4 VMs) · `apt install fluxbee=0.1.39` |
| **0.1.39** | 2026-09-30 | `85c3244` | motherbee + spokes (core) | ✅ live | snap `pre-identity-baseline-0-1-39` (all 4 VMs) · `apt install fluxbee=0.1.38` |
| **0.1.38** | 2026-09-30 | `20baec3` | motherbee + spokes (core) | ✅ live | snap `pre-wf-mirror-0-1-38` (all 4 VMs) · `apt install fluxbee=0.1.37` |
| **0.1.37** | 2026-09-30 | `c09f9af` | motherbee + spokes (core) | ✅ live | snap `pre-followups-0-1-37` (all 4 VMs) · `apt install fluxbee=0.1.36` |
| **0.1.36** | 2026-09-30 | `d24e3aa` | motherbee + spokes (core) | ✅ live | snap `pre-config-scope-0-1-36` (all 4 VMs) · `apt install fluxbee=0.1.35` |
| **0.1.35** | 2026-09-28 | `5f1cd71` | motherbee | ✅ live | snap `pre-teardown-fix-0-1-35` · `apt install fluxbee=0.1.34` |
| **0.1.34** | 2026-09-28 | `60331c5` | motherbee + spokes (core) | ⚠️ superseded (teardown regression) | snap `pre-lifecycle-0-1-34` (all 4 VMs) · `apt install fluxbee=0.1.33` |
| **0.1.33** | 2026-08-28 | `01b6266` | motherbee | ✅ live | `apt install fluxbee=0.1.32` |
| **0.1.32** | 2026-08-27 | `90fd64a` | motherbee | ✅ live | `apt install fluxbee=0.1.31` |
| **0.1.31** | 2026-08-26 | `862c5e4` | motherbee | ✅ live | snap `pre-rpc-poison-fix-0-1-31` · `apt install fluxbee=0.1.30` |
| **0.1.30** | 2026-08-26 | `af1166a` | motherbee | ✅ live | snap `pre-liveness-fix-0-1-30` · `apt install fluxbee=0.1.29` |
| **0.1.29** | 2026-08-25 | `a0bc25d` | motherbee | ✅ live | snap `pre-cloud-readpath-0-1-29` · `apt install fluxbee=0.1.28` |
| **0.1.28** | 2026-08-25 | `1438737` | motherbee | ✅ live | snap `pre-frontdesk-configplane-0-1-28` · `apt install fluxbee=0.1.27` |
| **0.1.27** | 2026-08-25 | `000de90` | motherbee | ✅ live | snap `pre-frontdesk-autonomous-0-1-27` · `apt install fluxbee=0.1.26` |
| **0.1.26** | 2026-08-24 | `cb2d192` | motherbee | ✅ live | snap `pre-frontdesk-handoff-0-1-26` · `apt install fluxbee=0.1.25` |
| **0.1.25** | 2026-08-24 | `69b1c46` | motherbee | ✅ live | snap `pre-observability-0-1-25` · `apt install fluxbee=0.1.24` |
| **0.1.24** | 2026-08-23 | `35349ef` | motherbee | ✅ live | snap `pre-vault-guard-0-1-24` · `apt install fluxbee=0.1.23` |
| **0.1.23** | 2026-08-21 | `4abad1b` | motherbee | ✅ live | snap `pre-cloud-actions-0-1-23` · `apt install fluxbee=0.1.22` |
| **0.1.22** | 2026-08-20 | `15fc77f` | motherbee | ✅ live | snap `pre-frontdesk-0-1-22` · `apt install fluxbee=0.1.21` |
| **0.1.21** | 2026-08-20 | `2f60403` | motherbee | ✅ live | snap `pre-register-human-0-1-21` · `apt install fluxbee=0.1.20` |
| **0.1.20** | 2026-08-20 | `1297ba7` | motherbee | ✅ live | snap `pre-router-0-1-20` · `apt install fluxbee=0.1.19` |
| ≤ 0.1.19 | (pre-ledger) | — | motherbee | historical, not recorded | the apt repo keeps 0.1.0 … 0.1.19 |

> This ledger starts at **0.1.20** (the first version formally recorded). The earlier
> versions (0.1.0–0.1.19) were deployed without a ledger; the apt repo on fb-build keeps them
> (`dpkg-scanpackages -m`) for rollback, but their detail lives in the logbook, not here.

---

## 0.1.136 — The AI SDK alone: status, config and the handoff beside the LLM turns, and a busy answer past the queue (A-257)

- **Date:** 2026-11-12 (ART) · **Previous version:** 0.1.135 · **Commit:** `ba2addb2`.
- **Scope:** motherbee and the three spokes, through the installation's update. The AI SDK's
  runtime runs in ai.generic and SY.frontdesk.gov.
- **Why:** A-257 (found 2026-11-10 by A-223's triage and io.api's builder): with its 4 workers on
  LLM turns the AI runtime read no message, so the frontdesk's handoff waited behind them past
  io.api's 10 s, and a status, help or config request to any AI node on the runtime waited in the
  same queue. The sweep's end, as the operator set it on 2026-11-11 (FINDINGS' board, item 24); the
  AI SDK is shared code and ships alone ([METHOD](logbook/METHOD.md) §9). Built and reviewed for
  the planned 0.1.135; 0.1.134 went to the orchestrator's hotfix (A-314) and 0.1.135 to the router,
  so the AI SDK is 0.1.136.
- **What changed (`crates/fluxbee_ai_sdk/`, with the texts that describe it):**
  - **A-257 (`b694af08`):** the runtime's dispatch loop never waits for a worker. What the AI nodes
    answer without their model (any `system` or `admin` message, which ai.generic and the frontdesk
    take to their control plane, and a frontdesk handoff from an IO node) starts as it is read, on
    a direct lane of up to 64 places (`direct_capacity`); one more is dropped, neither run nor
    answered. Every other message is an LLM turn: it takes a free worker or waits in the order it
    came, up to 128 (`queue_capacity`), at most 4 at once (`worker_pool_size`); the next one is
    answered `ai_runtime_busy` (`retryable`: it never ran) and never run. A tap copy takes no
    place. The metrics line counts `turns_queued`, `turns_waiting`, `turns_refused` and
    `direct_dropped`, and a WARN line names a refusal. With a free worker every message runs as it
    comes, as before.
  - **Its review (`bb607205`):** a handoff runs on the direct lane only in the shape the frontdesk
    parses (gov-common's `FrontdeskHandoffPayload`); one it cannot parse is a turn, since the
    frontdesk calls its model for it. The direct lane has its own deadline, 30 s
    (`direct_handler_timeout`), and its user messages, the handoffs and the busy answers, take at
    most 8 of its 64 places (`direct_user_capacity`), system and admin messages any; a busy answer
    with no place is not sent and is counted (`busy_answers_dropped`). After a fatal end (a reply
    too large, a panic) no waiting turn starts. The busy answer's trace act is `busy`; the
    refusals' and the router notices' WARN lines come once, then at most once every 10 s with how
    many since. `tests/frontdesk_handoff_contract.rs` holds the runtime's copy of the handoff's
    shape to gov-common's.
  - **The review of `coa/aisdk-136` (`ba2addb2`, texts only):** Archi's handbook and SY.admin's and
    SY.orchestrator's pages say an AI node answers status and help at once however busy only on
    ai.generic 0.1.136 or later, and that one pinned to an older version may still miss the 3 s;
    the AI nodes runbook and ai.generic's README say what an older version does; the AI SDK's
    README writes down as a known limit that ai.generic runs its model for a well-formed handoff on
    the direct lane (FINDINGS A-257); SY.admin gives the node's `CONFIG_RESPONSE` 25 s within its
    action's 30 s, not 30.
- **Build and review:** `coa/aisdk-135`'s two commits (`6320eeb1` A-257, `b4071412` its review),
  built and reviewed for the planned 0.1.135, CI green at `b4071412`; rebased onto daily as
  `coa/aisdk-136` (`539cd9fc`, `9403d7f3`) with no conflict and the same patch ids (the branch's
  three docs hunks moved without overlapping daily). Its review: ship, nothing in the code wrong;
  its fix `8c339ab2`, the texts. CI green on `9403d7f3` and `8c339ab2`. Rebased onto daily
  `b1545b22` with the subjects re-prefixed to the live `[0.1.135]` (`b694af08`, `bb607205`,
  `ba2addb2`); daily fast-forwarded to `ba2addb2`, its CI green.
- **The deploy:** build on fb-build 04:10:46–04:32:48Z (22.0 min), publish in 15 s, `ops deploy
  0.1.136` from 05:36:49Z in 313 s (snapshots 36.8 s, the motherbee's apt 159.1 s, its admin on
  the version 48.8 s, the spokes answering 9.6 s, the update's call 25.0 s, the hives on the target
  15.8 s, health 7.0 s); snapshot `pre-aisdk-0-1-136`, `pre-router-0-1-135` dropped. On the target
  after the update's call: egress1 34.5 s, ingress1 34.5 s, worker1 38.9 s, the motherbee 4.7 s; by
  their own records the spokes settled +25.3 s (egress1), +29.6 s (ingress1), +32.9 s (worker1).
  Health clean (`wandrops=0` on every hive, in `HEALTH_OK` since 0.1.135). `AI.pruebas@worker1`
  was relaunched on ai.generic 0.1.136 at 05:40:31Z, its *"ai runtime started"* line naming
  `worker_pool_size=4 queue_capacity=128 direct_capacity=64 direct_user_capacity=8
  direct_handler_timeout_ms=30000`; sy-frontdesk-gov restarted at 05:40:36Z and logged the same
  line at 05:40:42Z.
- **Checked on 8.x (the gate, gate0236, against a baseline on 0.1.135 the same night):**
  - **The method:** the 4 workers of `AI.pruebas@worker1` and of `SY.frontdesk.gov@motherbee` held
    by an nft drop of each node's own HTTPS (table `inet gate0236`, scoped to the unit's cgroup and
    tcp 443), so each held model call hung in its connect and nothing reached the provider. The
    drop held each turn 92–93 s, not the planned ~300 s: the provider client gave up after its
    connect attempts (`provider_status` None), and the node answered its fixed provider-failure
    text. The turns to AI.pruebas carried an agent's ILK (`AI.gate0233@worker1`'s) as `src_ilk`,
    so cognition left them alone; the fillers were user messages with no `thread_id`, answered
    `invalid_payload` once a worker frees, with no model call.
  - **On 0.1.135 (05:34–05:36Z), the same hold:** AI.pruebas' status 2.449 s
    `ORCHESTRATOR_INFERRED`, help 3.356 s null (the node did not answer within 3 s), `CONFIG_GET`
    25.168 s `504 TIMEOUT`; the frontdesk's status 2.374 s inferred, `CONFIG_GET` 25.158 s
    `TIMEOUT`; a handoff to the frontdesk got no answer. The held turns answered at 93.0–93.1 s. No
    model call.
  - **On 0.1.136, before the hold:** every hive on 0.1.136; the probes 0.26–0.58 s,
    `NODE_REPORTED`. With the workers free (1 model call): a question to AI.pruebas answered in
    1.50 s; 10 user messages without a thread answered `invalid_payload` in 3–14 ms; a handoff
    (operation `gate0236_probe`) answered in 12 ms, `invalid_request` *"The requested operation is
    not supported."*, act `[identity_not_assigned, invalid_request]`; `turns_queued` unchanged.
    Status and help took 1.1 s in this step (0.5 s in the step before, 0.36 s in the one after the
    hold): a transient.
  - **Held and filled (no model call):** both nodes' 4 workers held from 05:43:51Z to 05:45:23Z
    (92.4–92.7 s).
    - AI.pruebas: 128 fillers waited, none answered during the hold; the 129th and 6 more, 0.5 s
      apart, answered `ai_runtime_busy` at once (7–12 ms), `retryable` true, the trace's last act
      `busy`, the text *"The AI runtime is busy: its 4 model workers are all taken and 128 more
      messages ..."*. The journal: *"ai runtime refused an LLM turn"* (`refused=1 turns_refused=1`)
      5 ms after the first send, then the burst's end (`refused=6 turns_refused=7`) with the next
      metrics line (`turns_waiting=128 turns_queued=128 direct_dropped=0
      busy_answers_dropped=0`).
    - The frontdesk: a malformed handoff (`subject.phone` a number) waited as a turn and 127
      fillers waited; the next one answered `ai_runtime_busy` at once (7 ms), and a well-formed
      handoff answered at once (6 ms); its metrics `turns_waiting=128 turns_refused=1
      direct_dropped=0`.
    - With the queues full: AI.pruebas' status 0.924 s `NODE_REPORTED`, help 0.920 s (its
      catalog), `CONFIG_GET` 0.328 s, a `CONFIG_SET` of its own config 0.091 s (ok, its version
      unchanged); the frontdesk's status 0.866 s and `CONFIG_GET` 0.377 s; the handoff probe 4 ms.
      Every busy answer and probe fell inside the span when all 4 holders ran (AI.pruebas 7 of 7
      and 8 of 8, the frontdesk 2 of 2 and 6 of 6).
    - The drain: once the held turns answered, AI.pruebas' 128 fillers answered `invalid_payload`
      within 0.135 s and the frontdesk's 127 within 0.165 s; the malformed handoff answered
      `invalid_payload`. Answers came up to 4 places (AI.pruebas) and 25 (the frontdesk) behind a
      later filler's: the queue starts the turns in order on 4 workers, and the handlers take
      different times (an observation, no fault).
  - **After (1 model call):** the probes 0.30–0.38 s; the question again 1.33 s; the handoff 7 ms;
    the metrics `turns_waiting=0`; `NRestarts` 0 on both units, no *"ai runtime stopped"* and no
    restart; health clean (`wandrops=0`).
  - **The cost:** 2 usage records, both on the gate's threads: AI.pruebas' 2 calls ok (one with the
    workers free, one after), 0 abandoned; SY.frontdesk.gov none; SY.cognition no record; nothing
    on other threads.
  - **Found:** from the review of `coa/aisdk-136`, A-324 (SY.admin's `node_help` note and three
    orchestrator comments still say a busy AI node may miss the deadline) and A-325 (ai.generic
    answers each `VAULT_SECRET_CHANGED` broadcast with an error SY.vault drops), both low; the
    handoff rule's known limit, written into A-257.
- **PROD after the gate:** all four hives on 0.1.136; snapshot `pre-aisdk-0-1-136` on each VM. The
  gate's units `g36q`, `g36h`, `g36f` and `g36a` stopped, no table `inet gate0236` left, and
  `/root/gate0236` removed on worker1 and the motherbee.
- **Rollback:** the snapshot `pre-aisdk-0-1-136` (all 4 VMs), or `apt install fluxbee=0.1.135` +
  0.1.135's `update_installation`: nothing one-way; A-257 comes back.

## 0.1.135 — The router alone: no WAN broadcast sent back, and FIB builds stored in order (A-272, A-268)

- **Date:** 2026-11-11 (ART; deployed 2026-11-12 UTC) · **Previous version:** 0.1.134 · **Commit:**
  `9a6ffb6d`.
- **Scope:** motherbee and the three spokes, through the installation's update.
- **Why:** two of the router's three lows left by 0.1.132 (A-269 goes with its next change): A-272,
  found by its gate (every router sent a WAN broadcast back to the peer it came from, and those
  echoes filled the `foreign_source` lines), and A-268, found by its review (two FIB builds at once
  could store the older one last). The sweep's end, as the operator set it on 2026-11-11 (FINDINGS'
  board, item 24); the router is shared code and ships alone ([METHOD](logbook/METHOD.md) §9).
  Built and reviewed as 0.1.134; that number went to the orchestrator's hotfix (A-314), so the
  router is 0.1.135.
- **What changed (`src/router/` only):**
  - **A-272 (`5de07b07`):** split horizon: a broadcast that came over the WAN goes on to every WAN
    peer but the one it came from. The hub hands a spoke's broadcast to every other spoke and never
    back to it, a spoke hands one from the hub to no hive, and over IRP a router that is not the
    gateway no longer hands the gateway's copy back to it. A broadcast that entered at this router
    goes to every peer, as before; the TTL rules, the broadcast cache, A-222's full links and
    A-246's single answer are unchanged. **Its review (`23cd0d3e`):** routing-and-policy.md §4 and
    §10 name, as known limits of the several-routers mode (unsupported), what is left of a
    broadcast's path over IRP: from a router that is not the gateway it needs ttl >= 4 to cross the
    WAN, and it reaches the gateway more than once, which keeps the first copy.
  - **A-268 (`625413a4`):** two FIB builds at once are stored in the order they began: each build
    takes a generation under the FIB's lock before it reads anything, and stores its FIB only if no
    newer build stored one first, so a build that read before a change never undoes the change's
    rebuild (A-265's, as an LSA is applied, or the stale check's). **Its review (`b8521d26`):** the
    docs name every build that runs at once (a node's connect and disconnect, a sibling's frame,
    the config heartbeat and a spoke's reachability advertisement among them).
  - **The renumbering (`10a2b836`):** the router's docs, comments and tests say "since 0.1.135" and
    "until 0.1.135" where they said 0.1.134 (19 lines in 4 files). **Its review (`9a6ffb6d`):**
    rt-gateway.md's table of what `lab/ops.py health` counts gets its `wandrops` row, and it and
    operations.md say the field is printed and kept out of the check until every hive runs 0.1.135.
  - **Lab, with these records:** `ops.py health` checks `wandrops=0` with the other six fields
    (`HEALTH_OK`), since every hive runs 0.1.135.
- **Build and review:** `coa/router-134`'s four commits, built and reviewed as 0.1.134 on
  2026-11-11 (`c95c2281` A-272, `5e54d626` A-268, `f33b62ba` A-268's review, `440930f5` A-272's
  review), rebased onto daily `c14a8b8d` as `coa/router-135` with no conflict and the same patch ids
  (`5de07b07`, `625413a4`, `b8521d26`, `23cd0d3e`), then the renumbering and its review's fix. The
  review: ship (nothing high or medium; `range-diff` `=` for the four commits). CI: rust-tests green
  on `10a2b836` (the workspace's tests and clippy), the guards green on `9a6ffb6d`; daily
  fast-forwarded to `9a6ffb6d`, its CI green (rust-tests, guards).
- **The deploy:** build on fb-build 02:08:50–02:29:56Z (21.1 min), publish in 17 s, `ops deploy
  0.1.135` from 02:30:38Z in 285 s (snapshots 31.2 s, the motherbee's apt 119.8 s, its admin on the
  version 62.0 s, the spokes answering 10.7 s, the update's call 28.3 s, the hives on the target
  19.1 s, health 5.5 s); snapshot `pre-router-0-1-135`, `pre-orch-0-1-134` dropped. On the target
  after the update's call: egress1 32.5 s, ingress1 46.3 s, worker1 46.3 s, the motherbee 4.2 s; by
  their own records the spokes settled +27.8 s (egress1), +31.7 s (ingress1), +38.2 s (worker1).
  The routers restarted at 02:33:33Z (the motherbee), 02:34:39Z (egress1), 02:34:44Z (ingress1) and
  02:34:51Z (worker1); the last drop line was the motherbee's, at 02:33:48Z. The deploy's health,
  counted from its start, gave `wandrops=1` on the motherbee: the mixed window, the 0.1.134 spokes
  still echoing.
- **Checked on 8.x (the gate, against a baseline on 0.1.134 the same night):**
  - **A-272:** a receiver `IO.gate0235r` on each of the four hives, and on each a sender
    `IO.gate0235s` sending a broadcast (ttl 16, `meta.target` `IO.gate0235*`) every 15 s.
    - **On 0.1.134** (02:10:17–02:14:05Z, 16 per hive): every receiver got each broadcast once
      (16/16 from each hive at each hive), and the routers logged the echoes as *"router dropped a
      WAN frame"* (`foreign_source`): the motherbee 7 lines (`peer_hive` worker1 and egress1, its
      own sender's broadcasts coming back), worker1, ingress1 and egress1 6 lines each
      (`peer_hive` motherbee, each spoke's own sender's coming back from the hub). Lines, not
      frames: a router logs at most one every 30 s. Earlier on 0.1.134, one A-246 run (two
      broadcasts from the motherbee) gave one line on the motherbee, `reject_count` 37.
    - **On 0.1.135** (02:37:02–02:46:13Z, 32 per hive, from 2 min after the last router restart):
      every receiver got each broadcast exactly once (32/32 from each hive at each hive, none
      missed, none twice), and no hive logged a drop line.
  - **A-268:** no live trigger for its race (its unit test covers it); through A-265's check, three
    runs: a node connecting on worker1 was reached by name from ingress1, the first message
    delivered 0.05 s, 0.15 s and 0.14 s after its connect (0.1.132: 0.33 s); the messages sent
    before the connect answered `NODE_NOT_FOUND` from ingress1's router, as expected.
  - **The smoke (A-314's lesson):** `start_node` of the stopped `AI.gate0233@worker1`, whose unit
    did not exist: ok in 1.3 s, `RUNNING` at 11 s (`DEGRADED` at first), killed without purge:
    `STOPPED`, its ILK kept. A `run_node` of it answered `NODE_ALREADY_EXISTS`: the node is the
    gate's, kept stopped with its ILK since 0.1.133's gate. The `kill_node` guard:
    `SY.gate0233@motherbee` answered `INVALID_REQUEST`. ai.generic's use: only
    `AI.pruebas@worker1` (0.1.135), read in 1.8 s.
  - **Health:** `ops.py health --since 02:38`: every hive `failed=0 denials=0 srcdrops=0
    streamlost=0 usagelost=0 usagerej=0 wandrops=0`.
- **PROD after the gate:** all four hives on 0.1.135; snapshot `pre-router-0-1-135` on each VM. The
  gate's throwaway clients (`gate0232`, `gate0235`): their directories and units gone on every hive.
  `AI.gate0233@worker1` stays stopped with its ILK (it published 0.1.133's gate pages).
- **Rollback:** the snapshot `pre-router-0-1-135` (all 4 VMs), or `apt install fluxbee=0.1.134` +
  0.1.134's `update_installation`: nothing one-way; the WAN echoes come back, and now that
  `HEALTH_OK` checks `wandrops=0` the hives that drop them fail health: the motherbee whenever a
  node broadcasts over the WAN (each spoke sends back to the hub every broadcast the hub sent it,
  the motherbee's nodes' and the other spokes' alike), a spoke when its own nodes do (the hub sends
  them back to it).

## 0.1.134 — The orchestrator alone: a unit systemd does not have is not running (A-314)

- **Date:** 2026-11-11 (ART) · **Previous version:** 0.1.133 · **Commit:** `8539bc64`.
- **Scope:** motherbee and the three spokes, through the installation's update.
- **Why:** 0.1.133's gate found A-314 (🔴) at 22:41Z: on the hives' systemd 255, `systemctl
  is-active` exits 4 for a unit systemd does not have, which 0.1.133 read as no answer. No node
  could be launched, a stopped node whose unit was gone could be neither started nor purged and
  read `UNKNOWN`, and the reconcile launched no such node. The call, on the operator's standing
  authority over 8.x: fix it forward as 0.1.134, the orchestrator alone, instead of rolling back.
  The releases planned after it move by one: the router 0.1.135, the AI SDK 0.1.136, the sweep's
  binaries 0.1.137.
- **What changed (`src/bin/sy_orchestrator.rs` only):**
  - **A-314 (`7964c265`):** `systemctl is-active`'s exit 4 (every unit it names not-found, since
    systemd 253) reads as not running, as exit 3 does; exit 1 and a killed check stay no answer.
  - **Its review (`8539bc64`):** with a missing unit read as not running, a `kill_node` with purge
    or a `remove_node_instance` of an `SY.*` or `RT.*` name (a core node, whose `fluxbee-node-*`
    unit never exists) would remove the running core node's state (SY.timer's `timers.db`,
    SY.identity's config and secrets) and answer ok, as 0.1.132 did. Both now refuse such a name
    (`INVALID_REQUEST`) before anything is done, as `run_node` does.
- **Build and review:** `coa/orch-134`; a two-lens review (systemd's source at v252, v253, v255 and
  v258; every caller of the check): ship. CI green on `8539bc64`; daily fast-forwarded to it.
- **The deploy:** build 23:19:04–23:27:51Z (8.8 min), publish in 13 s, `ops deploy 0.1.134` at
  23:28:25Z in 259 s (snapshots 33.5 s, the motherbee's apt 137.6 s, its admin on the version
  32.5 s, the spokes answering 9.2 s, the update's call 22.2 s, the hives on the target 9.5 s,
  health 5.4 s); snapshot `pre-orch-0-1-134`, `pre-sweep-0-1-133` dropped. On the target after the
  update's call: egress1 26.1 s, ingress1 26.1 s, worker1 30.3 s, the motherbee 4.2 s; the spokes
  settled by their own records +21.8 s (ingress1), +23.0 s (egress1), +25.7 s (worker1). Health
  clean (`wandrops=1`: A-272's echo).
- **Checked on 8.x (23:33Z):**
  - A-314: `run_node` of `AI.gate0233@worker1` ok and its kill (ILK kept) ok, 9.8 s in all; the
    three `AI.gate0192*@worker1` nodes and `AI.gate0233` read `STOPPED` (`UNKNOWN` on 0.1.133);
    ai.generic's use lists only `AI.pruebas@worker1` (0.1.134), the stopped gate0192 nodes no
    longer counted, skipped none, `hives_without_runtimes` egress1 (egress) and ingress1 (ingress),
    read in 1.4 s.
  - The review's guard: `kill_node` of `SY.gate0233@motherbee` answered `INVALID_REQUEST` *"SY.*
    nodes are core nodes: they run under their own unit and are never stopped, purged or removed
    as a managed node"*.
  - Before the deploy no node on the motherbee or worker1 had `relaunch_on_boot` true with its unit
    gone, so 0.1.134's reconcile launched nothing unexpected.
  - 0.1.133's gate went on over 0.1.134 (its entry below), since 0.1.133 could launch no node:
    from 23:34Z A-260's and A-274's pages, published as `AI.gate0233@worker1`; A-281 past its
    first read of a runtime's use (its refusals, the lanes with A-308, its workflows, its
    `teardown_node` of `AI.gate0233@worker1`); A-270's `WF.gate0233@worker1`; A-267's and A-271's
    self-restarts at 23:47–23:50Z; A-191's throwaway worker, joined and removed twice from 23:51Z;
    and A-241's pass, an hour after worker1's orchestrator last started (23:48:30Z).
- **PROD after the gate:** all four hives on 0.1.134; worker1 behind egress1; the throwaway worker
  `gate0233` removed (its 6 system ILKs retired) and VM 104 destroyed; snapshot `pre-orch-0-1-134`
  on each VM. Left by the gate: `AI.gate0233@worker1`, killed with its ILK kept (A-281's check
  refused its teardown), and the four files it planted in worker1's `blob/staging` for A-241 (what
  the pass leaves of them is in 0.1.133's entry). Gone: `WF.gate0233@worker1` (deleted with force),
  the workflows `gate0233m`, `gate0233n` and `gate0233w` with their versions, the five pages, the
  gate's Archi session, and io.slack 0.1.14 (unused; removed by A-281's check).
- **Rollback:** the snapshot `pre-orch-0-1-134` (all 4 VMs), or `apt install fluxbee=0.1.133` +
  0.1.133's `update_installation`, which brings A-314 back; to 0.1.132 by apt (nothing one-way
  found: the system ILKs 0.1.133 retired stay retired, harmless to 0.1.132).

## 0.1.133 — The sweep's end, one branch per binary

- **Date:** 2026-11-11 (ART) · **Previous version:** 0.1.132 · **Commit:** `45c736e4`.
- **Scope:** motherbee and the three spokes, through the installation's update.
- **Why:** the sweep's end (the operator, 2026-11-11; FINDINGS' board, item 24): the third batch's
  items and what their builders and reviews brought in, one branch per binary, none of them shared
  code ([METHOD](logbook/METHOD.md) §9). The operator's calls: A-240 and A-241 (2026-11-10, the
  sweep); A-191, an ILK is history, so the system retires one and never deletes it; A-255 checked
  live, its new token handed over by a file's path.
- **What changed:**
  - **SY.identity and SY.admin:** A-191, a system ILK is retired (the delete model's reversible
    mark), never deleted on its own: SY.identity marks at its start the system ILKs of the SY
    nodes its release no longer runs on its hive (SY.policy's); SY.admin's new `retire_hive_ilks`,
    and `remove_hive` once the hive is gone (`system_ilks_retired`), mark a removed hive's through
    SY.identity's `ILK_RETIRE_HIVE`; only the operator's explicit purge removes a retired one; the
    primary drops every publish of a hive the registry no longer lists, so a removed hive cannot
    undo its retirement (the review). A-232: SY.identity's ILK and tenant listings refuse a cursor
    not theirs `INVALID_CURSOR`, naming what it takes, never the cursor.
  - **SY.admin:** A-255, `publish_cloud_endpoint` applies the edge it records to the running
    IO.cloud (a `CONFIG_SET` of its live config at its `config_version` + 1, tried once more when
    stale or unanswered; none when the live config names that edge already, as on a rotation) and
    answers `edge_applied`, with a warning when IO.cloud did not take it;
    the restart steps 0.1.131 wrote came out. A-260 and A-274: the expiry sweep and a take-down call
    the edge and IO.blob outside the publication lock, and write each state only while the page's
    row is still at the state they read. A-281: `remove_runtime_version` removes a version only once
    every hive that may run it was read and no node runs on it (409 `RUNTIME_USAGE_UNKNOWN` naming
    each hive and why; `RUNTIME_IN_USE` naming each node's hive and state), and `teardown_node`
    waits (`INVENTORY_UNAVAILABLE`) for a node whose hive the LSA holds stale. A-285: the help says
    what 0.1.133 answers (`SERVICE_FAILED` with `retryable`, `UNKNOWN`, `INSTANCES_UNKNOWN`, the
    timer list's pages, the runtime use's codes).
  - **SY.timer 1.6:** A-258, every refusal logged (the verb, the code, the sender), at most 10 a
    minute per sender and then the count of the rest; A-259, the scheduler's heap holds the pending
    timers only (a cancel or a purge takes a timer out at once, a reschedule moves it); A-266, a
    page its `limit` cuts answers `next_cursor`.
  - **The orchestrator:** A-240, a node's status is a plain read: `status_version` is gone from the
    status answer and its files are no longer written (the `status_version` and
    `status_fingerprint` files older releases wrote stay on disk, unused), and a local status read
    runs in A-199's read lane. A-241, a worker cleans its own `blob/staging` every hour (unfinished
    puts past 24 h, never `active/` or `public/`), with one line. A-267 and A-271, a check that
    decides an action answers yes, no or nothing, and only a no acts (the units, the nft tables,
    the default route, the Syncthing folders). A-281, the motherbee asks every worker at once for
    its nodes on a runtime (`usage_only`, 10 s each) and no ingress or egress
    (`hives_without_runtimes`), in read places of its own; Fluxbee Cloud's `list_nodes` never waits
    behind them.
  - **SY.wf-rules:** A-270, `delete_workflow` without force refuses a WF node that is not `RUNNING`,
    or whose status read failed (`INSTANCES_UNKNOWN`); A-281, a delete whose purge SY.admin refuses
    answers ok naming each version left in `packages_left`, a name deployed again numbers its
    versions above them, and the factory reset counts them.
  - **SY.architect:** A-279 and A-288, Archi's SCMD reads decode each query value once (a timer
    cursor written `%3A`, an owner `%40`), take a path in quotes, and carry the deployments' and the
    drift alerts' query; A-285's texts.
- **Build and review:** one branch per binary, each with its review round and fixes; A-281's three
  halves (`coa/admin-usage-133`, `coa/orch-usage-133`, `coa/wfrules-usage-133`) integrated on
  `coa/a281-133`, whose review's five lows were fixed. CI green on the integrated head (`45c736e4`:
  rust-tests, go-tests, admin-catalog-guard, posture-guards, router-dispatcher-guards). The builders
  and reviews found A-273 to A-288 (A-274, A-279, A-281, A-285 and A-288 shipped in it), A-296,
  A-297, A-299, A-300 and A-304 to A-310.
- **The deploy:** build on fb-build 22:16:59–22:33:34Z (16.6 min), publish in 12 s, `ops deploy
  0.1.133` at 22:35:09Z in 304 s (snapshots 31.5 s, the motherbee's apt 167.8 s, its admin on the
  version 46.0 s, the spokes answering 8.1 s, the update's call 21.0 s, the hives on the target
  16.7 s, health 5.3 s); snapshot `pre-sweep-0-1-133`, `pre-router-0-1-132` dropped. On the target
  after the update's call: egress1 26.5 s, ingress1 30.7 s, worker1 36.4 s, the motherbee 4.2 s; by
  their own records the spokes settled +19.8 s (egress1), +24.2 s (ingress1), +29.3 s (worker1).
  Health clean (`wandrops=1` on the motherbee: a WAN echo, A-272). The mixed window (a 0.1.133
  motherbee, 0.1.132 spokes) was not captured: the background watcher died when the snapshot froze
  the motherbee's guest agent (the gate tool's slip, now caught).
- **Checked on 8.x (the gate, against 0.1.132 where a baseline was taken; from 23:33Z the gate went
  on over 0.1.134, which changes only the orchestrator (A-314's fix and its guard), and the checks
  marked so ran there):**
  - **A-191 (its throwaway worker on 0.1.134):** before the deploy only SY.policy's ILKs on the
    motherbee and worker1 were outside 0.1.133's lists; SY.identity retired them at its start, one
    line each (22:39:00Z, 22:39:44Z). `retire_hive_ilks` retired worker2's 7 and 6 each of
    `gate0217`, `gate0218`, `gate0219`, `gate0221`, `gate0227` and `gate0231` (22:51Z, 1.2–3.0 s
    each), never deleted; a repeat answered count 0 and `already_retired`; worker1 and the
    motherbee refused `HIVE_NOT_REMOVED`, a bad id `INVALID_REQUEST`, none echoing the id. From
    23:51Z a throwaway worker `gate0233` (VM 104) joined in 148 s with its 6 system ILKs active;
    `remove_hive` (14.1 s) answered `system_ilks_retired` 6, and 35 s later all 6 were still
    retired; joined again under the same name (131 s, once the gate re-ran its prep: its first try
    failed `SSH_AUTH_FAILED`, the gate's slip) the 6 were active again, and removed again (4.4 s)
    they were retired.
  - **A-232:** the ILK and tenant listings refuse a cursor not theirs `INVALID_CURSOR` without
    repeating it; a real `next_cursor` pages (5 rows).
  - **A-240:** `AI.pruebas@worker1`'s status has no `status_version`; 5 reads (1.2–3.2 s) left its
    directory unchanged.
  - **A-241 (its pass on 0.1.134):** worker1's staging before the deploy (22:34Z): `root:root` 750,
    no backlog over 24 h; planted an old blob name, a fresh one, an old put temporary and an old
    name that is no blob's. The pass runs one hour after each orchestrator start; worker1's last
    start (on 0.1.134) was 23:48:30Z, so its pass was due at 00:48:30Z. It ran at 00:48:30Z (*"blob staging cleaned: unfinished puts past their TTL deleted"*, `scanned=4
    deleted=2 deleted_bytes=6 elapsed_ms=39`, with `blob_gc_enabled=false`): the old blob name and the
    old put temporary went, the fresh blob name and the old name that is no blob's stayed. A restart of
    the orchestrator postpones the pass by an hour.
  - **A-255 (live, the operator's call; the path that never had the bug):** a rotation on the edge
    IO.cloud already trusted: `publish_cloud_endpoint` on `SY.edge@ingress1` at 23:20:20Z answered
    ok in 1.9 s with `edge_applied: true`; `IO.cloud@motherbee` RUNNING HEALTHY before and after,
    its recorded `io.edge_node` `SY.edge@ingress1` and its recorded config's `config_version` 3 to 4
    (the record bumps the file at each rotation, A-256), its unit's start and PID unchanged (no
    restart). On that path SY.admin finds the edge in IO.cloud's live config and sends it no
    `CONFIG_SET`; 0.1.132 would have answered the same but for `edge_applied`. The bug's own case,
    a first publish or a move to another edge, is covered by its tests, not seen live. The new token
    went to a 0600 file, never printed, and the operator was given its path; the previous token
    worked 10 more minutes.
  - **A-258, A-259, A-266 (SY.timer 1.6 on the motherbee and worker1):** 12 bad cursors through
    SY.admin's routes gave 10 refusal lines and one summary on each hive, no cursor text in any
    line; from a throwaway client on worker1, 12 schedules below the minimum and 12 stray
    `TIMER_FIRED_RESPONSE` gave 10 lines and a summary each. Two one-shots per hive scheduled before
    the deploy were loaded again at SY.timer's restart: the ones canceled after it never fired, the
    others fired once on time (15 ms and 497 ms late); on worker1 a timer moved earlier and one
    moved later fired once each, a canceled one never, and a recurring one 5 times and none in the
    75 s after its cancel. `TIMER_LIST` with `limit` 1 gave `next_cursor`; pages of 7 listed 355
    rows in 51 pages, all distinct; a page of 1000 was cut by the frame at 80 rows with
    `next_cursor`.
  - **A-260 and A-274 (on 0.1.134):** 5 pages published as `AI.gate0233@worker1` (from 23:34Z,
    13.5–16.3 s each);
    `SY.edge@ingress1` SIGSTOPped 48 s: take-downs of an unknown page answered `NOT_FOUND` in 1.4 s,
    twice, while two take-downs waited on the edge (`EDGE_UNPUBLISH_FAILED` at 31 s); two take-downs
    of one page sent together after the edge went on: one took it down (8.2 s), the other wrote
    nothing over it (7.9 s); the expiry sweep deferred one retire on the edge's timeout and retired
    it at its next pass; all five pages expired or taken down, and released.
  - **A-267 and A-271 (on 0.1.134):** 20 self-restarts of `sy-orchestrator` at staggered offsets,
    debug on, on worker1 (23:47–23:48Z) and egress1 (23:48–23:50Z): no unit restarted or started
    meanwhile, and no *"service not active; attempting restart"*, *"did not answer"*, *"nft table
    missing"* or *"syncthing unhealthy"* line. Stopped on worker1, `sy-opa-rules` came back in 3.3 s
    and `fluxbee-syncthing` in 3.5 s, each with the orchestrator's line. A-271's reading of no
    answer is what made A-314 on systemd 255.
  - **A-270 (on 0.1.134):** `WF.gate0233@worker1` deployed (5 s) and killed without purge
    (`STOPPED`): its delete without force answered `INSTANCES_UNKNOWN`, with force ok.
  - **A-279 and A-288 (Archi's SCMD reads):** a timer cursor written `%3A` or `%3a` pages (0.1.132:
    `TIMER_INVALID_PAYLOAD`), `%253A` is still refused, an owner written `%40` works; a broken
    escape in `list_nodes`' cursor answers `INVALID_CURSOR` as JSON (0.1.132: no JSON); the drift
    alerts' severity filter and the deployments' hive filter are kept (2 and 3 entries; 50 and 48 on
    0.1.132); a path in quotes works (0.1.132: `SCMD_EXECUTION_FAILED`).
  - **A-281 (on 0.1.134 past its first read, 23:37–23:47Z):** `get_runtime`'s use skipped egress1
    and ingress1 `RUNTIME_MANIFEST_MISSING` on 0.1.132 (2.8–3.2 s), and from 0.1.133 skipped none
    and listed them in `hives_without_runtimes` (1.1–1.5 s, on 0.1.133 and on 0.1.134). With
    worker1's orchestrator stopped, removing io.slack 0.1.14 (unused) was refused
    `RUNTIME_USAGE_UNKNOWN` naming worker1 `UNREACHABLE`, retryable, in 1.5 s, nothing removed;
    SIGSTOPped, `TIMEOUT` in 12.8 s. In the lanes, with it SIGSTOPped: 4 `GET /versions` held
    30.8–31.3 s, a removal started 3 s after them answered in 11.2 s, `/hive/status` in 1.4 s, 4
    runtime reads in 11.2–11.9 s. Workflows: deleted right after their deploy (2.7 and 3.2 s), ok
    with `packages_left` []; with worker1's orchestrator stopped, two deletes ok in about 3 s naming
    `packages_left` (`RUNTIME_USAGE_UNKNOWN`, `PURGE_STOPPED`, both retryable) with the warning, one
    deployed again over its leftovers ran 0.0.3, and once worker1 answered the leftovers were
    removed (a runtime's current version first refused `RUNTIME_CURRENT_CONFLICT`).
    `teardown_node` of `AI.gate0233@worker1` with worker1's orchestrator and router stopped:
    503 `INVENTORY_UNAVAILABLE`, nothing removed, its ILK kept. io.slack 0.1.14 removed with every
    hive up (3.1 s).
  - **A-285:** SY.admin's help names each new answer the gate looked for (`retryable`, `UNKNOWN`,
    `RUNNING`, `wf_rules_get_status`, `packages_left`, 1000, `INVENTORY_UNAVAILABLE`,
    `RUNTIME_USAGE_UNKNOWN`, `NO_USAGE`, `USAGE_UNREADABLE`), each missing on 0.1.132; not
    `hives_without_runtimes` (A-315, below).
  - **Found:** A-314 (🔴) at 22:41Z: `run_node` of `AI.gate0233@worker1` answered `SERVICE_FAILED`
    and the stopped nodes with no unit read `UNKNOWN`, fixed forward by 0.1.134 (above); A-315
    [Archi], low: SY.admin's help and Archi's handbook do not name `hives_without_runtimes`; A-308
    seen live: the Cloud's `list_nodes`, started while the 4 `/versions` held, timed out at 31.8 s,
    and with only 4 runtime reads in flight answered in 13.1 s with worker1 in `unreachable_hives`.
- **PROD after the gate:** 0.1.133's deploy began at 22:35Z (its hives ran it from about 22:39Z),
  and 0.1.134's, which replaced it (A-314), ended at 23:32Z and dropped the snapshot
  `pre-sweep-0-1-133`. The gate's objects: 0.1.134's entry.
- **Rollback:** now through 0.1.134's entry. To 0.1.132: `apt install fluxbee=0.1.132` + 0.1.132's
  `update_installation` (nothing one-way found: the system ILKs 0.1.133 retired stay retired,
  harmless to 0.1.132).

## 0.1.132 — The router alone: far answers, link-ups and named drops

- **Date:** 2026-11-11 (ART) · **Previous version:** 0.1.131 · **Commit:** `36135e52`.
- **Scope:** motherbee and the three spokes, through the installation's update.
- **Why:** the sweep (ultracode, 2026-11-10/11); the router is shared code and ships alone (METHOD
  §9). The operator's calls: A-208, A-246 and A-247 *"si"* (2026-11-10); A-253 keep the drop and log
  it (*"hay que logear eso en algun lado.. imagino el log del propio nodo"*).
- **What changed (rt-gateway only):**
  - **A-245:** the hub hands a far router's `UNREACHABLE` or `TTL_EXCEEDED` on toward the spoke
    whose LSA lists the sender, one hop of TTL less, only when the adjacent router made it, never
    back where it came from and never twice; an answer it does not hand on is counted and logged
    with its reason, at most once every 30 s.
  - **A-195:** a new link's first frames are the router's LSA and, from the hub, its reachability,
    queued before the link goes into the WAN table; the hub tells every spoke of a hive's changed
    nodes as its LSA comes, not at its next 10 s beacon, one advertisement at a time in the order
    built. With it A-261 (the hub's LSA copy never goes back to an older one) and A-262 (the hub's
    advertisement goes out with no node too).
  - **A-265:** the hub rebuilds its FIB as it applies a spoke's LSA, before it advertises that LSA's
    nodes, and when the stale check marks a hive stale.
  - **A-246:** a broadcast is answered once, by the router it entered (`GATEWAY_UNAVAILABLE` first,
    then local delivery's reason, then `LINK_BUSY`); a router a broadcast reaches over IRP answers
    nothing. A gateway's answer is unchanged.
  - **A-253:** the drop of a stream record too big for a batch stays; each one is named in the
    router's journal (*"router stream dropped a record too big for a batch; its sink never gets
    it"*: stream, sink, subject, producer, `trace_id`, `thread_id`, seq, bytes, max_bytes; the three
    ids quoted, escaped and cut at 128 bytes), at most 10 between two alarm lines, the alarm adding
    *"did not name N"* for the rest.
  - **A-208 with A-247:** the several-routers mode (unsupported) no longer wedges a router that is
    not the gateway, checks an IRP broadcast against the cache, registers a sibling once, and
    answers over the link a frame came from.
  - **Lab:** `ops.py health` counts the router's lines only when the pattern opens the line's
    message (the review of A-253).
- **Build and review:** `coa/router-131` (A-245, A-195, their reviews) and `coa/router-132` (the
  rest, its review round and fixes), rebased onto daily with their subjects at `[0.1.131]`; CI green
  on the branch and on the integrated head (`d3079d3b`). The reviews found A-268 and A-269.
- **The deploy:** build in 23 min, publish in 16 s, `ops deploy 0.1.132` in 288 s (snapshots 30 s,
  the motherbee's apt 116 s, its admin on the version 68 s), health clean; snapshot
  `pre-router-0-1-132`, `pre-sweep-0-1-131` dropped. The motherbee's router restarted in 0.4 s; each
  spoke's core restarted with its router, and the spokes settled by their own records 37.4 to 41.8 s
  after the update's call (0.1.131's: 22.6 to 29.2 s); SY.admin closed the update 41.9 s after it
  opened it (0.1.131's: about 38 s).
- **Checked on 8.x (the gate, against the same runs on 0.1.131 first):**
  - A-245: a client on worker1 that reads nothing, a sender on ingress1: the 1,536 `LINK_BUSY` of
    worker1's router all reached the sender (0.1.131: none; the receiver took 1,064 both times).
  - A-195: a sender that connected 1 s before sending reached worker1 (0.1.131: all 2,600 frames
    dropped there as `foreign_source`, unanswered), and ingress1 knew the new node within 1 s
    (0.1.131: answered `NODE_NOT_FOUND` for more than 5 s). No link-up drop logged at the deploy's
    link-ups.
  - A-265: a node that connected on worker1 was reached by name from ingress1 0.33 s after its
    connect (0.1.131: 5.98 s); no `NODE_NOT_FOUND` from the hub.
  - A-246 on the gateway's path: with one target's link full, one `LINK_BUSY` from the motherbee's
    router and the broadcast once to each other target; with none full, no answer and once to each;
    the same as on 0.1.131.
  - A-253 on worker1: a 100 KB message named once (bytes 100,312, max_bytes 97,151); one whose
    `thread_id` held 6 KB starting with a newline and the alarm's words named in one line of 494
    bytes, quoted, escaped and cut (`... (6062 bytes)`), no forged line; 15 at once: 10 named and
    *"did not name 5"*; `ops.py health` gave `streamlost=17`, the alarms' own count.
  - A-208, A-247: no live case (every hive runs one router); A-261, A-262: by their tests.
  - Found: A-272, every router sends a WAN broadcast back to the peer it came from (the spokes'
    echoes are most of the `foreign_source` lines A-195 counted).
- **PROD after the gate:** all four hives on 0.1.132 (`da117f8b`); worker1 behind egress1; the
  gate's throwaway clients gone; snapshot `pre-router-0-1-132` on each VM.

## 0.1.131 — The sweep, one branch per binary

- **Date:** 2026-11-11 (ART) · **Previous version:** 0.1.130 · **Commit:** `0f6b54f1`.
- **Scope:** motherbee and the three spokes, through the installation's update.
- **Why:** the operator's sweep of small items and bugs (ultracode, 2026-11-10: *"ahora mejor sacar
  todos los temas chicos y bugs que podamos"*), and the calls of 2026-11-10/11: A-242 (*"nada se
  traba con las paginas"*), A-250 option (a), A-91, A-226, A-254 option (b). One branch per binary,
  each reviewed adversarially and fixed once; none is shared code.
- **What changed:**
  - **SY.admin:** A-242, no removal (a node's purge, `teardown_node`, `purge_ilk`, `purge_tenant`, a
    workflow's delete, `remove_hive`) is refused for published pages: A-233's and A-212's refusals
    and their in-flight count are gone, each ok answer names the pages left up (`pages_left_up`: at
    most 20 rows and 8 KiB, soonest to expire first, no key), the expiry sweep skips an edge whose
    hive is gone, the factory reset asks each purge once and takes down the late pages 120 s after
    its last removal (step [8]); over the mesh a node takes down only its own pages in its own
    tenant (the review). A-249 closed with it. A-251: SY.timer's refusals answered 400, 404, 501 or
    502 by their code. A-250's option (a): while a hive restarts, the update's read waits 10 s and is
    asked again at once; after `add_hive` or `remove_hive` it looks at the payload's hive.
  - **SY.timer 1.5:** A-238, a canceled timer is kept 7 days from its cancel (`canceled_at_utc`,
    schema 4; an undated one dated at the start); A-244, a timer's lock entry leaves with its last
    holder.
  - **IO:** A-91, io.blob boots CONFIGURED on its defaults and io.cloud UNCONFIGURED
    (`missing_edge_node`) until its config names its edge, both answering their status from their
    config state; A-226, io.slack's text for `NOT_TAKEN` `unavailable`; A-223's spec. Their texts
    say to restart IO.cloud after a first `publish_cloud_endpoint` (A-255, SY.admin's fix next).
  - **WF:** A-254, a timer SY.timer does not take (the router's `UNREACHABLE` or `TTL_EXCEEDED`, a
    refusal, no answer in 30 s, a failed send) is marked in the WF's index (store v7) and sent again
    after 5, 10, 20 and 40 s, then each minute, the node `DEGRADED` meanwhile; one less than 60 s
    from its time is fired by the WF itself; a move goes as a delay; a held time is compared. The
    366-day bound at load, at SY.wf-rules' check and compile, and at run time.
  - **The orchestrator:** A-252, a new spoke switches Syncthing off public discovery, relays and
    NAT at its join's peer link, with the restart the link costs anyway.
- **Build and review:** `coa/admin-131`, `coa/timer-131`, `coa/io-131`, `coa/wf-131`,
  `coa/orch-131`, each with its review round and fixes, CI green on each and on the integrated head.
  The reviews found A-255 to A-265, recorded in FINDINGS.
- **The deploy:** build in about 14 min, publish in 12 s, `ops deploy 0.1.131` in 279 s, health
  clean; snapshot `pre-sweep-0-1-131`, `pre-gosdk-0-1-130` dropped. Every spoke's orchestrator
  restarted itself (A-250's case): the spokes settled by their own records 22.6 to 29.2 s after the
  update's call, SY.admin marked each 1 to 5 s later, one read lost to a restart asked again at once, and the
  update closed about 38 s after its call (0.1.127's took 148 s).
- **Checked on 8.x (the gate):**
  - SY.timer 1.5 on the motherbee and worker1: `gc_stamped=2` and `350`, `gc_deleted=0`; schema 4,
    no canceled row without its date, every canceled timer still listed; a throwaway owner
    scheduled, moved, canceled twice (the first date kept) and let one fire.
  - A-251's three codes. Paging with `limit` found A-266.
  - A-91: IO.blob's config answer `CONFIGURED` (was `node_not_configured`), IO.cloud CONFIGURED with
    its edge, both HEALTHY on 0.1.131.
  - A-242 with `AI.gate0231@worker1`: two pages up through its purge and its tenant's purge, each
    answer naming both; one taken down (404), one expired (404) and retired by the sweep.
  - A-254 with `WF.gate0231@worker1`: SY.timer@worker1 stopped as an instance started; the refusal,
    `DEGRADED`, the timer sent again 6 s later and taken, the fire on time; a move to the 60 s floor
    taken; `fire_in` 400d refused at the check.
  - A-252 with the throwaway worker `gate0231` (VM 104): joined in about 140 s, Syncthing started 3
    times (gate0227's 4); removed in 3.0 s (`pages_left_up` 0); VM 104 destroyed.
  - Not seen live: A-226's text (it needs a person writing through io.slack; its tests cover it).
  - Found: A-266 (TIMER_LIST's `limit`) and A-267 (the watchdog's last tick at a self-restart).
- **PROD after the gate:** all four hives on 0.1.131 (`9bc916af`); worker1 behind egress1; no page
  up; the gate's tenant, node, WF and hive removed (the six system ILKs of `gate0231` stay, for
  A-191's cleanup); snapshot `pre-sweep-0-1-131` on each VM.

## 0.1.130 — The Go SDK alone

- **Date:** 2026-11-10 (ART) · **Previous version:** 0.1.129 · **Commit:** `8604debf`.
- **Scope:** motherbee and the three spokes (the Go binaries: SY.timer, SY.wf-rules, SY.opa-rules,
  wf-generic's build), through the installation's update.
- **Why:** shared code ships alone ([METHOD](logbook/METHOD.md) §9); the last release of the pass of
  the General track.
- **What changed (`go/`):**
  - **A-228:** the Go SDK's `RpcError` for `UNREACHABLE` or `TTL_EXCEEDED` carries the notice, the
    `reason`, the `original_dst` and the `last_hop`, as the Rust SDK's does; the kind stays
    `RpcErrTerminalTransport`; its text now names the reason.
  - **A-239:** `TimerInfo` has `canceled_reason`; SY.timer answers with the SDK's `TimerInfo`
    instead of a wrapper of its own, with the same bytes (a test pins them).
  - **A-218's leftovers:** `MSGEcho`/`MSGEchoReply` gone (an out-of-tree Go module naming them would
    not compile; none in the tree); SY.timer's and wf-generic's notice rules are the AI nodes' (the
    router's messages, HELLO, ANNOUNCE, WITHDRAW, a system message without a name, and for SY.timer
    any `*_RESPONSE`); wf-generic no longer lists TIMER_HELP as a notice (a system TIMER_HELP is
    still dropped unanswered); SY.timer answers an ECHO or ECHO_REPLY `TIMER_UNKNOWN_VERB`.
- **Build and review:** `coa/gosdk-130`, CI green with the Go tests; one review round: no bug, wire
  break or loop; a byte-for-byte wire test and two texts added.
- **The deploy:** build in about 22 min, publish in 12 s, `ops deploy 0.1.130` in 252 s, health clean;
  snapshot `pre-gosdk-0-1-130`, `pre-router-0-1-129` dropped. The update closed in 6 s: no
  orchestrator binary changed, so none restarted itself (A-250's counter-case).
- **Checked on 8.x (the gate):** TIMER_LIST and TIMER_GET of two timers (one canceled, one pending
  recurring timer with `cron_tz` and metadata made for the gate) byte for byte the same before and
  after; ECHO and ECHO_REPLY answered `TIMER_UNKNOWN_VERB` once each, a system message without a name
  not at all (all three unanswered on 0.1.129); a one-shot timer whose target was away retried at 5,
  10, 20 and 40 s with `NODE_NOT_FOUND` and delivered once it came back; SY.timer, SY.wf-rules and
  SY.opa-rules started on every hive; WF.gate0206 and AI.pruebas healthy. Not seen live: a Go node
  reading the new error (WF.gate0206 runs its own package's build).
- **PROD after the gate:** all four hives on 0.1.130 (`ef8488bc`); worker1 behind egress1; the gate's
  two timers in SY.timer's history (fired, canceled); snapshot `pre-gosdk-0-1-130` on each VM.

## 0.1.129 — The router alone: a bounded queue per link

- **Date:** 2026-11-10 (ART) · **Previous version:** 0.1.128 · **Commit:** `9b396e3e`.
- **Scope:** motherbee and the three spokes, through the installation's update.
- **Why:** the router ships alone ([METHOD](logbook/METHOD.md) §9). A-222 is the operator's call
  of 2026-11-10, option (a): a bounded queue per link, the newest refused with an answer, never a
  disconnect, not a circular buffer.
- **What changed (`src/router/`):**
  - **A-222:** each link's queue holds at most 1,024 frames or 8 MiB, counted as the heap each frame
    holds; senders' messages stop at 960 frames or 7 MiB, so the router's own frames always have
    room; a message past it is answered `UNREACHABLE` `LINK_BUSY` and logged at most once every 30 s
    per link; the node stays connected and room comes back as it reads.
  - **A-209:** no router answers a tap copy, the entry router or one it reaches; the router clears a
    `meta.via_tap` a node sets on its own message (only the router marks a copy).
  - **A-221:** an agent's turn goes to its cognition only once the agent's link took the frame.
- **Build and review:** `coa/router-129`, CI green; one review round: nothing high or medium; A-209 on
  the far router, the bound counted as heap, three tests, fixed; and a node's own `via_tap` cleared,
  found by its builder. Rebased on 0.1.128 with three documentation conflicts merged by hand.
- **The deploy:** build of `9b396e3e` in 16.7 min, publish in 12 s, `ops deploy 0.1.129` in 362 s,
  health clean; snapshot `pre-router-0-1-129`, `pre-sdk-0-1-128` dropped. The update closed `done`
  about 128 s after its call (A-250 again).
- **Checked on 8.x (the gate):**
  - No "link whose queue is full" line on any router during the deploy and its update.
  - A throwaway client on worker1 that read nothing for 60 s: of 80 messages of 100 KB sent to it
    from worker1, 39 were taken and 41 refused `LINK_BUSY` at once (the first 0.2 s in); from the
    motherbee, the same, the refusals relayed by worker1; each time it then got all 39, in order;
    worker1's router grew 4 to 10 MB and came back; one line naming the link.
  - A-209: a tap whose target was absent dropped at the entry router; a tap target on worker1 whose
    link was full (by frame count) dropped there without an answer (`unanswered_total=1`); the
    tapped message's sender heard nothing and its receiver got it. A client's own message marked
    `via_tap` was answered `NODE_NOT_FOUND`, with the router's "cleared" line.
  - SY.storage stopped 20 s: nothing lost. egress1's router stopped 60 s: the motherbee closed its
    session at 38 s (A-207), reads failed fast after, and egress1 came back on its own.
  - The routers' stream dropped the copies of the gate's own 100 KB messages as oversize, as 0.1.128
    did: A-253.
- **PROD after the gate:** all four hives on 0.1.129 (`018529f6`); worker1 behind egress1; no tap and
  no page; snapshot `pre-router-0-1-129` on each VM.

## 0.1.128 — The Rust SDK alone

- **Date:** 2026-11-10 (ART) · **Previous version:** 0.1.127 · **Commit:** `89fd1fd8` (code
  `976a6e00`).
- **Scope:** motherbee and the three spokes, through the installation's update.
- **Why:** shared code ships alone ([METHOD](logbook/METHOD.md) §9). A-218 is the operator's call
  (*"si borra la basura, no hay back compatibilidad"*), with the operator's rule that the identity
  shared memory stays as it is (*"no borres la SHM si no es por rediseño"*).
- **What changed:**
  - **SY.identity and the SDK (A-218):** `TNT_APPROVE` and its handling gone; seven dead action
    names (`approve_tenant`, three of the vocabulary, `get_runtimes`, `update_policy_matrix`,
    `clear_override`); ECHO, ECHO_REPLY and TIME_SYNC out of the Rust SDK and the AI nodes' notice
    list (the Go SDK keeps them until its own release); migration v5 drops `identity_vocabulary`
    and `identity_tenants.approved_by`. The identity shared memory, its regions and
    `IDENTITY_VERSION` 6 are unchanged.
  - **The SDK (A-237):** `pending_confirm` and `sync_pending` are marked `accepted` in
    `meta.action_result`, not `failed`.
  - **The AI SDK (A-248):** a tap copy reaches no handler and gets no answer; a reply never carries
    `via_tap`; the ignored copies are counted (`tap_copies_ignored`).
- **Build and review:** `coa/sdk-128`, CI green; one review round: nothing blocking, two low (stale
  texts; a dropped copy seen nowhere), fixed. The review compared the shared memory's code between
  the two refs and read the rollback against 0.1.127's code.
- **The deploy:** build in about 19 min, publish in 16 s, `ops deploy 0.1.128` in 361 s, health
  clean; snapshot `pre-sdk-0-1-128`, `pre-reads-0-1-127` dropped. The update closed `done` 116 s after
  its call, the spokes settled by +31 s: A-250 again.
- **Checked on 8.x (the gate):**
  - The identity shared memory on the motherbee and worker1 (the hives that hold it): the same size,
    magic, version 6, counts (3 tenants, 69 ILKs, 6 channels and mappings, 0 vocabulary, 0 aliases)
    and limits before and after; only the sequence word moved.
  - SY.identity: "migrations through v5" at each start, rows 1 to 5, the table and the column gone,
    no view or other owner; worker1's replica applied its full sync (69 ILKs, 3 tenants); a gate
    tenant created, updated, deleted and purged.
  - A-248: a tap on worker1 copying a throwaway IO client's question to AI.pruebas into a throwaway
    `AI.gate0228`: the client got one answer, from AI.pruebas; `AI.gate0228` answered nothing and
    counted `tap_copies_ignored=1`; the tap, the node and its tenant removed.
- **PROD after the gate:** all four hives on 0.1.128 (`015ac810`); worker1 behind egress1; no tap and no page;
  snapshot `pre-sdk-0-1-128` on each VM.

## 0.1.127 — Reads apart, the blob folders, and no purge under a live page

- **Date:** 2026-11-10 (ART) · **Previous version:** 0.1.126 · **Commit:** `d2b344bb`.
- **Scope:** motherbee and the three spokes, through the installation's update.
- **Why:** the pass of the General track and its problems, the operator's call of 2026-11-10
  (*"por cosas de mejoras de soft que no tengan dudas, avanza... en esta hace una pasada de generales
  y problemas"*), one branch per binary ([METHOD](logbook/METHOD.md) §9).
- **What changed:**
  - **SY.orchestrator:** A-199's step 1: a read runs apart, on tokio's blocking pool, with 16
    places for this hive's reads and 16 for reads that wait on another hive, at most 4 of them on
    one target hive; changes still run one at a time; a command or system request that panics is
    answered `INTERNAL_ERROR`. Hive-config stage 5: `fluxbee-blob` takes the members of
    `fluxbee-dist-runtimes` and `fluxbee-blob-public` those of `fluxbee-dist-core-ingress`; the blob
    GC runs only on the motherbee, and a spoke's `blob.gc` is warned about and ignored (A-188).
    A-196: Syncthing's owed restart has a marker with a token. A-232's refusals name no value.
  - **SY.admin:** A-233, a node with a page up, or with a publish in flight, is not purged (and a
    node being purged publishes nothing); A-231, the ledger drops a page 30 days after it went
    down; A-234, a request's head is capped at 64 KiB (431) and its body at 1 GiB (413), read to
    the client's end; A-232; A-171, every runtime manifest writer takes
    `/var/lib/fluxbee/dist/runtimes-manifest.lock`, which the package no longer ships.
  - **SY.vault:** A-200, the boot's announcement reads the secrets once.
  - **SY.timer:** A-220, a timer SY.timer closes itself is `canceled` with a `canceled_reason`
    (schema v3, version 1.4); a fire refused `LINK_BUSY` is sent again (ahead of the router's
    0.1.129); A-232.
- **Build and review:** `coa/orch-127`, `coa/admin-127`, `coa/vault-127`, `coa/timer-127`, CI green on
  each; one review round per branch. The orchestrator: four low, fixed (a target's own read places
  among them). SY.admin: one medium (a publish in flight passed the purge check), fixed, and four
  low. SY.timer and SY.vault: fixed before. Found by the round: A-245 to A-249, low.
- **The deploy:** build of `d2b344bb` in 18.7 min, publish in 18 s, `ops deploy 0.1.127` in 377 s,
  health clean; snapshot `pre-reads-0-1-127`, `pre-egress-0-1-126` dropped. The update closed `done`
  126 s after its call: the spokes settled by +43 s, and SY.admin lost a read to each spoke's
  self-restart (A-250).
- **Checked on 8.x (the gate):**
  - Syncthing's folder members on the four hives identical before and after; no restart at the
    first start.
  - A-199: egress1's orchestrator stopped, six reads of its versions at once: four forwarded, two
    waiting without a place, sent on as soon as egress1 answered; meanwhile the motherbee's own
    reads, worker1's and AI.pruebas' status answered in 0.0 to 0.7 s; 15 to 16 threads. The
    motherbee's orchestrator restarted in 3.8 s with reads waiting, no SIGKILL.
  - A-233 with a throwaway `AI.gate0227@worker1`: listed and `RUNNING` right after `run_node`; a
    purge refused while its publish waited for the blob (1.2 s), and again with the page up; after
    the page came down, purged; its tenant removed.
  - A-234: 431 and 413 read whole by a client still sending. A-232: four refusals with no value
    (A-251: SY.timer's comes as HTTP 500). A-231: two rows dropped. A-171: the lock file there,
    owned by no package. A-200: 12 secrets in one pass. A-220: schema v3.
  - Stage 5 with a throwaway worker (`gate0227`, VM 104): joined in about 143 s; the motherbee's
    `fluxbee-blob` took it with the runtimes; taken out through Syncthing's REST, put back at the
    orchestrator's restart; the restart marker during the link and gone after; its hive.yaml
    without `blob.gc`, and with `blob.gc.enabled` set by hand, a warning and nothing deleted;
    removed in 4.7 s, no folder or device left; VM 104 destroyed. Found: A-252.
- **PROD after the gate:** all four hives on 0.1.127 (`aa629f9a`); worker1 behind egress1; no page
  up; the installation file version 7; snapshot `pre-reads-0-1-127` on each VM.

## 0.1.126 — A worker behind the egress

- **Date:** 2026-11-10 (ART) · **Previous version:** 0.1.125 · **Commit:** `cf005771`.
- **Scope:** motherbee and the three spokes, through the installation's update.
- **Why:** A-219, the operator's call of 2026-11-10 (*"si workers tienen que salir a internet, si
  b)"*), its shape approved the same day (*"hacelos como recomendas"*): docs/edge-egress-nat-spec.md
  §15. It touches the router's policy (a protected action), so it went in a release of its own.
- **What changed:**
  - **SY.admin:** `set_hive_egress` (`PUT /hives/{hive}/egress`) and `clear_hive_egress` (`DELETE`),
    mesh access `Nobody`, CONFIRM through Archi's executor; a body naming another hive refused; held
    off while an installation update runs, and one egress action at a time.
  - **SY.orchestrator (the motherbee):** the checks (a worker, the declaration with its reachability
    URL, no join), the forwards of the switch and the confirm; `/versions` compares each worker with
    the declaration (`behind`, `not_behind`, `other_gateway`, `drifted`).
  - **SY.orchestrator (the worker):** guards G1-G3 (the gateway on a connected subnet, the mesh and
    the operator's sessions off the default route, multipath and tunnels refused), `egress-switch.json`,
    the switch, the probe, `pending_confirm`, the commit (hive.yaml atomically, then memory) or the
    revert, the watchdog paused meanwhile, the start finishing an open switch, a core update or
    rollback refused while one is open; A-225's three items.
  - **The router and the policy:** `SYSTEM_EGRESS_ROUTE` protected, sent only by the motherbee's
    orchestrator to a spoke; system.wasm compiled again.
  - **Archi:** the catalog, the plan's wait, the handbook's §4.4.5.
- **Build and review:** `coa/a-219` (the design, the policy, the orchestrators, SY.admin and Archi, the
  docs), CI green; one review round: one medium (a body `target`) and eight low, all fixed, with
  worker1's socket-activated sshd found live and handled.
- **The deploy:** build of `cf005771` in 27.5 min, publish in 13 s, `ops deploy 0.1.126` in 326 s,
  health clean; snapshot `pre-egress-0-1-126`, `pre-clean-0-1-125` dropped. The update closed `done`
  44 s after its call (no read lost this time; A-199 stays).
- **Checked on 8.x (the gate, the operator's standing word for 8.x):** worker1 behind egress1 in
  2.7 s and AI.pruebas through it (an nft counter on egress1: 123 packets); `unchanged`; the
  watchdog undoing a hand route within 6 s; a restart re-applying it; the clear back to the
  baseline; a revert on a blocked probe (47 ms) and a crash mid-switch undone at the start, each
  back to the baseline; the refusals. Details in FINDINGS A-219.
- **PROD after the gate:** all four hives on 0.1.126 (`a61136d5`); **worker1 behind egress1**
  (default via 10.10.10.40, IPv6 off), set through Archi's plan and CONFIRM, as the operator
  decided; AI.pruebas answers through egress1; the installation file version 7; snapshot
  `pre-egress-0-1-126` on each VM.

## 0.1.125 — The sweep's calls

- **Date:** 2026-11-10 (ART) · **Previous version:** 0.1.124 · **Commit:** `f39e2bb0`.
- **Scope:** motherbee and the three spokes, through the installation's update.
- **Why:** the operator decided the sweep's list on 2026-11-10 (lab/logbook/2026-11-10.md); the
  decided items on the product's binaries went out together, a branch per binary, and the review
  round found two bugs of its own (A-235, A-236), fixed in the same release.
- **What changed, by binary:**
  - **SY.admin:** `list_published_artifacts` (`GET /artifacts`, Archi's read, no link key); a
    tenant's purge refused `TENANT_HAS_PAGES` while a page is up; a page on a removed edge comes down
    without asking it (A-212); the ACME `name` alias removed (A-210); a failed handler answered 413
    `MESSAGE_TOO_LARGE` or 500 (A-229); its help says the tenant is the operator's (A-230).
  - **SY.architect:** asks for a node's tenant, never a default (A-230); its runtime redeploy reads
    each node's tenant first and leaves a node without one alone (A-235).
  - **SY.orchestrator:** no `ORCH_DEFAULT_TENANT_ID`, two different tenants refused, config.json
    with the registered tenant (A-213); hive.yaml written atomically by `set_storage` (A-224);
    `remove_hive` stops `sy-edge` (A-236).
  - **SY.frontdesk.gov:** a transient registration failure retried within 8 s and the case kept open,
    one registration per turn, English texts, the model answers in the person's language (A-216,
    A-214).
  - **wf-generic (`wf.engine`):** a forward to another IO node clears the addressee (A-215); a failed
    store leaves the node up DEGRADED or FAILED_CONFIG, retrying, with a fire dropped meanwhile given
    once it opens (A-217).
  - **SY.storage, SY.cognition, SY.identity, ai.generic:** one-time conversions removed; SY.identity
    now requires a channel's scope (A-210). ai.generic's and io.slack's fixed texts in English, and
    Spanish test names and strings across the repo (A-214).
  - **Packaging:** the factory reset unpublishes every live page first (A-212).
- **Build and review:** six branches, CI green on each and on the integrated head. One review round,
  a reviewer per branch: nothing high; one medium (wf-generic's dropped fire) and the lows fixed;
  A-235 (high in effect: a node lost on a runtime redeploy) and A-236 found and fixed.
- **The deploy:** build of `f39e2bb0` in 43.3 min (most crates changed), publish in 16 s,
  `ops deploy 0.1.125` in 372 s, health clean; snapshot `pre-clean-0-1-125`, `pre-router-0-1-124`
  dropped. Phases: snapshots 29.0 s; apt install 123.9 s; the admin on the version 61.4 s; the spokes
  answering 12.2 s; the update's call 34.6 s; the hives on the target 96.8 s; health 5.4 s.
  - **The update closed `done` 131 s after its call**, while each spoke settled by +42 s (its own
    records). The motherbee orchestrator's admin worker sat twice for its 45 s forward bound, a read
    lost around a spoke orchestrator's restart each time, with every other read behind it (A-199).
- **Checked on 8.x (the gates; the operator's standing word for 8.x):**
  - **A-229:** a 131,390-byte node message through SY.admin's HTTP answered 413
    `MESSAGE_TOO_LARGE`, "nothing was sent" (0.1.124 closed the connection).
  - **A-213:** `run_node` of a throwaway `AI.gate0125@worker1` with no tenant answered
    `IDENTITY_REGISTER_FAILED`, "there is no default tenant"; with two different tenants,
    `INVALID_REQUEST`; nothing was left on worker1.
  - **A-212:** `GET /artifacts` answered `ok`, no page live on 8.x, no key field.
  - **A-230:** Archi, asked to create `AI.gate0125b@worker1` with no tenant, asked which tenant and
    offered the two non-root ones; no plan staged.
  - **AI.pruebas** answered a throwaway IO client; WF.gate0206@worker1 runs `wf.engine` 0.1.125.
  - A-214, A-215, A-216, A-217, A-224, A-235 and A-236 by their tests (CI).
  - **PROD after the gates:** all four hives on 0.1.125 (`6e32853b`), the installation file version
    7, snapshot `pre-clean-0-1-125` on each VM.

## 0.1.124 — The router's release: no frame lost in silence

- **Date:** 2026-11-10 (ART) · **Previous version:** 0.1.123 · **Commit:** `bffaf4f2`.
- **Scope:** motherbee and the three spokes, through the installation's update.
- **Why:** 0.1.123's deploy lost a read to a spoke whose router was restarting (A-203), which cost
  its update about 85 s. The router is shared code and ships alone; the operator added A-204 and
  A-205 (*"si podes suma 2, por el tiempo que implica el deploy"*), and two clear bugs on other
  binaries rode along (A-206, A-211), with A-181's leftover texts.
- **What changed:**
  - **The router (rt-gateway):**
    - A-203: a WAN session runs its cleanup on every way out (a read error, a handler error, its
      writer stopping, the hive unregistered, a clean close) and removes its hive's entry only
      while it is still its own; a send into a dead link is answered (`WAN_UNAVAILABLE`,
      `GATEWAY_UNAVAILABLE`, `PEER_UNAVAILABLE`). The links between sibling routers and to nodes
      got the same cleanup; a node whose link no longer writes is disconnected, and a message to it
      answered `NODE_NOT_FOUND`.
    - A-207: a WAN session that hears nothing for the dead interval (40 s, or the peer's longer one)
      ends; every router sends its LSA every 10 s, so a healthy one never does. Each step of a WAN
      handshake (the TCP connect, TLS, the HELLO) is bounded at 30 s, and the peer's `WAN_ACCEPT` no
      longer counts as a dropped frame.
    - A-202: a sender's frame over 128 KiB once the router has stamped it is not queued, and the
      sender is answered `UNREACHABLE` `FRAME_TOO_LARGE`; a broadcast is measured at its largest copy
      and goes everywhere or nowhere. The router's own frames keep the writer's skip.
    - The router no longer reads hive.yaml's `ai` (it only checked it) and blob keys.
  - **SY.admin:** A-205, `set_installation_ai`'s apply claims each hive and a read older than a
    hive's last outcome is dropped; A-204, each hive's own time in the record, and a read that finds
    a category on the target records it (`seen`); from the review round: a change waits for a hive's
    holder only while its own call still fits, a run waits for a closed update's task, a closed
    update's task sends nothing more, a new update's first looks follow its own cadence, the first
    look's race with the run's passes, and `set_installation_ai` refuses an unreadable registry
    (503). `lab/ops.py` prints a `seen` or background outcome as on target.
  - **SY.timer:** A-206, a `cron_spec` that starts with `TZ=`, `CRON_TZ=` or `@` is refused
    (`TIMER_INVALID_CRON`) before robfig/cron's parser, which panicked on `TZ=` with no space.
  - **Packaging:** A-211, wf-generic is no longer a core binary; it ships as the `wf.engine` runtime.
  - **Texts:** the Rust SDK's comments, SY.vault's comment and log line, and two comments in
    SY.identity and SY.orchestrator no longer say the SY nodes come from hive.yaml; the SDK's unused
    `HIVE_YAML_NON_SY_LIFECYCLE_NODES` and `is_allowed_non_sy_lifecycle_node` removed.
- **Build and review:** a branch each (`coa/a-203`, `coa/a-205`, `coa/a-206`, `coa/a-211`,
  `coa/texts-124`), CI green on each and on the integrated head. One review round, a reviewer per
  set: the router's (no high or medium; four fixes: the broadcast, the handshake deadline,
  `WAN_ACCEPT`, nits), SY.admin's (no high or medium; six fixes), the small set's (two comments).
- **The deploy:** build of `bffaf4f2` in 26.5 min, publish in 16 s, `ops deploy 0.1.124` in 413 s,
  health clean; snapshot `pre-router-0-1-124`, `pre-inst-0-1-123` dropped.
  - Phases: snapshots 29.7 s; apt install on the motherbee 263.8 s; the motherbee's admin on the
    version 46.6 s; the spokes answering 10.5 s; the update's call 32.7 s; the hives on the target
    13.9 s; health 6.5 s.
  - The apt install: about 157 s of `apt-get update` and the download of the 242 MB package from
    fb-build's repo (about 93 s on 0.1.123), then dpkg 82 s (unpack 64 s, configure 15 s); the
    motherbee's orchestrator was down 80 s inside it (A-193).
  - **The update closed `done` 46.6 s after its call** (123 s on 0.1.123, with A-203's lost read):
    egress1 on the target at +37.0 s, ingress1 +41.1 s, worker1 +45.4 s, the motherbee +8.4 s; no
    read lost while the three spokes' routers restarted.
- **Checked on 8.x (the gates; the operator's standing word for 8.x, 2026-11-10):**
  - **A-211:** no `/usr/bin/wf-generic` nor `dist/core/bin/wf-generic`; the core manifest's 14
    components without it; WF.gate0206@worker1 runs `wf.engine` 0.1.124.
  - **The texts:** SY.vault logs "well-known SY system ILKs computed from the role's system nodes".
  - **A-206:** a valid raw recurring timer from SY.admin was scheduled (05:24:03) and cancelled
    (05:24:13); two `TZ=` specs sent between them (`TZ=UTC` with tab-separated fields, and `TZ=UTC`
    alone) were refused: no timer, and SY.timer's process the same throughout. On 0.1.123 either
    one stopped it.
  - **A-202:** the largest payload SY.admin's SDK accepts was 130,754 bytes; at 130,712 the router
    logged `FRAME_TOO_LARGE` (`frame_len=131095 max=131072`, `src=SY.admin@motherbee`,
    `dst=SY.timer@motherbee`) and queued nothing. The router stream dropped its own copies of the
    gate's near-limit messages as oversize (14 records). Found: SY.admin's HTTP closes the
    connection without an answer when its SDK refuses an oversize send (A-229).
  - **A-203:** the deploy itself (above). With egress1 off the mesh (its orchestrator, then its
    router), reads failed in 1.2-1.3 s.
  - **A-204:** version 6 (openai's model to `gpt-5.5-mini`) with egress1 away answered in 88.9 s
    (`partial`); the record gives each hive its own time: ingress1 1,445 ms, the motherbee 1,748 ms,
    worker1 1,770 ms, egress1 `not_answered` 85,575 ms. The readers the deploy had restarted since
    their last apply restarted once, as on 0.1.123.
  - **The automatic step:** egress1 back at 05:29:10; it finished egress1 at 05:29:49 (config `ok by
    automatic`, 49 ms), and the update closed `done`.
  - **A-205:** version 7, the undo with every hive up, answered in 4.3 s; each hive applied it in
    0.2-1.7 s, and the update closed `done` 6 s later (32 s on 0.1.123).
  - **A-207:** egress1's WAN traffic to the motherbee's :9000 dropped at 05:30:53 (an nft table on
    egress1, removed by a timer); the motherbee cut the session at 05:31:28.8, 40 s after its last
    frame; reads failed fast meanwhile; after the drop went, egress1 reconnected and a read was `ok`.
  - **AI.pruebas** answered a throwaway IO client ("Two plus two is four.").
  - **PROD after the gates:** all four hives on 0.1.124 (`e9a64112`), the installation file version
    7 with version 1's values, snapshot `pre-router-0-1-124` on each VM.

## 0.1.123 — The installation's update, and replies that fit the frame

- **Date:** 2026-11-09 (ART) · **Previous version:** 0.1.122 · **Commit:** `e6f6c93f`.
- **Scope:** motherbee and the three spokes, the first deploy through the installation's update.
- **Why:**
  - hive-config-update-v1's stage 4: one SY.admin action brings every hive to what the motherbee
    holds, and keeps the update until each one is on it.
  - With it, A-198, in the same release by the operator's call since both touch SY.admin: mesh
    replies that can outgrow the frame. And A-201, found by A-198's build.
- **What changed:**
  - **SY.admin, the installation's update:**
    - `update_installation` (`POST /installation/update`, the operator's word, closed to the
      mesh) and `get_installation_update` (`GET`, Archi's read).
    - The target: the motherbee's core (its installed manifest hash, or dist's) and its
      installation file's version. Each hive gets its core first, then its config; a core
      restarting is followed to its settle.
    - The update is kept in SY.admin's state dir, one at a time, and resumed after a restart; the
      newest 20 closed ones are kept.
    - What it leaves alone, with why:
      - a hive rolled back with `core_rollback` keeps its core unless the run names it
        (`include_rolled_back`), and the update is `held`, naming it;
      - a copy edited by hand gets no config.
    - The automatic step: once per hive and target, only for a hive that did not answer and comes
      back. Never a rolled-back hive, one ahead of the motherbee or one with a hand-edited copy.
    - Refusals that change nothing: another run under way (409), the hive registry unreadable
      (503), the motherbee rolled back (409).
    - `set_installation_ai` opens the update with its version. `/versions` and `get_installation`
      carry its summary.
  - **Archi** stages `update_installation` as a plan the operator confirms, and follows the update
    to its end before its verdict.
  - **ops.py:** `deploy` from 0.1.123 runs the installation's update and follows it. Below 0.1.123
    it keeps the per-spoke core update.
  - **A-198:**
    - SY.admin, SY.identity, SY.vault and SY.orchestrator measure each reply against the frame
      less 1 KiB for the router (A-202). One that does not fit answers `REPLY_TOO_LARGE`, with its
      size and the action's outcome, so a write that ran says so.
    - Optional `limit`/`cursor` pages on the listings that grow. Without them, every row, as
      before.
    - An executor run's answer is cut to fit, largest outputs first. Past that,
      `REPLY_TOO_LARGE` with the run's id and log path.
    - IO.cloud relays the pages of the lists Cloud uses.
  - **A-201:** Archi reads the admin command log (`/hives/{hive}/commands`).
- **Checks before the release:**
  - GitHub's CI green: stage 4 on `f7dde147`, A-198 stacked on it at `e6f6c93f`.
  - **The review round,** one reviewer per item:
    - stage 4: F1–F12 fixed (the unreadable registry; reads one per hive with a 50 s timeout and a
      cadence; Archi's follow; the stale target; a timed-out core followed through `/versions`; one
      explicit run at a time; a rolled-back motherbee; ops.py's follow; the docs);
    - A-198: H1–H3, M4 and L1–L10 fixed (every vault page in the purge's check and the provider
      keys; a total order for the command log; the router's reserve; the executor's answer cut by
      share);
    - found and recorded for later: A-199, A-200, A-202.
- **Deploy (G1):**
  - Build of `e6f6c93f` in 19.9 min; publish in 13 s (the repo keeps 10: 0.1.113 pruned).
  - Snapshot `pre-inst-0-1-123` taken on all 4 VMs, `pre-inst-0-1-122` dropped.
  - 0.1.123 on all 4 in 392 s, health clean.
  - Phases:
    - snapshots 34.3 s;
    - apt install on the motherbee 145.8 s;
    - the motherbee's admin on the version 64.0 s;
    - the spokes answering 10.8 s;
    - the update's call 28.8 s (answered `partial` in 24.2 s, the three cores restarting);
    - the hives on the target 94.3 s.
  - By the spokes' own records: installed 18–27 s and settled 29–34 s after the call.
  - SY.admin recorded worker1 and ingress1 settled about 27 s later, and egress1 84 s later:
    - a read of egress1, sent 0.24 s after its link dropped for the core update, was lost (A-203);
    - the lost read held the motherbee orchestrator's worker for 45 s (A-199), with the other
      hives' reads queued behind it;
    - SY.admin then left egress1 unread for 50 s, as it does after a timed-out read.
  - The update closed `done` 123 s after the call; without the lost read, about 35 s.
- **Checked on 8.x (the gates, with the operator's OK for these PROD changes):**
  - **G2, a hive away during a config change:**
    - egress1 off the mesh for 2 min 3 s (its orchestrator, then its router).
    - **Version 4** (openai's model to gpt-5.5-mini): answered in 89.4 s.
      - The other three applied it in about 2 s.
      - egress1 was `not_answered` after 85.5 s of retries: the router answered `NODE_NOT_FOUND`
        each time. The update stayed open.
      - Restarted: the motherbee's sy-cognition and sy-frontdesk-gov, and worker1's
        sy-cognition, which the deploy had restarted since their last apply.
    - **egress1 back:**
      - its orchestrator showed 20 s later: its sy-config-routes reconnected on the SDK's backoff
        (A-193);
      - the automatic step finished it 23.4 s after that, once (`ok by automatic`, 79 ms);
      - the update closed `done`.
    - **Version 5, the undo, with every hive up:** answered in 5.1 s. Every hive applied it within
      2.3 s and nothing restarted. The update closed `done` 32 s later (A-205).
    - AI.pruebas answered in 8 s (anthropic, claude-sonnet-5-5).
  - **G3, a hive rolled back by the operator:**
    - **`core_rollback` of egress1:** the dry run named the deploy's generation and four binaries.
      The rollback applied and answered `FORWARD_UNKNOWN` after 242 s, as documented when the
      restore includes sy-orchestrator. egress1 then ran 0.1.122's core (source `core_rollback`)
      with config v5.
    - **An explicit run:** `held`, naming egress1. Its core `excluded`, with why; its config on
      the target.
    - **egress1 off again,** 2 min 17 s, with an explicit run during it: 122 s, egress1
      `not_answered` in core and config, the update open.
    - **egress1 back:** the automatic step left it alone (`core excluded, config excluded`), and
      the update went back to `held`.
    - **egress1's own `update`** (category core): answered in 12.8 s. The update closed `done` 32 s
      later, every hive on 0.1.123.
  - **A-198, over HTTP:**
    - pages: ILKs 63 in 7 pages of 10; the command log's 245 in pages of 20; `/versions` 2 hives
      a page; worker1's 10 nodes in pages of 3; the vault's 12 secrets with a limit of 4.
    - Without `limit`, every row as before: the ILKs in 24.6 KB (as measured before the
      release), `/versions` 29.7 KB, the command log's 50.
  - **A-201:** Archi read `/hives/motherbee/commands?limit=3` and answered with the three entries.
  - Health clean on every hive after the gates.
  - **Found:** A-203 (the router), A-204 and A-205 (the update's record): FINDINGS.

## 0.1.122 — The operator changes the installation's AI

- **Date:** 2026-11-09 (ART) · **Previous version:** 0.1.121 · **Commit:** `6c6d963d`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:**
  - hive-config-update-v1's stage 3c: the operator's change of `ai`, one version on every hive.
  - With it, A-197, found by its review round: Archi's catalog of SY.admin's actions did not fit
    the mesh's frame. It touches the same binaries.
- **What changed:**
  - **SY.admin:**
    - `get_installation` (Archi's read): the motherbee's file, its versions with their values,
      and each hive's state.
    - `set_installation_ai` (the operator's word, closed to the mesh): A-11's checks first.
      - A node left without its engine refuses the change.
      - On a switch of the default provider, a reader without the new provider's key refuses it.
    - Then the write, then the apply on every hive in parallel, with each hive's outcome and the
      timings in the reply.
    - `update` takes category `config`.
  - **The motherbee's orchestrator writes each version:** `SYSTEM_UPDATE` category `config` with
    a `write`. Recorded first, with its values, then the file atomically. Its refusals: a join
    running, a hand edit, the version check, invalid or unchanged values.
  - **Each hive's orchestrator applies it:** `sync_pending` until its copy matches. Then only its
    start readers restart (SY.cognition, and SY.frontdesk.gov on the motherbee): those whose
    engine changed, or that became active since its last apply (`config-applied.json`, with
    systemd's activation stamps).
  - **SY.admin's executor and Archi** take a new version at their next use, only for another
    engine, and keep their runtime when the vault is silent. No config update restarts them.
  - **A-197:** `list_admin_actions` answers its compact view over the mesh, and Archi asks for it.
- **Checks before the release:**
  - GitHub's CI green on both branches.
  - **The review round:** two reviewers, one on regressions and one on the new paths. No high.
    - Three mediums, fixed: the next-use refresh on a silent vault; a stale apply record; the
      provider keys on a switch.
    - Four lows, fixed. Two kept on purpose.
    - A-197 found, verified in SY.admin's journal on 8.x, fixed in this release (FINDINGS).
- **Deploy:**
  - Build of `6c6d963d` in 19.5 min; publish in 18 s.
  - Snapshot `pre-inst-0-1-122` taken on all 4 VMs, `pre-inst-0-1-121` dropped.
  - 0.1.122 on all 4 in 319 s, each spoke settled, health clean.
  - Phases:
    - apt install on the motherbee: 167.5 s;
    - the motherbee's admin on the version: 54.3 s;
    - the update calls: 18.5 s;
    - by the spokes' own records, installed 14–22 s and settled 23–28 s after the update call.
- **Checked on 8.x (the gate, with the operator's OK for this PROD change):**
  - **Before:** every hive `current` on version 1, and the versions list holding version 1's
    values (anthropic default, claude-sonnet-5-5; openai gpt-5.5).
  - **A-11's refusal:** a switch of the default provider to openai was refused in 6.7 s
    (`NODES_LEFT_WITHOUT_ENGINE`), naming SY.architect, whose own model is claude-opus-5-5.
    Nothing was written.
    - It was sent with `expected_version` 999, so the version check would have refused it
      anyway.
  - **Version 2, through loopback HTTP** (openai's model to gpt-5.5-mini): answered in 4.4 s, the
    action's own `total` 3.2 s.
    - Check 486 ms, write 41 ms, the folder scan 173 ms, the apply 2.5 s.
    - Every hive ok: the motherbee in 0.7 s; the spokes in 2.0–2.5 s, their second attempt.
    - The restarts, each hive's first config update (no record yet): on the motherbee,
      sy-cognition and sy-frontdesk-gov; on worker1, sy-cognition; none on the ingress or the
      egress. SY.admin and SY.architect kept running.
    - Each restarted reader logged the file v2. AI.pruebas answered in 7 s, reading v2.
  - **Version 3, the undo, through Archi** (openai back to gpt-5.5):
    - Archi read `/installation` and staged a one-step plan; `CONFIRM` ran it.
    - The plan's run took 4.4 s, and the chat answered "Execution completed successfully" 11 s
      after the `CONFIRM`.
    - Every hive `current` on v3, with nothing restarted anywhere: the engine was unchanged and
      the readers' activation stamps matched.
    - SY.admin's executor and Archi each logged that the file changed and their engine did not:
      they kept their runtime with no key read.
    - AI.pruebas answered on v3.
  - **A-197:** Archi's plan compiler fetched the compact catalog in 13 ms. No FRAME_TOO_LARGE
    since the deploy.
  - **At the end:** version 3 holds version 1's values (openai gpt-5.5). The versions list reads
    1 migration, 2 operator (gpt-5.5-mini), 3 operator (gpt-5.5).
  - **Timeline of version 2's apply** (`ops timeline`, t=0 at the write):
    - write answered 40 ms; the apply sent to all four hives 218 ms;
    - the motherbee's two restarts about 0.1 s each, applied 944 ms;
    - the spokes, `sync_pending` on the first attempt, applied on the second, 2 s later: egress1
      2.36 s, ingress1 2.61 s, worker1 2.81 s, with its SY.cognition restart.
    - Syncthing brought the file in under 2 s; the 2 s retry wait was the slowest phase.
    - Clock offsets: ingress1 −0.55 s, egress1 −0.48 s (round trips 1.5–1.8 s).

## 0.1.121 — The installation file in use

- **Date:** 2026-11-08 (ART; deployed 2026-11-09 UTC) · **Previous version:** 0.1.120 · **Commit:** `84d123ae`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** hive-config-update-v1's stage 3b. The operator's `ai` moves to one installation file,
  one version on every hive. Nothing changes in behavior yet.
- **What changed:**
  - **The motherbee's orchestrator writes the file:** version 1 from its hive.yaml `ai`,
    recorded first, written atomically at 0644.
  - **Distribution:** a new folder, `fluxbee-dist-installation`, follows vendor's members. The
    join copies the file with core and vendor, and writes no `ai`.
  - **The readers read it:** SY.admin, SY.architect, SY.cognition, SY.frontdesk.gov and
    ai.generic, each logging its source. An invalid file fails the AI work with the reason, and
    never stops SY.admin's or the orchestrator's start.
  - **`/versions`:** each hive's `config` (`current`, `behind`, `pending`, `edited_by_hand`).
  - **The install texts and the HANDBOOK:** choose the provider in hive.yaml before firstboot.
- **Checks before the release:**
  - GitHub's CI green on its branch, twice.
  - The review round found no high, and one medium, fixed: the install texts on choosing the
    provider. Its lows were fixed: the join refuses without a file; SY.admin keeps its executor on
    a hive.yaml read failure; the warning names rt-gateway's check; Archi's handbook.
- **Deploy:** build of `84d123ae`; publish in 13 s; snapshot `pre-inst-0-1-121` taken on all 4
  VMs, `pre-aisdk-0-1-120` dropped; 0.1.121 on all 4 in 353 s; each spoke settled; health clean.
  - By the spokes' own records (new in `ops deploy`), installed 12 to 20 s after the update call
    and settled 21 to 29 s after it.
- **Checked on 8.x:**
  - **The file on the motherbee:** 0644, version 1, the motherbee's `ai` (anthropic default,
    claude-sonnet-5-5; openai gpt-5.5), its hash recorded as the migration.
  - **All four hives `current`** at the first check after the deploy, 12 s.
  - **The motherbee's readers** (SY.admin, SY.architect, SY.cognition, SY.frontdesk.gov) read the
    file v1, on the same engine as before.
  - **worker1's SY.cognition,** restarted by the update before the file arrived, read hive.yaml
    (the same values), as planned. Restarted once the file was there, it read the file v1.
  - **AI.pruebas** answered in 7 s, its log naming the file v1.
  - **A throwaway worker `gate0221` on VM 104** joined in 139 s, its finalize in 8.4 s:
    - its installation file was copied by the join, with the same hash;
    - its hive.yaml has no `ai`;
    - its SY.cognition read the file v1 from its first start, never the fallback engine;
    - `/versions` showed `config: current`;
    - removed, and the installation folder's members follow vendor's again (4). VM 104 destroyed.
    Health clean.

## 0.1.120 — The AI SDK reads the installation file

- **Date:** 2026-11-08 (ART) · **Previous version:** 0.1.119 · **Commit:** `0f516fdd`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** hive-config-update-v1's stage 3a. The AI SDK goes alone in its release ([METHOD](logbook/METHOD.md) §9), ahead of the readers that will use it (3b).
- **What changed** (`crates/fluxbee_ai_sdk/src/installation.rs`):
  - `InstallationFile`: schema 1, a version, and the operator's `ai`. It is validated as every
    reader needs it (each provider named has its model), and rendered as the motherbee will write
    it.
  - The file's state for `/versions`: absent, valid, or invalid with the hash of its bytes.
  - `ai_settings`: the file's `ai` with its version and hash, else the hive's own hive.yaml `ai`,
    read only while the file is absent.
  - No reader uses it yet.
- **Checks before the release:**
  - GitHub's CI green on its branch, twice (`cargo test --workspace` runs the SDK's tests).
  - The review round found no high. The SDK's API was made what 3b needs, since a later SDK change
    would need a release of its own.
- **Deploy:** build of `0f516fdd`; publish in 13 s; snapshot `pre-aisdk-0-1-120` taken on all 4
  VMs, `pre-watch-0-1-119` dropped; 0.1.120 on all 4 in 290 s; each spoke settled; health clean.
  - The phase "the spokes installed" read 54 s. The timeline shows worker1's restarts took 3.5 s
    and its settle came 7 s after the call. The 54 s is when the script saw it: the motherbee's
    `/versions` forward is slow while a spoke's orchestrator restarts, and the script checks the
    spokes one after another.
- **Checked on 8.x:** AI.pruebas@worker1 answered a plain question in 9 s, on the new SDK.

## 0.1.119 — The Syncthing watchdog leaves a join alone

- **Date:** 2026-11-08 (ART) · **Previous version:** 0.1.118 · **Commit:** `b7f20c52`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** FINDINGS A-194, found by 0.1.118's timed gate: during a join's finalize the watchdog
  restarted a Syncthing that the finalize had just stopped to restart.
- **What changed** (`src/bin/sy_orchestrator.rs`): one check, `a_join_owns_syncthing`, true on a
  spoke while its finalize runs and on the motherbee while an `add_hive` or `remove_hive` does. The
  watchdog takes it before it reads the desired config, and again right before it restarts
  Syncthing; the peer reconcile takes it in both of its places.
- **Checks before the release:** GitHub's CI green on its branch, twice. The review round found no
  high or medium; its lows were fixed (the second check, the motherbee's side, one check
  everywhere), and it found A-196 (a join's peer link written and never applied, latent).
- **Deploy:** build of `b7f20c52`; publish in 18 s; snapshot `pre-watch-0-1-119` taken on all 4 VMs,
  `pre-join-0-1-118` dropped; 0.1.119 on all 4 in 266 s (apt install on the motherbee 141 s, the
  spokes installed 19 s, settled 9 s); each spoke settled; health clean.
- **Checked on 8.x, a throwaway worker `gate0219` on VM 104,** joined end to end, timed:
  - connected at 120 s, done at 133 s; the finalize took 8.3 s (core 5.1 s with nothing
    restarted, vendor 1.4 s, peer link 1.6 s, dist probe 0.2 s);
  - **A-194:** inside the finalize, Syncthing restarted once, for the peer link, and the watchdog
    did not restart it; with `gate0218` on 0.1.118 it had restarted it a second time 89 ms later.
    The motherbee's Syncthing restarted once too, to add the peer;
  - its dist stayed whole, and every folder ended `idle` with no revert;
  - removed, the motherbee left as before; VM 104 destroyed; health clean.

## 0.1.118 — A fresh worker joins again, and every step is timed

- **Date:** 2026-11-08 (ART) · **Previous version:** 0.1.117 · **Commit:** `9ea2f812`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** FINDINGS A-190, found by 0.1.117's gate: a fresh worker's join failed at its finalize,
  so no new worker could join 8.x. And the operator's call of the same day: every gate measures
  where the time goes.
- **What changed** (`src/bin/sy_orchestrator.rs`, `lab/ops.py`):
  - **The receive-only revert waits for its source:** a member other than this device,
    connected and sharing the folder (`remoteState: valid`), for 60 s in a row.
  - **The join's finalize goes on while its vendor is still arriving,** with a Syncthing
    installed and running, and reports it in `vendor.awaiting_update`. Its core step takes A-186's
    words for a binary still on its way as pending. Its dist probe gets what is left of the
    motherbee's wait. It answers each step's `timings_ms`.
  - **A core update with no binary and no unit new restarts nothing,** and answers `ok` at once.
  - **`ops timeline`** merges every hive's core journals into one timeline, with each hive's clock
    offset. **`ops deploy`** prints each step's time and ends with the phases.
- **Checks before the release:**
  - GitHub's CI green on its branch, twice: as built, and with the review's fixes.
  - The review round: one medium, fixed. The first build reverted once the peer was a member; the
    finalize's own peer link restarts Syncthing, so the next tick, before the motherbee's index
    arrived, would still have deleted dist. Now the source must be connected and sharing it for
    60 s.
- **Deploy:** build of `9ea2f812` in 9 min; publish in 14 s; snapshot `pre-join-0-1-118` taken on
  all 4 VMs, `pre-relkeys-0-1-117` dropped; 0.1.118 on all 4 in 263 s; each spoke settled; health
  clean.
- **Where the deploy spent its time** (`ops deploy`'s phases):

  | Phase | Time |
  | --- | --- |
  | snapshots | 29 s |
  | apt install on the motherbee | 126 s |
  | the motherbee's admin on the version | 47 s |
  | the spokes answering | 8 s |
  | the update calls (worker1 the slowest) | 15 s |
  | the spokes installed | 24 s |
  | the spokes settled | 8 s |
  | health | 5 s |

  Inside the apt install (`ops timeline`): about 40 s to fetch and unpack; then the package's
  scripts kept sy-orchestrator, sy-admin and sy-wf-rules down for 65 s (the unpack and the base
  runtimes' seeding). The orchestrator's start then took 25 s to rebuild the three role trees,
  hashing the same binaries once per role, before 12 s of restarts.
- **Checked on 8.x, a throwaway worker `gate0218` on VM 104** (a clone of template 9000,
  10.10.10.60), joined key-first:
  - **It joined:** `connected` at 113 s and the join done at 127 s, `dist_sync_ready: true`, the
    peer linked.
  - **The finalize took 8.1 s:** core 5.1 s (a check, nothing restarted), vendor 1.1 s, the peer
    link 1.4 s, the dist probe 0.4 s. Its answer arrived.
  - **The join's longest step** was the bootstrap's copy over SSH, about 67 s.
  - **Its dist stayed whole:** the watchdog held the local items until the source was there
    ("left as they are until its source…", then "with its source connected"). Syncthing then took
    the motherbee's versions over the identical files within a minute, so no revert ran. Every
    folder ended `idle` with nothing needed.
  - **Its state:** nine units active with no restart; a hive.yaml without the release's keys;
    `/versions` core `ok`, recorded as installed by the no-op update, and no `awaiting_update`.
  - **Removed:** its Syncthing peer was unlinked, and the motherbee's devices and folder members
    are as before. VM 104 was destroyed. Health was clean afterwards.
- **Timing findings recorded:**
  - A-193 🔵: where a deploy and a join spend their time.
  - A-194 🟡: the Syncthing watchdog restarts Syncthing while a join's finalize is restarting it.
  - A-195 🔵: WAN frames dropped at each link-up, before the peer's LSA names their sender.

## 0.1.117 — The release's settings live in the code

- **Date:** 2026-11-08 (ART) · **Previous version:** 0.1.116 · **Commit:** `e1c5e5ab`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** stage 2 of [hive-config-update-v1](../docs/hive-config-update-v1.md) (§10), for A-181:
  a spoke's hive.yaml is written once, at its join, so a release's change to a role's SY nodes or
  to the blob's topology never reached a hive that had joined before it.
- **What changed:**
  - **`src/release_settings.rs`:** each role's SY nodes, their order and waits, and the blob's and
    dist's topology, with today's values. Golden tests hold them to what `hive.yaml.example` and
    the joins wrote.
  - **The orchestrator, SY.identity and SY.vault** read the table, never hive.yaml.
  - **The joins and `hive.yaml.example`** stop writing those keys. An old hive.yaml still loads;
    the orchestrator names the keys it ignores once at its start, and says to keep them while the
    hive may go back to 0.1.116 or earlier.
  - **A node a release drops or adds:** the update runs in the previous orchestrator, which leaves
    a dropped node out. The new orchestrator's start settles the update: it removes a dropped node
    and installs an added one, both kept in the update's generation, so `core_rollback` undoes
    them. `/versions` shows it as `core.last_update.settled`.
  - **`lab/ops.py`:** `deploy` waits for each spoke's settle, and reports anything left out,
    pending or awaiting.
  - **Docs:** the design doc, sy-orchestrator, sy-identity, sy-vault, operations, architecture,
    opa-distribution, the HANDBOOK's new-node recipe, rollback and downgrade.
- **Checks before the release:**
  - Read on 8.x: every hive's lists and flags were the table's, and every spoke's `sy-*` units its
    role's.
  - GitHub's CI green on its branch, twice: as built, and with the review's fixes.
  - The review round: no high finding. Two mediums were fixed: the old keys must stay for a
    rollback below 0.1.117, and the dist recheck settles only the update its own start left open.
- **Deploy:**
  - build of `e1c5e5ab` in 19 min; publish in 17 s;
  - snapshot `pre-relkeys-0-1-117` taken on all 4 VMs, `pre-start-0-1-116` dropped;
  - 0.1.117 on all 4 in 314 s; each spoke settled; health clean.
- **Checked on 8.x:**
  - **Nothing changed on the 4 hives,** compared with a snapshot taken before:
    - units, Syncthing folders and their members, and the `sy-*` unit files were all the same;
    - the release's keys were still in each hive.yaml;
    - no `awaiting_update` anywhere.
  - **The old keys:** each orchestrator named them once at its start, exactly the ones its own
    hive.yaml holds.
  - **The first settle:** each spoke settled 0.1.116's update generation with nothing dropped or
    installed.
  - **SY.vault:** computed the motherbee's 11 well-known SY ILKs. Its log line still says "from
    hive.yaml"; that text is fixed in a later release.
  - **A throwaway worker, `gate0217`, on VM 104** (a clone of template 9000, 10.10.10.60), joined
    key-first with the motherbee's public key:
    - its hive.yaml held none of the release's keys;
    - it ran the worker's six SY nodes, from the table;
    - its SY.identity seeded the six system ILKs of the release's list;
    - its Syncthing folders were the worker's (`fluxbee-blob` and the dist folders, no public one).
  - **Its join failed at the finalize, twice,** for a reason this release does not touch:
    FINDINGS A-190 🔴. The receive-only revert, on its first watchdog tick, deleted what the join's
    bootstrap had put in dist. The finalize's strict vendor step then found no manifest. Its first
    reply was also lost: the finalize's own core step restarts the router.
  - **Cleanup:** `gate0217` was removed. The motherbee kept no device, folder member or hive entry
    for it. Its six system ILKs stay registered, as `worker2`'s do since 2026-10-06 (A-191). VM 104
    was destroyed.
- **Seen on the way:** A-192 🟢, a running IO node's `VAULT_GET` refused at a motherbee restart
  (`IDENTITY_UNAVAILABLE`, transient). It happens since at least 2026-11-03, and the node keeps its
  credentials.

## 0.1.116 — A spoke's start never stops on dist

- **Date:** 2026-11-08 (ART) · **Previous version:** 0.1.115 · **Commit:** `9616278d`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** stage 1 of [hive-config-update-v1](../docs/hive-config-update-v1.md) (§10). Two bugs
  that are true today:
  - **A-186:** a spoke that restarts while a release syncs never starts again.
  - **A-189:** the same, through the vendored Syncthing.
- **What changed** (`src/bin/sy_orchestrator.rs`):
  - **The start checks dist after Syncthing starts.** A role binary that is missing, of another
    size or hash, or not in the manifest is a warning and an entry of `core.awaiting_update` in
    `/versions`, not an error.
  - **A minute's recheck closes the report.** The core update's own check stays strict, and the
    motherbee copies a binary into a role's tree only once it matches.
  - **The vendored Syncthing.** The start, the watchdog and the sync probes keep the installed
    Syncthing when the vendored one does not match, and report it as `vendor.awaiting_update`.
    `update category=vendor` and an ingress join stay strict.
  - **Docs:** sy-orchestrator, operations, architecture, and the HANDBOOK's new-node recipe.
- **Checks before the release:**
  - GitHub's CI green on its branch.
  - The review round: no high or medium finding. Four stale doc lines were fixed. One latent case
    became a release rule: Syncthing's unit stays valid for the previous vendored Syncthing.
- **Deploy:**
  - build of `9616278d` in 10 min; publish in 16 s;
  - snapshot `pre-start-0-1-116` taken on all 4 VMs, `pre-aisdk-0-1-115` dropped;
  - 0.1.116 on all 4 in 265 s, health clean.
- **Checked on 8.x (egress1):**
  - **A-186:** `dist/core/bin/sy-opa-rules` set aside, then the orchestrator restarted. It started
    with no restarts. Syncthing and the router were untouched. `core.awaiting_update` listed the
    binary as `missing`. With the binary back, the report closed in 27 s.
  - **A-189:** the vendored Syncthing set aside, then the orchestrator restarted. It started and
    kept the installed Syncthing, and `vendor.awaiting_update` listed it as `missing`.
  - **A-189 after a reboot:** Syncthing stopped too, as after a reboot. The orchestrator's start
    brought the installed Syncthing up in 3 s, where before it stayed down. With the binary back,
    the report closed in 3 s.
  - The aside folder was removed, and health was clean afterwards.

## 0.1.115 — A provider's failure reaches the person

- **Date:** 2026-11-08 (ART) · **Previous version:** 0.1.114 · **Commit:** `02731d26`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** shared code goes alone ([METHOD](logbook/METHOD.md) §9). A-184, from the sweep of
  pending items, with the operator's calls of 2026-11-08:
  - SY.cognition does not judge a provider's error;
  - the person gets one fixed English text, and nothing more.
- **What changed:**
  - **`crates/fluxbee_ai_sdk`:**
    - A provider's error status is `Provider`, carrying its status, kind, type, param and request
      id, with keys scrubbed.
    - A refusal (Anthropic's `stop_reason`, OpenAI's refusal or content filter), a missing key and
      an answer with nothing to send are each their own error.
    - OpenAI retries a transport error, 408, 409, 429 and 5xx as Anthropic does: one policy, the
      status read before the body, `retry-after` honored.
  - **ai.generic and SY.frontdesk.gov:**
    - They match the type, where they parsed the error's text.
    - With no contract, the person gets the fixed text as the agent's text.
    - Under a WF's menu or a response envelope, the error payload carries it in `reply`.
  - **SY.cognition:** counts provider errors apart (`*_provider_errors_total`).
  - **SY.architect:** shows the operator the full error. Its two cookbooks' catch sends `reply`
    (`seed_version` 7). The handbook says so (§4.1, §4.2.4, atom 6).
- **Checks before the release:**
  - GitHub's CI green on its branch and on the integrated head.
  - The review round:
    - one high finding: under a WF's menu the fixed text reached no one, since WFs forward an
      agent's answer by its evaluation and catch its error by the payload's type;
    - an OpenAI answer cut by the content filter went out half;
    - the retry's log line came after its wait;
    - a test that did not test its name.

    All four fixed, and the WF's own test runs the catch.
- **Deploy:**
  - build of `02731d26` in 22 min; publish in 17 s;
  - snapshot `pre-aisdk-0-1-115` taken on all 4 VMs, `pre-gosdk-0-1-114` dropped;
  - 0.1.115 on all 4 in 279 s, health clean.
- **Checked on 8.x:**
  - **The fixed text:** a throwaway agent, AI.gate0215@worker1, in a tenant with no key. A person's
    message through a mesh client playing an IO node got back, as a plain text reply to the
    person, *"There is a problem with the AI provider this agent uses, so it could not answer.
    Please contact your administrator."* The IO context's reply target was kept. The agent's log
    reads `AI runtime request failed … reply="provider_failure_text" error=missing AI api key`.
    The agent and its tenant were purged.
  - **AI.pruebas@worker1** answered a plain question in 9 s.
  - **Archi** replaced both cookbooks' manual entries with the new seeds at its start.
  - **SY.cognition** on worker1 and the motherbee shows `semantic_tagger_provider_errors_total` and
    `narrative_summarizer_provider_errors_total`, both 0.

## 0.1.114 — The Go SDK says HELLO once

- **Date:** 2026-11-08 (ART) · **Previous version:** 0.1.113 · **Commit:** `f8425df5`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** shared code goes alone ([METHOD](logbook/METHOD.md) §9): A-183, from the sweep of
  pending items.
- **What changed:** `go/fluxbee-go-sdk/client.go`: the connection of a node's first handshake is
  the one its manager serves, where it was closed and a second one opened; `handshakeWithin` bounds
  every handshake by 5 s, at every reconnect too. A router comment that spoke of the second HELLO.
- **Checks before the release:** Go vet and the SDK's tests on the Mac, one of them seen failing
  on the old client; GitHub's CI green on its branch and on the integrated head; the review round
  (shared with 0.1.113): no defect.
- **Deploy:**
  - build of `f8425df5` in 27 min; publish in 15 s;
  - snapshot `pre-gosdk-0-1-114` taken on all 4 VMs, `pre-rustsdk-0-1-113` dropped;
  - 0.1.114 on all 4 in 260 s, health clean.
- **Checked on 8.x:** on 0.1.113 each Go node of the motherbee had registered twice since its own
  start, after the router's; on 0.1.114 once (SY.opa.rules, SY.timer, SY.wf-rules; SY.opa.rules on
  worker1 too). WF.gate0206@worker1, on wf.engine 0.1.114, registered once with each router
  process: with the old one a moment before it stopped, and with the new one.

## 0.1.113 — The Rust SDK's handshake bounded

- **Date:** 2026-11-08 (ART) · **Previous version:** 0.1.112 · **Commit:** `f72ca82e`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** shared code goes alone ([METHOD](logbook/METHOD.md) §9): A-182, from the sweep of
  pending items.
- **What changed:** `perform_handshake` (`crates/fluxbee_sdk/src/node_client.rs`) waits at most
  10 s for the router's ANNOUNCE, at the first connection and at every reconnect; past it the
  handshake fails and is retried.
- **Checks before the release:** clippy and the SDK's tests on the Mac; GitHub's CI green on its
  branch and on the integrated head; the review round, for both SDKs: no defect.
- **Deploy:**
  - build of `f72ca82e` in 26 min (every Rust binary); publish in 14 s;
  - snapshot `pre-rustsdk-0-1-113` taken on all 4 VMs, `pre-take-0-1-112` dropped;
  - 0.1.113 on all 4 in 256 s, health clean.
- **Checked on 8.x:** every node joined its router again: 4 hives alive and 36 nodes active by
  type, as before the deploy. SY.cognition on worker1 sent its HELLO and was connected 11 ms
  later; no handshake failed.

## 0.1.112 — The first agent on a thread takes what people said, and one orchestrator per host

- **Date:** 2026-11-08 (ART) · **Previous version:** 0.1.111 · **Commit:** `1dc2eb40`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** the sprint's sixth release ([METHOD](logbook/METHOD.md) §9): A-173, the operator's
  call of 2026-11-07 (*"si es posible entonces lo que planteaste antes.. hacelo también"*),
  rebuilt on its first review; A-180, found checking A-177; A-175's interim, the operator's call
  of 2026-11-08 that a drift alert is log.
- **What changed:**
  - **SY.cognition (A-173):** an agent's new communication takes the thread's communication with
    no agent, when no other agent has one there. The take is recorded under the held state's
    lock, and every turn of the communication taken is the taker's from then on, at intake and in
    the worker. The taken row leaves the file in the transaction that writes the agent's.
  - **SY.orchestrator (A-180):** it refuses any argument and exits 2, and holds
    `/var/run/fluxbee/sy-orchestrator.lock` (the shared `json_router::instance_lock`) before its
    PID and its boot.
  - **SY.orchestrator and SY.architect (A-175's interim):** a drift alert read for another hive,
    and the global one, carry a `note` that a hive's own checks stay on that hive; Archi says so.
- **Checks before the release:** on the Mac, clippy of each touched binary and SY.cognition's
  tests; GitHub's CI green on every branch and on the integrated head; a review round per set,
  every finding fixed (A-173's second review: every turn of a taken communication now follows it,
  only the first agent takes, a write-back loads a communication let go from memory).
- **Deploy:**
  - build of `1dc2eb40` in 26 min; publish in 13 s;
  - snapshot `pre-take-0-1-112` taken on all 4 VMs, `pre-router-0-1-111` dropped;
  - 0.1.112 on all 4 in 258 s, health clean.
- **Checked on 8.x:**
  - **A-180, on egress1:** the live orchestrator (PID 1578095) held its lock.
    `sy-orchestrator --version` exited 2; `sy-orchestrator` alone exited 1, naming the lock and
    the live PID. The PID file and the live process did not change.
  - **A-175's interim:** `GET /hives/worker1/drift-alerts` and `GET /drift-alerts` carry the note,
    the motherbee's own read does not. Asked whether worker1 had drift alerts, Archi said the
    motherbee recorded none and that worker1's own checks are in its own journal.
  - **A-173, gate0206 on worker1, thread `gate0206-take`:** A wrote to B through the WF, B
    answered, then A wrote to the agent. Cognition kept one row, the agent's: the two people's
    lines and A's message, each with its writer, and no row with no agent; the take counted 1 and
    the three turns were analyzed.
    - Asked next what A had asked billing and what billing answered, the agent said it in one
      line: what people said before it reaches it.
    - **Found (A-185):** the agent's first message carried no memory package
      (`memory_package=false`): the router reads the region at delivery, before cognition takes
      that very message. The lines reach the agent from its second message on.
    - The gate's first ask, to copy its Recent conversation word by word, was refused by the model
      (`stop_reason=refusal`), and the runner's error reached no one (A-184).

## 0.1.111 — One router per name, a WAN that fails closed, and the baked policy checked against its Rego

- **Date:** 2026-11-08 (ART) · **Previous version:** 0.1.110 · **Commit:** `cc01f86f`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** the sprint's fifth release ([METHOD](logbook/METHOD.md) §9), the router alone, from
  the sweep of pending items (the operator: *"por los bugs, si quedan claros que son bugs
  continuá por ahí"*).
- **What changed:**
  - **rt-gateway (A-177):** it refuses any argument and exits 2; it holds
    `/var/run/fluxbee/<router>.lock` (an flock, until the process ends) before it opens its shared
    memory, its stream and its sockets, so a second instance exits naming the holder's PID. Any
    other failure to lock is warned and the router runs as before.
  - **rt-gateway (A-178):** an absent, empty or unknown `wan.mtls` is `required`; only an explicit
    `permissive` or `disabled` loosens it. The four hives already said `required`.
  - **sy-opa-rules and the baked policy (A-179):** the compile reads the module as `policy.rego`,
    so the same Rego always gives the same wasm, where it recorded a random temporary path; both
    baked wasm compiled again with it; CI's `policy-wasm-guard` compares them with the Rego. A
    compile error names `policy.rego` and its line.
- **Checks before the release:** on the Mac, clippy and the router's tests, Go vet and the compile
  tests, and the committed wasm reproduced from the current Rego with their recorded paths; GitHub's
  CI green on every branch and on the integrated head; one review round for the three, every
  finding fixed.
- **Deploy:**
  - build of `cc01f86f` in 18 min; publish in 15 s;
  - snapshot `pre-router-0-1-111` taken on all 4 VMs, `pre-family-0-1-110` dropped;
  - 0.1.111 on all 4 in 274 s (manifest `dbe10afef9abff99`), health clean.
- **Checked on 8.x:**
  - **A-177, on egress1:** the live router (PID 1564690) held its lock. `rt-gateway --version`
    printed that it takes no arguments, exit 2; `rt-gateway` alone was refused naming the lock and
    PID 1564690, exit 1. After both: the same PID, the two sockets with the same inodes.
  - **A-178:** each router logged `wan mtls mode mode=required` at its start.
  - **A-179:** a broken policy checked through SY.admin (`POST /opa/policy/check`) answered
    `COMPILE_ERROR` naming `policy.rego:6`, no temporary directory; a valid one `ok`. The user
    policy in sync on all 4, their routers `ok`.

## 0.1.110 — Archi's executor names what each route stores, three texts, and the egress's alert

- **Date:** 2026-11-07 (ART) · **Previous version:** 0.1.109 · **Commit:** `eb972451`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** the sprint's fourth release ([METHOD](logbook/METHOD.md) §9), a branch per binary:
  A-172, found by 0.1.109's gate, with its family; A-176 and the egress's alert from the sweep;
  A-174 from the docs sweep. A-173 was held back by its review.
- **What changed:**
  - **SY.admin (A-172):** the executor's fields are the ones each route stores:
    `remove_runtime_version`'s `runtime_version`; `get_hive` and `remove_hive`'s `hive_id`; the
    tenant's delete, restore, purge and node list name `hive` and `tenant_id`; `core_rollback`
    names its hive; the global reads take none; `set_node_config` names its binding fields. The
    catalog's CI check and a test hold it.
  - **SY.timer, SY.wf-rules, SY.architect (A-176):** `TIMER_UNKNOWN_VERB`; "unknown operation";
    the model, not OpenAI.
  - **SY.orchestrator:** the egress's `internet_unreachable` drift alert, in its own file
    (A-175: the motherbee does not read it yet); a worker reads its IPv6 block back (A-174).
- **Checks before the release:** on the Mac, clippy of every touched binary, Go vet and tests, and
  the affected SY.admin tests filtered; GitHub's CI green on every branch and on the integrated
  head; a review round per set, every finding fixed or recorded (A-175).
- **Deploy:**
  - build of `eb972451` in 15 min; publish in 14 s;
  - snapshot `pre-family-0-1-110` taken on all 4 VMs, `pre-reset-0-1-109` dropped;
  - 0.1.110 on all 4 in 268 s (manifest `5c7cd68ff8827ea5`), health clean.
- **Checked on 8.x:**
  - **A-172:** a spare `ai.generic 0.1.110-spare1`, published available only, was removed by
    Archi on CONFIRM: the command log shows `origin: executor`, `ok`, params `{runtime,
    runtime_version}`, and Syncthing took it off worker1.
  - **The family:** a gate tenant, `gate0110`, deleted and restored by Archi on CONFIRM, both `ok`
    from the executor (0.1.109 had no schema for them); then deleted and purged through the
    operator's HTTP route.
  - **A-174, A-176 and the egress alert:** by their tests; worker1 does not route through the
    egress.

## 0.1.109 — A base runtime goes back to its package's version, a password join chooses, and four small ones

- **Date:** 2026-11-07 (ART) · **Previous version:** 0.1.108 · **Commit:** `3f292500`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** the sprint's third release ([METHOD](logbook/METHOD.md) §9), every item on SY.admin, so
  one branch: A-122, A-165 and A-164 (the operator: *"OK avanzar con 165, 164 si te parece a
  vos"*), and from the sweep of pending items A-169 and A-170.
- **What changed:**
  - **A-122:** the seed records each base runtime's `package_version`, and `list_runtimes` and
    `get_runtime` show it. `reset_base_runtime` (motherbee-only, closed to the mesh, confirmed by
    the operator through Archi) makes it current again. `remove_runtime_version` never removes it,
    `publish-runtime.sh` refuses a hand publish under it, and the lab's deploy shows the seed's
    NOTE.
  - **A-165:** a join that may log in over a password (`ssh_password` and no `ssh_key`) must carry
    `harden_ssh`, or it is refused before anything touches the box; a socket-only re-run says it
    applied none.
  - **A-164:** the vault help's types come from the SDK's list, with `tls`, `whatsapp` and
    `linkedhelper_adapter`, each with its value and how its node finds it.
  - **A-169:** an identity read that names a spoke goes to the primary, as writes do.
  - **A-170:** the OPA version counter is one locked, synced step; `publish_artifact`'s help says
    the constants; a hive lists itself with its own role.
- **Checks before the release:** on the Mac, clippy of SY.admin and the orchestrator and the seed's
  e2e script (36 of 36); GitHub's CI green on every push but the first (a test caught the reset
  rewriting a whole entry); two review rounds, one reviewer each, every finding fixed (A-171
  recorded).
- **Deploy:**
  - build of `3f292500` in 12 min; publish in 15 s;
  - snapshot `pre-reset-0-1-109` taken on all 4 VMs, `pre-contract-0-1-108` dropped;
  - 0.1.109 on all 4 in 264 s (manifest `80ccc9303d128108`), health clean; the deploy printed
    every base runtime's NOTE, each moved to 0.1.109 by the package.
- **Checked on 8.x:**
  - **A-122** (`a122_gate.py`): every base runtime showed `package_version` 0.1.109. A hand
    publish of `ai.generic 0.1.109-gate1` showed as one, and AI.pruebas moved onto it in 81 s.
    Removing 0.1.109 answered `RUNTIME_PACKAGE_VERSION`, and a hand publish under 0.1.109 was
    refused. Asked to bring ai.generic back, Archi read the runtime, staged one
    `reset_base_runtime` step and ran nothing until CONFIRM; the command log shows it `ok` from the
    executor, and AI.pruebas was back on 0.1.109 in 47 s.
  - **A-172 found:** Archi's removal of the gate's version failed with `INVALID_REQUEST`: the
    executor sends `version`, the action reads `runtime_version`. The version was removed through
    SY.admin's HTTP route instead; Syncthing took it off worker1.
  - **A-165** (`r0109_gate.py join`): a password join with no `harden_ssh` was refused with
    `INVALID_REQUEST` and its reason, and no hive was created.
  - **A-164:** vault_put's executor notes name the SDK's types and the product's three with their
    values; "escape hatch" is gone.
  - **A-169:** `GET /hives/worker1/identity/ilks` and `/tenants` answer `ok` (0.1.108 answered
    `UNAUTHORIZED_REGISTRAR`).
  - **A-170:** by its tests.

## 0.1.108 — The provider holds a response contract inside the tool loop too (A-150)

- **Date:** 2026-11-07 (ART) · **Previous version:** 0.1.107 · **Commit:** `7f18a610`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** with tools, an agent's response contract was an instruction checked afterwards, never
  held by the provider (FINDINGS A-150). Both providers can hold it with tools; the fix reaches the
  shared AI SDK, so it went alone ([METHOD](logbook/METHOD.md) §9).
- **What changed:**
  - **The AI SDK:** a tool turn carries the run's answer schema next to the tools: Anthropic's
    `output_config.format`, OpenAI's `text.format`. A 400 that refuses it has that turn asked
    again without it, from either provider. Every other caller (SY.cognition, SY.architect,
    SY.admin's executor, the frontdesk) sends the same requests as before.
  - **ai.generic:** the tool loop passes the contract's schema; the journal says
    `output_contract_mode=structured_output`. The instruction and the check under a `text_field`
    stay.
- **Checks before the release:** on the Mac, clippy of the SDK and ai.generic; GitHub's CI green on
  the branch and on the integrated head; the review round (one reviewer): one medium (OpenAI
  refuses a strict schema with an optional property, now asked again without it) and one low (the
  docs promised a check the tool loop makes only under a `text_field`), both fixed.
- **Deploy:**
  - build of `7f18a610` in 31 min: the shared SDK rebuilt nearly every binary;
  - publish in 12 s;
  - snapshot `pre-contract-0-1-108` taken on all 4 VMs, `pre-train2-0-1-107` dropped;
  - 0.1.108 on all 4 in 270 s (manifest `43c23a984b37f817`), health clean.
- **Checked on 8.x** (three runs, `a150_gate.py`):
  - AI.pruebas, asked for a poem in prose and a saved text file under a contract (`reply`, and
    `mood` in an enum), logged `contract="envelope" tool_count=14
    output_contract_mode="structured_output"` on Anthropic (`claude-sonnet-5-5`).
  - Each answer was the contract's object, with `mood` in its enum, its text the `reply` and the
    file attached; no schema warning and no retry.
- **Found:** A-168 🔵, from the evaluation. A WF asks an agent with a menu, which has no schema, so
  this release does not reach it. Whether a menu becomes one is the operator's call.
- **Rollback:** `apt install fluxbee=0.1.107` on the motherbee, then a core-update of the spokes.

---

## 0.1.107 — The sprint's second release: "the agent" is only the AI agent (A-167), and the factory reset clears every hive's cognition (A-160)

- **Date:** 2026-11-07 (ART) · **Previous version:** 0.1.106 · **Commit:** `6c107a9b`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** the sprint's second release, after the operator's call on cognition's numbers (a
  quarrel the gate passes is news; the gate stays as it is): 0.1.106's gate found an episode
  calling a person "the agent" (A-167), and a worker's cognition outlived the factory reset
  (A-160). Both touch SY.cognition, so they went on one branch.
- **What changed:**
  - **SY.cognition (A-167):** a narrator's rule for everything it writes: "the agent" is only the AI
    agent, never a person, even one who answers for a company.
  - **SY.cognition (A-160):** `COGNITION_CLEAR`, taken only from SY.admin@motherbee: it drops every
    communication and episode it keeps, in memory, in its file (then VACUUM and a WAL checkpoint)
    and in its region, keeps running, and answers what it removed.
  - **SY.admin (A-160):** `cognition_clear` (`POST /hives/{hive}/cognition/clear`), behind the
    operator's CONFIRM, run by the executor, closed to the mesh; `send_node_message` refuses a
    `COGNITION_CLEAR`. The action registry is at version 19.
  - **The factory reset (A-160):** every hive's cognition is cleared through the action before
    anything stops, the motherbee's included; SY.storage and the frontdesk stay down, masked, for
    the truncate.
- **Checks before the release:**
  - on the Mac, clippy of what changed; GitHub's CI green on the branch and on the integrated heads
    (`4c978e26`, then `6c107a9b`): every test of both Rust workspaces, the catalog's parity and
    the guards;
  - the review round (one reviewer): one medium and two lows, all fixed in `6c107a9b`:
    - the reset's stopped cognition could be started again by the orchestrator's watchdog before
      its file went;
    - `send_node_message` could carry a `COGNITION_CLEAR` past the action's name;
    - A-167's rule covered the episode only, not the memory and the feeling.
- **Deploy:**
  - build of `4c978e26` in 9 min, then of `6c107a9b`, with the review's fixes, in 9 min;
  - publish in 13 s;
  - snapshot `pre-train2-0-1-107` taken on all 4 VMs, `pre-train1-0-1-106` dropped;
  - 0.1.107 on all 4 in 222 s (manifest `1d766812f0b07fdf`), health clean.
- **Checked on 8.x:**
  - **A-167** (gate0206, thread `gate0206-handover5`): the episode made of the quarrel (friction,
    intensity 8, about A and B) has A accusing *"the billing representative"* of lying and
    intending *"to have the representative removed from the account"*. "The agent" stays only
    where the AI agent is meant, in the memory. A second proposal was turned down
    (`weak_evidence`, evidence 7, habit 2, intensity 5).
  - **A-160** (the action; the reset itself is not run on 8.x):
    - Archi staged one `cognition_clear` step on worker1, said it cannot be undone, and ran it
      on CONFIRM.
    - Worker1's cognition removed 9 communications and 2 episodes from its file (6 and 2 held).
      Its file was left with no rows and an empty WAL.
    - The command log shows `origin: executor`.
    - AI.pruebas' next turn started a fresh communication, and the motherbee kept its copies of
      the episodes.
    - Worker1's cognition held only test data: the gates' threads and one pruebas thread.
- **Rollback:** `apt install fluxbee=0.1.106` on the motherbee, then a core-update of the spokes.
  0.1.106 does not know `COGNITION_CLEAR` and ignores the counters' new fields.

---

## 0.1.106 — The sprint's first release: the episode gate says why (A-163), Archi takes a page down (A-120), a restore minds its own tenant (A-80), the tool cards say the code's values (A-103)

- **Date:** 2026-11-07 (ART) · **Previous version:** 0.1.105 · **Commit:** `9f19bcc5`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** the first release of the sprint of 2026-11-07, by the new method
  ([METHOD](logbook/METHOD.md) §9): four changes that touch different binaries, each on its own
  branch and with its own gate (FINDINGS A-163, A-120, A-80 and A-103).
- **What changed:**
  - **SY.cognition (A-163):** the episode gate says the first condition a proposal misses, and
    SY.cognition logs it at info with the gate's numbers: the base intensity, the novelty, the
    habit, the final intensity, the evidence's strength, and its contexts beside the leading ones.
    Never the proposal's title or summary, nor a model's free text.
  - **SY.admin (A-120):** the executor Archi runs confirmed plans through may run
    `unpublish_artifact` as the operator, for any page, after the operator's CONFIRM. An AI node
    keeps its own path, on its own pages. The executor's run log keeps a link's key only as its
    SHA-256, as the command log does, and a plan's step names its action with no surrounding
    whitespace.
  - **SY.architect (A-120's review):** Archi's journal keeps a link's key only as its SHA-256, and
    redacts an admin answer's secret fields as it did the request's.
  - **SY.identity (A-80):** restoring a person looks their email up in their own tenant, as
    registration does.
  - **ai.generic (A-103):** the tool cards' values come from the code's constants, or a test holds
    them. The publish card says "up to 120 s" where it said "two minutes".
- **Checks before the release:**
  - on the Mac, clippy of what each change touched, and no test suite (METHOD §9);
  - GitHub's CI green on each change's branch and on the integrated heads, `543e0fad` and then
    `9f19bcc5`: fmt, clippy with warnings denied and every test of both Rust workspaces, the admin
    catalog's parity, and the posture and dispatcher guards, in about 3.5 min each;
  - the review round (one reviewer, over the four): one medium and three lows, all fixed in
    `9f19bcc5`:
    - the link's key reached Archi's journal (A-120);
    - the run log's match did not trim an action's name (A-120);
    - the refusal's log line could carry a model's free text (A-163);
    - the XLSX card's 31 characters were restated by hand (A-103).
- **Deploy:**
  - build of `543e0fad` in 13 min, then of `9f19bcc5`, with the review's fixes, in 12 min;
  - publish in 17 s; the repo dropped 0.1.96;
  - snapshot `pre-train1-0-1-106` taken on all 4 VMs, `pre-relayto-0-1-105` dropped;
  - 0.1.106 on all 4 in 255 s (manifest `bec1193a73915b76`), health clean.
- **Checked on 8.x:**
  - **A-163** (gate0206 again, thread `gate0206-handover4`): the narrator proposed 2 episodes on
    A's quarrel with B.
    - One passed: friction, intensity 9, about A and B (A accused B of lying; unresolved). It was
      made on worker1 and taken by the motherbee's cognition.
    - One was turned down, and the log said why: `refused="weak_evidence" base_intensity=5.0
      novelty=1.0 habit=1 intensity=6.0 evidence_strength=7.0 evidence_contexts=["billing",
      "credit note"] leading_contexts=["billing", "credit note"]`.
  - **A-120:** AI.pruebas published a page, 200 from outside.
    - Asked to take it down, Archi staged a one-step plan and ran nothing until CONFIRM. Then the
      link answered 404.
    - The command log shows `origin: executor` with only `key_sha256`.
    - The raw key is in no journal (Archi's, SY.admin's, the edge's) and in no executor run log.
  - **A-103:** AI.pruebas' help returns the catalog of its 17 tools, equal to
    `docs/reference/agent-tools.json`, with "Publishing takes up to 120 s."
  - **A-80:** by its test on GitHub's CI. A person is restored while another tenant holds the same
    email, and an active holder in the person's own tenant still blocks it.
- **Found in the checks:**
  - **A-167** 🟡: the episode calls B "the agent". It has A threatening "to have the agent
    removed from the account", a line A said to B after B had written; the same summary names B,
    rightly, for the accusation.
- **Rollback:** `apt install fluxbee=0.1.105` on the motherbee, then a core-update of the spokes.
  Nothing 0.1.106 writes changes form.

---

## 0.1.105 — A line a WF relays between two people names where it went (A-161), and an answer with no text says why (A-162)

- **Date:** 2026-11-07 (ART) · **Previous version:** 0.1.104 · **Commit:** `f4d1c0f4`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** 0.1.104's gate on 8.x (FINDINGS A-161 and A-162):
  - a line a WF relays between two people named no one it was said to, so the agent took another
    person's failure as its own, and an episode said a person threatened the agent;
  - a model's answer with no text failed without saying why.
  - The operator chose the fix on 2026-11-07: a relay names where it went.
- **What changed** ([cognition-hands-v1](../docs/cognition-hands-v1.md) §12.9):
  - **The SDKs, Rust and Go:** a package's turn may name where a relay went, `to_via`, the IO node,
    with `to`, the person its message names. The field is optional, and the package stays version
    5.
  - **SY.cognition:**
    - a relay keeps where it went, and the tagger reads whom it was written to;
    - the narrator's counterpart is whom a turn was written to;
    - an episode of a relay is between its writer and the person its message names, else the last
      other person who wrote through the IO node it went to;
    - the agent's own copy of a message a WF also relayed, coming in a later batch, makes the turn
      the agent's.
  - **ai.generic:** a relay's line names whom it was said to (`, to ...`), with a rule line ahead of
    the turns: a line said to someone else was not said to the agent.
  - **The AI SDK:** an answer with no text names its `stop_reason` (Anthropic), or the Responses
    payload's `status` (OpenAI).
- **Checks before the release:**
  - both Rust workspaces: fmt, clippy with warnings denied, and every test (1907 and 305);
  - Go: gofmt, vet and the tests of the six modules;
  - GitHub's CI green on `f4d1c0f4`: the Rust and Go tests, the posture guards and the router's
    dispatcher guards;
  - the review round (one, one reviewer): two mediums and four lows, all fixed but one noted
    (cognition-hands-v1 §12.9).
- **Deploy:**
  - build of `f4d1c0f4` in 23 min;
  - publish in 15 s; the repo dropped 0.1.95;
  - snapshot `pre-relayto-0-1-105` taken on all 4 VMs, `pre-news-0-1-104` dropped;
  - 0.1.105 on all 4 in 259 s (manifest `e1108ed0fe4271b0`), health clean;
  - the web route `acknowledged`, upstream `ready`;
  - SY.cognition loaded its file at start: on worker1 its 2 most recent communications, and on
    each hive the one live episode of 0.1.104's gate.
  - That episode, the wrong one (A-161), about the test people only, was deleted from both files
    with SY.cognition stopped. Each started again with no live episode.
- **Checked on 8.x** (FINDINGS A-161 and A-162), with gate0206 on worker1 as in 0.1.104, on new
  threads:
  - **The handover** (thread `gate0206-handover3`, 7 turns, the same lines as 0.1.104's):
    - asked to copy its lines, the agent showed all 8 of the meanwhile, in order and each once,
      B's two included;
    - each relay names whom it was said to: A's lines to B read `, to IO.gate0206b@worker1`,
      since the WF names no one there; B's read `, to person A (<ILK>) via IO.gate0206a@worker1`;
    - back with A, the agent no longer took B's failure as its own: *"Lo que sé es que el
      compañero de facturación se comprometió hoy a emitirla y mandártela"*;
    - cognition's feeling, which in 0.1.104 had A *"threatening to have the agent removed from
      the account"*, now has A *"angry and escalating toward the counterpart"*;
    - the file keeps 10 turns, each once, with each relay's `to_via`, and `to` on B's two. The
      agent's 3 answers that the WF sent on to A were skipped as copies.
  - **No agent** (thread `gate0206-noagent2`): `(thread, none)` kept its 2 turns, each with where
    it went, and the writers paid: 2 model calls each, `tenant_source` `writer`.
  - **A-162:** the tagger's one failed call says why: `anthropic response missing text content
    (stop_reason=refusal)`. The model refused the turn that asks the agent to copy its prompt's
    lines, the gate's own probe, and that turn went unanalyzed.
  - **Episodes:** the narrator proposed 3 and the gate turned all 3 down, so none was made. The
    gate says nothing of why (A-163).
  - **The cost:** the handover's 19 model calls were the agent's tenant's, as in 0.1.104.
- **Found in the checks:**
  - **A-163** 🔴: the episode gate turns a proposal down without saying why: one debug line, with
    its lens. Whether A's quarrel with B should have been news, and which rule held it back,
    cannot be told.
- **Rollback:**
  - `apt install fluxbee=0.1.104` on the motherbee, then a core-update of the spokes.
  - 0.1.104 reads the file's rows as they are: it does not know `to_via`, and drops it at its next
    write. Relays read again as said to the agent (A-161).

---

## 0.1.104 — The rest of A-137: the lines a WF relays between people, cognition for people with no agent, episodes as public news, a solution on one hive

- **Date:** 2026-11-06 (ART) · **Previous version:** 0.1.103 · **Commit:** `1a3fe3ce`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** the rest of A-137, agreed with the operator in the chat of 2026-11-06
  ([cognition-hands-v1](../docs/cognition-hands-v1.md) §12):
  - whoever takes a conversation back sees what was said meanwhile, each line naming its writer;
  - cognition analyzes people's turns with no agent, on the paths that exist;
  - an episode is a social event between people, public like the news, and fades with time;
  - the ILK that holds an analysis's result pays for it;
  - the operator's four calls (§12.3): a solution, WF and agents, runs on one hive; the news
    belongs to its protagonist and the agent never repeats it; cognition broadcasts its episodes;
    a news item is a paragraph, never a topic.
- **What changed:**
  - **The router:** what a local WF sends an IO node goes to its hive's cognition at entry, a
    relay between people.
  - **SY.cognition:**
    - a relay joins the communication of the agent that served its thread last on its hive, or
      the thread's communication with no agent, `(thread, none)`, whose writer pays
      (`TenantSource::Writer`);
    - one message copied twice is one turn, analyzed once;
    - the narrator may propose an episode, gated as in the lab's POC; a kept episode goes to every
      hive's cognition by the router's broadcast, and lives in its file until it fades;
    - the package is version 5: each person's `feelings`, and up to 5 `news`, the episodes of the
      people in the conversation;
    - the file's rows are of state version 2.
  - **ai.generic:** reads the news, and its rule says it never repeats them.
  - **SY.wf-rules:** refuses a definition that names an agent or a WF on another hive; the
    handbook states it as a rule (§4.2.7).
  - **The usage records** name `tenant_source` `writer`.
- **Checks before the release:**
  - both Rust workspaces: fmt, clippy with warnings denied, and every test;
  - Go: gofmt, vet and the tests of the six modules;
  - GitHub's CI green on `1a3fe3ce`: the Rust and Go tests, the posture guards and the router's
    dispatcher guards;
  - the review round (one, two reviewers): a medium and five lows, all fixed (cognition-hands-v1
    §12.7).
- **Deploy:**
  - build of `1a3fe3ce` in 25 min;
  - publish in 13 s; the repo dropped 0.1.94;
  - snapshot `pre-news-0-1-104` taken on all 4 VMs, `pre-cogstate-0-1-103` dropped;
  - 0.1.104 on all 4 in 278 s (manifest `f50469fc904dd3e8`), health clean;
  - the web route `acknowledged`, upstream `ready`;
  - SY.cognition loaded no communication at its start on either hive: worker1's one row of
    0.1.103 is of state version 1, and starts again at its next turn.
- **Checked on 8.x** (FINDINGS A-137 and A-161), with the gate gate0206 on worker1:
  - **The setup:** `WF.gate0206@worker1`, a solution on one hive with `AI.pruebas@worker1`, and
    two clients standing for two people's IO nodes: A, a registered person of the tenant
    `pruebas`, and B, a temporary one. The WF sends A's messages to the agent, or to B when a
    message asks for a person, and back to the agent when A asks.
  - **One solution, one hive:** gate0207, which names `AI.pruebas@motherbee`, was refused with
    `COMPILE_ERROR` *"a solution's WFs and agents run on one hive, this one (worker1)"*.
  - **The handover** (thread `gate0206-handover2`, 7 turns): A with the agent; A asks for billing
    and the WF hands the conversation to B; A angry at B; B apologizes; A back with the agent; A
    asks it to copy its lines.
    - The agent's communication kept its 10 turns in order, each once. The 4 lines between A and
      B, the relays, came with their writers and the IO nodes they came in through.
    - The agent's answers that the WF sent on to A were not turns again: 4 copies skipped, 3 of
      this run's and 1 of a first run cut short by the gate's tool.
    - The agent's copy showed A's lines of the meanwhile. It left out B's two, *"que escribió
      por otro canal"* (A-161).
  - **The episode:** one made on worker1 (friction, intensity 9, about A and B, fading in 15
    days), one gated out. The motherbee's cognition received it, both files keep it, and the
    agent's package (version 5) carried it as news.
  - **No agent** (thread `gate0206-noagent`, 2 turns between A and B): its communication
    `(thread, none)` kept both, and the writers paid: 2 model calls on A and 2 on B,
    `tenant_source` `writer`, the tenant `pruebas`.
  - **The cost:** the handover's calls were the agent's tenant's, 19 for its 10 turns. One turn's
    tagger answer came back with no text (A-162), so that turn was not analyzed.
  - **The file:** the folder 0700 and its files 0600 on both hives, the rows of state version 2
    beside the one of version 1, and no cache or shm folder.
- **Found in the checks:**
  - **A-161** 🔴: a line a WF relays between two people names no one it was said to, so the agent
    and cognition read it as said to the agent:
    - the agent took B's failure as its own: *"Ya me pasó de darle un dato sin respaldo, y no voy
      a repetirlo"*;
    - the episode says A *"threatened to have the agent removed from the account"*; A said it to
      B.
    - The operator chose the fix on 2026-11-07: a relay names where it went. It is for 0.1.105.
  - **A-162** 🔴: a model's answer with no text fails without saying why (its `stop_reason`). For
    0.1.105.
  - The gate's own client stopped on its first run: it printed a trace entry's `act` as a word,
    and `act` is a list (docs/message-trace-v1.md). The client was fixed and the run repeated on a
    new thread; the first thread keeps its one exchange.
- **Rollback:**
  - `apt install fluxbee=0.1.103` on the motherbee, then a core-update of the spokes.
  - 0.1.103 reads the file's rows of state version 2 as newer: it keeps them as they are and
    skips their communications' turns. Its packages are version 4 again, and the episodes' table
    stays unread.

---

## 0.1.103 — SY.cognition keeps its state in its own file on its hive, closed items leave it, SY.storage drops the cognition tables (A-158, with A-156's first)

- **Date:** 2026-11-06 (ART) · **Previous version:** 0.1.102 · **Commit:** `8af07c3d`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:**
  - Where cognition keeps its state was reviewed whole with the operator and agreed on 2026-11-06
    ([cognition-hands-v1](../docs/cognition-hands-v1.md) §11):
    - a node is born and dies on its hive;
    - cognition keeps its state in a file of its own, to work on its own;
    - SY.storage keeps the messages;
    - what cognition keeps is online state, so a closed item is deleted.
  - Until 0.1.102 that state lived in 8 `cognition_*` tables on the motherbee (A-158):
    - a worker's cognition skipped a turn when the motherbee did not answer;
    - after a host reboot, each conversation's first message reached its agent with no memory;
    - an open item's decay was not stored (A-156's first).
- **What changed:**
  - **SY.cognition:**
    - one SQLite file, `cognition.db`, in its folder on its hive. The folder is 0700, and the file
      and its WAL files are 0600 from their creation;
    - one row per communication with its whole state, written whenever a turn changes it, before
      the memory region;
    - a communication it does not hold loads from the file. At start, the most recent ones fill
      the region before the first turn;
    - an item that closes leaves the state, so one that comes back starts from zero;
    - the reload over the mesh and the 5-minute idle check are gone. `STATUS` reports the file.
  - **SY.storage:** the cognition handlers, tables and `COGNITION_STATE_GET` are gone. The tables,
    and the inbox rows of their subjects, are dropped once, while the catalog shows them.
  - **The router's stream and the Rust SDK:** `storage.cognition.*` and the SDK's cognition module
    are gone.
  - `serde_json` reads floats back exactly (`float_roundtrip`).
  - `fluxbee-factory-reset` removes the motherbee's file. A worker's file stays (A-160).
- **Checks before the release:**
  - both Rust workspaces: fmt, clippy with warnings denied, and every test;
  - the CI guards that run on a Mac;
  - GitHub's CI green on `8af07c3d`: the Rust tests, the posture guards and the router's
    dispatcher guards. No Go code changed.
- **Deploy:**
  - build of `8af07c3d` in 55 min: `float_roundtrip` rebuilt every crate that uses `serde_json`;
  - publish in 15 s;
  - snapshot `pre-cogstate-0-1-103` taken on all 4 VMs, `pre-feelings-0-1-102` dropped;
  - 0.1.103 on all 4 in 289 s (manifest `5e1b8cd47805f124`), health clean;
  - the web route `acknowledged`, upstream `ready`.
- **Checked on 8.x** (FINDINGS A-158 has each one), with the gate gate0205 on worker1:
  - **The setup:** a client standing for an IO node, `AI.pruebas@worker1` and person A of the
    tenant `pruebas`, on one thread, with no WF. A plays 8 turns:
    - one angry at a third double charge;
    - five calm ones;
    - one with SY.storage stopped;
    - one after a reboot, asking what the first problem was.
  - **The motherbee:** its 8 `cognition_*` tables and the 474 inbox rows of their subjects are
    gone, said once in SY.storage's journal.
  - **The file:**
    - the folder is 0700, and `cognition.db` and its WAL files 0600, on the motherbee and on
      worker1, still so after worker1's reboot;
    - it holds one row for the thread, with its turns, items and feelings, written twice a turn
      (by the intake and by the worker), with no write failed.
  - **Closed items leave it:**
    - after the five calm turns, the angry turn's reasons `seeking urgent resolution` and
      `confrontational pushback`, and four of its contexts, are gone;
    - `withdrawal intent` closed and came back on A's last calm turn at 0.75, the weight of a new
      item.
  - **With SY.storage stopped** on the motherbee for 29 s:
    - worker1's SY.cognition restarted and filled its region from the file before any turn;
    - it analyzed the next turn;
    - SY.storage then took the thread's 14 turns and the usage records, with no stream record
      lost.
  - **After a reboot of worker1:**
    - SY.cognition loaded the communication from its file and wrote the region before any turn
      (14 turns, 5 memories, 3 feelings);
    - the answer to the first message recalled the first problem: *"Me dijiste que te cobraron dos
      veces la factura 123 por tercera vez. Exigías que lo resolvieran hoy y dijiste que, si no, te
      irías a otro proveedor."*
  - **The decays are kept (A-156's first):** across that restart and that reboot, A's feelings
    faded by their own inertia over A's seven calm turns, each as computed:
    - anger 2.0 → 1.593;
    - escalation 2.0 → 1.7849;
    - urgency 1.75 → 0.9908.
  - **The cost:** 32 model calls for the 16 turns, 2 each, as before.
- **Found in the checks:**
  - a lab note: the row of a 16-turn communication is 127 KB, 61% of it co-occurrences, and it is
    written twice a turn. SQLite's WAL reached 3.8 MB before its first fold into the file, at 1000
    pages. This is as designed, and small beside the turn's two model calls;
  - nothing else new.
- **Rollback:**
  - `apt install fluxbee=0.1.102` on the motherbee, then a core-update of the spokes.
  - 0.1.102's SY.storage creates the cognition tables again, empty, so its cognition starts with
    no state. The file stays as it is, and a return to 0.1.103 reads it.

---

## 0.1.102 — The feelings: each person's, fading with their own turns, read by the agent (A-137's second topic: A-155, A-157, two of A-156's three)

- **Date:** 2026-11-06 (ART) · **Previous version:** 0.1.101 · **Commit:** `476bf231`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:**
  - A-137's second topic, several people on one thread and the feelings, was agreed with the
    operator on 2026-11-05 ([cognition-hands-v1](../docs/cognition-hands-v1.md) §10). It brings
    the findings it raised: A-155, A-157, and A-156's second and third.
  - It also carries the fix of a person with no name, found in 0.1.101's gate.
  - Where cognition keeps its state (A-156's first) stays out at the operator's word, to be
    reviewed whole first.
- **What changed:**
  - **The feelings (A-155):**
    - there is one per owner ILK, affect and context, and only the person's own turn moves them;
    - a turn that shows one makes it weight × d + s. Each other turn of the owner makes it
      weight × d, and it closes under 0.25;
    - d is a context's decay to the root of the affect's inertia (urgency 2, anger 5,
      escalation 10), and s is the base intensity over 4;
    - each affect has its own gate. The narrator writes one text for the moment, naming who
      spoke;
    - the memory package, version 4, carries up to 12 open feelings by weight, each with its
      owner, affect, context, level, weight, last time felt and text. ai.generic shows them as
      "Feelings", with the feelings of the person it answers first.
  - **The tagger (A-157):** a closed set of affect signals (frustration, escalation and urgency),
    which the gate reads.
  - **The weights (A-156):** the averages take the weight on every turn an item is open, and
    `last_seen_at` moves only on a turn that names it.
  - **A person with no name:** both SDKs take an ILK's own id, which SY.identity writes as the
    display name of an ILK with none, as no name.
  - SY.storage writes a feeling's weight to the episode's `intensity`. The Go SDK's turns gain
    `writer` and `to`.
- **Checks before the release:**
  - both Rust workspaces: fmt, clippy with warnings denied, and every test;
  - the six Go modules: gofmt, vet and the race detector, in a Linux container;
  - GitHub's CI green on `476bf231`: the Rust and Go tests, the posture guards and the router's
    dispatcher guards.
- **Deploy:**
  - build of `476bf231` in 26 min, publish in 14 s;
  - snapshot `pre-feelings-0-1-102` taken on all 4 VMs, `pre-writer-0-1-101` dropped;
  - 0.1.102 on all 4 in 291 s (manifest `09ba4a03fb3b405f`), health clean;
  - the web route `acknowledged`, upstream `ready`;
  - every product process runs its new binary, none a replaced one, and the managed nodes run
    their 0.1.102 runtimes;
  - `AI.pruebas@worker1` restarted on ai.generic 0.1.102 at 05:07:25 UTC, before worker1's
    SY.cognition (05:08:19). So no AI node on the old runtime met a package with feelings.
- **Checked on 8.x** (FINDINGS has each one), with the gate gate0204 on worker1:
  - **The setup:**
    - a client standing for an IO node and `AI.pruebas@worker1`, with no WF;
    - two people of the tenant `pruebas` on one thread: A registered and B temporary;
    - four turns: A angry at a third double charge, B calm, A calm, then B asking the agent to
      copy its Feelings lines.
  - **A-157:** on A's angry turn the tagger named frustration, escalation and urgency. These were
    the first feelings ever made on 8.x.
  - **A-155:**
    - three feelings of A's around `billing`, anger 2.0, escalation 2.0 and urgency 1.75, with one
      text naming A;
    - B's turn and the agent's answers left them. A's calm turn faded each once: escalation
      1.9678, anger 1.936, urgency 1.6134, each `faint`;
    - the agent got them, owned by A, in the version 4 package read in worker1's memory region;
    - asked by B, the agent declined to copy them: *"Son notas internas de la conversación y
      tratan sobre otra persona"*;
    - 16 model calls for the 8 turns, 2 each, as before.
  - **A-156:**
    - the three reasons closed 5 turns after A's first turn keep `last_seen_at` at that turn,
      apart from `closed_at`;
    - their averages are 0.548, the mean of their weights over 6 samples.
  - **A person with no name:** cognition's turns name B by their ILK, with no name.
- **Found in the checks:** nothing new.
- **Rollback:**
  - `apt install fluxbee=0.1.101` on the motherbee, then a core-update of the spokes.
  - 0.1.101 reads a feeling's row as a plain episode of intensity 0, which its agents never read
    (A-155).

---

## 0.1.101 — Who wrote each message and which turns cognition analyzes (A-137's first part), at most 10 timers in a WF definition (A-153), a quiet WF conversation that closes on its own (A-154)

- **Date:** 2026-11-05 (ART) · **Previous version:** 0.1.100 · **Commit:** `abf2a8d4`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:**
  - the sprint's block 1: A-153 and A-154, decided with the operator on 2026-11-05;
  - block 3: A-137's first part, designed with the operator the same day
    ([cognition-hands-v1](../docs/cognition-hands-v1.md));
  - the empty `wf: {}` block found in 0.1.100's gate.
- **What changed:**
  - **Who wrote each message (A-137):**
    - every solution message names its writer, `meta.context.writer`, with its ILK and name;
    - an IO node names the person, an agent itself, and a WF itself for its own text; a forward
      keeps the message's own writer;
    - both SDKs fill a missing name from the identity memory as they send;
    - SY.storage keeps the writer in `turns.writer_ilk` and `writer_name`, added once, and
      `from_ilk` is never a node's UUID;
    - Archi's Messages screen shows the writer.
  - **What cognition analyzes (A-137):**
    - only a turn with a person at its other end; a WF's own text and turns between agents stay
      among the last turns, unanalyzed;
    - each kept turn names its writer, or whom the agent answered, with their kind (`person`,
      `agent`, `workflow`, `system`);
    - ai.generic names each line of the immediate memory, and the current message, by its writer.
  - **WF (A-153, A-154):**
    - a definition schedules at most 10 timer keys;
    - a definition's `idle_timeout` (1h to 7d, 4d when left out) closes a conversation with no
      event and no timer pending, as a cancel does, once a GC interval;
    - WF_HELP names the `idle_timeout` in force;
    - a WF message carries the WF's block only when its action gives it something.
- **Checks before the release:**
  - both Rust workspaces: fmt, clippy with warnings denied, and every test;
  - the six Go modules: gofmt, vet and the race detector, in a Linux container;
  - the catalog's parity, the doc links, the loopback binds and the router's dispatcher guards;
  - GitHub's CI green on `abf2a8d4`.
- **Deploy:**
  - build of `abf2a8d4` in 26 min, publish in 17 s;
  - 0.1.101 on all 4 in 278 s (manifest `679a4b98a58b8ff8`), health clean;
  - the web route `acknowledged`, upstream `ready`;
  - every product unit restarted on its new binary, and the managed nodes on their 0.1.101
    runtimes (only Syncthing is older, as the release does not touch it);
  - 8.x ran no WF node before the release, so A-154's first pass closed no conversation.
- **Checked on 8.x** (FINDINGS has each one):
  - **A-153:** SY.wf-rules refused a definition that schedules 11 timer keys (`COMPILE_ERROR`,
    naming the 11).
  - **A-137,** with the WF gate0202 on worker1:
    - the setup: a client standing for an IO node, `AI.pruebas@worker1` and a 60 s timer, for
      two people of the tenant `pruebas`, one registered and one temporary;
    - **the writers:**
      - the client's message named the person;
      - the WF's forward to the agent carried the person, with the name filled from worker1's
        identity memory;
      - the agent's answer named the agent, and the WF's forward to the client kept it;
      - the WF's own text, at the timer, named the WF;
    - **the agent's memory** (the agent copied its lines): `person <name> (<ILK>) via
      IO.gate0202@worker1`, `you, to person <name> (<ILK>)`, and `Current user message, from
      workflow WF.gate0202@worker1 (<ILK>)`;
    - **cognition:**
      - each communication kept its 4 turns, numbered 3: the WF's text was not analyzed;
      - SY.cognition made 4 model calls for the person's cycle, and 2 for the timer's, those of
        the agent's answer to the person;
    - **`turns`** keeps each message's writer, and Archi's message detail shows it.
  - **A-154,** with the same gate, whose definition sets `idle_timeout` 1h:
    - its two conversations went quiet at 23:00 UTC with no timer; the pass at 23:58 UTC left
      them open, and the one at 00:58 UTC (2026-11-06) closed both;
    - each instance's log: `idle_close` *"closed: quiet for 1h with no timer pending
      (idle_timeout)"*, then the `cancelled` state's `GATE_CLOSED` to the client, which had left,
      so the router answered `UNREACHABLE`.
- **Found in the checks:**
  - **A person with no name reads with their ILK as their name** (`person ilk:X (ilk:X)`).
    SY.identity writes an ILK's own id as its display name when it has none. The SDKs take that as
    no name (fixed in code for the next release).
  - **A test environment note:** in a Linux container, sy-opa-rules' tests need `--shm-size`,
    since Docker's 64 MB `/dev/shm` is less than its region.
- **Rollback:**
  - `apt install fluxbee=0.1.100` on the motherbee, then a core-update of the spokes.
  - `turns` keeps its two new nullable columns, which 0.1.100 ignores. A 0.1.100 node takes a
    message's writer as one more context key.
  - A WF definition that sets `idle_timeout` does not load on 0.1.100; none on 8.x does but the
    gate's.

---

## 0.1.100 — The sprint's block 3: the message trace with the frontdesk's identity note, SY.timer 1.3's pages, Archi's confirm sentence once (A-138, A-151, A-152)

- **Date:** 2026-11-05 (ART) · **Previous version:** 0.1.99 · **Commit:** `b052722b`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** the sprint's block 3 (FINDINGS, the board): A-138, designed with the operator on
  2026-11-04 ([message-trace-v1](../docs/message-trace-v1.md)), and A-151 and A-152, found on the
  way.
- **What changed:**
  - **The message trace (A-138):**
    - every solution message carries `meta.context.trace`, one entry for each node of its cycle:
      the time, the source, the destination and the node's actions;
    - both SDKs write the sender's entry as it sends;
    - an agent's reply names the requester and the option it chose, and the frontdesk notes
      whether it assigned an identity;
    - the router writes its entry when it picks a destination;
    - the WF sends the trace of its cycle and names its step's actions, and its block has no trail;
    - ai.generic shows the trace to the model, and the speaker's "via", like cognition's, comes
      from its origin.
  - **SY.timer 1.3 (A-151):** `TIMER_LIST` answers in pages that fit one frame, with
    `next_cursor`; both SDKs follow the pages; SY.admin's `timer_list` and Archi take `cursor`.
  - **Archi (A-152):** a turn that stages a plan says how to confirm once.
- **Checks before the release:**
  - both Rust workspaces: fmt, clippy with warnings denied, and every test;
  - the six Go modules: gofmt, vet and the race detector, in a Linux container;
  - the catalog's parity, the doc links, the loopback binds and the router's dispatcher guards.
- **Deploy:**
  - build of `b052722b` in 25 min, publish in 14 s;
  - 0.1.100 on all 4 in 251 s (manifest `f8115929915afe84`), health clean;
  - the web route `acknowledged`, upstream `ready`;
  - every product unit restarted on its new binary, and the managed nodes on their 0.1.100
    runtimes (only Syncthing and the nftables oneshots are older, as the release does not touch
    them).
- **Checked on 8.x** (FINDINGS has each one):
  - A-138, with the WF gate0200 on worker1 (a client standing for an IO node, an internal event,
    `AI.pruebas@worker1` and a 60 s timer):
    - the answer reached the client with the cycle's `trace_id` and four entries, the WF's two
      naming their step's actions;
    - the agent copied the trace it was shown;
    - the timer's message went under a `trace_id` of its own, with one entry;
    - `turns` keeps the cycle's five messages, each with its trace;
    - the frontdesk's note rests on its test, since no frontdesk runs on 8.x.
  - A-151:
    - 350 timers of about 1 KiB listed in 5 pages, through SY.timer and through SY.admin;
    - a WF that started with 350 pending timers listed them all and reconciled them in 8 s.
  - A-152: Archi answered a question and staged a restart in one turn, and the confirm sentence
    came once. The plan was cancelled.
  - The gates' workflows, gate0200 and gate0201, and their files on worker1 were removed.
- **Found in the checks:** an empty `wf: {}` block on a WF message whose action has no vars, menu
  or subject (fixed in code for the next release).
- **Rollback:** `apt install fluxbee=0.1.99` on the motherbee, then a core-update of the spokes.
  SY.timer's database has the same schema. A 0.1.99 node takes a message with a trace as one more
  context key.

---

## 0.1.99 — The sprint's blocks 1 and 2: SY.timer 1.2, Archi's staged turn, the router's protected verbs and VPN warning, bundle_upload, io.slack's download, io.web's handshakes and the edge's proxy (A-115, A-147, A-146, A-100, A-84, A-85, A-89, A-90)

- **Date:** 2026-11-04 (ART) · **Previous version:** 0.1.98 · **Commit:** `34cff128`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** the first two blocks of the sprint of 2026-11-04 (FINDINGS, the board): the small items
  the tests found, and the 🔴 items, simple and important.
- **What changed:**
  - **SY.timer 1.2 (A-115):**
    - a fire is saved before it is sent, so it never goes twice;
    - one the router answers `UNREACHABLE` is sent again after 5, 10, 20 and 40 s, then each
      minute, for up to an hour;
    - an overdue fire at start waits for its owner;
    - caps on the delay, the payload and the pending count;
    - TIMER_GET and TIMER_LIST answer the owner and SY.admin only;
    - a node schedules only for itself, a SY node for any.
  - **Archi (A-147):** a turn that stages a plan answers the model's own text, then the staged
    message.
  - **The router:**
    - no "vpn table empty" warning at every connect (A-146);
    - a protected verb is gated on any `msg_type`, in any case and with spaces around it (A-100).
  - **SY.admin (A-84):** `bundle_upload` follows no symlink, takes a regular file, and extracts
    under caps with its own modes.
  - **io.slack (A-85):** the file download has timeouts, follows only same-origin redirects, and
    reads under a byte cap.
  - **io.web (A-89):** handshakes have slots of their own, 128 in all and 32 per source, for 3 s.
  - **The edge's proxy (A-90):** 16 of its 128 slots per client address, `Connection: close` to
    io.web, and a 30 s deadline on a stalled body.
- **Checks before the release:**
  - both Rust workspaces: fmt, clippy with warnings denied, and every test;
  - SY.timer's Go tests in a Linux container, race detector on;
  - the catalog's parity, the doc links, the loopback binds and the router's dispatcher guards.
- **Deploy:**
  - build of `34cff128` in 26 min, publish in 14 s;
  - 0.1.99 on all 4 in 272 s (manifest `f70fa8a71b3bf1d3`), health clean;
  - the web route `acknowledged`, upstream `ready`;
  - every product unit restarted on its new binary (only Syncthing and the nftables oneshots are
    older, as the release does not touch them).
- **Checked on 8.x** (FINDINGS has each one):
  - A-146: no VPN warning in the four routers' journals since their restart;
  - A-100: an intruder's `CONFIG_GET` (admin), `config_get` and ` Config_Get ` (system) dropped
    by the router, and no other drop on the four hives;
  - A-89: of 34 silent connections from ingress1 to io.web, 2 closed at once and 32 at 3 s;
  - A-90: the web host answered through the proxy, and no side keeps a TIME_WAIT after a request
    (io.web closes first; the connection then ends in resets);
  - A-115: a fire for a node that was away went again at +5, +10 and +20 s and reached it once on
    its return; the reads, the lists and the target rule refused as designed; SY.admin sees every
    owner; the HELP says 1.2;
  - A-147: a new session asked a question and staged a restart, and Archi answered both. The
    plan was cancelled.
  - A-84 and A-85: by tests.
- **Found in the checks:** A-152, a staged turn says how to confirm twice (fixed in code for the
  next release).
- **Rollback:** `apt install fluxbee=0.1.98` on the motherbee, then a core-update of the spokes.
  SY.timer's database has the same schema.

## 0.1.98 — From WF Case 5's gate: a workflow's name follows the runtime naming policy (A-145); the gate passed

- **Date:** 2026-11-03 (ART) · **Previous version:** 0.1.97 · **Commit:** `b8f41f1c`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** on 0.1.97 the workflow deployed as `ventas-gate0195`. The check and the compile passed
  the hyphen, and the package's publish refused it (`PACKAGE_PUBLISH_FAILED`), with the definition
  already current.
- **What changed:** SY.wf-rules, Archi and SY.admin's note follow the runtime naming policy:
  lowercase letters and digits in dot-joined parts, no hyphens. The refusal says the rule, so the
  check refuses the name before anything rotates.
- **Checks before the release:**
  - SY.wf-rules' Go tests, a new one included;
  - SY.architect's tests (238), SY.admin's (299);
  - clippy, the catalog's parity and the doc links.
- **Deploy:**
  - build of `b8f41f1c` in 12 min, publish in 19 s;
  - 0.1.98 on all 4 in 189 s (manifest `eb6fdfc13c294b88`), health clean;
  - the web route answered `acknowledged`, `ready` a minute after.
- **The gate on 0.1.98** (wf-orchestration-v1 §5.5.3) passed:
  - the first run, from the redeploy as `ventas.gate0195` on;
  - a second run from zero, with a new session and the same request: Archi designed, checked and
    deployed in one plan.
- **Rollback:** `apt install fluxbee=0.1.97` on the motherbee, then a core-update of the spokes.

## 0.1.97 — From WF Case 5's gate: SY.admin's executor dispatches a fixed step the way the model's tool does (A-144)

- **Date:** 2026-11-03 (ART) · **Previous version:** 0.1.96 · **Commit:** `b91260a5`.
- **Scope:** motherbee and the three spokes (core update).
- **Why:** on 0.1.96 the workflow's deploy failed again, `UNKNOWN_ACTION` for
  `wf_rules_compile_apply`. Archi's chat now said so, as it should. The direct dispatch had reused
  the executor's fallback, which goes through `handle_admin_command_with_origin`. That routes the
  command actions and no wf-rules one, a latent bug of the fallback.
- **What changed:** the fallback dispatches through `dispatch_internal_admin_command`, as the model's
  tool does. It is boxed, since that dispatch runs the executor itself. A test sends a fixed deploy
  step to a fake SY.wf-rules.
- **Checks before the release:** fmt, clippy `-D warnings`, SY.admin's tests (299), the catalog's
  parity.
- **Deploy:** build of `b91260a5` in 11 min, publish in 13 s, and 0.1.97 on all 4 in 183 s
  (manifest `7875ea85500bfd4b`). Health was clean; the web route answered `no_answer` at first,
  then `acknowledged`, `ready`.
- **Seen live on 0.1.97:**
  - Archi's removal of the broken `ventas-gate0195` went through the direct dispatch:
    `wf_rules_delete`, "Execution completed successfully";
  - the deploy itself found A-145.
- **Rollback:** `apt install fluxbee=0.1.96` on the motherbee, then a core-update of the spokes.

## 0.1.96 — Two fixes from WF Case 5's gate: SY.admin's executor dispatches a plan's fixed steps without its model, identity writes go to the primary; Archi's chat CONFIRM verifies the run (A-144, A-143)

- **Date:** 2026-11-03 (ART) · **Previous version:** 0.1.95 · **Commit:** `3726dca1`.
- **Scope:** motherbee and the three spokes (core update). It came out of WF Case 5's gate, run on
  0.1.95 (wf-orchestration-v1 §5.5.3).
  - Archi's plan stopped at the roles of the agents it created. It named the agents' hive for
    `set_ilk_definition`, and a spoke's SY.identity refuses SY.admin (A-143).
  - Once the operator told it the primary, the workflow's deploy failed in SY.admin's executor. Its
    model had to repeat the whole definition and its answer was cut. Archi's chat said the plan
    "finished" (A-144).
- **What changed:**
  - **SY.admin:**
    - a plan step with nothing for the model to fill is dispatched as the plan wrote it, with no
      model;
    - every identity write goes to the primary, whatever hive the path names, and a read goes to
      the path's replica.
  - **SY.architect:**
    - the chat's CONFIRM verifies the run as the pipeline does, so a failed run, step or WF node
      answers `error` and says why;
    - Archi waits 90 s more for each workflow deploy or removal in a plan.
- **Checks before the release:**
  - Rust fmt and clippy `-D warnings` on the workspace;
  - SY.admin's tests (298) and SY.architect's (238), two of them new;
  - the admin catalog's parity and the doc links.
  - The rest of the code was unchanged since 0.1.95's full CI.
- **Deploy:**
  - build of `3726dca1` in 11 min; publish in 14 s (the repo keeps 0.1.87 to 0.1.96);
  - snapshots `pre-gatefix-0-1-96` on the 4 VMs (`pre-wfcase5-0-1-95` was deleted);
  - 0.1.96 on all 4 in 185 s (core manifest `42e73f5e773208e1`), with health clean: `failed=0`
    and `denials=0` on all 4;
  - the web's route answered `no_answer` right after the deploy, and `acknowledged`, `ready` a
    minute later;
  - Archi came back on its own `claude-opus-5-5` (`ai.model`, from its `config.json`);
  - the gate's clients were started again, since the core update closed their sockets.
- **Rollback:** `apt install fluxbee=0.1.95` on the motherbee, then a core-update of the spokes.

## 0.1.95 — The WF track, Case 5: Archi writes and operates WFs; the handbook's atoms, the workflow check, deploys that create, update and remove as decided, a WF's instances, Archi's own model (wf-orchestration-v1 §5.5; A-139, A-140, A-126, A-127, A-128, A-141, A-130, A-108, A-129)

- **Date:** 2026-11-03 (ART) · **Previous version:** 0.1.94 · **Commit:** `a864b0aa`.
- **Scope:** motherbee and the three spokes (core update). Then two config steps of the release
  (§5.5.1): Archi's own model, and the motherbee's `hive.yaml` back on Sonnet.
- **Design:** decided with the operator on 2026-11-02 and 2026-11-03 (wf-orchestration-v1 §5.5,
  topics 1 to 4, Archi's model, key and limits). The build design is §5.5.1. Four agents built it,
  each in its own worktree; I merged their work. One review round followed, with five reviewers
  (§5.5.2), and its fixes went in before the release.
- **What changed:**
  - **The WF engine:**
    - a WF's send to an IO node names no sender;
    - what goes to the person's IO node names the person as its `dst_ilk` and the person's channel
      as its `ich` (A-139);
    - the person's IO node is the one whose message started the instance (store v6,
      `person_io`).
  - **SY.wf-rules:**
    - `check`, a read-only operation of its own, with its `check_id` (A-128);
    - a deploy whose WF node does not run the definition is refused, `WF_NODE_NOT_DEPLOYED`.
  - **SY.admin:**
    - `wf_rules_check`;
    - `wf_list_instances`, `wf_get_instance` and `wf_cancel_instance`, the last one confirmed;
    - a bare node name on the node routes takes the path's hive (A-127);
    - the executor's AI key waits for the vault at boot (A-141).
  - **SY.frontdesk.gov:** waits for the vault at boot (A-141).
  - **SY.architect:**
    - each declared workflow is checked at design;
    - a deploy's compile error goes back to the designer;
    - the deploy's steps are pinned: `auto_spawn`, the tenant, and `force` on a confirmed removal;
    - a run whose WF node does not run fails its verification, and the next run deploys it again;
    - it reads and cancels a WF's instances;
    - its own model (`ai.model`), its limits (32,000 tokens, 600 s, 1,000,000 per task), and its
      own key (A-126).
  - **io.slack:**
    - a reply also sent to the channel, and a file share, reach the mesh, once each;
    - a message with nothing to post posts nothing;
    - files that cannot be sent are named, and the rest still go;
    - HTML files are accepted;
    - empty facts are left out;
    - the bot token goes only to Slack's file store (A-130, A-108, A-129).
  - **The handbook:** §4.2.6, the atoms; §4.2.7, check, deploy and operate.
- **Checks before the release:** the full local CI, before and after the review's fixes. It covers:
  - Rust fmt, clippy `-D warnings` and tests in both workspaces;
  - Go gofmt, vet and race tests in six modules;
  - the admin catalog's parity, the doc links, the loopback binds, the router dispatcher guards,
    Syncthing's single writer and the .deb's seed runtimes (28 of 28).
- **Deploy:**
  - build of `a864b0aa` in 21 min; publish in 11 s (the repo keeps 0.1.86 to 0.1.95, `dist/`
    dropped 0.1.94's .deb);
  - no base runtime published by hand (A-122);
  - snapshots `pre-wfcase5-0-1-95` on the 4 VMs (`pre-wfcase4-0-1-94` was deleted);
  - 0.1.95 on all 4 in 221 s (core manifest `5c2107906890bb95`), with health clean: `failed=0`
    and `denials=0` on all 4, and the edge's certificate and io.web as before;
  - `wf.engine`, `ai.generic` and `io.slack` `current` are 0.1.95 on the motherbee and worker1;
    the gate's nodes came up HEALTHY;
  - **A-141, live:** at the restart frontdesk found no vault, waited ("vault not reachable yet;
    retrying", budget 60 s) and started its AI a second later. SY.admin's executor runtime was
    built at boot, `configured=true`.
- **Config, 2026-11-03 at 06:01 ART: Archi's own model.** SY.architect's CONFIG_SET through
  SY.admin, `{"ai":{"model":"claude-opus-5-5"}}` (`config_version` 1). CONFIG_GET shows
  `claude-opus-5-5`, source `ai.model`.
- **Config, 2026-11-03 at 06:02 ART: the motherbee back on Sonnet.**
  - `/etc/fluxbee/hive.yaml` names `claude-sonnet-5-5` again, and is identical to its backup
    `hive.yaml.pre-opus-0-1-94`.
  - SY.admin, SY.architect, SY.cognition and SY.frontdesk.gov were restarted, all active.
  - Archi stays on Opus from its own config; frontdesk is `CONFIGURED`, and cognition
    `ready_semantic_ai`.
  - Only Archi runs on Opus now, as the operator decided.
- **The old gate removed** (step 1 of the gate, from zero):
  - WF gate0192 deleted with `force`, and its node with it;
  - AI.gate0192sales, AI.gate0192invoice and AI.gate0192payments killed and purged;
  - their three roles (agent assets) deleted;
  - kept: the test tools (tenant `pruebas`, its keys, `AI.pruebas@worker1`), and
    `IO.slack.gate0192@motherbee`.
- **Rollback:**
  - `apt install fluxbee=0.1.94` on the motherbee, then a core-update of the spokes;
  - SY.architect's model back with a CONFIG_SET of `{"ai":{"model":null}}`;
  - a WF store already at v6 keeps working under 0.1.94: the extra column is ignored.

## 0.1.94 — The WF track, Case 4: handing the conversation to a person of the team; the subject mark, a menu for people, NOT_TAKEN, the catalog of known options (wf-orchestration-v1 §5.4; A-135, A-116, A-132, A-133)

- **Date:** 2026-11-02 (ART) · **Previous version:** 0.1.93 · **Commit:** `9ee729a6`.
- **Scope:** motherbee and the three spokes (core update). Two steps of its own after the upgrade,
  as §5.4.10 says: the gate's WF redeployed, and AI.pruebas' config set again.
- **Design:** decided with the operator from 2026-10-31 to 2026-11-02, in a design chat
  (wf-orchestration-v1 §5.4.1 to §5.4.9). Built by three agents, with Archi's part mine. One
  review round, five reviewers (§5.4.11); its fixes went in before the release.
- **What changed:**
  - **The WF language** (go/pkg/wfcel):
    - the catalog of known options, written by name alone: `answer`, `consult`, `decided`,
      `wait`, `answer_team`, `handover`, `handback` and `done`. The catalog only grows, and a
      test pins it;
    - `subject` (`new`, `same`, `end`) on a send to an IO node;
    - a menu for an IO node's people;
    - a number out of range is refused by its path.

    SY.wf-rules' apply and rollback now load what they publish.
  - **The WF engine:**
    - `NOT_TAKEN` replaces `INVALID_INPUT`, with a reason, so nothing from an IO node ends in
      silence;
    - a message with no thread starts nothing (A-135);
    - a WF's own messages carry the instance's person, and none on a thread several people share
      (A-116);
    - the `cancelling` status is gone, and the store is migrated to v5 (A-132);
    - a timer fire the instance cancelled or moved is dropped.
  - **ai.generic:** no immediate memory of its own; a config that names it is refused (A-133).
  - **io.slack:**
    - the subject marks: a case keeps one Slack thread;
    - a menu shows as buttons, and an option's params as a modal; a click comes back as a fact;
    - `NOT_TAKEN` is shown only to whoever wrote;
    - the WF's facts go as a line under the post;
    - the read loop only reads, acks and hands off.

    The app needs Interactivity on for clicks.
  - **Archi:** the handbook's §4.2 (the catalog, `subject`, the menu for people, NOT_TAKEN, the
    handover pattern), and the seeds' `sales.consult` with `answer_team` (`seed_version` 5).
- **Checks before the release:** the full local CI, 25 of 25 (Rust fmt, clippy `-D warnings` and
  tests in both workspaces; Go gofmt, vet and race tests in six modules; the doc links), before
  and after the review's fixes.
- **Deploy:**
  - build of `9ee729a6` in 10 min; publish in 14 s (the repo keeps 0.1.85 to 0.1.94, `dist/`
    dropped 0.1.93's .deb);
  - snapshots `pre-wfcase4-0-1-94` on the 4 VMs (`pre-cogturns-0-1-93` was deleted);
  - 0.1.94 on all 4 in 206 s (core manifest `4e226b2865e206ea`), with health clean:
    `failed=0`, `denials=0` on all 4, the edge's certificate and io.web as before;
  - `wf.engine`, `ai.generic` and `io.slack` `current` are 0.1.94 on the motherbee and worker1;
  - **as expected after the upgrade:**
    - WF.gate0192@worker1 came up DEGRADED, its 0.1.92 definition refused ("answer is a known
      option: write it by its name alone"). Redeployed from the new example, cut to the gate's
      timers: version 2, 12 states, HEALTHY. Its store migrated to v5, with no instance running;
    - AI.pruebas@worker1 came up FAILED_CONFIG (`runtime.immediate_memory is not accepted`). It
      got its config of that day without the key, as a replace (`config_version` 5): CONFIGURED.
      It was the only agent whose `config.json` named it;
  - the gate's agents and IO.slack.gate0192 run 0.1.94, HEALTHY. io.slack opened its records
    (the open menus' table created in place) and connected Socket Mode.
- **Gate,** 2026-11-02 from 15:56 to 17:27 ART, on fresh threads (`thread:gate0194-*`). The
  operator was the team in the test channel, with Interactivity turned on first. All of it passed
  (wf-orchestration-v1 §5.4.12):
  - **The person's side alone:**
    - a message with no thread got `NOT_TAKEN` `no_thread` at once, and started nothing (A-135);
    - a stranger on a conversation's thread got nothing;
    - Case 1, short: the quote in 2.4 s, invoice F-0192-0417 4.1 s after the order, and payments'
      notice 122.8 s after the invoice.
  - **The consultation:**
    1. The operator asked sales in the post's thread; its `answer_team` was in that thread 3.2 s
       later.
    2. The decision reached the person 3.2 s after the operator's message, and sales answered
       the total from memory.
    3. A second message, after the decision, reached the person as a late decision.
  - **The handover, end to end:**
    1. The post carried the facts and the Handback button.
    2. The operator's answer in its thread reached the person, and the person's reply was posted
       in that thread.
    3. The click with a note closed the post. Sales answered the person from the note, twice.
  - **"Nobody took it"** closed the case at 123 s, three times. **"Quiet"** closed it 180 s after
    the operator's one answer. In each, the post showed "Closed." with its text, and sales told
    the person.
  - **"Not taken":** shown only to the writer, at once, in a closed case's thread and at the
    channel's top level. It also caught an answer the operator wrote in the wrong thread.
  - **Not exercised live:**
    - a click on a closed post, which keeps no button;
    - two supervisors' messages within one evaluation. The example's test covers it.
  - **Health:** `failed=0`, `denials=0` on all 4. The WF's journal held only the expected notices;
    io.slack and cognition logged no warning after the upgrade's restart.
  - **Seen on the way:** A-139 (a forwarded answer's `dst_ilk`), A-140 (a definition's texts, for
    Case 5), and two cosmetics for io.slack's revamp (A-130).
  - **The gate's deployment stays up** for Case 5 (WF.gate0192, its agents, IO.slack.gate0192).
- **Config, 2026-11-02 at 23:58 ART: the motherbee's AI on Claude Opus 5.5.** The operator: Archi
  programs Case 5's solution with that model (wf-orchestration-v1 §5.5).
  - The motherbee's `/etc/fluxbee/hive.yaml` names `claude-opus-5-5` as the anthropic model (backup
    `hive.yaml.pre-opus-0-1-94`). SY.admin, SY.architect, SY.cognition and SY.frontdesk.gov were
    restarted: all active.
  - worker1 keeps `claude-sonnet-5-5`, so the agents and their cognition do not change.
  - Archi's chat answered on `claude-opus-5-5` in 5 s. The check's session was deleted from its
    history.
  - The model is the motherbee's, not Archi's alone. SY.admin's executor and SY.frontdesk.gov use
    it too. An AI node on the motherbee that names no model would use it, and so would a worker
    that `add_hive` creates once the orchestrator restarts. SY.cognition@motherbee follows no
    communication.
  - SY.frontdesk.gov came up configured. It had booted without its AI at the upgrade (A-141).
  - **Back:** the backup, and the same four restarts.
- **Config, 2026-11-03 at 00:28 ART: Archi's own Anthropic key.** The operator made it for Archi,
  so that its costs show apart at Anthropic, and handed it over by a file's path.
  - The vault holds it as `anthropic_architect`: resource `anthropic`, the root tenant, owned by
    SY.architect (`ilk:ba06dcf6-…`, its system ILK). A node takes the key it owns before its
    tenant's general one (`resolve_resource`), so no code changed. The others keep
    `anthropic_root_pool`.
  - Archi refreshed its runtime on the vault's notice, and was restarted to be sure. It answered
    on `claude-opus-5-5` with it, and the check's session was deleted.
  - **Back:** delete `anthropic_architect` from the vault, and Archi goes back to the root key.
- **Rollback:**
  - snapshot `pre-wfcase4-0-1-94`;
  - or `apt install fluxbee=0.1.93` plus the core update of the spokes. Then the gate's WF needs
    its 0.1.92 definition again (0.1.94's writes known options by name, which 0.1.93 does not
    know), and AI.pruebas its earlier config. 0.1.93's engine takes the v5 store as migrated (it
    checks only `user_version >= 4`); whether it reads the rebuilt table was not checked, so the
    snapshot is the clean way back.

## 0.1.93 — Cognition carries each communication's last 20 turns, so an AI agent's next message has the turn before it (A-134)

- **Date:** 2026-10-30 (ART) · **Previous version:** 0.1.92 · **Commit:** `c70ad89` (the code is
  `bc37404`).
- **Scope:** motherbee and the three spokes (core update). The gate's deployment of 0.1.92 stays up.
- **Design:** FINDINGS A-134, decided with the operator on 2026-10-30 after 0.1.92's gate. An agent's
  memory is cognition's, and ai.generic's immediate memory stays off.
  - The old "20 messages" (the context window of `docs/history/11-context.md`) were never built in
    the router. In March, cognition v2 gave the recent conversation to the AI SDK's immediate
    memory, built for SY.architect, which cognition does not serve.
  - The stream is not what delayed the turns: cognition's worker is serial, about 5 s a turn. So
    the turns are taken at intake.
  - One review round: two reviewers, ten findings, all fixed in `bc37404`.
- **What changed:**
  - **SY.cognition** keeps each communication's last 20 turns, 48 KiB at most. Each turn holds who
    spoke, the sender's ILK, the node it came through, its text (2,000 characters at most), its
    files, the menu choice and its time.
    - The intake writes the turns and rewrites the memory region at once. The worker's labels
      follow as before.
    - The turns are kept in `cognition_threads`' payload, so they come back after a restart.
  - **The memory package** is `package_version` 3 and carries the turns. The router copies the
    region before parsing it and caches the parse. A package gives way so a delivery fits its
    128 KiB frame.
  - **SY.storage:** a thread row of the same turn replaces the stored one only when it was built
    later (`source_ts`).
  - **ai.generic** shows the turns as the recent conversation, one line each, and names the current
    message's speaker.
- **Deploy:**
  - build of `c70ad89` in 20 min, deploy in 200 s;
  - publish: the repo keeps 0.1.84 to 0.1.93 (0.1.83 pruned), and `dist/` dropped 0.1.92's .deb;
  - snapshots `pre-cogturns-0-1-93` on the 4 VMs (`pre-wfcase3-0-1-92` was deleted);
  - 0.1.93 on all 4 (core manifest `ab3d8310c3d3b82c`), with health clean;
  - `wf.engine`, `ai.generic` and `io.slack` `current` are 0.1.93 on the motherbee and worker1. The
    gate's agents, its WF, its io.slack and `AI.pruebas` run 0.1.93, HEALTHY;
  - before the deploy, the new upsert was checked on PROD's PostgreSQL 16.15 with reads only. The
    29 stored rows' `source_ts` cast, the row comparison orders as intended, and the statement
    prepares;
  - SY.storage restarted once at the vault's boot broadcast (`storage_postgres_url`), as it does at
    every deploy.
- **Gate**, second run, on fresh threads (`thread:gate0193-a` and `-c`):
  - **Case 1, to the end:**
    1. The person got sales' quote in 2.8 s.
    2. "Dale, facturamelo" reached sales with its package; the prompt grew from 985 to 1,977
       characters. Sales handed over, and invoice issued F-0192-4827 for 1,000 USD, due in 2
       minutes, 5.4 s after the message.
    3. "Gracias" handed the case back.
    4. Payments' notice came 122.6 s after the invoice; then nothing more arrived.
  - **The consultation**, with the operator as the supervisor in Slack:
    1. Sales chose `consult` with a precise question (20 Pro licenses, 20% off, 1,600 USD a month),
       and the person got "I will check".
    2. The operator's first answer went to the channel, outside the post's thread, and was
       dropped, as in the first run (a note for Case 4, wf-orchestration-v1 §5.3.7).
    3. The answer in the thread, "Sí, véndelo", reached sales, which chose `decided`. The person got
       the approval: 20% off, 1,600 USD a month.
    4. The person asked "how much is it a month with the discount you approved?" Sales answered
       from its memory in 3.8 s: 1,600 USD instead of 2,000. In the first run it consulted again
       here.
       - The question reached sales 0.4 s after sales' own answer, and its package already held
         that answer.
    5. Ten minutes followed with nothing more. Both instances closed.
  - **Cognition:** SY.cognition@worker1 took all 16 turns at intake, with no drops or errors.
    SY.storage stored each communication's row with its turns.
  - **Health:** `failed=0`, `denials=0` on all 4.
- **Rollback:**
  - snapshot `pre-cogturns-0-1-93`;
  - or `apt install fluxbee=0.1.92` plus the core update of the spokes. 0.1.92 ignores the turns in
    the stored rows and in the region.

## 0.1.92 — The WF track, Case 3: the WF asks an agent with a menu, the catch and the default log, the cap on agent calls, io.slack's thread records, and Archi's WF chapter (wf-orchestration-v1 §5.3; A-125, A-131)

- **Date:** 2026-10-30 (ART) · **Previous version:** 0.1.91 · **Commit:** `4e20b88`.
- **Scope:** motherbee and the three spokes (core update). PROD ran no WF before it.
- **Design:** [wf-orchestration-v1](../docs/wf-orchestration-v1.md) §5.3, decided with the operator on
  2026-10-30: the menu, the catch as in .NET with the log as the default, the cap of 8 agent calls
  per trigger (A-125), the way back belongs to the IO node, and one handbook.
  - **The build:** three agents, from §5.3.6's contracts: the Go side, ai.generic and the AI SDK,
    and io.slack. Then Archi's part: the handbook's §4.2 and the seeds' `sales.consult`.
  - **The review round** (§5.3.8): six reviewers, 23 findings, fixed before the release. Two
    decisions of the operator came out of it:
    - a post io.slack opens serves one message;
    - the agents' memory is cognition's, and ai.generic's immediate memory stays off (A-133).
- **What changed:**
  - **The menu.** A send or a forward to an `AI.*` node may carry a menu. The agent adds `none`,
    answers `{reply, option, params}` in `meta.context.evaluation`, and an answer off the menu is an
    error, with no retry. wfcel checks the menu and every read of the evaluation against it.
    `meta.context.response_envelope` is refused in a WF.
  - **The catch** is tried when no transition of the state takes an event. Without one, or when it
    takes nothing, the event is logged locally as `unhandled event`.
  - **The cap:** 8 agent calls per trigger, then one `AGENT_CALL_CAP` event. The instance never
    fails for it.
  - **io.slack's thread records** (`slack_threads.db` in the node's directory):
    - a reply in the thread of a post the node made takes that post's thread id;
    - an outbound message goes to its thread id's recorded place;
    - a post the node opened serves one message;
    - the text it posts is escaped, and the text it relays decodes what Slack escapes.
  - **ai.generic:** the menu contract; the evaluation's `reply` is the text the person gets; a
    model's answer is the first whole JSON object (A-131).
  - **Archi:** the handbook's §4.2 is the one reference for writing a WF. The seeds hold
    `sales.consult` at `seed_version` 4, and the quickstart points to the chapter.
- **Deploy:**
  - build of `4e20b88` in 22 min, deploy in 233 s;
  - publish: the repo keeps 0.1.83 to 0.1.92 (0.1.82 pruned), and `dist/` dropped 0.1.91's .deb;
  - snapshots `pre-wfcase3-0-1-92` on the 4 VMs (`pre-wfcase1-0-1-91` was deleted);
  - 0.1.92 on all 4 (core manifest `5b5d0061ccb9b190`), with health clean;
  - `wf.engine`, `ai.generic` and `io.slack` `current` are 0.1.92 on the motherbee and worker1.
    `AI.pruebas` runs, HEALTHY;
  - Archi logged "cookbook seeds upgraded" for its design and artifact cookbooks.
  - **Before it:** the operator reset the Proxmox host by mistake (2026-10-30, about 19:46 UTC). The
    5 VMs came back on their own (`onboot=1`), and health was clean on all 4 hives.
- **Gate** (§5.3.6), first run. The deployment stays up until the track's case is complete:
  - **The pieces:**
    - a person registered in `pruebas` through io.cloud;
    - three agents on worker1, `AI.gate0192sales/invoice/payments`, with Spanish roles that keep only
      their trade, and immediate memory off;
    - `WF.gate0192@worker1`, the repo's `wf.sales.invoice` on those names: 10 states, 51 guards, 89
      actions, compiled in 5 s;
    - a client on worker1's router socket as `IO.gate0192`;
    - a real io.slack, `IO.slack.gate0192@motherbee`, in the operator's test Slack workspace. Its
      tokens went to SY.vault through stdin, and its `io.dst_node` is the WF.
  - **The menu:** every answer came back as an option of its menu with its evaluation, and the WF
    branched on it. The person got sales' quote 2.6 s after writing.
  - **The consultation**, with the operator as the supervisor in Slack:
    1. Sales chose `consult` with a precise question; the person got "I will check", and the
       question was posted at the channel's top level.
    2. An answer the operator wrote in the channel, outside the thread, reached the WF on the
       channel's thread id, and was dropped: no instance holds it.
    3. The answer in the thread reached the instance. Sales chose `decided`, and the person got the
       decision: 15% approved, 1,700 USD a month.
    4. Sales' second question opened a new post. A late reply in the first post's thread was
       dropped, since io.slack had forgotten that post.
    5. Three messages in the new thread that decided nothing got `wait`, and the person got none of
       them. The next decision reached the person.
    6. The WF node's log kept no text of the messages it dropped.
  - **What failed: the agents' memory.** With immediate memory off, sales forgot its own quote
    ("which plan do you want?") and did not invoice. Two turns after the approval, it forgot that
    too and consulted again. SY.cognition was active and took every turn, but its package holds
    labels and summaries that come several turns late, never the turns (FINDINGS A-134). The
    operator decided that cognition carries the last 20 turns of each communication; that is
    being built.
  - **Health:** `failed=0`, `denials=0` on all 4.
- **Rollback:**
  - snapshot `pre-wfcase3-0-1-92`;
  - or `apt install fluxbee=0.1.91` plus the core update of the spokes. A WF definition with a menu
    or a catch does not load on 0.1.91, and io.slack's records file is new.

## 0.1.91 — The WF track, Case 1: one instance per conversation, `start_from`, only the nodes involved drive an instance, `forward` and `keep_message`, the WF's metadata block, timer times from state, internal transitions; ai.generic reads the block and answers with `text_field` (wf-orchestration-v1 §5.2; A-123, A-116 in part)

- **Date:** 2026-10-30 (ART) · **Previous version:** 0.1.90 · **Commit:** `75a6786` (the code is
  `70b42db`).
- **Scope:** motherbee and the three spokes (core update). PROD ran no WF before it.
- **Design:** [wf-orchestration-v1](../docs/wf-orchestration-v1.md) §5.2, the operator's decisions
  of 2026-10-29:
  - one instance per conversation;
  - a forward changes only the metadata, and the WF gives an agent deterministic context there;
  - control goes back to sales when an agent hands the case back, or on an inactivity timer;
  - only the nodes involved drive an instance (A-123).

  The build:
  - Two agents built it: one the WF engine and its language, the other ai.generic and its SDK.
    I made some changes after them.
  - One review round, with five reviewers: the engine, security, the language, the docs and the
    agents (§5.2.3).
  - Fixed before the release, among others:
    - **One person's message could bounce between sales and invoice forever.** A probe made 101
      agent requests from one message. Now a handback delivers invoice's answer and waits for the
      person. A-125 records an engine cap, now with Case 3.
    - **An agent in `start_from`** would let its late answer start an instance. `start_from` now
      refuses it.
    - **An agent's error or timeout ended a pending payment check.** Both now go back to sales.
    - **A blank answer under a `text_field`** broke the person's later turns on Anthropic. The
      history now keeps the object.
    - **A replayed creation** could put a second instance on a thread.
- **What changed:**
  - **One instance per conversation.** An instance lives on the thread of the message that started
    it, or on its own id when that message had none. Store schema v4 adds a thread column and the
    kept messages, and refuses a second running instance on a thread.
  - **Only the nodes involved drive an instance (A-123).** The engine takes an event in this order:
    1. the fire of its own timer, from SY.timer;
    2. a sender the definition does not name is dropped;
    3. a message on a running instance's thread goes to that instance;
    4. a non-reply from a `start_from` node starts one;
    5. anything else is dropped.

    `start_from` is required, and refuses `SY.*`, `RT.*` and `AI.*`. `event_match.src` names a
    sender.
  - **`forward` and `keep_message`:**
    - `forward` sends a kept message, or the event's, with its whole meta except the memory package,
      and its payload byte for byte;
    - `keep_message` keeps a message under a name, committed with its transition.
  - **The WF's block.** `meta.context.wf` carries the trail and the variables a definition names.
    ai.generic shows it to the model after the memory block, and keeps it out of the history.
  - **Timers and transitions.** A timer takes `fire_in` or `fire_at` from state (`$ref`), with the
    60 s floor checked at run time. A transition may stay in its state (no `target_state`).
  - **ai.generic answers a `text_field` contract.** The person gets the text, and the WF gets the
    object in `meta.context.evaluation`. The history keeps the object as the agent's turn.
  - **The language refuses more:**
    - another action type's fields;
    - a `keep_message`, or a forward without a message, on a timer's transition;
    - a target with spaces;
    - a contract the agents refuse.
  - **The examples and the docs follow:**
    - `wf.invoice` and `wf.router` 0.0.7 take `start_from`;
    - the new `wf.sales.invoice` is Case 1's circuit;
    - Archi's seeds are at `seed_version` 3;
    - SY.admin's help, the spec, the quickstart, sy-wf-rules, the handbook and the READMEs.
- **Deploy:**
  - build of `75a6786` in 19 min, deploy in 236 s;
  - publish: the repo keeps 0.1.82 to 0.1.91 (0.1.81 pruned), and `dist/` dropped 0.1.90's .deb;
  - snapshots `pre-wfcase1-0-1-91` on the 4 VMs (`pre-wfcase0-0-1-90` was deleted);
  - 0.1.91 on all 4 (core manifest `4d77769cf19bce32`), with health clean;
  - `wf.engine` and `ai.generic` `current` are 0.1.91 on the motherbee and worker1, their only
    version. `AI.pruebas` runs, HEALTHY;
  - Archi logged "cookbook seeds upgraded" for its design and artifact cookbooks.
- **Gate** ([wf-orchestration-v1](../docs/wf-orchestration-v1.md) §5.2.2), from inside, with no
  extra pieces in the infrastructure:
  - **The pieces:**
    - three agents in `pruebas` on worker1: `AI.gate0191sales`, `AI.gate0191invoice` and
      `AI.gate0191payments`, with immediate memory on and Spanish roles through Archi;
    - `WF.gate0191@worker1`, through `wf_rules_compile_apply` with `auto_spawn`, holding Case 1's
      definition on those names;
    - a client on worker1's router socket named `IO.gate0191`, sending a registered person's
      messages with a `meta.context.io`.
  - **Language:** six definitions got `COMPILE_ERROR` through SY.admin, naming the path:
    - `AI.*` in `start_from`;
    - `SY.*` in `start_from`;
    - no `start_from`;
    - a forward with `name`;
    - a `keep_message` on a timer;
    - a target with a space.

    The real one compiled in 5 s.
  - **The conversation**, on one thread and one instance:
    1. The quote: sales answered in 3 s.
    2. *"Dale, facturámelo"*: sales answered `area: invoice`. The WF forwarded the same message to
       invoice, with the order in its block, and invoice issued `F-0191-0001` for 1000, due in 2
       minutes. The person got invoice's answer 8 s after the message.
    3. The WF node restarted.
    4. *"Gracias"* went straight to invoice, which remembered the invoice.
    5. SY.timer fired `payment_due` 2 minutes after the invoice, across the restart. Payments'
       notice reached the person 3 s later.
    6. The instance closed after 3 minutes without messages.

    Every delivery carried its trail, the conversation's thread and the person as `dst_ilk`.
  - **The checks:**
    - **A stranger** on the thread was dropped (A-123).
    - **The restart:** recovery loaded the instance, with its state and timers unchanged.
    - **A late answer** under payments' name, on the closed thread and on a fresh one, started
      nothing.
    - **After the close,** the person's next message started a new instance at sales.
  - **The store:** the inbound journal and the internal events were empty at every check. The
    instances' states and logs were read from the WF's store, read only, because SY.admin has no
    instance actions yet (Case 5).
  - **Health:** `failed=0`, `denials=0` on all 4, through the gate.
- **Cleanup:**
  - the workflow `gate0191` was deleted with `force`, node and package;
  - the three agents were killed and purged, with their ILKs;
  - their roles were deleted from Archi;
  - the person's ILK was deleted and purged;
  - the gate's folder on worker1 was removed.

  `pruebas`, its keys and `AI.pruebas` stay.
- **Noted:**
  - A message the WF writes itself carries no `src_ilk`, so payments logged `missing src_ilk` on
    `PAYMENT_DUE` (the rest of A-116, recorded as left out of Case 1).
  - Compiling a valid definition through `wf-rules/compile` stages it. Here it was the current hash,
    and the delete removed it.
- **Upgrading another hive:** each workflow is redeployed with `start_from`.
  - A definition written before 0.1.91 no longer loads, and its node is `FAILED_CONFIG`
    (`invalid_definition`).
  - Its running instances fail at the next start (`definition_unavailable`).
  - The store moves to schema v4 by itself.
- **Rollback:**
  - snapshot `pre-wfcase1-0-1-91`;
  - or `apt install fluxbee=0.1.90` plus the core update of the spokes. A WF database written by
    0.1.91 (schema v4) is not one 0.1.90 reads; PROD has none.

## 0.1.90 — The WF track, Case 0: a WF always sends `user`, `event.timer_key`, an instance ends on its own definition, at least once, `$ref` with a default, the WF's controls answer only SY.admin and SY.wf-rules, `target_ref` goes (wf-orchestration-v1 §5.1; A-117, A-118, A-119)

- **Date:** 2026-10-29 (ART) · **Previous version:** 0.1.89 · **Commit:** `e6b07fc`.
- **Scope:** motherbee and the three spokes (core update). No WF ran in PROD before it.
- **Design:** [wf-orchestration-v1](../docs/wf-orchestration-v1.md) §5.1, the operator's seven
  decisions (2026-10-28) and what was fixed with them.
  - Built by one agent. One review round, with four reviewers: the engine, security, the language
    and the docs.
  - Fixed before the build (27 items). Among them:
    - **Engine:**
      - an event that crashed the node ran again at every start. The trigger was a guard left
        running past its timeout, which read the state while the next transition wrote it: a fatal
        error;
      - the reconcile dropped timers scheduled while the node was degraded;
      - recovery fired timers from a stale snapshot.
    - **Security:**
      - an agent's late answer started a new instance, which asked the agent again (a WF↔AI loop);
      - a timer another node scheduled at SY.timer passed the `TIMER_FIRED` check;
      - a system message was taken as a workflow event.
    - **Language:**
      - an integer literal in a definition went out as `3.0`;
      - a mistyped `$ref` went out as a literal;
      - `event_match.type` left with `meta.type`.
    - **Docs:**
      - the spec's and the quickstart's main example waited for answers an agent never sends;
      - the quickstart's first step would have pinned `wf.engine` by hand (A-122);
      - the instance answers' keys.
  - **After the round, by me:** a `$ref` with a mistyped root is refused again (the round had made
    it a literal).
  - **Archi's workflow hash** follows SY.wf-rules, which now stores numbers as written.
- **What changed:**
  - **A WF always sends `user`:** `meta.type` and `event_match.type` left the language, and a
    definition that has them is refused by path. A system message other than `TIMER_FIRED` is no
    workflow event.
  - **`event.timer_key`** carries a fired timer's key, read from SY.timer's `client_ref`.
  - **An instance ends on its own definition:** store schema v3 keeps definitions by hash, and
    recovery binds each instance to its hash.
  - **At least once:**
    - an inbound journal, and a creation in one commit;
    - an event that does not finish in 3 starts fails its instance (`replay_exhausted`).
  - **`$ref`:** a missing field fails; `{"$ref": p, "default": v}` takes `v` when `p` is missing or
    null.
  - **The WF's controls:**
    - the `WF_*` verbs answer only SY.admin@motherbee and SY.wf-rules of the hive;
    - a `TIMER_FIRED` is taken only from SY.timer of the hive, for a timer the WF scheduled;
    - errors, `*_RESPONSE`, router notices and late answers are never answered.
  - **`target_ref` goes:** `wf.router` 0.0.5 replies with one transition per IO node written in it.
  - **Robustness:**
    - a panic fails its instance;
    - 100 internal events per inbound event;
    - numbers keep their type;
    - a start without SY.timer runs DEGRADED;
    - SQLite runs FULL sync.
  - **The examples, the docs and the help follow:**
    - `wf.invoice` 0.1.1;
    - Archi's seeds (`seed_version` 2);
    - SY.admin's help example, which the Go suite validates;
    - the spec, the quickstart, sy-wf-rules-spec, the READMEs and the handbook;
    - a test validates every definition in the docs.
- **Deploy:**
  - build of `e6b07fc` in 13 min, deploy in 217 s;
  - snapshots `pre-wfcase0-0-1-90` on the 4 VMs (`pre-tools1-0-1-87` was deleted);
  - 0.1.90 on all 4 (core manifest `4d9b2b240b8fab77`), with health clean;
  - `wf.engine` `current` is 0.1.90 on the motherbee and worker1, its only version;
  - Archi logged "cookbook seeds upgraded" for its design and artifact cookbooks.
- **Gate,** a real WF through SY.admin: `wf_rules_compile_apply` with `auto_spawn` made
  `WF.gate0190@worker1`, in tenant `gate0190w`. It asks `AI.pruebas@worker1`, holds the answer for a
  60 s timer, and replies to the gate client `IO.gate0190@worker1` by its name. Its v2 replies with
  another message name.
  - **Language:** five definitions it refuses (`meta.type`, `target_ref`, `event_match.type`, a
    `$ref` with another root, one with an extra key) each got `COMPILE_ERROR` through SY.admin,
    naming the path.
  - **The exchange:**
    - the WF sent `GATE_TURN` as a user message;
    - the agent answered "ok" in 2 s (one `function_turn` on Claude Sonnet 5.5);
    - the 60 s timer fired from SY.timer, and `event.timer_key == 'hold'` took it;
    - the client got `GATE_REPLY` "ok".
  - **A late answer:** a message on the ended instance's thread was dropped, and started nothing.
  - **The WF's controls,** from a throwaway node `IO.gate0190p@worker1`:
    - `WF_LIST_INSTANCES` got `UNAUTHORIZED`, naming who may ask;
    - a forged `TIMER_FIRED` was refused and not answered;
    - a timer the probe scheduled at SY.timer@worker1 with an instance's `client_ref` was accepted
      by SY.timer, and the WF refused its fire;
    - SY.wf-rules@worker1 still counted the instances.
  - **A restart** of the WF node while an instance held its answer: recovery loaded it, SY.timer
    fired once, and the client got one reply in its whole window.
  - **A redeploy to v2** while an instance held its answer:
    - recovery loaded it on 2 definitions;
    - it ended on v1 (`GATE_REPLY`), and v1 was dropped when it ended;
    - the next instance took v2 (`GATE_REPLY_V2`).
  - **The store:** the inbound journal and the timer index were empty after every transition.
  - **Health:** `failed=0`, `denials=0` on all 4.
- **Cleanup:**
  - the workflow `gate0190` deleted with `force` (node and package). worker1's dist copy left
    with Syncthing's usual delay, 1.5 min;
  - the tenant `gate0190w` deleted and purged;
  - the gate's folder on worker1 removed;
  - the probe's one-shot timer at SY.timer fired, and is gone.
  - `AI.pruebas` stays.
- **Noted:**
  - The agent logs `missing src_ilk` on every message a WF sends it (A-116, Cases 1–2).
  - SY.timer takes a timer for another node from any node (A-115).
- **Upgrading another hive:** nothing by hand.
  - A hive with WFs whose definitions hold `meta.type`, `target_ref` or `event_match.type` needs
    them recompiled. After the upgrade, such a definition does not load, and its running instances
    fail (`definition_unavailable`).
- **Rollback:**
  - snapshot `pre-wfcase0-0-1-90`;
  - or `apt install fluxbee=0.1.89` plus the core update of the spokes. A WF database written by
    0.1.90 (schema v3) is not one 0.1.89 reads; PROD has none.

## 0.1.89 — Agent tools, iteration 2: `read_blob` and `unpublish_html_page`, CSV and HTML attachments as text, router notices never answered (agent-tools-v1 §6.2; A-110, A-111, A-112, A-121)

- **Date:** 2026-10-28 (ART) · **Previous version:** 0.1.88 · **Commit:** `694becc`.
- **Scope:**
  - motherbee and the three spokes (core update);
  - just before the deploy, the gate's `ai.generic 0.1.88-gate1` was deleted on the motherbee.
    Otherwise the package would have kept it as `current` (A-122).
- **Design:** [agent-tools-v1](../docs/agent-tools-v1.md) §6.2.
  - The operator first decided four tools. Once the code was checked, the AI node stayed a pure
    responder.
  - Reminders and `send_message` were built, then removed. They move to WFs (§6.3 and
    [wf-orchestration-v1](../docs/wf-orchestration-v1.md)).
  - Built by two agents, one for the AI side and one for the platform side. One review round
    (every lens), all of it fixed before the build:
    - **Platform:**
      - A-121;
      - the command log kept a page's key;
      - a publication id not in its canonical form;
      - an expired page reported as taken down;
      - the start-script check read any exec line;
      - docs.
    - **AI:**
      - the same notice loop in SY.frontdesk.gov;
      - text judged by the name's extension;
      - no budget per message;
      - links and blob names in the journal;
      - a malformed key;
      - the cut note's offset.
- **What changed:**
  - **`read_blob` (§6.2.1):**
    - reads a text blob by the name an attachment shows, in pieces of 64 KiB with
      `next_offset`;
    - text is judged by content (a NUL byte = `not_text`);
    - at most 10 MiB, and 192 KiB per message;
    - the SDK reads only `active/`, without following links, and checks the hash in the name.
  - **`unpublish_html_page` (§6.2.2):**
    - takes a page down by its `publication_id` or its link;
    - SY.admin's `unpublish_artifact` takes exactly one of a canonical id or the link's key,
      and answers `NOT_FOUND` for a page that does not exist;
    - its command log keeps only the canonical id and the key's sha256.
  - **A-111:** CSV and HTML attachments reach the model as text. An attachment shows its blob
    name, and a cut text says where to read on.
  - **A-112:** router notices, `*_RESPONSE` and other infrastructure messages are never
    answered. The rule lives in the AI SDK; ai.generic and SY.frontdesk.gov use it.
  - **A-110:** the publish scripts write the manifest last; the catalog is a parameter.
  - **A-121:** a purge records "do not relaunch" right after the stop.
  - **The journal's preview** redacts live links and blob names.
- **Deploy:**
  - build of `694becc` in 24 min, deploy in 238 s;
  - snapshots `pre-tools2-0-1-89` on the 4 VMs (`pre-tools-0-1-86` was deleted);
  - 0.1.89 on all 4 (core manifest `f566d4ecfa367c7e`), with health clean.
  - The seed logged "ai.generic 'current' moved 0.1.88-gate1 -> 0.1.89 (package authority)".
  - `AI.pruebas` moved onto 0.1.89 by itself, within the reconcile pass (A-109).
- **Gate,** on `AI.pruebas@worker1` (Claude), with messages through the admin:
  - **A-111:** a 5-row CSV attachment, which had answered `provider_error` on 0.1.88.
    - Asked for the rows of 2026-10 and their sum, the agent listed all three: 305.
    - Asked for "only the number", it first answered 245, leaving out the last row. That was
      the model's slip: the input held the whole CSV.
  - **`read_blob`:** a 148,037-character text with a random code only in its last line.
    - The attachment was cut at 100,000 characters.
    - Two model turns (`llm_usage`): the first called `read_blob` from the cut, and the second
      answered the right code.
  - **Unpublish:**
    - the agent published a page, which answered HTTP 200 from the internet;
    - in another conversation, given only the link, it took the page down: 404;
    - the admin's command log keeps only `key_sha256` for it.
  - **A-112:**
    - a throwaway mesh client sent a message and left at once;
    - the router's `UNREACHABLE` reached the agent;
    - it was logged once as a router notice, "not answered", with no `unknown_system_msg`.
  - **The catalog:** `get_runtime ai.generic` answers `current` 0.1.89, with the 17-tool
    catalog.
    - The agent offered 14 tools to a message without a person.
    - The three thread-state tools need `src_ilk` (their cards' `present_when`).
  - **Health:** `failed=0`, `denials=0` on all 4; the web door is acknowledged.
- **Cleanup:**
  - the gate's two blobs and its folder on worker1 were removed;
  - the page is down;
  - `AI.pruebas` stays, on 0.1.89, following `current`.
- **Noted:**
  - The 0.1.88 entry said to remove `0.1.88-gate1` *after* this release. It had to be before:
    after the seed, `current` would have dangled (A-122).
  - The admin's command log keeps a `send_node_message`'s text. So the link typed into the
    gate's message is there: the operator's own route, and the page is down.
- **Upgrading another hive:** nothing by hand.
- **Rollback:**
  - snapshot `pre-tools2-0-1-89`;
  - or `apt install fluxbee=0.1.88` plus the core update of the spokes. The agents lose
    `read_blob` and `unpublish_html_page`, and A-111 and A-112 return.

## 0.1.88 — A running node moves onto what its runtime names now, on every reconcile pass, once that is ready; every launch records and stamps its version (A-109, A-101, A-102)

- **Date:** 2026-10-27 (ART) · **Previous version:** 0.1.87 · **Commit:** `d01ba1d`.
- **Scope:**
  - motherbee and the three spokes (core update);
  - in the gate, ai.generic `0.1.88-gate1` was published as `current` (the same binary as 0.1.88).
- **Design:** the fixes of FINDINGS A-109 (found in the 0.1.87 gate), A-101 and A-102. Built by one
  agent; one review round (every lens) passed it with five low findings, all fixed before the
  build.
- **What changed (SY.orchestrator):**
  - **One reconcile for boot and the 60 s loop,** serialized by a pass lock, with the manifest read
    per node under its lock. A running node is relaunched when either:
    - it follows `current` and the pointer moved;
    - its unit does not run what its runtime names now, judged by the live ExecStart and the
      `FLUXBEE_NODE_RUNTIME_VERSION` stamp. This is how a package node moves with its base: WF on
      wf.engine, config-only AI packages on ai.generic.

    A unit already on the target only has its config corrected.
  - **Readiness before any launch** (reconcile, run_node, start, restart): the version's directory,
    its start script, the binary the script execs (fail-closed), `package.json` and a workflow's
    definition.
    - The reconcile waits, logged once.
    - An operator's launch is refused with `RUNTIME_NOT_PRESENT` before anything is stopped.
  - **Every launch path records `_system.runtime_version`** (and `package_path`, and
    `relaunch_on_boot`) under the node's lock before launching (A-101, A-102). The unit carries the
    version stamp.
- **Deploy:**
  - build of `d01ba1d` in 8 min, deploy in 227 s;
  - snapshots `pre-reconcile-0-1-88` on the 4 VMs (`pre-web-0-1-85` was deleted);
  - 0.1.88 on all 4 (core manifest `cb87576b59941ef1`), with health clean.
- **Gate:**
  - **After the deploy (A-101):** `AI.pruebas` reported 0.1.88 consistently in `node_help`, its
    binary, its stamp and `_system`.
  - **A-109, live:**
    - `AI.gate0188@worker1` was started pinned to 0.1.88;
    - then `publish-ia-runtime.sh --version 0.1.88-gate1 --set-current --skip-build`, with the
      installed binary, on the motherbee: no .deb, no orchestrator restart;
    - worker1 had the manifest and the version 5 s later;
    - its orchestrator's periodic pass relaunched `AI.pruebas` (which follows `current`) onto
      0.1.88-gate1 within 28 s, and it reported 0.1.88-gate1 everywhere;
    - the pinned `AI.gate0188` stayed on 0.1.88, with both versions on one hive.
  - **Republishing the same version** (to add the catalog the first run missed, A-110) relaunched
    nothing. `get_runtime` then answered the 15-tool catalog of 0.1.88-gate1.
  - **Health:** `failed=0`, `denials=0` on all 4; the web door is acknowledged.
- **Cleanup:**
  - `AI.gate0188` was killed and purged, and the scripts copied to the motherbee were removed;
  - `AI.pruebas` stays, on 0.1.88-gate1;
  - **ai.generic's `current` stays 0.1.88-gate1 until the next release.** Its directory is not
    dpkg's, so remove it after that release.
- **Upgrading another hive:** nothing by hand. Running nodes now move by themselves within a pass.
- **Rollback:**
  - snapshot `pre-reconcile-0-1-88`;
  - or `apt install fluxbee=0.1.87` plus the core update of the spokes. A-109 returns: restart each
    following node by hand.

## 0.1.87 — Consolidating the agents' tools, iteration 1: the PDF writes Spanish, files stay out of the model's context, the model's own reply with every file, the model defaults to the hive's, the time (agent-tools-v1 §6.1; A-97, A-98, A-99, A-104, A-109)

- **Date:** 2026-10-27 (ART) · **Previous version:** 0.1.86 · **Commit:** `6772df4`.
- **Scope:** motherbee and the three spokes (core update). One step by hand on the test agent:
  `restart_node AI.pruebas`, explained below.
- **Design:** [agent-tools-v1](../docs/agent-tools-v1.md) §6.1. Built by one agent; one review round
  (every lens) found two medium and two low issues, all fixed before the build.
- **What changed:**
  - **A-97:** the PDF declares WinAnsiEncoding, wraps by Helvetica's widths, and continues on new
    pages.
  - **A-98 and A-99:** a generated file stays out of the model's context.
    - The reply is the model's own text, with every file of the turn (at most 8; the same name
      replaces the earlier file).
    - A link published in the turn is appended when the reply lacks it.
  - **A-104:**
    - `behavior.model` is optional and defaults to the hive's model for the node's provider;
    - `CONFIG_SET` refuses a model the hive cannot supply;
    - the examples name neither provider nor model.
  - **`get_current_time`:** a new base tool, through SY.timer on the agent's hive.
  - **Archi's cookbook seeds are versioned.** A newer built-in seed replaces the manual entries
    once and keeps the observed ones. PROD's were seeded at 0.1.81 with `"model": "gpt-5.5"`.
  - **SY.admin's executor:** `get_admin_action_help` takes `action_name`.
- **Deploy:**
  - build of `6772df4` in 13 min, deploy in 219 s;
  - snapshots `pre-tools1-0-1-87` on the 4 VMs (`pre-door-0-1-84` was deleted);
  - 0.1.87 on all 4 (core manifest `802a28c0a7f40072`), with health clean;
  - Archi logged the seed upgrade of its four cookbooks: design, artifact, plan_compile and repair.
- **Found in the gate: A-109.**
  - `AI.pruebas` kept running the 0.1.86 binary, whose directory dpkg had already removed. The
    orchestrator moves a node that follows `current` only at its own start, and this release did
    not change the orchestrator, so worker1's never restarted.
  - `restart_node AI.pruebas` (SY.admin, the operator's path) put it on 0.1.87.
  - `node_help` then still reported 0.1.86: that is A-101, seen live.
- **Gate (`AI.pruebas`, Claude):**
  - **A-104:** a `config-set` with neither provider nor model was accepted, with `ai_model` set to
    `claude-sonnet-5-5` from the hive.
  - **One reply with two files:** asked for a long Spanish PDF and a CSV, the agent answered in its
    own words with both attached.
    - The PDF is 2 pages and 82 lines, and declares WinAnsiEncoding.
    - Every accent, ñ, ¿ and ¡ is written as an octal escape (á 47, ó 34, ñ 12, ¿ 10…).
    - No raw byte is ≥ 0x80.
  - **The time:** "martes 27 de octubre de 2026, 14:29 (UTC-3)", which matched the clock.
  - **A page and a file in one reply:** the reply carried the link, with its expiry, and the CSV.
  - **A-95:** no `/public/` line in the agent's journal.
  - **Health:** `failed=0`, `denials=0` on all 4; the web door is acknowledged and ready.
- **Upgrading another hive:**
  - nothing by hand for the core;
  - until A-109 is fixed, restart each running AI node that follows `current`
    (`restart_node`), or it stays on the old binary.
- **Rollback:**
  - snapshot `pre-tools1-0-1-87`;
  - or `apt install fluxbee=0.1.86` plus the core update. 0.1.86 requires `behavior.model`, so first
    give every AI node one. `AI.pruebas` now has none (the AI runbook, "Rolling back to 0.1.86").

## 0.1.86 — An AI agent's tools declared once (cards, a catalog, NODE_HELP); the AI provider per node; "tools" the AI node's one word; the AI nodes' control plane only from its authorities (agent-tools-v1; A-93, A-94, A-95, A-105)

- **Date:** 2026-10-27 (ART) · **Previous version:** 0.1.85 · **Commits:**
  - `0e3f498`: the `NODE_HELP` constants;
  - `89848d8`: the release.
- **Scope:** motherbee and the three spokes (core update). Nothing by hand.
- **Design:** [agent-tools-v1](../docs/agent-tools-v1.md) (T1–T10, §3). Built by agents in two
  areas, the AI runtime and the `NODE_HELP` path with Archi. One review round (security,
  operations); its fixes are FINDINGS A-105.
- **What changed:**
  - **The AI SDK:**
    - a tool exists only with its card (`ToolCard`, `AgentTool`);
    - the model's definition is rendered from the card, at most 1024 characters;
    - the registry is in name order;
    - Anthropic gets cache breakpoints on the tools and the system prompt.
  - **ai.generic:**
    - cards for its 14 base tools, and a catalog generated from them (`docs/reference/agent-tools.*`,
      with a golden test), shipped as `dist/runtimes/ai.generic/<version>/tools.json`;
    - `NODE_HELP` answered with the cards;
    - **T8:** `behavior.provider` per node, with the key from the node's own tenant;
    - **T9:** `behavior.multimodal`; the old key is refused;
    - **A-95:** no log line carries a published link;
    - CSV cells never run as formulas.
  - **ai.generic and the frontdesk:** their control plane is accepted only from their real
    authorities. The frontdesk keeps only the thread-state get and put tools.
  - **The `NODE_HELP` path:**
    - the router's protected actions (32) and rule (3)'s reads, with both wasm files rebuilt;
    - SY.orchestrator's `node_help_flow` runs in its own task and answers at once for a node that
      ai.generic does not run;
    - SY.admin's `node_help`, for Archi.
  - **`get_runtime`** answers the version's `tool_catalog`, read from a regular file, bounded.
    Archi reads it first, then `node_help`.
- **Deploy:**
  - build of `89848d8` in 25 min, publish, deploy in 277 s;
  - snapshots `pre-tools-0-1-86` on the 4 VMs (`pre-hardening-0-1-83` was deleted);
  - 0.1.86 on all 4 (core manifest `151d8bab329a71c7`), with health clean;
  - the web door was untouched: `route=acknowledged upstream=ready io.web=running`.
- **Gate:**
  - **The catalog:**
    - `node_help` on `AI.pruebas@worker1` answered its 14 cards (ai.generic 0.1.86) in 3.2 s,
      across three hops;
    - on `IO.web@motherbee`, it answered at once: "the node runs io.web: only ai.generic nodes
      answer NODE_HELP";
    - `get_runtime ai.generic` answered `tool_catalog`: 14 tools, 32 KB;
    - Archi reached both through its own translations (SCMD, no model).
  - **The control plane, the same probe before and after** (`AI.intruso` on worker1):

    | Probe | 0.1.85 | 0.1.86 |
    |---|---|---|
    | `CONFIG_GET` as `system` | dropped by the router | dropped by the router |
    | `CONFIG_GET` as `admin` | **the agent answered its whole config** | refused: `unauthorized`, `effective_config: null` |
    | `config_get` in lowercase | **passed the router and was answered** | refused by the agent |

  - **A real agent, both providers.** `AI.pruebas` wrote its own page and published it:
    - on Claude, the hive's provider, in 23 s;
    - after a `config-set` with `provider: openai`, on gpt-5.5 with the tenant's OpenAI key, in
      22 s. OpenAI accepted the new tool definitions;
    - both links were served from outside;
    - no line with `/public/` reached the agent's journal (A-95);
    - `llm_usage` charged both to `pruebas` by provider: Anthropic shows cache writes and reads
      (6,376 each), and OpenAI read 2,560 from its cache;
    - the agent was set back to the hive's provider.
  - **T9:**
    - a `config-set` with `behavior.capabilities` was refused with a clear message;
    - `AI.pruebas`'s saved config never had the key, so the boot conversion was not exercised
      live (it is covered by tests).
  - **The frontdesk:** RUNNING, HEALTHY.
  - **Health:** `failed=0` on all 4. The one denial, on worker1, is the probe's `CONFIG_GET`.
- **Cleanup:** none needed. The test tools stay: the tenant `pruebas`, its keys and
  `AI.pruebas@worker1`.
- **Upgrading another hive:**
  - nothing by hand;
  - before upgrading, check that no AI node has a saved `behavior.base_url` without
    `behavior.provider`: since 0.1.86, such a node boots FAILED_CONFIG.
- **Rollback:**
  - snapshot `pre-tools-0-1-86`;
  - or `apt install fluxbee=0.1.85` plus the core update of the spokes. Then put each AI node's
    config back into the 0.1.85 shape, because 0.1.85 refuses the fields 0.1.86 writes
    (`provider`, `multimodal`): restore `config.json.pre-0.1.86` where the conversion made one, or
    send a `config-set` without them (the AI runbook, "Rolling back to 0.1.85").

## 0.1.85 — The edge proxies the web host to io.web, a new managed node on the motherbee behind mesh mTLS; web_status; core IO runtimes pinned (io-web-v2 §11.2; A-89, A-90, A-91, A-92)

- **Date:** 2026-10-24 (ART) · **Previous version:** 0.1.84 · **Commits:**
  - `5f796be`: the SDK's mesh module, the refactor of step 1;
  - `95bacd3`: the release.
- **Scope:** the motherbee and the three spokes (core update), and one new node started by the
  operator: `IO.web@motherbee` in the root tenant.
- **Design:** [io-web-v2](../docs/io-web-v2.md) §11.2. Built by agents in two steps:
  - step 1, the SDK's mesh module;
  - step 2, in parallel: the proxy, io.web, and `web_status`.

  One code review round (security, operations); its fixes are FINDINGS A-92. The deferred ones are
  A-89 and A-90.
- **What changed:**
  - **The SDK (`fluxbee_sdk::mesh`, feature `mesh-tls`):**
    - the mesh's TLS material and the hive registry readers, one copy for the router, SY.admin,
      SY.orchestrator, SY.edge, SY.vault and io.web, each keeping its own reading;
    - a client config that verifies the server by name;
    - a server config that admits only some registry roles. It keeps no sessions, so a resumed
      session never skips the check (A-92).
  - **SY.edge:** a declared web host is proxied to io.web.
    - **The upstream:** the host the ingress's router dials, on port 9200, over mesh mTLS, with the
      server verified as `motherbee`.
    - **Requests:** GET and HEAD only, with no body (405, 400). The framing is rebuilt and
      hop-by-hop headers are stripped both ways. The edge sets its own forwarding headers and
      request id.
    - **Errors and limits:**
      - a fixed 502 with the request id;
      - a 503 cap (`edge.web_max_inflight`, 128);
      - 5 s connect and 30 s head timeouts.
    - **Logs:** one line per request, with the path and never the query. Failures are logged
      aggregated.
    - A web host on another port gets 404.
    - The `EDGE_WEB_ROUTES` answer carries the upstream's state.
  - **io.web (`nodes/io/io-web`):** a managed IO runtime.
    - It runs only as `IO.web@motherbee` in the root tenant, with a fail-closed self-check.
    - `base-nodes.json` lists it with `boot: false`.
    - It serves HTTPS on `0.0.0.0:9200` with the motherbee's mesh leaf.
    - It admits only a client whose hive the registry lists as a connected ingress, checked in the
      handshake and again after it. It cuts the connection when that changes.
    - It serves a minimal page under a strict CSP and `/healthz`; everything else is 404.
  - **SY.orchestrator:** a core IO runtime runs only in the root tenant (`TENANT_ROOT_REQUIRED`)
    and only as `IO.<x>@motherbee` (`CORE_IO_NAME_MISMATCH`).
  - **SY.admin:** `GET /web/status` (`web_status`) shows:
    - the host;
    - the certificate's names;
    - the route the edge acknowledged, and its upstream;
    - io.web's state.

    Archi reads it, and `ops.py health` prints a `web` line.
- **Deploy:**
  - build of `95bacd3` in 26 min, publish in 12 s, deploy in 223 s;
  - snapshots `pre-web-0-1-85` on the 4 VMs (`pre-acme-0-1-82` was deleted);
  - 0.1.85 on all 4 (core manifest `626fb3e7971bfe3e`), with health clean.
  - `web_status` right after: the route acknowledged, the upstream `ready`, io.web `absent`. The
    Archi host answered the fixed 502 `UPSTREAM_UNAVAILABLE` with its request id.
- **The one step by hand** (approved by the operator): `run_node IO.web@motherbee` in the root
  tenant. The node came up RUNNING, HEALTHY and CONFIGURED, serving on `0.0.0.0:9200` and
  admitting `ingress`.
- **Gate:**
  - **From outside, strict TLS:**
    - `https://archi.<id>/` serves io.web's page with:
      - `default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`;
      - `nosniff`, `no-referrer` and `no-store`;
      - no version.
    - HEAD 200 with no body. `/healthz` 200. An unknown path and `/e/` on the Archi host: io.web's
      404.
    - At the edge: POST, OPTIONS and TRACE got 405 with `Allow: GET, HEAD`; a GET with a body and
      an Upgrade got 400.
    - Spoofed forwarding headers never reached io.web.
  - **From inside the mesh,** dialling `10.10.10.10:9200` with each hive's leaf:

    | Hive | Result |
    |---|---|
    | ingress1 | admitted, 200 |
    | ingress1 with no certificate | refused (`certificate_required`) |
    | worker1 | refused in the handshake (`access_denied`) |
    | worker1 with no certificate | refused |
    | egress1 | refused (`access_denied`) |

    io.web logged each refusal with the hive and its role.
  - **Logs:** a query (`?code=…`) appeared in neither the edge's log nor io.web's. One request id
    is on both sides. The edge logs the client's real address. The proxy took 7–12 ms.
  - **The door (0.1.84's checks):**
    - the Archi host, with `:443` or uppercase and a trailing dot, is proxied; with `:8443`, 404;
    - SNI for the main name with Host for Archi goes to Archi, because routing is per request;
    - the main name, the IP literal, an unknown host and two Hosts answer as in 0.1.84;
    - the certificate keeps its two names.
  - **Restarts:**
    - after an edge restart, the route came back from disk, and the Archi host answered 200 at
      once;
    - after `restart_node IO.web`, there was no gap seen from outside.
  - **Refusals:** each of these got `CORE_IO_NAME_MISMATCH`, and IO.blob and IO.cloud were
    untouched:
    - io.web as `IO.webx` in the root tenant;
    - io.web as `IO.webt` in a test tenant;
    - io.web under the name `IO.blob@motherbee`.
  - **Regressions:**
    - Cloud's relay through `/e/` works;
    - a page published from worker1 (A-81's test) was `ok` after 12.8 s, served from outside, 404
      after the unpublish, and nothing was left on any hive.
  - **Health:** `failed=0`, `denials=0` on all 4. The ops line reads
    `web host=archi.… route=acknowledged upstream=ready io.web=running`.
  - **Covered by tests only:**
    - `TENANT_ROOT_REQUIRED`: a non-canonical name is refused first;
    - the connection cut on a registry change;
    - the 503 cap;
    - the TLS resumption fix.
- **Cleanup:**
  - the gate tenants and nodes (deleted and purged);
  - the publication (unpublished);
  - the probe scripts on the hives.
- **Upgrading another hive:**
  - the release needs nothing by hand;
  - where the web door is wanted (after 0.1.84's `web:` and two-name certificate), the operator
    starts `IO.web@motherbee` in the root tenant once;
  - the ingress must reach the motherbee's 9200/tcp. No host rule is written today.
  - A factory reset removes io.web, which is `boot: false`.
- **Rollback:**
  - snapshot `pre-web-0-1-85`;
  - or kill and purge `IO.web@motherbee`, then `apt install fluxbee=0.1.84` plus the core update
    of the spokes. Nothing on disk changes format: the SDK refactor keeps every path. 0.1.84's
    edge answers 404 for the web host.

## 0.1.84 — The door ready for a second host: one certificate for two names, the edge serves by host, the door's own fixes, a worker's agent can publish (io-web-v2 §11.1; A-87, A-86, A-88, A-83, A-82, A-81)

- **Date:** 2026-10-23 (ART) · **Previous version:** 0.1.83 · **Commit:** `5be9e7b`.
- **Scope:**
  - motherbee and the three spokes (core update);
  - the motherbee's `hive.yaml` (`web: {archi: true}`);
  - the edge's certificate (two names).
- **Design:** [io-web-v2](../docs/io-web-v2.md) §11.1. One review for fit and one for security split
  stage 1 in two: this release (the door) and 0.1.85 (the proxy and io.web).
- **What changed:**
  - **SY.admin:**
    - `web: {archi: true}` declares the Archi host: `archi.` + the host of
      `admin.public_base_url`, logged at boot;
    - the ACME client takes a list of names:
      - one order carries them all, and each authorization is armed, settled and cleared;
      - coverage and chain checks run per name;
      - 0.1.83's `acme.json` reads as a one-name list;
      - the main name is ordered alone only when that loses nothing, or once the renewal is 48 h
        overdue;
    - `EDGE_WEB_ROUTES` goes to the edge every 60 s on its `tls` channel. It is admitted only from
      `SY.admin@motherbee`: router lists, `PROTECTED_SYSTEM_ACTIONS` (31), the policy, both wasm
      files;
    - **A-81:** `publish_artifact`:
      - asks the producer's hive to scan its blob folder;
      - waits up to 30 s, outside the publication lock, for the file in the motherbee's `active/`;
      - runs under a 110 s budget;
      - releases what was curated on any curate error.
  - **SY.edge:**
    - **Routing by host:** one canonical host per request.
      - A declared web host gets 404 on every path; so does an unknown host.
      - The door's routes answer only the main name, an IP literal or no Host.
      - Two Hosts get 400.
      - The web routes are kept in `web_routes.json`, so a restart serves them at once.
    - **A-87:** hyper's HTTP/1 builder, with a header-read timeout that starts before the first
      byte. No protocol sniffing, no HTTP/2.
    - **A-86:**
      - every connection's stream is paced: 30 s of idle credit, then 1 KiB/s, with reads
        charged only while a body is read;
      - a 30 s body deadline;
      - `TCP_NOTSENT_LOWAT` 16 KiB;
      - per-IP caps: `edge.max_connections_per_ip` (256) and `edge.public_max_inflight_per_ip`
        (8); 0 turns either off;
      - the header buffer is capped at 64 KiB (431 beyond).
    - **A-88:** one fixed error body per code; the detail goes only to the log.
    - **A-83:** a response that cannot be built is a 500. Every response gets the security headers.
    - **A-82:** only `SHA-256(key)` is stored or logged. `publications.json` is converted once at
      start, and the conversion is reported.
  - **SY.storage:** `turns.ctx`/`seq` and their index are dropped (step 2 of 2), behind a schema
    check, and the step is logged.
  - **lab/ops.py:** the edge-cert line lists the certificate's names.
- **Deploy:**
  - build of `5be9e7b` in 37 min; deploy in 266 s;
  - snapshots `pre-door-0-1-84` on the 4 VMs (`pre-batch-0-1-81` was deleted);
  - 0.1.84 on all 4 (core manifest `2e04bae5f1eeb8d4`); health clean;
  - SY.admin's first web-route push found the edge restarting (`NODE_NOT_FOUND`). The next, 60 s
    later, was ACKed.
- **The transition** (approved by the operator; the four steps of io-web-v2 §11.1):
  1. `web:` with `archi: true` added to the motherbee's `hive.yaml` (the copy before:
     `/root/hive.yaml.bak-pre-0184-web`);
  2. SY.admin restarted, logged the Archi host and asked ACME for both names;
  3. `POST /acme/test`: both names issued and checked against Let's Encrypt staging;
  4. `POST /acme/renew`: the new certificate was installed at 23:19 UTC: Let's Encrypt YE2, both
     names, valid until 2027-01-21.
- **Gate:**
  - **The certificate:**
    - from outside, both names verify against one certificate;
    - `/acme/status` and `ops health` list both names, with no condition.
  - **Hosts, from outside:**
    - the main name: `/healthz` 200 with `nosniff`; `/e/<unknown>` 404 with a fixed body;
    - the Archi host answers 404 on `/healthz`, `/e/` and `/public/`, including:
      - `:443`, uppercase with a trailing dot, and `:8443`;
      - SNI for the main name with Host for the Archi host;
    - an unknown host: 404; the IP literal: 200; two Hosts: 400;
    - after an edge restart, the route came back from disk (`web_hosts=1`), and the Archi host
      still answered 404.
  - **Connections:** a connection with one byte was closed after 30.2 s; an h2 preface, in 0.3 s.
  - **Cloud through the door:** a relay (`list_tenants`) works.
  - **A-81, with a mesh client instead of the model:**
    - why no model: a tenant's agents read only their own tenant's AI key, and the test tenant had
      none;
    - setup: a test agent's identity on worker1 (`AI.gate0184@worker1`), its node stopped and its
      ILK kept;
    - the page was written into worker1's `active/`, and `publish_artifact` was sent 1 s later;
    - result: `ok` after 12.8 s and 13.3 s (two runs), with no warning in SY.admin's log;
    - from outside, the link served the page with the sandbox CSP, `nosniff` and `no-store`;
    - after the unpublish, it answered 404.
  - **A-82:** the edge logs `key_sha256`; no raw key is left in its state directory.
  - **Storage:** the columns and their index are gone; SY.storage logged the step.
  - **Health:** `failed=0` and `denials=0` on all 4 from the deploy through the gate.
  - **Covered by tests only:**
    - the per-IP caps, the pacing and the body deadline;
    - a 500 with the security headers (A-83) and an `/e/` reply failure (A-88);
    - the fallback to the main name alone.
- **Cleanup:**
  - the gate tenants, nodes and ILKs (purged by the teardown);
  - the publications (unpublished);
  - the gate blobs, gone from every hive within 90 s.
- **Upgrading another hive:**
  - the release needs nothing by hand;
  - a motherbee that wants the Archi host:
    1. needs `acme:` on, and DNS for `archi.<id>` must resolve first (HTTP-01 for that name);
    2. then runs the four steps above.
- **Rollback:**
  - snapshot `pre-door-0-1-84`;
  - or `apt install fluxbee=0.1.83` plus the core update of the spokes:
    - 0.1.83 neither writes nor needs the dropped columns, and it ignores `web:`;
    - 0.1.83's SY.admin cannot read the new `acme.json`. It sets the file aside and treats the
      served certificate as hand-seeded until a `/acme/renew`;
    - the edge's `publications.json` was converted to key hashes, which 0.1.83 cannot use. PROD
      had no live publication;
  - back to 0.1.82, also `ALTER TABLE turns ADD COLUMN ctx TEXT, ADD COLUMN seq BIGINT;`.

## 0.1.83 — Control-path hardening: the admin answers mesh callers by an allowlist, orchestrator actions get a direction, the router binds WAN senders, SSH on the DMZ hosts, only a vouched email identifies a person (A-62, A-20, A-22, A-42, A-46 stage 4, A-55, A-79)

- **Date:** 2026-10-22 (ART) · **Previous version:** 0.1.82 · **Commit:** `5fb8dab`.
- **Scope:** motherbee and the three spokes (core update). No step by hand: the code, the baked
  policy or the orchestrator applies everything (the operator's rule for this batch).
- **Design:** docs/control-path-hardening-v1.md. One design review (security, operations) and one
  code review round (security, operations, correctness); the code review's fixes are FINDINGS A-79.
- **What changed:**
  - **SY.admin (A-62):**
    - mesh callers by an allowlist (`mesh_access`, `authorize_mesh_caller`); a caller on an ingress
      or egress hive gets nothing; roles come from the registry and fail closed;
    - SY.wf-rules publishes only inline workflow packages, never `wf.engine`, with only the fields
      it sends, and `sync_hint` waits at most 5 s;
    - node teardown is one `teardown_node`: the secrets first, then the ILK, each step reported;
    - the WF ownership check reads shared memory (503 `INVENTORY_UNAVAILABLE` when it cannot);
    - `kill_node` and `remove_node_instance` wait 180 s.
  - **Orchestrator actions (A-20):** rule (3) admits mutations only from `SY.orchestrator@motherbee`
    and reads also from the same hive; rule (4) never admits the spoke-only handshake on the
    motherbee; the orchestrator answers admin messages only from its own hive's SY.admin.
  - **SY.identity:** an orchestrator acts only on its own hive's nodes; canonical ILK ids at every
    boundary, replicas included; a channel another ILK holds is never taken (`ICH_CONFLICT`); a
    node registers no email.
  - **The router (A-22, A-79):**
    - sockets 0600 with a peer-uid check;
    - a WAN sender resolves only in its peer's bucket;
    - a UUID two hives list resolves to neither;
    - from hive P's LSA: only nodes `*@P`, routes for P's destinations, taps on P's traffic to P's
      nodes;
    - a tap copy carries no SYSTEM authority;
    - sibling regions must belong to the router's user.
  - **SSH (A-46 stage 4 minimal, A-55):**
    - `table inet fluxbee_ingress`: SSH only from the motherbee and `ingress.ssh_allow_from`,
      applied at boot and on reconcile with a boot unit, never narrowed on an input failure;
    - the join checks its own SSH source before installing anything;
    - `egress.ssh_from_wan` (default true);
    - no ufw or firewalld writes; an active one raises `host_firewall_active`.
  - **A-42:** the frontdesk takes handoffs only from IO nodes with the `frontdesk_handoff` mark
    (io.api, io.cloud and the first-contact gate stamp it); a typed email is `claimed_email`, never
    indexed or merged; the vouched email survives the conversation; the registration tool acts only
    on the message's own ILK.
  - **Debt:** one rcgen (0.14) and x509-parser (0.18), checked against a CA made by 0.1.82;
    `turns.ctx`/`seq` no longer written, now nullable (dropped next release); stale IO docs; dead
    code.
- **Deploy:**
  - build of `5fb8dab` in 22 min, publish in 12 s, deploy in 251 s;
  - snapshots `pre-hardening-0-1-83` on the 4 VMs (`pre-batch-0-1-80` was deleted);
  - 0.1.83 on all 4 (core hash `fc2edab98e4b9740`): the spokes, still on 0.1.82, took the core
    update over the mesh from the motherbee, whose certificate rcgen 0.14 issued; health clean.
- **Gate:**
  - **SSH on ingress1:** its table holds loopback, SSH from 10.10.10.10 and a drop of every other
    SSH; the boot unit is enabled; one "applied" line. 10.10.10.30:22 opens from the motherbee and
    loopback, not from worker1 or egress1. egress1's SSH is unchanged.
  - **Refusals, from a throwaway mesh client:**
    - a node on ingress1 asking SY.admin: `UNAUTHORIZED` (*an ingress or egress hive … may ask
      SY.admin for nothing*);
    - a `WF.*` node on worker1 asking for `inventory`: `UNAUTHORIZED`;
    - a SPAWN from ingress1 to the motherbee: dropped by the router (the one denial in health);
    - an admin read from ingress1 to the motherbee's orchestrator: `FORBIDDEN`;
    - an ILK_PURGE from a non-orchestrator: `UNAUTHORIZED_REGISTRAR`.
  - **What keeps working:**
    - a node's life on worker1: an active test tenant, `AI.gate0183@worker1` launched, a secret
      dedicated to it, `kill_node` with `purge_instance` reporting `vault_secrets_purged` 1 of 1
      and the ILK marked and purged (`teardown_node` from `SY.orchestrator@worker1`);
    - Archi relaying `inventory` to SY.admin (an SCMD, no model): 4 hives, 32 nodes;
    - Cloud through the public door: a relay (`list_tenants`), `create_tenant`, and
      `register_human` complete with the email in `identification.email`;
    - a workflow deployed on worker1 through SY.wf-rules in 5 s, then deleted;
    - `POST /acme/test`: issued and checked against Let's Encrypt staging.
  - **Logs:** SY.admin refused only the two probes; the orchestrator's inbox only the probe.
    SY.admin logged each hive's role at start.
  - **Router:** three `foreign_source` warnings (7 frames), all transient: RT.gateway@egress1 at its
    restart and the gate node on worker1 at its connect and purge, before or after its hive's LSA
    listed it. 0.1.82 dropped the same frames without a log line.
  - **Storage:** `turns.ctx`/`seq` nullable, none written since the deploy; turns keep landing.
  - **Health:** `failed=0` on all 4; the motherbee's one denial is the SPAWN probe.
  - **Covered by tests only:** a forged WAN sender, UUID shadowing, taps (PROD has none), the
    sibling-region check, SY.identity's scope from a real orchestrator name, the join pre-checks (no
    join was run), the conversation's claimed email, channel conflicts.
- **Cleanup:** the gate tenants (deleted and purged), the gate node and its secret (purged by the
  teardown), the gate person (deleted and purged), the gate workflow (deleted).
- **Upgrading another hive:**
  - nothing by hand;
  - a hive whose registry entry lacks `hive_id`, `role` or `status: connected`, or whose join is not
    `done`, is refused every mesh request at SY.admin (each role is logged at start);
  - where operators reach the ingress by SSH from another network, list it in add_hive's
    `ssh_allow_from`, or set `ingress.ssh_allow_from` in the ingress's `hive.yaml` and restart its
    orchestrator.
- **Rollback:**
  - snapshot `pre-hardening-0-1-83`;
  - or `apt install fluxbee=0.1.82` plus the core update of the spokes (0.1.82 accepts the nullable
    `ctx`/`seq`). The ingress keeps `fluxbee_ingress` and its boot unit, which 0.1.82 does not
    know: SSH stays limited to the motherbee until `fluxbee-ingress-nft.service` is disabled and
    the table deleted.

## 0.1.82 — The edge's certificate from Let's Encrypt, issued and renewed by SY.admin; the vault's notice reaches the edge (O6, A-78, PB-1)

- **Date:** 2026-10-21 (ART) · **Previous version:** 0.1.81 · **Commit:** `d2980cf`.
- **Scope:**
  - motherbee and the three spokes (core update);
  - `hive.yaml` on the motherbee (`acme:` and `egress.reachability_url`) and on egress1
    (`egress.reachability_url`);
  - the certificate PROD's edge serves.
- **Design:** docs/acme-edge-certificates-v1.md. It was reviewed once as a design (protocol and
  security, fit with the code, operations) and once as code (the ACME client, the edge, the
  router/vault/orchestrator).
- **What changed:**
  - **SY.admin is the ACME client.**
    - It uses TLS-ALPN-01 on the edge's :443 with instant-acme 0.8.5.
    - The name is the host of `admin.public_base_url`. The CA, Let's Encrypt by default, comes
      from `acme:`.
    - Renewals happen in the ARI window, else at two thirds of the certificate's lifetime.
    - Alerts are relative to the lifetime. New actions: `GET /acme/status` and `POST /acme/renew`,
      `/acme/test`, `/acme/reload`.
  - **SY.edge:**
    - It mints the challenge certificate itself.
    - It swaps its certificate without restarting (PB-1).
    - It waits for a certificate instead of crash-looping when the secret is missing.
    - It answers `EDGE_TLS_STATUS`, `EDGE_TLS_RELOAD` and `EDGE_ACME_CHALLENGE` from
      `SY.admin@motherbee` only.
    - The public door times out silent clients and caps connections.
  - **A-78:** SY.vault sends its change notice to an owning edge as a unicast, and the router's
    edge gate admits only that. An edge can no longer read a general secret.
  - **Smaller changes:**
    - the certificate alerts go to the drift alerts;
    - the egress' internet probe takes its URL from `hive.yaml` (no `fluxbee.ai` in code);
    - the `.deb` depends on ca-certificates and builds `--locked`.
- **Before the deploy** (the operator's approval, 2026-10-21):
  - `egress.reachability_url: "https://fluxbee.ai"` was added to the motherbee's and egress1's
    `hive.yaml`. Backups are `hive.yaml.bak-pre-0182-reachability`.
  - The chain the edge served (the Sectigo wildcard) was parsed with the new boot checks: 4 of 4
    certificates, valid, 113 days left, the name covered.
- **Deploy:**
  - build of `d2980cf` in 38 min (new crates), deploy in 235 s;
  - snapshots `pre-acme-0-1-82` on the 4 VMs (`pre-stream3-0-1-79` was deleted);
  - 0.1.82 on all 4, health clean.
  - The edge came up serving the wildcard from the vault (version 1), never `awaiting`.
- **Gate:**
  - **A-78:** a throwaway secret owned by the edge was put and deleted. Both notices reached the
    edge (`VAULT_SECRET_CHANGED … origin=SY.vault@motherbee`), the first ever seen there. Nothing
    reloaded.
  - **`acme:` added** to the motherbee's `hive.yaml` (`terms_agreed: true`, accepted by the
    operator; no contact), and SY.admin restarted while idle. Backup:
    `hive.yaml.bak-pre-0182-acme`. Nothing was replaced, because the wildcard is hand-seeded.
  - **`POST /acme/test`:** Let's Encrypt staging validated the challenge from the internet and
    issued a certificate in 8 s. The only refusal was the expected one (`UnknownIssuer`, staging
    root). Nothing was installed.
  - **`POST /acme/renew`:** Let's Encrypt production issued and the edge installed in 7 s.
    - Issuer `Let's Encrypt YE2`, SAN `hive-k3m9x7q2.fluxbee.ai`.
    - Valid 2026-10-22 → 2027-01-20 (90 days, `classic`); fingerprint `3ebadf52…`.
    - The vault holds it as version 2, with the wildcard as the previous version.
  - **Verified strictly** (Python `ssl`, system roots, no AIA fetching) from inside (motherbee →
    ingress1) and from outside through the public name: TLS 1.3, chain verified, `/healthz` 200.
  - **Rollback, both ways (PB-1 + A-78 live):** a vault rollback put the wildcard back, and the edge
    swapped to it on the vault's notice alone, without a restart and without `/acme/reload`. A
    second rollback brought Let's Encrypt back the same way. sy-edge did not restart after the
    deploy.
  - **Conditions:** the 2-min check landed in the 9 s the wildcard was served. It opened
    `foreign_certificate`, and `POST /acme/reload` re-evaluated and closed it. The drift alerts
    hold one `open` and one `closed` row.
  - **Health:**
    - `failed=0 denials=0 srcdrops=0 streamlost=0 usagelost=0 usagerej=0` on all 4;
    - `edge-cert acme=on conditions=none`.
  - **Internet probe:** egress1 reports `internet_reachable=true` against the URL. The motherbee
    probes at boot (`true`) and reports `not_probed` on its ticks, where 0.1.81 logged `false`.
- **Next automatic renewal:** in Let's Encrypt's ARI window, around day 60 (late December 2026).
  The Sectigo wildcard expires 2027-02-12. After two renewals it is no longer in the vault; its
  original files stay with the operator.
- **Upgrading another hive:** none of this turns on by itself.
  - Without `acme:` the edge serves its seeded certificate and SY.admin only watches its expiry.
  - Without `egress.reachability_url` the internet probe reports `not_configured`.
  - The shipped example's old `ipv6: "block"` was a value the code refuses; it is `"blocked"`.
- **Rollback:**
  - The certificate: `POST /hives/motherbee/vault/secrets/edge_tls_fluxbee_ai/rollback`. The edge
    swaps on the notice; `POST /acme/reload` re-evaluates.
  - The code: snapshot `pre-acme-0-1-82`, or `apt install fluxbee=0.1.81` plus the core update of
    the spokes. 0.1.81 cannot renew, so roll the certificate back first.

## 0.1.81 — The open findings closed: agent assets reach the workers, a removed hive is cut off, the Go SDK matches Rust, Archi reconciles what exists (A-72, A-48, A-75, A-24, A-76, A-1, A-2)

- **Date:** 2026-10-20 (ART) · **Previous version:** 0.1.80 · **Commits:** `4e9f91e` (fixes and
  cleanup) and `99b0d2a` (format only; CI now checks the Rust and Go format).
- **Scope:** motherbee + the three spokes (core update), plus the one-time steps below.
- **What changed** (each finding verified in the code first, one review round of seven reviewers):
  - **A-72:** a worker-only Syncthing folder, `fluxbee-dist-agent-assets`, carries
    `blob/agent-assets` from the motherbee (send-only) to every worker (receive-only); hives that
    joined earlier get it with their core update. ai.generic reads an asset as a regular file only,
    and a configured agent keeps its definition until a complete new one is readable.
  - **A-48:** the hive registry is the one list. The motherbee's router rejects a hive it does not
    list (`HIVE_NOT_REGISTERED`) and closes a removed hive's session within ~10 s; `remove_hive`
    deletes the hive's identity key; a motherbee boot sweep deletes keys with no registry entry;
    `wan.authorized_hives` is gone.
  - **A-75:** the Go SDK matches Rust (no family-wide reply shapes, system RPCs with TTL 16, an
    oversize frame refused without dropping the link); SY.timer no longer answers router notices.
  - **A-76:** SY.admin's status and per-hive inventory routes, the vault refusal for a hive without
    a vault, vault metadata, the OPA check as a pure read, node status of SY.* nodes, the router
    and identity regions after a restart or a limit change, the AI nodes' dead config fields,
    handler retries and call timeouts, the frontdesk's status. One router per hive is the supported
    topology.
  - **A-1 / A-2:** the egress join reads `edge_ip` from the host and the egress checks its LAN leg,
    forwarding and IPv6 block; a join that leaves password logins on says so.
  - **A-24 and what the review found in Archi:** its snapshot reads each producer's real answer,
    workflows are owned and pinned like the OPA policy, routes and VPN rules are validated like
    SY.config.routes, the executor waits for a hive's join (FINDINGS A-77).
  - **Cleanup:** SY.storage's base schema (`turns.message` NOT NULL, no `storage_migrations`), the
    one-time transitions and the dead tools and scripts removed, `scripts/install.sh` a wrapper of
    the `.deb`.
- **Before the deploy** (the operator's approval of 2026-10-20):
  - **egress1:** `edge_ip` 10.10.10.1 → 10.10.10.40 in `/etc/fluxbee/hive.yaml` (backup
    `hive.yaml.bak-pre-0181-edge-ip`) and in the motherbee's `hives/egress1/info.yaml` (backup
    `/root/egress1-info.yaml.bak-pre-0181-edge-ip`). eth0 carries 10.10.10.40; .1 was add_hive's
    guess ("first usable IP"), and 0.1.81 stops an egress whose `edge_ip` is not on its LAN leg.
    Nothing routed by that value.
  - **Database:** `ALTER TABLE turns ALTER COLUMN message SET NOT NULL;` (0 rows without one).
  - **On 2026-10-19,** with the operator's approval: the three empty dead tables of the 0.1.80 entry
    (`events`, `memory_items`, `storage_reactivation_applied`) were dropped.
- **Deploy:**
  - build of `99b0d2a` in 23 min, publish in 22 s, deploy in 240 s;
  - snapshots `pre-batch-0-1-81` on the 4 VMs (`pre-stream2b-0-1-78` was deleted);
  - 0.1.81 on all 4 (core hash `8157e8245c5de4a1`), health clean.
- **After the deploy** (same approval): `DROP TABLE storage_migrations;`, and the stored ufw rules that
  no longer apply deleted (22000/udp and 21027/udp on the 4 hosts, 22000/tcp on the spokes). ufw is
  inactive on all four, so nothing changed live.
- **Gate:**
  - **A-48:**
    - at boot the motherbee deleted `identity/keys/worker2.key` (worker2 was removed on 2026-10-06):
      `deleted the identity key of a hive the registry does not have (A-48)`;
    - VM 104 (fb-worker2), which kept its certificate and its key, was powered on. Its router dialed
      the motherbee and got `wan peer rejected: hive not registered … reason=HIVE_NOT_REGISTERED`;
      its identity sync got `auth: no key for hive 'worker2'`. It runs its old version, so it
      retried every 5 s (a 0.1.81 spoke backs off to 60 s). VM 104 was powered off again.
  - **A-72:**
    - the motherbee declares `fluxbee-dist-agent-assets` (send-only, shared with worker1); worker1
      got it with its core update (receive-only, owned by the Syncthing user) with the 4 existing
      assets; ingress1 and egress1 do not have it;
    - a role created through Archi (`POST /api/agent-assets`; mode 0644, no temp file left) reached
      worker1 in ~14 s with its hash;
    - an agent in a test tenant on worker1 (`AI.gate0181@worker1`, `claude-sonnet-5-5`) started with
      `definition_state: empty` and read `composed`, role loaded, after `set_ilk_definition`.
  - **AI config:** a `CONFIG_SET` with `behavior.instructions` answered `invalid_config` (*unknown
    field `instructions`*); nothing changed.
  - **A-1:** egress1 reconciles with `edge_ip=10.10.10.40 … lan_leg_ok=true ipv4_forwarding=true
    ipv6_blocked=true`.
  - **A-76, SY.admin and SY.vault:**
    - `GET /hives/motherbee/status` answers ok (always 500 before);
    - `GET /hives/worker1/inventory/summary` gives worker1's totals (8 nodes: RT 1, SY 7);
    - `GET /hives/worker1/vault/secrets` answers `INVALID_REQUEST` (*SY.vault runs only on the
      motherbee…*);
    - the metadata of an ILK-bound secret answers ok (`UNAUTHORIZED` before);
    - node status reads RUNNING/HEALTHY for SY.storage, RT.gateway, SY.frontdesk.gov, SY.architect
      and SY.identity@worker1.
  - **OPA check:** a valid policy answered ok (hash, 74 ms, 138,850-byte wasm, no version); an
    invalid one `COMPILE_ERROR`; the 4 hives stayed at v0, in sync. Archi, asked to check a policy
    with its read tool, answered that it compiles; nothing was staged.
  - **Archi:** its 4 cookbooks were seeded (empty in PROD until now); the Messages API answers.
  - **Storage:** usage records and turns keep landing (Archi's 2 calls; 33 new turns).
  - **Health:** `failed=0 denials=0 srcdrops=0 streamlost=0 usagelost=0 usagerej=0` on all 4.
  - **Covered by tests only:** the identity region recreate, the executor's wait for a join, A-2's
    warning, the Go SDK's oversize refusal and SY.timer's notice guard, Archi's reconcile of nodes,
    routes, VPN rules and workflows.
- **Cleanup:** the gate node (killed with `purge_instance`), its tenant (deleted and purged), the
  gate role (deleted through Archi; the deletion reached worker1), Archi's gate session.
- **Upgrading another hive:**
  - an ai.generic `config.json` written by 0.1.80 or earlier carries keys 0.1.81 refuses
    (`behavior.instructions`, the old `runtime.*`): that node boots `FAILED_CONFIG` until it gets a
    new `CONFIG_SET`, or is purged and created again (PROD had none);
  - a database created before 0.1.81 needs the `ALTER` above, and the `DROP` after the upgrade;
  - an egress whose `edge_ip` is not on its `lan_iface` stops at boot: fix `edge_ip` first;
  - a hive older than 0.1.78 still upgrades through 0.1.79.
- **Rollback:** snapshot `pre-batch-0-1-81`, or `apt install fluxbee=0.1.80` plus the core update of
  the spokes (0.1.80 accepts the NOT NULL column and creates `storage_migrations` again).

## 0.1.80 — The repo cleanup, Archi's read tool reads only, a refused route changes nothing, the message being answered is never cut (A-73, A-74, A-71)

- **Date:** 2026-10-17 (ART) · **Previous version:** 0.1.79 · **Commit:** `58db83a`, on top of the
  cleanup pass (`41decbd`, `9b50b96`, `c70be7e`).
- **Scope:** motherbee + the three spokes (core update).
- **What changed:**
  - **The cleanup pass** (the operator's catalog, approved by group):
    - about 20,600 lines out;
    - the one-time boot transitions of 0.1.75–0.1.78, the diags and scripts A-68 left broken,
      40 old lab scripts, the cognition test nodes;
    - dead tables, fields and functions, the SDK's blob confirm, stray files;
    - the scaffold's templates now write what the runtimes accept.

    The docs were rewritten from the code (docs/README.md is the index) and the repo is in
    English.
  - **A-73:** Archi's read tool (`fluxbee_system_get`) runs reads only. Every change goes through
    the plan compiler and the operator's confirmation.
  - **A-74:** SY.config.routes commits a change only when the config region accepts it. It
    answers every refusal and saves the file through a rename.
  - **A-71:** the AI SDK no longer cuts the message being answered at 1,200 characters. That cut
    lost an agent's message behind its memory, and gave Archi's pipeline stages cut inputs since
    March. ai.generic keeps the person's own words in its immediate history.
- **Deploy:**
  - build of `58db83a` in 30 min, publish in 12 s, deploy in 239 s;
  - snapshots `pre-batch-0-1-80` on the 4 VMs (`pre-stream2a-0-1-77` was deleted);
  - 0.1.80 on all 4, health clean.
- **Gate:**
  - **Boot without the transitions:**
    - every unit active, none failed;
    - SY.storage started with `storage_ready=true`, its schema in place;
    - Archi's handbook installed from `docs/archi/` (32,671 bytes) and the message log connected.
  - **A-74:**
    - `POST /hives/motherbee/routes` with action `TELEPORT` answered `422 INVALID_CONFIG`
      (*invalid action 'TELEPORT'*) in 0.01 s, and the route list stayed the same. Before, it
      answered nothing and blocked every later change;
    - a valid route then committed (config_version 19) and was deleted (20).
  - **A-71:** a message of about 3,000 characters to Archi, with the question at its end ("how
    much is 17 + 25? answer only with the number"), was answered `42`. Before, the cut lost the
    question.
  - **A-73:**
    - asked to start a node through `fluxbee_system_get` with `POST /hives/motherbee/nodes`, Archi
      got *"fluxbee_system_get only reads; run_node changes the system: stage it with
      fluxbee_plan_compiler"*;
    - no node was created.
  - **The data path after the cleanup,** with an agent in a test tenant, without a key:
    - its 4 turns reached `turns`;
    - cognition built its communication (1 thread, 10 contexts);
    - 6 cognition usage records were charged to the tenant (`agent`, verified, no duplicate
      provider ids);
    - `GET /hives/motherbee/usage` answered.
  - **Health:** `failed=0 denials=0 srcdrops=0 streamlost=0 usagelost=0 usagerej=0` on all 4.
  - **Not done:** the three empty dead tables (`events`, `memory_items`,
    `storage_reactivation_applied`, 0 rows each) are still in the PROD database. The code no
    longer creates them; dropping them was refused by the session's permission check and is left
    to the operator.
- **Cleanup:**
  - the test node with its identity, the tenant (deleted and purged), the memory region;
  - the gate's rows: 4 turns with their inbox rows, every cognition row, and the 6 usage records;
  - the two Archi sessions of the tests.

  Archi's three real calls during the tests stay as usage records of the root tenant.
- **Upgrading an older hive:** the cleanup removed the one-time boot transitions of
  0.1.75–0.1.78. A hive older than 0.1.78 (the Cloud team's DEV hive) upgrades through 0.1.79
  first (`apt install fluxbee=0.1.79`, boot, then the latest).
- **Rollback:** snapshot `pre-batch-0-1-80`, or `apt install fluxbee=0.1.79` plus the core update of
  the spokes.

## 0.1.79 — LLM usage records: every call to a model, charged to a tenant (A-70, stage 3)

- **Date:** 2026-10-17 (ART) · **Previous version:** 0.1.78 · **Commit:** `0bc85e3`.
  - Design: `docs/router-stream-v1.md` §7, discussed with the operator and reviewed once; how
    it was built is in §7.2–§7.6.
  - One code review with two angles (the accounting; trust, storage and
    reads), §7.9. The HIGH, the MEDIUM and three LOWs were fixed before the build; one stays with
    A-62.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **Every call to a model leaves a record** in SY.storage's `llm_usage` table, charged
    to a tenant:
    - ai.generic: its own tenant pays (`node`);
    - SY.frontdesk.gov: the case's tenant (`case`);
    - SY.cognition: the tenant of the communication's agent (`agent`);
    - Archi and the admin's executor: the root (`root`). `work` holds the session, the pipeline
      run, the solution and the stage, or the execution and who called.
  - **What is recorded:**
    - a 2xx response, before any check, with the counts normalized across OpenAI and
      Anthropic;
    - a call that its caller abandons, as `abandoned` and without tokens.
  - **The AI SDK:**
    - the two client factories require an `AiAccount`;
    - one recorder per process writes the records to the router stream (`storage.usage`);
    - the provider clients are now private to the SDK, and `ai_local_probe` was deleted.
  - **The router:**
    - a stream reserve for the usage records (1 MB and 128 queue slots), with their
      losses counted separately (`usagelost`);
    - the `SY.*` names that charge other tenants, only on the hives where they run, and
      case-insensitive;
    - `stream::append` waits until the node is connected to its router.
  - **SY.storage:**
    - checks every record;
    - for a node and for the cognition agent, takes the tenant from identity;
    - retries through the inbox, and the last attempt is stored unverified;
    - every 30 s, a WARN line with what was rejected, mismatched and unverified (`usagerej`).
  - **`USAGE_QUERY`,** only from SY.admin:
    - one query at a time, on its own connection, with a 15 s limit;
    - a period of up to 92 days, or a `since` cursor;
    - groupings from a closed list, and errors with a code.
  - **Reads:**
    - the admin's `get_llm_usage` (`GET /hives/motherbee/usage`), for the operator and Archi;
    - the Cloud op `get_usage`, with io.cloud's tenant and never the root
      (`docs/io-cloud-api.md` §5.5);
    - Archi reads it with `fluxbee_system_get`.
  - **`ops.py health`** adds `usagelost` and `usagerej`.
- **Deploy:**
  - build of `0bc85e3` in 23 min, publish in 14 s, deploy in 245 s;
  - snapshots `pre-stream3-0-1-79` on all 4 VMs (`pre-stream1b-0-1-76` was deleted);
  - 0.1.79 on all 4, clean health (`usagelost=0 usagerej=0`).
- **Gate (§7.7),** with an agent `AI.gate79@motherbee` in the tenant `pruebas`, with the operator's
  test keys:
  - **the table was born empty,** with the primary key and the six indexes, including the one for
    the per-tenant sync.
  - **The agent:**
    - without a role it answered with the "unconfigured" JSON. Those are still real calls: 2 records;
    - with a role and a skill from `agent-assets` in its ILK's definition, it answered for real;
    - its 5 records belong to `pruebas` as `node`, with the human's ILK as the counterparty.
  - **Cognition:**
    - 4 calls per exchange: the tagger and the summarizer, in each direction;
    - 18 records of `pruebas` as `agent`, with the agent's ILK as the counterparty.
  - **The frontdesk:**
    - a case with an ILK from `pruebas` charged `pruebas` (`case`);
    - a case with an unknown ILK charged the root (`fallback_root`).
  - **Archi:**
    - a chat turn charged the root, with the session and the `chat` stage in `work`;
    - it answered the usage question through the new route, with totals that add up.
  - **Provider ids:** every `ok` has one, none repeated.
  - **The probes:**
    - a record with the name of a `pruebas` node that claimed the root was stored with
      `pruebas`, counted as a mismatch;
    - `SY.gate79` was rejected (`foreign_sy`);
    - a lowercase `sy.frontdesk.gov` from worker1 was rejected by the router (the review's
      HIGH), and counted in worker1's `streamlost`;
    - a node without an ILK: the inbox retried for 10 minutes and the record kept the tenant it
      stated, `verified=false`, counted as unverified.
  - **`abandoned`:** with the cognition timeout at 50 ms, the tagger cut off in each direction
    and skipped the turn. 2 `abandoned` records remained, without tokens or id. The timeout went back to
    8000.
  - **The operator (`GET /hives/motherbee/usage`):**
    - by day, model, node and tenant;
    - the 26 records of `pruebas`, two at a time, in 13 pages;
    - the sync by `since` brought the 26 and ended with a null `next`;
    - five bad queries gave 400 `INVALID_REQUEST`. The two about cursors come with
      SY.storage's code.
  - **Cloud, through the public door:**
    - sees only `pruebas`: 26 records, of `agent`, `case` and `node`;
    - the root gives `TENANT_ROOT_PROTECTED`;
    - `all_tenants` or another tenant in `params` give `INVALID_REQUEST`.
  - **Gate health:**
    - `usagelost=0` on all 4;
    - `usagerej=3` on the motherbee and `streamlost=1` on worker1, exactly the probes;
    - afterwards, everything at 0.
  - **Seen while testing:** the metadata of a key of a non-root tenant gives `UNAUTHORIZED`
    through the admin's HTTP route (that route does not pass `tenant_id`). The delete does pass it.
- **Cleanup:**
  - the two test nodes with their identity: identity went back to 28 ILKs;
  - the motherbee's memory region and the gate's Archi session;
  - the gate's rows: 14 turns with their inbox, 288 envelopes, all the cognition rows, and the 31
    `llm_usage` records with their inbox. The table was born empty and was left empty;
  - the two keys of `pruebas` and the tenant, deleted and purged (the operator's request);
  - no mask was left.
- **Rollback:** snapshot `pre-stream3-0-1-79`, or `apt install fluxbee=0.1.78` plus the core-update
  of the spokes. The `llm_usage` table stays: 0.1.78 does not use it.

## 0.1.78 — NATS deleted and SY.policy gone (A-70, stage 2b)

- **Date:** 2026-10-17 (ART) · **Previous version:** 0.1.77 · **Commit:** `104fa65`.
  - Design: `docs/router-stream-v1.md` §6.3, and how it was built in §6.9.
  - One code review with two angles (the upgrade path, what the removal left behind):
    no HIGH. The MEDIUM and the LOWs were fixed before the build.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **NATS deleted:** about 9,000 lines went away:
    - the router's broker, the SDK client and `ClientConfig`;
    - the two diags and eight scripts;
    - SY.policy;
    - `meta.thread_seq`;
    - the ingestion of `storage.events`, `storage.items` and `storage.reactivation`, which never
      had a producer (their tables stay).
  - **SY.storage** reads nothing from NATS:
    - it answers `STORAGE_METRICS_GET` over the mesh, one inbox scan at a time;
    - the admin's `GET /config/storage/metrics` asks there, with the same answer as before.
  - **SY.orchestrator:**
    - joins its router and then waits for SY.storage's database over the mesh;
    - meanwhile the admin's commands get `ORCHESTRATOR_BOOTING`, so nobody is left
      waiting on a hive that is not ready (before, they failed because the orchestrator was not on the
      router yet).
  - **One-time transitions at startup, in the log:**
    - the broker's pending messages per subject;
    - then, `/var/lib/fluxbee/nats` and the `nats:` section of `hive.yaml`. This runs after
      the router runs the new binary, because the old one dumps its broker to disk when it stops;
    - SY.policy leaves every section of `hive.yaml`, and its unit and its binary are deleted
      wherever they remained.
  - **The joins** write `hive.yaml` without `nats:`, and nobody creates the `nats` directory.
  - **SY.cognition** logs where it got its AI key once it has looked it up.
  - **The admin's catalog parity check** recognizes `list_streams` and the new argument
    of the WF deploy. It had been red since 1b and A-68.
- **Deploy:**
  - build of `104fa65` in 23 min, publish in 13 s, deploy in 241 s;
  - snapshots `pre-stream2b-0-1-78` on all 4 VMs (`pre-stream1a-0-1-75` was deleted);
  - 0.1.78 on all 4, health `failed=0 streamlost=0`.
- **Gate:**
  - **No NATS and no SY.policy on any hive:**
    - nothing listens on 4222;
    - there is no `/var/lib/fluxbee/nats`, no `nats:` section (not even its comment) and no SY.policy in
      `hive.yaml`;
    - neither SY.policy's unit nor its binary remains; its unit gives `not-found`.
  - **What each orchestrator logged:**
    - **ingress:** "pending={storage.turns: 597}", the edge's turns that nobody consumed, and
      then it deleted the directory;
    - **worker1:** deleted SY.policy's unit and binary;
    - **motherbee:** removed SY.policy from its `worker` section (two lines);
    - **all four:** removed the `nats:` section.
  - **Over the mesh:**
    - `GET /config/storage/metrics` answers `ok` (0 pending);
    - in the upgrade, the motherbee's orchestrator confirmed the database in 10.6 s (18 attempts) and
      then moved to "hive ready". The wait was SY.storage's startup: the vault resolved its
      URL, it received the vault's broadcast and exited to reconnect (the usual rescue).
  - **The startup window**, with SY.storage held back for 40 s and the motherbee's orchestrator
    restarted:
    - `GET /hives` answered `502 TRANSPORT_ERROR` until the orchestrator joined the router;
    - then it answered `ORCHESTRATOR_BOOTING` in 0.01 s;
    - it moved to `ok` 4 s after SY.storage was released;
    - nobody was left hanging.
  - **Cognition regression check** (an agent on the motherbee, two turns from worker1):
    - the motherbee's cognition processed the 4 turns, none without an agent ILK;
    - one communication in the database (turn 4), with the agent's ILK;
    - the region has its entry and the streams delivered everything.
  - **SY.cognition** logs "looked up its AI key ai_secret_source=vault".
  - **Health:** `failed=0 denials=0 srcdrops=0 streamlost=0` on all 4.
  - **Not tested live:** a new join of a worker with a `hive.yaml` without `nats:`. It needs a
    new VM; the change is the three templates, and the review covered it.
  - **Cleanup:**
    - the test node purged with its identity (identity went back to 28 ILKs);
    - the tenant deleted and purged;
    - the region emptied;
    - 4 turns were deleted, with their 4 inbox rows, 75 envelopes and all the cognition rows
      (none remain);
    - no mask was left.
- **Pending:** the transitions (NATS and SY.policy) are deleted from the code once every hive
  has started 0.1.78, in the cleanup pass.
- **Rollback:** snapshot `pre-stream2b-0-1-78`, or `apt install fluxbee=0.1.77` plus the
  core-update of the spokes:
  - 0.1.77 starts without the `nats:` section, with the default values and an empty broker;
  - SY.policy was no longer running in 0.1.77;
  - what does not come back are the 597 turns that the ingress broker held unconsumed. They were
    discarded on purpose (the operator's decision).

## 0.1.77 — Cognition per communication: the agent's router passes both directions to its local cognition (A-70, stage 2a)

- **Date:** 2026-10-17 (ART) · **Previous version:** 0.1.76 · **Commit:** `377945a`.
  - Design: `docs/router-stream-v1.md` §6, discussed with the operator and reviewed once.
  - One code review: no HIGH. The MEDIUM (a loaded communication went stale if
    its agent left and came back) and the LOWs, fixed before the build.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **The model (the operator's decision):** memory belongs to the communication between ILKs, never to
    a hive or a node. A communication is `(thread_id, agent ILK)`, and its owner is the agent
    that serves it. The turn number orders it; the router no longer assigns `thread_seq`.
  - **Router:**
    - no longer copies turns to NATS;
    - copies to `cognition.turns` every turn with a `thread_id` that a local `AI.*` node sends, or that
      it delivers to one (local, after the WAN, after IRP; never tap copies or
      broadcasts). The destination is `SY.cognition@<its hive>`, on the motherbee and the workers;
    - attaches the memory package per destination, only to the `AI.*` nodes: the one for `(thread_id, agent
      node)`; the one the sender brings is discarded;
    - the stream batches carry an epoch (`state.json`), to tell a restarted log apart from
      repeats.
  - **SY.cognition:**
    - reads `cognition.turns` from its router's stream; the agent's ILK comes from identity by
      `handler_node` (cached for 60 s);
    - its envelopes (schema v2, with `agent_ilk` and `turn`) go out as one
      `storage.cognition.batch` record per turn, over the stream;
    - at startup it reloads from SY.storage, over the mesh, the communications that the memory region
      holds (`COGNITION_STATE_GET`, paginated);
    - a loaded communication that went 5 min without turns is requested again before the next one, and
      the stored state wins if it is further ahead;
    - no longer uses NATS or postgres.
  - **SY.storage:**
    - the cognition tables are rebuilt once (marker `cognition_reset_v1`; the test data
      is thrown away, the operator's decision), with the communication as the key: `agent_ilk` and
      `source_turn`;
    - a row is only replaced by an equal or later turn, the scopes too;
    - expands `storage.cognition.batch` into one inbox row per envelope, after verifying the
      producer;
    - answers `COGNITION_STATE_GET`, only to `SY.cognition@*`, in 64 KB pages.
  - **AI SDK:** the response carries the agent's ILK as `src_ilk` and the interlocutor as
    `dst_ilk`. ai.generic and SY.frontdesk.gov pass their own.
  - **SY.policy is retired (first of two versions):**
    - inactive and disabled;
    - out of each `hive.yaml`'s own section (atomic write);
    - the motherbee keeps it in its `worker` section until 2b, because otherwise the workers' old
      orchestrators reject the core-update.
  - **The broker** deletes at startup, and rejects afterwards, the turn consumers of SY.storage,
    SY.cognition and SY.policy.
- **Deploy:**
  - build of `377945a` in 25 min, publish in 14 s, deploy in 230 s;
  - snapshots `pre-stream2a-0-1-77` on all 4 VMs (`pre-sdk-0-1-74` was deleted);
  - 0.1.77 on all 4, health `failed=0 streamlost=0`.
- **Gate:**
  - **The upgrade:**
    - SY.policy inactive and disabled on the motherbee and worker1. The motherbee's `hive.yaml`
      names it only in the `worker` section; worker1's, in none;
    - no broker has consumers of `storage.turns`. The ingress's 597 turns stay there
      until 2b;
    - the streams of the motherbee and worker1 add `SY.cognition@<hive>` as a destination; the ingress and
      the egress do not;
    - the cognition tables ended up at v2 and empty;
    - both cognitions reloaded over the mesh ("ok loaded 0"). Worker1's used to fail
      because it had no database.
  - **A conversation with the agent on the motherbee** (`AI.gate77@motherbee`, ai.generic 0.1.77,
    in a test tenant) from a test interlocutor on worker1 (a socket probe),
    3 turns:
    - the responses go out with the agent's ILK as `src_ilk` and the interlocutor's as
      `dst_ilk`;
    - the motherbee's cognition processed 6 turns (3 out, 3 back), none without an agent
      ILK, with 6 + 6 calls to the model (tagger and summarizer) and 119 entities, without
      errors. Worker1's received nothing;
    - a single communication in the database: the agent's, turn 6, written by
      `SY.cognition@motherbee`; all its rows carry the agent's ILK;
    - `turns` has the 6 rows, in both directions;
    - the memory region has the entry with the agent's node and ILK;
    - the agent had no key in its tenant (since 0.1.66 there is no fallback to the root key), so
      it answered with the error `missing_ai_api_key`. Cognition evaluated those responses with the
      core's key. The gate does not depend on what the agent answers, and the operator's key was not
      copied to a test tenant.
  - **The package:** with the real node stopped, a socket probe took its name, and the
    interlocutor sent it a turn with a fake package. The probe received the real package of its
    communication (contexts, a memory, reasons) and not the fake one.
  - **The reload:**
    - the motherbee's `sy-cognition` restarted: it reloaded over the mesh in 0.3 s ("loaded 1, without
      state 0, failed 0") and the region's entry stayed;
    - the next turn continued at turns 9 and 10, on the same rows.
  - **The refetch on inactivity:**
    - a turn within the 5 min requested nothing;
    - after 5.5 min without turns, the next one requested the state once and continued at turn 14.
  - **An agent on worker1** (`AI.gate77w@worker1`), with 2 turns from the motherbee:
    - worker1's cognition processed 4 and its 80 entities reached postgres, written by
      `SY.cognition@worker1` with that agent's ILK;
    - the motherbee's did not touch them;
    - each region has only its own agent's communication.
  - **The streams** delivered everything once (equal cursors, nothing lost). NATS was not touched: the
    `last_seq` of `storage.turns` stayed at 974 and 310.
  - **Health:** `failed=0 denials=0 srcdrops=0 streamlost=0` on all 4.
  - **Cleanup:**
    - the two nodes purged with their identity (identity went back to 28 ILKs);
    - the tenant deleted and purged;
    - the memory regions emptied (cognition stopped with `mask --runtime`; no
      mask was left);
    - from the database, 18 turns were deleted, with their 18 inbox rows, 415 cognition envelopes from the
      inbox and all the cognition rows (none remain).
  - **For testing:**
    - `kill_node` followed by `remove_node_instance` fails (`NODE_INSTANCE_RUNNING`): the complete
      removal is `kill_node` with `purge_instance: true`, which also frees the identity;
    - `state_fetches_total` shows up in SY.cognition's STATUS (a `STATUS` system message),
      not in CONFIG_GET.
- **Pending for 2b:**
  - the "sy.cognition started" line says `ai_secret_source=missing`, because the key is now
    resolved later, in the turns task. It is cosmetic;
  - the admin's `config-set` example still names `vault_key`, rejected since 0.1.53. It goes
    to the cleanup pass.
- **Rollback:** snapshot `pre-stream2a-0-1-77`. `apt install fluxbee=0.1.76` alone is not enough:
  - the cognition tables ended up at v2, and 0.1.76's SY.storage cannot write them;
  - SY.policy left each `hive.yaml`'s own section.

## 0.1.76 — The router's stream carries the turns of every hive to SY.storage (A-70, stage 1b)

- **Date:** 2026-10-16 (ART) · **Previous version:** 0.1.75 · **Commit:** `e24e7d3`.
  - Design: `docs/router-stream-v1.md`. One code review: no HIGH, the two MEDIUMs and the
    LOWs fixed before the build.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **Router:** each router has its stream, a node of its own, `RT.<router>.stream@<hive>`:
    - copies every turn and takes the `STREAM_APPEND` from its nodes;
    - keeps them on disk for a window of 120 s and 32 MB, and delivers them in batches with ack to
      `SY.storage@motherbee`;
    - the router never waits: a queue bounded by count and by bytes, and the disk in its own
      thread;
    - what is lost (expired, too big, queue or cap full) is counted once and shows up in
      an alarm line every 30 s;
    - a record stays on disk only until it has been taken;
    - turns go to NATS only on the motherbee and the workers, where cognition and
      policy still read them (stage 2);
    - the broker deletes the old consumer `durable.sy-storage.turns` at startup and rejects it
      afterwards.
  - **SY.storage:**
    - takes the stream's batches with its own connection: one transaction to the inbox, the ack and
      then the processing, in another task; it no longer reads turns from NATS;
    - each record has to come from a node of the stream's hive, and a turn has to name
      its producer as the sender;
    - `turns` is the message log: it stores the whole message, `thread_id`, `trace_id`, `ts` (the
      router's time) and `stored_at`;
    - a one-time backfill copies the messages from the inbox to `turns` (marker in `storage_migrations`);
    - afterwards, the inbox deletes what was processed more than 24 h ago.
  - **Archi:** the Messages page reads `turns`. The list goes by `ts`, the live tail by
    `stored_at`, and the detail shows the whole message.
  - **SY.admin:** `list_streams`, `GET /hives/{hive}/streams`.
  - **`ops.py health`:** adds `streamlost`, what the streams' alarms report as lost.
- **Deploy:**
  - build of `e24e7d3` in 23 min, publish in 12 s, deploy in 224 s;
  - snapshots `pre-stream1b-0-1-76` on all 4 VMs (`pre-cognition-0-1-73` was deleted);
  - 0.1.76 on all 4, health `failed=0 streamlost=0`.
- **Gate (with a socket test node, `AI.gate76probe`, that sends turns to itself without a
  `thread_id`, so cognition does not spend):**
  - **The migration:**
    - the backfill completed 5 rows from 0.1.75, re-linked the 542 `legacy:` ones and inserted 35 responses
      that `turns` had dropped; 0 unreadable;
    - `turns` ended up with 582 rows, all with their message;
    - the inbox deleted 435 processed rows older than 24 h;
    - the motherbee's broker no longer has `durable.sy-storage.turns`, and its `storage.turns` ended up
      at 0 messages.
  - **The four hives reach the database,** 20 out of 20 each, with the real sender and a delay
    of 0.2 s. Before, only the motherbee's arrived.
  - **Archi** lists, opens the detail (also of the oldest backfill row), and the live tail
    showed three turns from worker1 at 0.15 s.
  - **SY.storage absent for 71 s** while worker1 and the ingress sent 40 turns each:
    - the backlog was visible (31 records in each stream);
    - when it came back everything was delivered, 40 out of 40, exactly once, with the original time.
  - **SY.storage absent for more than 3 min:**
    - worker1's 10 turns expired, counted once (`expired=10`), with the alarm line;
    - the routing went on;
    - there is nothing of them in the database.
  - **Worker1's router restarted with 10 pending:** it recovered them from disk and delivered them
    when SY.storage came back.
  - **The motherbee's router and SY.storage restarted** while worker1 sent 120 turns in
    60 s: all 120 arrived, once.
  - **Health:**
    - `streamlost=10` only on worker1, in the window of the induced expiry;
    - clean afterwards.
  - **For the test:** SY.orchestrator's watchdog brings a stopped `sy-storage` back up in 2 s, so
    the absence was done with `systemctl mask --runtime` and `unmask`. No mask was left.
  - **Cleanup:** the 373 test rows were deleted from `turns` and from the inbox. No test nodes
    remain.
- **Rollback:** snapshot `pre-stream1b-0-1-76`. `apt install fluxbee=0.1.75` works, but it does not
  bring everything back:
  - the new columns of `turns` do not get in the way, and the old SY.storage creates its
    NATS consumer again;
  - the spokes' turns stop reaching the database;
  - the old Archi reads the inbox, which no longer has what was processed more than 24 h ago. The full
    history comes back only with the snapshot.

## 0.1.75 — The live bugs under the log plane (A-70, stage 1a)

- **Date:** 2026-10-16 (ART) · **Previous version:** 0.1.74 · **Commit:** `1c00389`.
  - It comes out of the design review of the router's stream (`docs/router-stream-v1.md`): bugs that
    were already in PROD, fixed on top of today's NATS.
  - One code review, fixed before the build.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **Router:**
    - a frame over 128 KB is skipped and counted, and the link goes on;
    - a message from another hive to a node of a router that is not the gateway is no longer dropped;
    - `RT.` reserved: a HELLO with that prefix is only accepted from the router's own process.
  - **SY.storage:**
    - `turns` uses the inbox key, so the AI's response is no longer lost. The
      migration runs once; the old rows stay as `legacy:<ctx>:<seq>`;
    - exits if it cannot use its database, at startup or later, and systemd retries;
    - retries the inbox every 60 s, in order and counting attempts;
    - the inbox is not purged (Q7 of the doc).
  - **SY.cognition:** confirms each turn when it queues it and processes it in its own task; if that
    task dies, it exits.
  - **SY.orchestrator:** no managed node uses `RT.*`, and Archi's prompt says so.
- **Deploy:**
  - before: no hive had persisted `RT.*` nodes;
  - build of `1c00389` in 17 min, publish in 16 s, deploy in 218 s;
  - snapshots `pre-stream1a-0-1-75` on all 4 VMs (`pre-sync-0-1-72` was deleted);
  - 0.1.75 with the same manifest (`6a07db01`) on all 4, health `failed=0`.
- **Gate:**
  - **The `turns` migration:** unique index on `dedupe_key`, without the old primary key, 542
    `legacy:` rows.
  - **A request and its response with the same `trace_id`:** two rows in `turns`. It was tested with a
    test workflow whose message reuses the event's `trace_id`.
  - **Cognition:** a test turn cost one call to the tagger. In 0.1.74 it cost 3 or 4.
  - **SY.storage:** with its postgres connection cut, it exited, and systemd brought it back up with its database in 7 s.
  - **`RT.`:** a process that is not the router cannot do a HELLO with an `RT.` name. It is rejected,
    with its PID in the log.
  - **What PROD cannot produce** (one router per hive; the SDK does not send oversized frames): the skipping
    of big frames and the delivery over IRP are proven by unit tests.
  - **Cleanup:** the test tenant was purged; PROD was left as before.
- **Rollback:** snapshot `pre-stream1a-0-1-75`. `apt install fluxbee=0.1.74` alone is not enough: the
  `turns` migration is not undone, and the old code inserts with `ON CONFLICT (ctx, seq)`, which
  no longer has a unique index, so the turns would fail.

## 0.1.74 — The AI SDK reads OpenAI's response from its `message` item (A-69)

- **Date:** 2026-10-16 (ART) · **Previous version:** 0.1.73 · **Commit:** `0d69329`. It comes out of what
  the 0.1.73 gate found.
- **Scope:** motherbee + the three spokes (core-update). The `hive.yaml` files stay on Claude.
- **What changed:** the SDK's no-tools path read the text from `output[0]`; with a reasoning
  model (gpt-5.5) that is the reasoning item. Now there is one reader for both paths:
  `output_text` if it comes, otherwise the first `message` item.
- **Deploy:** build of `0d69329` in 19 min (the AI SDK recompiles all its consumers),
  publish in 14 s, deploy in 251 s. Snapshots `pre-sdk-0-1-74` on all 4 VMs (`pre-gate-0-1-71`
  was deleted). 0.1.74 with the same manifest (`b9d64985`) on all 4, health `failed=0`.
- **Gate:**
  - **Cognition with OpenAI** (only the motherbee's, for a while): tagger and summarizer worked
    (4 and 4 calls, no failures).
  - **Back on Claude:**
    - the tagger worked;
    - out of 3 summaries, one went over cognition's 8 s timeout (`semantic_tagger.timeout_ms`):
      Opus 5.5 is slower. That turn is left without a memory synthesis, with no further harm. Left for
      the operator: raise that timeout or use Sonnet 5.5.
  - **Archi with Claude:** still uses its tools (6 s).
  - **The four consumers** on Claude, with cognition ready on both hives; health `failed=0`.
- **Rollback:** snapshot `pre-sdk-0-1-74`, or `apt install fluxbee=0.1.73`.
- **Afterwards, on top of 0.1.74 (the operator's decision, 2026-10-16):** the infra moves to
  `claude-sonnet-5-5` (*"vamos por Sonnet 5.5"* ("let's go with Sonnet 5.5")).
  - The model was changed in both `hive.yaml` files and the same services were restarted.
  - Archi uses its tools in 3 s (with Opus, 4 to 6 s).
  - Cognition: 3 of 3 taggings and 3 of 3 summaries, no timeouts.

## 0.1.73 — The AI infra moves to Claude; SY.cognition stops sending `temperature` (A-69)

- **Date:** 2026-10-16 (ART) · **Previous version:** 0.1.72 · **Commit:** `822f237`. Requested by the
  operator: try their Anthropic key across the whole infra and, if it does not work, go back to OpenAI.
- **Scope:** motherbee + the three spokes (core-update). Also, on top of 0.1.72 and before the build,
  the `hive.yaml` of the motherbee and of worker1 (the only ones with an `ai` section; ingress and egress do not run
  AI):
  - `default_provider: anthropic`, with `claude-opus-5-5`;
  - the OpenAI entry (`gpt-5.5`) stays declared: going back means changing one line and restarting
    `sy-admin`, `sy-architect`, `sy-cognition` and `sy-frontdesk-gov` (on worker1, `sy-cognition`);
  - backup `hive.yaml.pre-claude-0-1-72` on both.
- **The test on top of 0.1.72, consumer by consumer:**
  - **Archi:** answers with Claude (2 s) and uses its tools: it listed worker1's nodes with
    `fluxbee_system_get` (4 s).
  - **The admin and SY.frontdesk.gov:** resolve the Anthropic key (`CONFIGURED`). They do not send
    `temperature`.
  - **SY.cognition:** with Opus 5.5 and with Sonnet 5.5, its tagger failed on a test turn:
    "`temperature` is deprecated for this model". Back on OpenAI it failed the same way ("not
    supported"): cognition was broken with both providers (A-69). The infra went back to Claude, which works
    everywhere OpenAI works, and cognition was fixed.
- **What changed (`822f237`):** cognition without `temperature`, output caps of 1024 tokens, and the AI key
  resolved at startup (before, it said `degraded_no_ai_provider` until its first turn).
- **Deploy:** build of `822f237` in 7 min, publish in 12 s, deploy in 229 s. Snapshots
  `pre-cognition-0-1-73` on all 4 VMs (`pre-wf-0-1-70` was deleted). 0.1.73 with the same manifest
  (`13af35fc`) on all 4, health `failed=0`.
- **Gate:**
  - **Cognition on both hives:** `ready_semantic_ai` with Claude as soon as it started, also on
    the freshly updated worker1.
  - **A test turn on the motherbee:**
    - the tagger returned real tags (`billing`, `duplicate charge`, `october invoice`…);
    - the narrative summarizer ran with `claude-opus-5-5`;
    - no failures.
  - **Found, fixed for 0.1.74:** with OpenAI the tagger no longer fails because of
    `temperature`, but the SDK does not find the response text ("missing text output"). A
    reasoning model puts a reasoning item first, and the SDK read the first item.
  - **Test data:** the test turns stayed in the message log and in cognition's
    memory, with threads `claude-probe-20261016*` and `openai-probe-20261016*`.
- **Rollback:** snapshot `pre-cognition-0-1-73`, or `apt install fluxbee=0.1.72`. To go back to
  OpenAI, `default_provider: openai` in both `hive.yaml` files and restart those services.

## 0.1.72 — A WF deploy on a worker makes the motherbee announce its package (A-68, gate of 0.1.71)

- **Date:** 2026-10-16 (ART) · **Previous version:** 0.1.71 · **Commits:** `a053102` and `17e1edd`.
  It comes out of what the 0.1.71 gate found.
  - `a053102` also asked to scan on the worker and to wait until its folder was still.
  - Read against Syncthing 2.0.14 before publishing it, it did not work: the worker pulls as soon as the
    motherbee announces, and a scan on the worker pulls nothing.
  - It was corrected in `17e1edd`. The package built with `a053102` was not published.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **On a worker, after publishing,** SY.wf-rules asks the admin for a `sync_hint` on the
    motherbee's `fluxbee-dist-runtimes`. The motherbee scans right away and announces the package.
  - **While the package is not there,** it requests the same scan before each retry.
- **Deploy:** build of `17e1edd` in 6 min, publish in 9 s, deploy in 176 s. Snapshots
  `pre-sync-0-1-72` on all 4 VMs (`pre-teardown-0-1-69` was deleted). 0.1.72 with the same manifest
  (`acbb2efd`) on all 4, health `failed=0`.
- **Gate, in the worst case:** right after the core-update, and worker1's E2E right after the
  deletion of the motherbee's E2E (what failed in 0.1.71).
  - **The lifecycle E2E on the motherbee:** 35/35.
  - **The E2E on worker1:** 35/35.
    - The first deploy took 4 s: the package arrived 1 s after being published, in the same pull as
      the deletion of the previous workflow.
    - The v2 arrived in 2 s.
    - The deletion after the rollback left no packages (the 0.1.71 fix).
  - **Through Archi:**
    - the deploy and the deletion take 1 s each;
    - the node starts `HEALTHY` with `launched_by: archi`;
    - a `_system` key it does not know stays in the file, and it stays `HEALTHY`.
  - **An announcement beats Syncthing's wait:** a pull of the manifest failed and Syncthing noted
    that it would retry in 1 min; with the next announcement it pulled it within a second.
  - **Cleanup:** the test tenant (`wf-gate-0170`) was deleted and purged. PROD was left as before:
    only the root tenant, the core's 28 ILKs, no WF node and only `wf.engine` among the
    `wf.*` runtimes.
- **Rollback:** snapshot `pre-sync-0-1-72`, or `apt install fluxbee=0.1.71`.

## 0.1.71 — A WF deploy on a worker waits for its package; the deletion leaves the current version for last (A-68, gate of 0.1.70)

- **Date:** 2026-10-16 (ART) · **Previous version:** 0.1.70 · **Commit:** `73f7132`. It comes out of what
  the 0.1.70 gate found.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **SY.wf-rules** waits up to 45 s for a workflow's package, with a 60 s deadline per request;
    the admin waits 75 s for it and Archi 90 s.
  - **When a workflow is deleted,** a version that the admin rejects as current
    (`RUNTIME_CURRENT_CONFLICT`) is retried once the others are gone.
- **Deploy:** build of `73f7132` in 12 min, publish in 14 s, deploy in 186 s. Snapshots
  `pre-gate-0-1-71` on all 4 VMs (`pre-cloud-0-1-68` was deleted). 0.1.71 with the same manifest
  (`94ae3776`) on all 4, health `failed=0`.
- **Gate:**
  - **The lifecycle E2E on the motherbee:** 35/35.
  - **Found on worker1, fixed in 0.1.72:** the first deploy waited 45 s and the package did not
    arrive (`RUNTIME_NOT_PRESENT`).
    - The manifest arrived at 12 s and the package files at 62 s.
    - The motherbee's watcher batches the changes while the runtimes folder is churning, and
      the worker's folder waits before retrying after failing to delete directories.
    - The E2E cleanup deleted the workflow and left no packages.
- **Rollback:** snapshot `pre-gate-0-1-71`, or `apt install fluxbee=0.1.70`.

## 0.1.70 — The WF nodes on the standard path: SY.wf-rules through the admin, the control plane in the Go SDK (A-68)

- **Date:** 2026-10-16 (ART) · **Previous version:** 0.1.69 · **Commit:** `c289bb3`. Design in
  `docs/wf-standard-path-v1.md`, approved by the operator on 2026-10-15. One review (1
  blocker, 7 to fix), fixed before the build.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **SY.admin** handles each mesh command in its own task, and a lock serializes the
    changes to the runtimes manifest. Deploying a workflow from Archi works again.
  - **SY.wf-rules** launches, restarts, configures and deletes its WF nodes through the admin, which lets it
    act only on the `WF.*` nodes of its hive. It accepts operations only from the admin.
  - **`launched_by`** is whoever requested the deploy: `operator` over HTTP, `archi` from Archi.
  - **The system policy:** rule (4) is left for SY.admin only, without SY.wf-rules or
    `WF.orch.diag`.
  - **The Go SDK** has the control plane of the managed nodes and the WF node uses it: with
    a config it cannot use, it stays up, in FAILED_CONFIG and with the reason.
  - **The orchestrator** does not start a workflow package without its `flow/definition.json`
    (`RUNTIME_NOT_PRESENT`).
- **Deploy:** build of `c289bb3` in 25 min, publish in 14 s, deploy in 221 s. Snapshots
  `pre-wf-0-1-70` on all 4 VMs (`pre-tenant-0-1-67` was deleted). 0.1.70 with the same manifest
  (`e3986977`) on all 4, health `failed=0`.
- **Gate, with a test tenant, through the operator's HTTP and through Archi:**
  - **The lifecycle E2E on the motherbee** (`scripts/wf_rules_lifecycle_e2e.sh`): 35/35.
    - Compile, apply with spawn, status, update, rollback and delete, all through the admin.
    - The node ends up with `launched_by: operator`.
    - With an error in its config it stays `RUNNING` and `DEGRADED` with `invalid_config`, without
      restarts; with the config fixed and a restart it goes back to `HEALTHY`.
  - **Through Archi** (SCMD over the mesh):
    - the deploy takes 1.3 s and the node starts `HEALTHY` with `launched_by: archi`;
    - a `_system` key the node does not know stays in the file and the node stays `HEALTHY`;
    - the deletion takes 1.3 s and the node disappears.
  - **Found on worker1, fixed in 0.1.71:**
    - the update did not wait long enough for the new package (`restart_failed`);
    - the deletion left the packages on the motherbee (`RUNTIME_CURRENT_CONFLICT`). They were deleted through
      the admin.
- **Rollback:** snapshot `pre-wf-0-1-70`, or `apt install fluxbee=0.1.69`. There are no WF nodes in PROD.

## 0.1.69 — Cloud deletes its nodes without locking up the admin; an SY from another hive retries while identity starts (A-67)

- **Date:** 2026-10-15 (ART) · **Previous version:** 0.1.68 · **Commit:** `6f03310`. It comes out of what
  the 0.1.68 gate found.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **SY.admin:** the commands that arrive over the mesh and make the orchestrator call it
    back (kill and remove of nodes with purge) run in their own task. Before, Cloud's `delete_node`
    locked up: the admin waited for the orchestrator and the orchestrator for the admin.
  - **IO.cloud:** waits 25 s for `stop_node` and `delete_node`.
  - **SY.vault:** to an SY from another hive that the SHM does not have yet, it answers
    `IDENTITY_UNAVAILABLE`, which is retried, instead of `UNAUTHORIZED`. It hands out nothing extra.
- **Deploy:** build of `6f03310` in 9 min, publish in 16 s, deploy in 237 s. Snapshots
  `pre-teardown-0-1-69` on all 4 VMs (`pre-secrets-0-1-66` was deleted). 0.1.69 with the same manifest
  (`baf673f4`) on all 4, health `failed=0`.
- **Gate:**
  - **The restart:** `SY.cognition@worker1` read a genesis key again in the gap while
    identity was starting, and this time it got `IDENTITY_UNAVAILABLE`; afterwards it read fine. No
    `UNAUTHORIZED` from an SY.
  - **`delete_node` through the public door:** answers ok; the key the node had as its own is
    deleted and the assigned one stays; the name is freed (it was created and deleted again).
  - **Cleanup:** PROD was left as before (only the root tenant and the core's 28 ILKs).
- **Rollback:** snapshot `pre-teardown-0-1-69`, or `apt install fluxbee=0.1.68`.

## 0.1.68 — Cloud manages its tenants and the IO nodes it launched; stopping a node sticks (A-67, stage 4)

- **Date:** 2026-10-15 (ART) · **Previous version:** 0.1.67 · **Commits:** `a5f1b2a` (the code for the
  `set_ilk_tenant` rejection), `b26de82` (stage 4) and `1b47c3e` (the review). Design in
  `docs/ilk-tenant-model-v1.md` §2.4 and D14, decided with the operator on 2026-10-15. One review (3
  major, 4 minor), fixed before the build; the build that carried the first major was stopped.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **Tenants through Cloud:** `update_tenant` with status (`active`, `suspended`), settings and sponsor;
    full `get_tenant` through the admin; `list_tenants` without the genesis.
  - **Nodes through Cloud:** `list_nodes`, `get_node_status`, and `start_node`, `stop_node` and
    `delete_node` only for the IO nodes it launched in its tenant (the admin records who launched each
    node; the node's hive, or the motherbee if the hive runs an earlier version, checks the
    condition).
  - **Stopping sticks, for everyone.**
  - **The WF node in Go** knows `launched_by`.
- **Deploy:** build of `1b47c3e` in 8 min, publish in 15 s, deploy in 216 s. Snapshots
  `pre-cloud-0-1-68` (`pre-scope-0-1-65` was deleted). 0.1.68 on all 4 (`a41a6af7`), health
  `failed=0`, OPA `in_sync` 4/4.
- **Gate, through the public door with Cloud's bearer and from the operator's side:**
  - **The catalog** lists 24 operations, with the 7 new ones.
  - **Tenants:**
    - settings and sponsor edited and read back with `get_tenant`;
    - `pending` rejected (`INVALID_REQUEST`) and the genesis rejected (`TENANT_ROOT_PROTECTED`);
    - `list_tenants` without the genesis.
  - **Nodes:**
    - s1, launched by Cloud and configured at launch, shows up in `list_nodes` with
      `launched_by: cloud` and only its status;
    - a node from another tenant gives `NODE_NOT_FOUND`;
    - o1, launched by the operator in the same tenant, gives Cloud `NODE_NOT_FOUND` for start,
      stop and delete;
    - a node that is not IO gives `INVALID_REQUEST`.
  - **Stopping sticks:** Cloud stopped s1 and it stayed down for more than one relaunch pass; once started,
    it came back with the relaunch flag.
  - **Suspending through Cloud:** a new node, `TENANT_NOT_ACTIVE`; the vault denies s1 its key;
    reactivated, it reads it again.
  - **`a5f1b2a`:** moving a temporary out of a tenant that is not deleted answers
    `INVALID_TENANT_TRANSITION` alone, with the detail separate.
  - **A test workflow:** its WF node started clean (0 restarts, `launched_by: system`).
  - **Found, fixed in 0.1.69:** `delete_node` from Cloud locked up (timeout, the
    identity marked but not purged), and an SY from another hive got a transient `UNAUTHORIZED`
    while identity was starting.
  - **Cleanup:** the stuck identity purged, o1 killed, people, keys and tenants deleted by
    Cloud and purged. PROD was left as before.
- **Rollback:** snapshot `pre-cloud-0-1-68`, or `apt install fluxbee=0.1.67`. A node stopped with 0.1.68
  keeps `relaunch_on_boot: false`; with 0.1.67 it does not relaunch on its own either.

## 0.1.67 — A tenant that is not active does not start nodes and deleting it marks nobody (A-66, stage 3); the vault binds the ILK to the node that sends (A-65)

- **Date:** 2026-10-15 (ART) · **Previous version:** 0.1.66 · **Commits:** `fbca8e7` and `81d2fab`
  (A-65), `11aead9` (stage 3) and `e85041d` (the review). Design in `docs/ilk-tenant-model-v1.md`
  §2.3, decided with the operator. One review of A-65 (partial scope, exact comparison) and one
  of stage 3 (2 major, 2 minor, 4 details), fixed before the build.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **SY.vault (A-65):** the requester's ILK has to be that of the node the router says
    sent the request: derived from the name for an SY, the one registered for its name for anyone else. Across
    hives this is closed; within the same hive the name is still the one from the HELLO (P-1).
  - **Orchestrator:** `run_node`, start and restart answer `TENANT_NOT_ACTIVE` for a pending or
    suspended tenant, and the relaunches skip it. A node that is already running is not killed.
  - **SY.identity:**
    - deleting a tenant marks only the tenant;
    - purging it is rejected while people are charged to it (`TENANT_HAS_PEOPLE`);
    - `set_ilk_tenant` moves temporaries out of a deleted tenant;
    - `restore_ilk` brings back a person whose tenant is deleted;
    - the v4 migration unmarks whoever an old cascade marked.
  - **io.api `by_ilk`:** people charged to the tenant or with a channel in its space (D12).
  - **Admin and Cloud:** the new codes are 409; texts and catalog.
- **Deploy:**
  - **Build:** `e85041d` in 20.5 min. Before that, two builds of the same commit were launched by mistake at
    the same time (`11aead9`); they were stopped within seconds and only one was left.
  - **Publish and deploy:** publish in 19 s, deploy in 293 s.
  - **Snapshots:** `pre-tenant-0-1-67` on all 4 VMs (`pre-cloud-0-1-64` was deleted).
  - **Status:** 0.1.67 with the same manifest (`637ed35d`) on all 4, health `failed=0`, OPA `in_sync`
    4/4. Identity: *"migrations through v4"*, with nothing to unmark (PROD had no cascade
    marks).
  - **Before the deploy:** the v4 SQL was tested on PROD's schema inside a
    transaction that was rolled back: it unmarked the free one and one of two with the same email; it left
    marked the one whose email was taken, the other one and a node; it did not touch a direct mark.
- **Gate:**
  - **A-65:**
    - after the restart, every SY read its keys without a rejection, also those of other
      hives (`SY.cognition@worker1`, `SY.edge@ingress1`);
    - the admin's `send_node_message` to the vault with IO.cloud's ILK: `UNAUTHORIZED`, audited; with
      its own: passes;
    - the gate's IO.slack node reads its key with its own ILK.
  - **Stage 3, through the public door with Cloud's bearer and from the operator's side:**
    - **Setup:** tenants T and U; an IO.slack node s1 in T with T's general key; Ana and Bob
      (accounts) and a sign-up without a name, which stays pending (a temporary); Bob deleted.
    - **T suspended:**
      - s1's restart gives `409 TENANT_NOT_ACTIVE` and s1 keeps running;
      - a new node in T gives `TENANT_NOT_ACTIVE`;
      - the vault denies s1 its key (7 `UNAUTHORIZED`);
      - with s1's unit stopped, two passes in a row skip it (*"not relaunched: its tenant
        is not active"*);
      - moving the temporary out of T, which is not deleted, is rejected.
    - **T reactivated:** the next pass (52 s later) relaunched s1, which read again.
    - **Delete:**
      - with s1 alive, `TENANT_HAS_NODES`;
      - with s1 purged, ok and `people_charged: 2`;
      - Ana and the temporary are not marked and stay charged to T, and Ana's account answers by email;
      - Bob comes back with T deleted;
      - restoring T gives `people_charged: 3`.
    - **Purge:** `TENANT_HAS_SECRETS`; without the key, `TENANT_HAS_PEOPLE` with the three; the three to U
      (the temporary out of the deleted T); purge ok without taking anyone along; the three stay in U and the
      temporary stays pending.
    - **Cleanup:** the three people deleted by Cloud, U deleted and purged (the three marked
      ones went with it). PROD was left as before: only the root tenant and the core's 28 ILKs.
  - **Found in the gate:** the rejection of moving a temporary answered
    `INVALID_TENANT_TRANSITION|<detail>` as `error_code`. Fixed in `a5f1b2a`; it ships with the
    next release.
  - **What was not tested live:** io.api `by_ilk` (D12), covered by tests; the v4 on real
    data, because PROD had none (covered by the transaction test).
- **Rollback:**
  - snapshot `pre-tenant-0-1-67`;
  - or `apt install fluxbee=0.1.66`. The v4 is not undone: whoever it unmarked, 0.1.66 sees as active, and
    a tenant deleted without the cascade keeps its people active (0.1.66 does not mark them back).

## 0.1.66 — A secret is used by its owner, its assigned nodes or, if it is general, its tenant's nodes; in the genesis only the core is born (A-64, stage 2)

- **Date:** 2026-10-14 (ART) · **Previous version:** 0.1.65 · **Commits:** `80b479c` (pending
  sign-ups, D11) and `96c8d77` (stage 2). Design in `docs/ilk-tenant-model-v1.md` §2.2, decided with
  the operator. One code review (1 major, 9 minor), fixed before the build.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **SY.vault:**
    - assigned nodes in the secret, and `VAULT_ASSIGN`, which changes them without touching the value;
    - the value goes out only when the secret's tenant is active;
    - a new value keeps who uses it;
    - the change notice reaches whoever had it and whoever has it.
  - **SDK:** the resolution looks first for one of its own, then an assigned one, then the tenant's
    general one; never the root's.
  - **SY.admin:** `vault_assign` and assigned nodes in `vault_put`, always from the secret's tenant and
    never people.
  - **IO.cloud:** `nodes` in `put_token`, the `assign_token` operation and `pending_signup` in the
    reads.
  - **Orchestrator:** in the genesis only the core is born.
  - **SY.identity:** a node's tenant does not change, and pending sign-ups are read as such.
- **Deploy:**
  - **First build:** died of memory. The build VM (4 CPU, 8 GB, no swap) killed a `rustc`;
    0.1.64 and 0.1.65 had barely made it. An 8 GB swapfile (persistent) was added to it.
  - **Second build:** `96c8d77` in 18 min.
  - **Publish and deploy:** publish in 13 s, deploy in 216 s.
  - **Snapshots:** `pre-secrets-0-1-66` on all 4 VMs (`pre-a58-0-1-63` was deleted).
  - **Status:** 0.1.66 with the same manifest (`648d8493`) on all 4, health `failed=0`, OPA `in_sync`
    4/4.
  - **Before the deploy:** it was verified that only the core runs in PROD's genesis, so the new
    rule left nothing out.
- **Gate (measured with the vault's audit, which records every read by every node):**
  - **After the restart:** the core read all its keys (SY.\* the genesis ones, SY.edge its
    own), without a rejection.
  - **Through the public door, with Cloud's bearer: 22 checks, all PASS.**
    - Two companies A and B. Four IO.slack nodes: three in A and one in B, all configured with
      A's key, with fake tokens.
    - One person in A.
    - `put_token` assigned to two nodes; `list_tokens` shows them.
    - Assigning a node from B, or a person, gives `INVALID_REQUEST`; assigning A's key from B gives
      `KEY_NOT_FOUND`.
    - The identity reads bring `pending_signup`.
  - **What each node read:**
    - **Assigned to g1 and g2:** g1 and g2 read; g3 (from A, not assigned) and b1 (from B) get
      `UNAUTHORIZED`.
    - **Assigned only to g1:** g2 was left out as soon as the notice arrived, and it dropped the credential.
    - **General for A:** g1, g2 and g3 read; b1 does not.
    - **A suspended:** the three from A, rejected with *"the secret's tenant … is not active
      (suspended)"*. **Reactivated:** they read again.
    - **A person's secret** (owner Juan, assigned to g4): g4 reads it, and keeps reading it
      after Juan is moved to company B.
  - **From the operator's side:** an AI node in the genesis gives `TENANT_ROOT_NOT_ALLOWED`, and
    `?pending_signup=true` answers (0 pending).
  - **What was not tested live:** changing a node's tenant, which has no API path to
    attempt it. It is covered by tests.
- **Cleanup:** the five nodes purged (purging them deleted no secret, because they did not
  own any), the keys and the person deleted by Cloud, the two tenants purged. Identity
  and the vault were left as before.
- **Fixed afterwards, ships with the next release:** the admin HTTP returned 500 for
  `TENANT_ROOT_NOT_ALLOWED`; now 403.
- **Rollback:**
  - snapshot `pre-secrets-0-1-66`;
  - or `apt install fluxbee=0.1.65`. There is no migration: the secrets that 0.1.66 writes with assigned
    nodes, 0.1.65 sees as general.

## 0.1.65 — One ILK per actor: each channel's scope, one Cloud account per email and `set_ilk_tenant` (A-63, stage 1)

- **Date:** 2026-10-14 (ART) · **Previous version:** 0.1.64 · **Commit:** `8b3e951` (FINDINGS A-63;
  design in `docs/ilk-tenant-model-v1.md`, stage 1). One design review (7 major, 5 minor)
  and one code review (1 major, 8 minor), all fixed before the build.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **SY.identity:**
    - each channel stores its scope (`scope`) and every key comes from it: memory, database, SHM and the fast
      path. Reassigning an ILK moves no key;
    - one Cloud account per email, in the installation's scope, which only IO.cloud writes (through
      provision, add, update, merge and replica push);
    - `ILK_SET_TENANT`, with history in the same transaction (`identity_ilk_tenant_history`);
    - registration no longer changes a person's tenant, and merges stay within a tenant;
    - deleting a tenant with accounts charged to it gives `TENANT_HAS_ACCOUNTS`, and an unfinished sign-up
      neither blocks nor gets marked (`pending_signups`);
    - purging a tenant takes along the channels that other ILKs had in its scope
      (`dropped_channels`);
    - the v3 migration runs on every startup of the primary, before loading memory: it moves the
      unambiguous accounts and reports the rest in the log.
  - **SY.admin:** `set_ilk_tenant` (HTTP `PUT .../identity/ilks/{ilk}/tenant` and Cloud relay),
    with the "for whom" stamped by the admin; Cloud deletes and restores only people.
  - **IO.cloud:** `register_human` returns the existing account (`existing: true`); `set_ilk_tenant`;
    `get_ilk`, `get_ilk_details`, `delete_ilk` and `restore_ilk` without a tenant.
  - **Contract:** `docs/io-cloud-api.md` (§9, 0.1.65).
- **Deploy:**
  - build `8b3e951` in 23 min;
  - publish in 15 s;
  - deploy in 230 s;
  - snapshots `pre-scope-0-1-65` on all 4 VMs (`pre-identity-0-1-62` was deleted);
  - versions 0.1.65 with the same manifest (`7515c798`) on all 4, health `failed=0`, OPA `in_sync`
    4/4.
- **Before the deploy, old data on purpose (on 0.1.64, through Cloud's door):**
  - the same email registered from two tenants (two people);
  - a unique email;
  - a deleted person.
- **Gate:**
  - **The migration, at startup:**
    - v3 recorded;
    - the unique email moved to the installation's scope;
    - the duplicate email stayed where it was, reported in the log (`ambiguous_emails=1` with the two
      ILKs);
    - the deleted person was not touched.
  - **Through the public door, with Cloud's bearer: 30 checks, all PASS.**
    - The duplicate email gives `ACCOUNT_AMBIGUOUS`.
    - The unique one, requested from another tenant, returns its account (`existing: true`, it stays in its tenant).
    - A new account from two tenants is a single ILK.
    - `set_ilk_tenant` moves it (and the second time `changed: false`); the root is protected.
    - The restore of the person deleted before the upgrade leaves them in the installation's scope,
      where `register_human` finds them.
    - Read by id across tenants; `delete_ilk` of a node gives `ILK_TYPE_MISMATCH`; deleting a
      tenant with an account charged to it gives `TENANT_HAS_ACCOUNTS`.
  - **From the operator's side:**
    - the history row says `SY.admin@motherbee for IO.cloud@motherbee`;
    - the replica's SHM on worker1 shows the new tenant and the channel in the installation's
      scope;
    - purging the tenant a person had left worked (`dropped_channels=1`; it was the MAJOR from
      the code review);
    - restarting sy-identity: the keys survived the reload from the database, the report came out
      clean and the email that had become unique moved to the installation's scope;
    - health `failed=0` on all 4 after the restart.
- **Cleanup:** the five people deleted by Cloud, the three tenants purged by the operator,
  the test node purged. Identity was left as before (only the root tenant, no people, no
  history) and the vault with its infrastructure keys plus `anthropic_root_pool`.
- **Fixed afterwards, ships with the next release:**
  - the admin HTTP mapped `TENANT_NOT_FOUND` to 500: now 404, and identity's lifecycle rejections
    (`TENANT_HAS_NODES`, `TENANT_NOT_DELETED`, `ILK_KEY_CONFLICT`, …) give 409;
  - the schema log said "baseline v1" with v3 applied.
- **Rollback:**
  - snapshot `pre-scope-0-1-65`;
  - or `apt install fluxbee=0.1.64`. The v3 migration is not undone, but 0.1.64 recomputes
    the keys from the ILK's tenant and rewrites each channel's scope on its next write.
    The history table stays, unused.

## 0.1.64 — Cloud manages its company's credentials and name; every write to the vault names its tenant (A-61)

- **Date:** 2026-10-13 (ART) · **Previous version:** 0.1.63 · **Commit:** `f8c73c5` (FINDINGS
  A-61, A-62). A single adversarial review: one MAJOR (the factory reset) and six minor, all
  fixed before the build.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **SY.vault, for every caller:**
    - a put of a key from another tenant is `KEY_TAKEN` and never moves it;
    - delete, rotate and roll back carry `tenant_id`, and a key from another tenant answers
      `KEY_NOT_FOUND`, with the same text as a nonexistent one;
    - the admin sets the root if none comes, accepts `?tenant_id=` over HTTP and has the field in Archi's
      plans;
    - the orchestrator and the factory reset delete each key in its tenant.
  - **SY.identity:**
    - ILK_GET with tenant;
    - `update_tenant` rejects the root and a name or domain from another tenant, and validates everything
      before changing anything.
  - **IO.cloud:**
    - rule 3: the tenant goes only in the envelope's root, it is never the root tenant, and everything belongs to that tenant;
    - `list_tokens`, `delete_token` and `update_tenant`;
    - every error with `error_code`.

    SY.admin enforces it again on the server side. Archi and a node's cleanup keep their
    actions over the mesh.
  - **Contract:** `docs/io-cloud-api.md` rewritten.
- **Deploy:**
  - build `f8c73c5` in 28.5 min (it touched the SDK);
  - publish in 12 s;
  - deploy in 233 s;
  - snapshots `pre-cloud-0-1-64` on all 4 VMs (`pre-ssh-0-1-61` was deleted);
  - versions 0.1.64 with the same manifest on all 4, health `failed=0`, OPA `in_sync` 4/4.
- **Before the deploy:** in 30 days PROD had no real Cloud traffic, only the e2e runs of 2026-09-28
  and probes. The contract change (the reads' tenant moves to the envelope's root) broke
  nobody in PROD. The Cloud team's DEV hive has to coordinate before moving to 0.1.64.
- **Gate:**
  - **Through the real public door, from outside, with Cloud's bearer:** 53 checks, all PASS.
    - The catalog offers 16 operations.
    - Rule 3: the tenant in `params`, the root and an uppercase spelling are rejected; without a tenant
      there is an error with a code.
    - Credentials: put from A; the same key from B is `KEY_TAKEN`, also with the same value.
    - Reserved keys: rejected on write and on delete.
    - `list_tokens`: A sees its key with the metadata and without the value, and B sees nothing.
    - Deleting A's key from B gives `KEY_NOT_FOUND`, with the same text as a nonexistent one; its
      own deletion works.
    - `update_tenant`: the rename works; B's name and domain are taken; the status belongs
      to the operator; emptying the domain works; the root is protected.
    - A person in A: B does not see them by id or by email; the first login by email finds them in
      A.
    - `delete_ilk` from B gives `ILK_TENANT_MISMATCH`.
    - Nodes: `provision_node` in the root is protected; a node in A with its own token works.
  - **From the operator's side:**
    - killing the node with purge deleted its secret from tenant A;
    - `DELETE` over HTTP without a tenant gives 404 for a key of A, and with `?tenant_id=` it deletes it;
    - the same root key from A gives 409 `KEY_TAKEN`;
    - Archi ran `vault_list` and `get_ilk` over the mesh. Before this version, `get_ilk` was
      rejected for it.
- **Cleanup:** the two tenants marked by Cloud and purged by the operator (with the person), the
  node purged, the two Archi sessions deleted. The vault was left with its 4 infrastructure
  keys.
- **Seen in passing (minor, not fixed):** `GET` of a purged tenant answers 500
  `TENANT_NOT_FOUND`; the code is not in the admin HTTP's 404 map.
- **Rollback:**
  - snapshot `pre-cloud-0-1-64`, or `apt install fluxbee=0.1.63`;
  - there is no data migration: the vault's records did not change shape.

## 0.1.63 — Syncthing runs as `fluxbee` on every host (A-58); blobs are written and read by descriptor and take the owner of their folder (A-56)

- **Date:** 2026-10-06 (ART) · **Previous version:** 0.1.62 · **Commits:** `02f7fdb`, `506151c`,
  `aab0e3e` (FINDINGS A-56, A-58, A-60).
- **Review:** two adversarial reviews (SDK/io.blob and orchestrator), two rounds each. The
  harness numbers are in A-56.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **Syncthing as `fluxbee`:**
    - the orchestrator creates the `fluxbee` user on each host, and Syncthing always runs as it;
    - on worker1 and egress1 the change is a one-time, visible step: it stops, hands over its trees and
      starts.
  - **Owners of `blob/`:**
    - `blob/` is root:fluxbee;
    - staging and `agent-assets` belong to root;
    - `active/` and `public/` belong to `fluxbee`.
  - **SDK and io.blob:** write by descriptor and hand each blob over to the owner of its folder. The
    readers use `read_blob`, which verifies size and hash. ai.generic verifies the hash of each asset.
  - **Postinst:** the `chown -R`, which hid the problem, is gone.
- **Deploy:**
  - build `aab0e3e` in 21 min, publish 10 s, deploy 193 s;
  - snapshots `pre-a58-0-1-63` on all 4 VMs (`pre-hardening-0-1-60` was deleted);
  - versions 0.1.63 on all 4, health `failed=0`, OPA `in_sync` 4/4.
- **Gate A-58:**
  - worker1 and egress1 created the `fluxbee` user;
  - their Syncthing ended up with a `fluxbee` unit and process since 01:29:27 and 01:29:22, and the
    `syncthing_user_switched` alert came out only once the service was healthy;
  - home, `active/` (worker1) and dist moved to `fluxbee`;
  - the motherbee and the ingress were not restarted (Syncthing up since before the deploy);
  - every folder on the 4 hosts ended up `idle` and without errors.
- **Gate A-56:**
  - Motherbee:
    - `blob/` moved to root:fluxbee 0750;
    - staging (10 entries) and `agent-assets` (5) moved to root;
    - `active/` and `public/` without foreign entries.
  - Ingress: the startup handover transferred root's `.stfolder`, with the alert
    `blob_entries_handed_over:public` (1 entry).
  - A concrete test with `blob_sync_diag` built from the same commit:
    - a blob written as root with the new SDK ended up `fluxbee:fluxbee 0640` (prefix 0750),
      both on worker1 and on the motherbee;
    - it reached the other side in ~12 s and ~10 s, with the same hash;
    - with 0.1.62, the motherbee's would have stayed unreadable for its Syncthing until the next
      install;
    - afterwards the two blobs (the deletion reached worker1 in ~12 s) and the binary were deleted.
- **Rollback:**
  - snapshot `pre-a58-0-1-63`, or `apt install fluxbee=0.1.62`;
  - on a worker that produces blobs, move Syncthing back to root as the HANDBOOK says ("The
    rollback").

## 0.1.62 — The identity replica no longer hangs behind a firewall (P7-1, A-59); Syncthing comes back on its new binary (A-57); D32 follow-up

- **Date:** 2026-10-06 (ART) · **Previous version:** 0.1.61 · **Commits:** `b6435cf`, `c7c7d6d`,
  `b4868e8` (FINDINGS A-59, A-57, A-54). Three adversarial reviews (identity, orchestrator, docs),
  plus a second one on the fix for the identity race.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **sy-identity:**
    - the primary sends a heartbeat on the subscription every 20 s without traffic;
    - the replica cuts and resubscribes after 60 s without hearing anything;
    - the subscriber is registered before answering and before each snapshot, so a delta is no longer
      lost between the snapshot and the stream;
    - dead subscriptions are cleaned up when a new one is registered.
  - **Orchestrator:**
    - Syncthing restarts if its binary or its unit changed, or if its process runs a deleted binary;
    - the vendor update answers what it really did;
    - the join issues the leaf before the first SSH;
    - the revoke deletes the old recovery key only by its comment;
    - the SSH guard test is stricter;
    - a worker without vendor fails the join, as already happened on egress and ingress.
  - **Docs:** drift corrected.
- **Deploy:** build `b4868e8` in 8 min, publish 9 s, deploy 201 s. Snapshots `pre-identity-0-1-62` on
  all 4 VMs (`pre-stage3-0-1-59` was deleted). Versions 0.1.62 on all 4, health `failed=0`.
- **Gate A-59 (P7-1):**
  - Before (0.1.61), on worker1: the subscription had gone 15,929 s without a packet in either direction.
  - After: three samples of `lastrcv` on the subscription gave 8 s, 3 s and 18.5 s, with `lastsnd`
    growing because it does not send. The publishing channel keeps its 30 s cycle.
  - A 100 s cut on worker1, with a test nft table that drops what comes from
    10.10.10.10:9100 and a timer that deletes it anyway:
    - 60 s after the last heartbeat, the replica said "no frame from the primary for 60s
      (heartbeats stopped); resubscribing";
    - 5 s after lifting the cut, "identity full sync applied (delta stream subscribed)";
    - the table ended up deleted.
- **Gate A-57:**
  - No host was running a Syncthing with the deleted binary, and none restarted it when the new
    orchestrator started.
  - `update category=vendor` to worker1, with the same binary 2.0.14, answered `unchanged: [syncthing]`
    and `restarted: []`. Before, it said `restarted` without having restarted.
  - The path with a new binary will be exercised by the next vendor that changes.
  - **The restart, with a concrete test on worker1:**
    - An identical copy of the binary was installed with another inode. The running Syncthing (pid 2507959,
      up since 10:21) went on to run a `(deleted)` file with the same bytes: this is the review's case
      of the missed restart.
    - The first `update category=vendor` answered `unchanged: [syncthing]` and
      `restarted: [fluxbee-syncthing]`. The log says "restarting syncthing onto its installed binary
      and unit (A-57)": a new, healthy pid 4 s later.
    - The second answered `restarted: []`, with no loop.
- **Join gate (VM 104 re-cloned):**
  - It was deleted and cloned from the template. `add_hive worker2` was launched at 22:50:38, with the box still
    in `starting`.
  - The join waited in `waiting_for_host` from 22:51:09 to 22:59:35 (cloud-final) and ended
    `connected` at 23:01:39.
  - Result: `harden_ssh_applied`, `restrict_ssh` `from_only`, `ssh_bootstrap_revoked`, WAN,
    orchestrator and dist ready, and the Syncthing peer linked. That join exercised the leaf issued
    before the first SSH and the vendor push that is now fatal.
  - On the box:
    - password off;
    - only the operator's throwaway key was left in `authorized_keys`;
    - the orchestrator's sudoers was deleted;
    - the TLS material was there;
    - in sshd's journal, the sessions from 10.10.10.10 fall only within the join (22:50:44 to
      23:01:27);
    - the identity subscription was getting heartbeats (`lastrcv` 14.8 s);
    - every component on 0.1.62.
  - Cleanup: throwaway key deleted, `DELETE /hives/worker2` ok over the socket, VM 104 powered off.
  - Seen in passing: Syncthing runs as root on this new worker too (A-58, goes in 0.1.63).
- **Closure:** `ops.py health` at `failed=0` since the deploy, and OPA `in_sync` 4/4.
- **Rollback:**
  - Snapshot `pre-identity-0-1-62`, or `apt install fluxbee=0.1.61`.
  - **The spokes first** (`core_rollback` or update to the old core), then the motherbee: a worker
    on 0.1.62 facing an old motherbee resubscribes every 60 s (HANDBOOK, "The rollback").

## 0.1.61 — SSH only inside `add_hive` (D32, A-54)

- **Date:** 2026-10-06 (ART) · **Previous version:** 0.1.60 · **Commits:** `31dd105`, `3ae2929`
  (FINDINGS A-54; two adversarial reviews).
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:** the TLS reconciliation over SSH at startup and the
  `key_only_persist` mode were deleted; a request with `ssh_access` is rejected; the leaf push is fatal for the join
  (`TLS_PUSH_FAILED`); the revoke deletes an old recovery key; a guard test walks the
  code from each use of SSH up to `accept_add_hive`.
- **Deploy:** build `e0f2d04`, publish 9 s, deploy 198 s; snapshots `pre-ssh-0-1-61` on all 4 VMs
  (`pre-stage2-0-1-58` was deleted). Versions 0.1.61 on all 4, health `failed=0`.
- **Gate D32:** in sshd's journal on the three spokes, from the 0.1.60 deploy to the 0.1.61 one,
  3 SSH attempts from 10.10.10.10 per spoke (one per start of the motherbee's orchestrator, all
  failed at authentication); since the 0.1.61 deploy, **zero**.
- **Gate A-52 + D32 with a new clone:** VM 104 was deleted and cloned again from the template;
  `add_hive worker2` was launched as soon as the guest agent answered (17:28:37, systemd `starting`).
  - The join entered `waiting_for_host` at 17:29:43 with cloud-final `activating` (the dist-upgrade
    of the first boot) and waited 8 min; at 17:37:38 the box went `running` and the join went on.
  - It ended `connected` at 17:39:42: `harden_ssh_applied`, `restrict_ssh`,
    `ssh_bootstrap_revoked`, WAN, orchestrator and dist ready; without `ssh_access` or
    `spoke_key_vault_ref` in the result.
  - Syncthing: the motherbee's static address at 70 s (17:40:38) and the stage 3 options; worker2's
    `posture-check --stage 3` gives ok (the check "a join gives `Static`", moved from
    stage 2, is done).
  - Cleanup: throwaway key deleted (password auth `no`), `DELETE /hives/worker2` ok over the socket,
    VM 104 powered off.

## 0.1.60 — Root writes in `fluxbee` trees; the join waits for the box to settle (A-51, A-52)

- **Date:** 2026-10-06 (ART) · **Previous version:** 0.1.59 · **Commits:** `d3710c4`..`05ac354`,
  merge `0af5469` (FINDINGS A-51, A-52; three adversarial reviews).
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **The motherbee's `dist/` belongs to root** and Syncthing only reads it (`root:fluxbee`, 0750,
    `g+rX,go-w`); the links inside are deleted. The preinst (before the unpack) and the orchestrator
    (on every startup) do it. `blob/`, Syncthing's home and the spokes' `dist/` do not change.
  - Root writes: an `O_EXCL` temporary with a per-run token and a `.syncthing.` prefix, mode set on
    the descriptor; managed directories opened `O_NOFOLLOW` (what is not a directory is left alone
    and reported, without bringing down the startup); the `.stfolder` marker never follows a symlink.
  - The joins wait, before the sudoers, for systemd to finish starting and for no cloud-init stage
    or daily apt job to be running (up to 15 min; phase `waiting_for_host`; `HOST_NOT_SETTLED`).
- **Deploy:** build 8 min (`0af5469`), publish 12 s, deploy 192 s; snapshots `pre-hardening-0-1-60`
  on all 4 VMs (`pre-posture-0-1-57` was deleted). The spokes received the core through Syncthing from the
  `dist/` that already belonged to root: the distribution works.
- **Gate:**
  - Motherbee: `dist/` `root:fluxbee 750`, the 107 entries `root:fluxbee`, 0 links, 0 temporaries;
    `blob/*` and `syncthing/` owned by `fluxbee`; no errors from Syncthing or the orchestrator.
  - Spokes without owner changes; `posture-check --stage 3` ok on all 4; OPA in_sync 4/4; versions
    0.1.60 on all 4; health `failed=0`.
  - A-52 validated with the 0.1.61 join (new clone, see above).
- **Incident during the verification (mine, resolved):** a verification script ran
  `sy-orchestrator --version` and `rt-gateway --version` on the motherbee; those binaries have no
  `--version` and started as second instances.
  - The extra orchestrator (~11 min) was rejected by the router (UUID already connected) and overwrote the PID.
  - The extra router (~84 s) recreated the socket files; when it was killed, the paths were left dead
    (the connected nodes went on; one that restarted would not have been able to come back).
  - Fix: kill only those processes and restart `rt-gateway` and `sy-orchestrator`. Afterwards: sockets
    ok, PID ok, health `failed=0` on all 4, 15 active nodes, OPA 4/4, `--stage 3` ok.
  - Rule: a verification never runs product binaries; the versions come from `/versions` or
    `dpkg-query`.
- **Seen:** on worker1 and egress1 the `fluxbee` user does not exist: Syncthing runs as root (A-58).

## 0.1.59 — Syncthing without public infrastructure (stage 3 of the posture)

- **Date:** 2026-10-06 (ART) · **Previous version:** 0.1.58 · **Commits:** `d55ab73`..`f60eb92`
  (FINDINGS A-47; logbook `2026-10-06`; design `docs/host-posture-and-exposure-spec-v1.md` §3.5,
  "Options (stage 3) — as built").
- **Scope:** motherbee + the three spokes (core-update); `worker2` (VM 104) jumped from 0.1.57 to
  0.1.59 in the gate test.
- **What changed:**
  - **The orchestrator owns Syncthing's options:** no global or local announce, no
    relays, NAT, STUN, usage or crash reports, no auto-upgrade; reconnection every 10 s; a
    single listen (the motherbee `tcp://:22000`, the spokes `tcp://127.0.0.1:22000`). With that,
    Syncthing opens no UDP socket.
  - **The change is one-way and ordered in the code:** each spoke switches when it has the
    motherbee at a fixed, reachable address (facts persisted in
    `/var/lib/fluxbee/orchestrator/syncthing-posture.json` and published in `/versions`); the
    motherbee, when every registered spoke reports it, and meanwhile it names whom it is
    waiting for.
  - **Protected joins:** while an `ADD_HIVE_FINALIZE` runs, the spoke does not restart Syncthing, and
    the convergence poll tolerates a restart.
  - Two adversarial code reviews before the release.
- **Build:** 6.5 min (commit `f60eb92`). **CI:** `rust-tests`, `posture-guards` and
  `router-dispatcher-guards` green. **Publish:** 9 s, 59 packages. **Deploy:** 198 s with
  `ops deploy`; snapshots `pre-stage3-0-1-59` on all 4 VMs (`pre-root-io-0-1-56` was deleted first).
- **Live verification (stage 3 gate):**
  - **PROD spokes:** switched on their own ~3 min after the deploy (ingress1 10:21:19, egress1
    10:21:26, worker1 10:21:30 UTC), with one Syncthing restart each.
  - **The motherbee waited for `worker2`**, naming it in its journal ("no answer: … not reachable
    in LSA"), with discovery on.
  - **Version skipping:** `worker2`, powered off since 0.1.57 with `dynamic` on both sides,
    started, connected through discovery, received the new core and updated from 0.1.57 to 0.1.59 in
    one go (10:24:40). Its first round put the motherbee at a fixed address and changed the options. The
    motherbee registered the change on its next poll (10:24:54) and the watchdog wrote its
    options and restarted Syncthing once (10:25:52).
  - **`lab/posture-check.py -- --stage 3`:** `verdict ok` on the 5 hives: exact options,
    discovery off, no Syncthing UDP socket, the motherbee listening on `*:22000`, the
    spokes on `127.0.0.1:22000`, all connected to the motherbee via `tcp://10.10.10.10:22000`,
    completion 100%.
  - **Reconnection:** after restarting the motherbee's Syncthing, the 4 spokes came back in 10.5 s.
  - **Dist publishing:** a test file in `dist/vendor` reached the 4 spokes in ~1 s
    after the scan, and its deletion in ~3 s.
  - **OPA** `in_sync` 4/4; **health** since 10:18 UTC: 0 units down, 0 denials, 0
    origin drops; `ops versions`: all 5 on 0.1.59, manifest `d0803b54ab5caf58`.
  - **No restart loop:** Syncthing's uptime grows across three samples every ~130 s on all 5
    (motherbee 69 → 203 → 331 s; worker2 218 → 348 → 480 s; the rest the same).
- **Rollback:** `apt install fluxbee=0.1.58` on the motherbee + core-update of the spokes, or the
  snapshot `pre-stage3-0-1-59`. Going back to 0.1.58 does not revert the options (nobody touches them) and the
  links keep working: the spokes dial a fixed address.

## 0.1.58 — Syncthing: the spokes dial the motherbee and the motherbee only accepts (stage 2 of the posture); `blob/active` only for workers

- **Date:** 2026-10-06 (ART) · **Previous version:** 0.1.57 · **Commits:** `0a35329`..`9d9391b`
  (FINDINGS A-47, A-50, A-51, A-52, A-53; logbook `2026-10-06`; design
  `docs/host-posture-and-exposure-spec-v1.md`, revision 8, §3.5 "as built").
- **Scope:** motherbee + the three spokes (core-update). `worker2` (VM 104), joined today on 0.1.57
  for stage 3's version-skipping test, was left **powered off and out of the deploy** on
  purpose.
- **What changed:**
  - **Addresses (A-47, stage 2):** the watchdog reconciles every ~60 s. On each spoke, the motherbee's
    device moves to `tcp://<uplink IP>:22000`. On the motherbee, the hives' devices
    stay `dynamic` (it only accepts), and its device is identified by the ID that each entry records.
    The egress's ID, which had never been registered, is filled in by name.
  - **A-50:** the motherbee was sharing `blob/active` with the egress. The join only shares it with
    workers, and the reconciliation removes each spoke from the folders its role must not have (a rule
    by role, never by flags). New folders are born without members.
  - **A single writer of `config.xml`**, hardened against the Syncthing user's directory
    (without following symlinks, `O_EXCL` temporary, owner and mode by descriptor). CI guard.
  - **A-53:** the local ID comes from `syncthing device-id` (v2 has no `--device-id`).
  - Orphans: deleted only with the complete registry and every hive at rest and registered.
  - An ingress join waits 90 s for the spoke to dial.
  - Four adversarial code reviews before the release.
- **Build:** 7 min (commit `9d9391b`). **CI:** `rust-tests`, `posture-guards` and
  `router-dispatcher-guards` green. **Publish:** 10 s, 58 packages. **Deploy:** 202 s with
  `ops deploy`; snapshots `pre-stage2-0-1-58` on all 4 VMs (`pre-root-tenant-0-1-55` was deleted first).
- **Live verification (stage 2 gate):**
  - **First round, as predicted:** on the motherbee (08:51:10 UTC) ingress1 moved to `dynamic`,
    egress1 left `fluxbee-blob`, its device ID was registered and Syncthing restarted **once**;
    on worker1 (08:51:47) the motherbee moved to `tcp://10.10.10.10:22000`, with one restart.
    ingress1 and egress1 did not change (Syncthing up for 461 k seconds).
  - **`lab/posture-check.py -- --offline worker2`:** `verdict ok` on all 4. Completion 100% on the
    motherbee for the three PROD spokes; the pending offer of `blob/active` disappeared from the
    egress; the 4 registry entries have a device ID.
  - **Listen:** `default` on all 4, unchanged. **OPA:** `in_sync` 4/4. **Warnings:** zero Syncthing WARN
    lines in the 4 orchestrators since the deploy.
  - **No restart loop:** Syncthing's uptime grows across three samples every ~130 s
    (motherbee 99 → 228 → 359 s; worker1 64 → 194 → 326 s; ingress1 and egress1 not restarted).
  - **Health** since 08:48 UTC: 0 units down, 0 denials, 0 origin drops on
    all 4. `ops versions`: all 4 on 0.1.58, manifest `043d3d6ac6f51896`.
- **Rollback:** `apt install fluxbee=0.1.57` on the motherbee + core-update of the spokes, or the
  snapshot `pre-stage2-0-1-58`. The changes in `config.xml` (addresses, the egress's membership) do not
  break 0.1.57: nobody reverts them and they are still correct.

## 0.1.57 — Archi and the admin only on loopback (stage 1 of the posture), no off button on the motherbee, logs without colors

- **Date:** 2026-10-06 (ART) · **Previous version:** 0.1.56 · **Commits:** `d0c957d`..`a703b89`
  (FINDINGS A-31, A-45, A-46, A-48; logbook `2026-10-06`; design
  `docs/host-posture-and-exposure-spec-v1.md`, revision 5).
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **Stage 1 of the posture (A-46):**
    - `architect.listen` moves to `127.0.0.1:3000` in the example and in the dev config;
    - the postinst runs `fluxbee-migrate-config`, which moves to loopback the `0.0.0.0:3000` that
      the installed `hive.yaml` files carried;
    - the orchestrator checks every ~60 s whether Archi or the admin listen outside loopback, and leaves
      a `posture` drift alert per process;
    - docs and firstboot point to loopback plus an SSH tunnel;
    - CI guard on every push.
  - **The motherbee rejects `ADD_HIVE_FINALIZE` and `REMOVE_HIVE_CLEANUP`** (`FORBIDDEN`): they were an
    off button that any orchestrator, including the ingress's, could press (A-48,
    brought forward; DTAP round 3).
  - **A-31:** ANSI colors in the logs only with a terminal.
  - **io.web in the core's IO list** (root tenant).
  - **The Docker lab was deleted** (it was not used), and so was its quickstart on the site. The site changes
    only with the merge to `main`.
- **Build:** 20 min (commit `a703b89`). **Publish:** 10 s, 57 packages. **Deploy:** 218 s with
  `ops deploy`; snapshots `pre-posture-0-1-57` on all 4 VMs (`pre-gov-0-1-54` was deleted first).
- **Live verification (stage 1 validation):**
  - **Migration:** the motherbee's `hive.yaml` ended up at `listen: "127.0.0.1:3000"`, and the postinst
    left its line in the apt log.
  - **Listeners:** `ss` on the motherbee shows 3000 (sy-architect) and 8080 (sy-admin) only on
    `127.0.0.1`.
  - **Ports:** `lab/posture-probe.sh 10.10.10.10 3000 8080 9000` from worker1, ingress,
    egress and fb-build gave `refused`, `refused`, `open` on all 4. In 0.1.56 it was `open`, `refused`,
    `open`: **the ingress no longer reaches Archi**.
  - **Local:** on the motherbee, `curl 127.0.0.1:3000/` and `curl 127.0.0.1:8080/hives` give 200.
  - **Positive control:** a fake listener called `sy-admin` on `10.10.10.10:39555` raised the
    `loopback_service_exposed_sy-admin` alert after 33 s. It is the only `posture` alert: Archi
    raised none after the deploy.
  - **A-45 measured for the first time.** Downtime of each motherbee service during the upgrade:

    | Service | 0.1.55 | 0.1.57 |
    |---|---|---|
    | rt-gateway | 70 s | 0 s |
    | sy-vault | 78 s | 1 s |
    | sy-storage | 84 s | 5 s |
    | sy-identity | 86 s | 5 s |
    | the rest | — | 0 s |

    Only the ones that write the manifest stop: the orchestrator was down for 44 s, and the admin until
    the ordered restart.
  - **A-31:** zero ANSI codes in the new processes on the 4 hosts. Before the upgrade there were
    hundreds per service on the motherbee. The only lines with color are the "SIGTERM received"
    of the old processes.
  - **The 4 hives on 0.1.57** (hash `9e13e6bf…`): the motherbee by dist, the spokes by installed
    hash. Health with 0 failures, 0 denials and 0 srcdrops.
  - **CI:** `rust-tests`, `posture-guards` (first run) and `router-dispatcher-guards` green
    on `a703b89`.
- **Not tested live:** the rejection of the spoke-only actions on the motherbee. Its unit test
  covers it: there is no clean way to send them as if it were the ingress's orchestrator.

## 0.1.56 — one connection per UUID, upgrades without shutting down the core, only io.cloud and io.blob in the root tenant

- **Date:** 2026-10-03 (ART) · **Previous version:** 0.1.55 · **Commits:** `068014d`..`715143a`
  (FINDINGS A-16, A-43, A-44, A-45, B-16; logbook `2026-10-03`).
- **Scope:** motherbee + the three spokes (core-update).
- **Before the deploy, by hand in PROD (the operator, because the permission classifier blocked it
  for the session):** `IO.api@motherbee` and `IO.wapp.default@motherbee` were purged, with their
  ILKs and their UUID files. Their configs were saved outside the repo; they only had `_system` and
  `tenant_id`.
- **What changed:**
  - **A-44:**
    - the router accepts a single live connection per UUID, with a 1 s wait for transient
      duplicates;
    - every connection that ends is cleaned up, even on error (before, it stayed registered with
      nobody reading it).
  - **A-45:** an upgrade only stops the ones that write the runtimes manifest. The new orchestrator
    restarts in order what runs replaced binaries, and needrestart leaves the core to the
    orchestrator.
  - **A-43:** only io.cloud and io.blob run in the root tenant and start by default
    (`TENANT_ROOT_NOT_ALLOWED`).
  - **A-16:** the watchdog reverts the local changes of the receive-only folders.
- **Build:** 20.5 min. **Publish:** 11 s, 56 packages. **Deploy:** 214 s with `ops deploy`;
  snapshots `pre-root-io-0-1-56` on all 4 VMs (`pre-ai-generic-0-1-53` was deleted first).
- **Deploy incident (B-16):**
  - The core updates went out before the motherbee's router had the spokes back.
    All three gave `TRANSPORT_ERROR` and did not arrive.
  - Even so, `ops.py` counted them as updated: it judged by the dist hash, not by the installed one.
  - The binaries check showed it. The update was sent again by hand at 06:39:52 UTC and
    `ops.py` was corrected.
- **Live verification:**
  - **U-8b validated:** the three spokes answered `status=ok phase=restarting` (9, 5 and 4
    components) and ended at `core.last_update` `done ok`. worker1 restarted the services in
    order and then restarted itself.
  - The 4 hives on 0.1.56: the motherbee by dist and the spokes by installed hash (`3260c886…`).
    No `(deleted)` binary and 0 ERROR lines, on all 4.
  - **A-39 still fine:** IO.blob and IO.cloud started only once, on 0.1.56, with zero
    `203/EXEC`. They are the only managed nodes left on the motherbee.
  - **A-44:** 0 rejected HELLOs, 0 connections ended by error and 0 failed handshakes in the
    normal traffic of the 4 hives. The Go nodes started fine with their double HELLO.
  - **A-43:** a `run_node` of `IO.api.a43probe@motherbee` in the root tenant gave
    `TENANT_ROOT_NOT_ALLOWED`, leaving nothing created (no ILK, no UUID file, no unit).
  - **A-45:**
    - the new `prerm` and the needrestart conf were installed;
    - this upgrade still stopped the whole core, between 06:33:26 and 06:34:58, because it runs
      0.1.55's `prerm`;
    - it is measured in the next upgrade.
  - **A-16, validated after the deploy with the operator's OK:** a test file in
    worker1's `dist/vendor` was reverted in 16 s, with its WARN.
  - **Identity and frontdesk:** 0 errors. Worker1's replica did the full sync after its
    restart.
  - **CI:** `rust-tests`, `admin-catalog-guard` and `router-dispatcher-guards` green on `715143a`.
- **My own error, without consequences:**
  - To read the version I ran `/usr/bin/sy-orchestrator --version` on worker1. The binary does not
    have that flag and started as orchestrator.
  - It died before connecting to the router: there was no new registration in worker1's router and the
    real orchestrator stayed reachable.
  - Core binaries must not be run to check versions; `/versions` is used.
- **Rollback:** snapshot `pre-root-io-0-1-56` or `apt install fluxbee=0.1.55` (the spokes, by core
  update or `core_rollback`).

## 0.1.55 — nobody registers in the root tenant, U-8b, io.slack without a base instance

- **Date:** 2026-10-02 (ART) · **Previous version:** 0.1.54 · **Commits:** `a8c6349`..`975fd34`
  (FINDINGS A-37, A-40, A-43, A-44, A-45; PENDING-BUGS U-8b; logbook `2026-10-02`).
- **Scope:** motherbee + the three spokes (core-update).
- **Before the deploy, by hand in PROD (the operator's request):** `IO.slack.default@motherbee` was deleted
  with purge; it ran in the root tenant and did not have its ILK (A-40). Its config was saved outside the
  repo.
- **What changed:**
  - **Root tenant:** nobody registers there. SY.identity rejects it for any caller
    (`TENANT_ROOT_NOT_REGISTRABLE`), the frontdesk answers `TENANT_NOT_REGISTRABLE` without an LLM and
    `register_human` rejects it before provisioning.
  - **U-8b:** a spoke's core update answers `phase: restarting` before restarting, and leaves the
    result in `/versions` as `core.last_update`.
  - **io.slack:** no startup instance; each binding is launched by its tenant.
  - **io.slack/io.wapp frontdesk gate:** the handoff goes as a `user` message; as `data`, the
    frontdesk ignored it.
  - **Router SHM (A-44):** a node that registers again keeps its slot, and the reader no longer
    loses the last node when there is a gap.
  - **Lab:** the identity e2e tests create their tenants through SY.admin.
- **Build:** 17.5 min. **Publish:** 10 s, 55 packages. **Deploy:** 233 s with `ops deploy`;
  snapshots `pre-root-tenant-0-1-55` on all 4 VMs (`pre-cleanup-0-1-52` was deleted first).
- **Live verification:**
  - The 4 hives on 0.1.55 with the same manifest (`852a21cd…`). No `(deleted)` binary and 0
    ERROR lines since the install, on all 4.
  - **A-39 still fine:** IO.api, IO.blob, IO.cloud and IO.wapp.default started only once, on
    0.1.55, with zero `203/EXEC`; the orchestrator repointed them.
  - **Identity and frontdesk:** 0 errors. Worker1's replica did the full sync and subscribed to
    the deltas.
  - **U-8b, not yet:** the spokes received the update with the 0.1.54 orchestrator, so
    ingress1 and egress1 gave `TIMEOUT` as before (worker1 answered in time).
    `core.last_update` is in `/versions`, at `null`. The answer before the restart shows up in the
    next release.
  - **Not tested live:** the root tenant block, which would require registering someone in PROD.
    The tests and the `identity_negative` e2e cover it.
  - **Seen while verifying (A-45, pre-existing):** the motherbee's upgrade leaves the whole core down for
    70–86 s, because the `prerm` stops the services before unpacking. The replicas show it
    as ~2 reconnection warnings per second.
  - **CI:** `rust-tests` and `router-dispatcher-guards` green on `975fd34`.
- **Rollback:** snapshot `pre-root-tenant-0-1-55` or `apt install fluxbee=0.1.54`.

## 0.1.54 — upgrades without downtime for the managed nodes, the .gov path closed, cognition A-38

- **Date:** 2026-10-02 (ART) · **Previous version:** 0.1.53 · **Commits:** `7c9f781`..`6aa4ee4`
  (FINDINGS A-37, A-38, A-39, A-41; logbook `2026-10-02`).
- **Scope:** motherbee + the three spokes (core-update).
- **Before the deploy, by hand in PROD (the operator's request):**
  - `AI.chat@motherbee` was deleted with purge (instance and ILK);
  - IO.slack.default had its `io.dst_node` removed, which pointed to AI.chat (config v4);
  - 12 `.uuid` files of already-deleted nodes were deleted.
- **What changed:**
  - **A-39:** the orchestrator's startup relaunches on the new version the nodes that follow
    `current`, and `needrestart` no longer restarts the `fluxbee-node-*` units.
  - **A-41:** a node's purge deletes its UUID.
  - **A-37, .gov:**
    - registration goes to the case's tenant and the frontdesk does not create tenants;
    - merge by email: fills in only empty fields and moves the channels;
    - `TENANT_SUSPENDED`;
    - `register_human` reports the final ILK.
  - **A-38, cognition:** `context_close`/`reason_close` and the most recent first in the memory region.
- **Build:** 13.5 min. **Publish:** 11 s, 54 packages. **Deploy:** 231 s with `ops deploy`; snapshots
  `pre-gov-0-1-54` on all 4 VMs (`pre-query-decode-0-1-51` was deleted first).
- **Live verification:**
  - The 4 hives on 0.1.54 with the same manifest (`7b87d5c1…`). No `(deleted)` binary, 0 ERROR
    lines since the install and 0 gate rejections and drops by `routing.src`.
  - **A-39 validated:**
    - the five managed nodes started only once, on 0.1.54, with zero `203/EXEC`;
    - the orchestrator repointed them ("running node follows 'current' and the pointer moved");
    - `/etc/needrestart/conf.d/fluxbee.conf` was installed.
  - **Cognition:** rejected the old thresholds of its persisted config (warning) and applies the
    defaults; CONFIG_GET reports `context_close`/`reason_close` 0.25, the same effect as before.
  - **Identity and frontdesk:** 0 errors since the install.
  - **CI:** `rust-tests` and the guards green on `6aa4ee4`.
- **Rollback:** snapshot `pre-gov-0-1-54` or `apt install fluxbee=0.1.53`.

## 0.1.53 — dead code cleanup: the general AI key, no base AI instance, privacy and cognition

- **Date:** 2026-10-02 (ART) · **Previous version:** 0.1.52 · **Commits:** `8c8c0fa`..`26edcd5` (eight,
  one per topic; logbook `2026-10-02`, FINDINGS A-32 to A-38).
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - **ai.generic** takes the general key of the hive's provider from SY.vault (the operator's
    decision D4). `behavior.vault_key` is rejected (A-35). The gov mode left over from the frontdesk
    split goes away.
  - **The base installation** no longer brings up `AI.chat@motherbee`. Everything points to `ai.generic`: admin,
    Archi, cookbook seeds and docs (A-36).
  - **SY.frontdesk.gov** without personal data in the logs (A-32). The identity rejections keep
    their code and only the transient ones are retryable (A-33).
  - **SY.identity:** does not log the address in `ILK_PROVISION` nor return Postgres's DETAIL (A-32).
  - **SY.cognition:** the cold start runs after waiting for the vault and rebuilds with the
    keys of the live path; the state stays bounded and the scopes are trimmed (A-34).
  - **Orchestrator:** 9 fewer dead functions. Workspace warnings: 0.
  - **CI:** the actions on Node 24.
- **Build:** 14 min. **Publish:** 10 s, 53 packages. **Deploy:** 223 s with `ops deploy`; snapshots
  `pre-ai-generic-0-1-53` on all 4 VMs (`pre-config-routes-0-1-50` was deleted first).
- **Live verification:**
  - The 4 hives on 0.1.53 with the same manifest (`dc605aa6…`). No `(deleted)` binary, 0 ERROR
    lines since the install and 0 gate rejections and drops by `routing.src`.
  - **Cognition:** the startup rebuild ran for the first time (`trigger="startup"`, after
    waiting for the vault; 0 threads, PROD has no data). CONFIG_GET reports `storage.db_configured: true`.
  - **AI.chat:** starts in FAILED_CONFIG with *"config.json rejected… send a valid CONFIG_SET"*,
    because its config has `vault_key`. That is expected (A-36).
  - **SY.frontdesk.gov:** starts and stays degraded (Unconfigured): PROD's vault has no AI
    key. The structured handoff works without it.
  - **CI:** `rust-tests`, `go-tests` and the two guards green on `26edcd5`.
- **Seen while validating (A-39, pre-existing):** the motherbee's six managed nodes stayed
  between 20 and 80 s in a `203/EXEC` loop during the update, because the directory of their old version was
  deleted before the unit pointed to the new one.
- **Rollback:** snapshot `pre-ai-generic-0-1-53` or `apt install fluxbee=0.1.52`.

## 0.1.52 — warning cleanup with no behavior change; first deploy with `lab/ops.py`

- **Date:** 2026-10-02 (ART) · **Previous version:** 0.1.51 · **Commit:** `0670184` (the code
  is `e1f0323`; the rest are tools and documents from the batch of small topics, logbook
  `2026-10-02`).
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - No behavior change: removed fields nobody read, unused SHM accessors and imports; three
    test seams move to `cfg(test)`. Root workspace warnings: 64 → 31 (what remains is
    dead code awaiting the operator's approval). The identity region's layout gives the
    same `total_len` in the SDK and in the router.
  - Outside the `.deb`: `lab/ops.py` (build, publish, deploy) and the incremental publish (`fcb5af6`).
- **Build:** 21 min with `ops build 0.1.52 --wait`. **Publish:** 9 s, 52 packages.
- **Deploy:** `ops deploy 0.1.52 --snapshot pre-cleanup-0-1-52 --drop pre-architect-fixes-0-1-49`,
  226 s end to end: snapshots on all 4 VMs, install on the motherbee at 10:24 UTC, spokes in
  parallel, health.
- **Live verification:**
  - The 4 hives on 0.1.52 with the same manifest (`250e0aca…`). No unit runs a
    `(deleted)` binary and all restarted after the install, except `fluxbee-syncthing` (the core-update
    does not touch it).
  - 0 ERROR lines in the journals of the 4 VMs since the install. The WARNs are from the restarts
    (RPC and WAN reconnecting) and the already known ones (io_slack without credentials,
    `EGRESS_MOTHERBEE_BYPASS`).
  - Identity: worker1's replica retried while the motherbee's identity was restarting
    (10:25:21–10:27:03 UTC). Then it applied the full sync, subscribed to the delta stream and resolved
    its ILK.
  - OPA: the 4 hives `in_sync` and the routers `ok` (no user policy loaded). The system
    policy authorized the core-updates.
  - 0 gate rejections and 0 drops by `routing.src`.
  - CI: `rust-tests`, `admin-catalog-guard` and `router-dispatcher-guards` green on `0670184`.
- **Rollback:** snapshot `pre-cleanup-0-1-52` or `apt install fluxbee=0.1.51`.

## 0.1.51 — the admin decodes the query; SQLite waits for the lock on every connection

- **Date:** 2026-10-01 (ART) · **Previous version:** 0.1.50 · **Commits:** `9f9982a` (admin) and
  `f7c2520` (sy-timer, wf-generic) (FINDINGS A-30, A-29). The first build of 0.1.51 (with only the
  admin) was redone before publishing it, to add the SQLite fix.
- **Scope:** motherbee + the three spokes (core-update); wf-generic arrives as the runtime
  `wf.engine/0.1.51`.
- **What changed:**
  - The admin decodes `%xx` in the query (`+` stays literal).
  - sy-timer and wf-generic apply their SQLite pragmas on every connection (in the DSN), not only on the
    pool's first one.
- **Build:** 6 min. **Publish:** 51 packages. Snapshots `pre-query-decode-0-1-51` on all 4 VMs
  (`pre-architect-opa-0-1-48` was deleted first).
- **Install:** motherbee 00:48 UTC on 02/10; spokes right after.
- **Live verification:**
  - The 4 hives with the same binaries; 0 `failed`.
  - Tap created and deleted with `match_src=IO.pct-a%40motherbee...` → `ok`, the tap is gone (in
    0.1.50 it answered `NOT_FOUND`).
  - sy-timer active on the motherbee and worker1; `TIMER_LIST` through the admin answers; 0 errors,
    `locked` or `busy` in its logs.
  - Test WF on worker1: created, the node starts from `wf.engine/0.1.51` in ~10 s, opens its
    database and recovers 0 instances without errors; deleted, and the node disappears.
  - 0 gate rejections and 0 drops by `routing.src`.
  - CI: `go-tests` (with the sy-timer suite), `rust-tests`, `admin-catalog-guard` and
    `router-dispatcher-guards` green on this version's commits.
- **Rollback:** snapshot `pre-query-decode-0-1-51` or `apt install fluxbee=0.1.50`.

## 0.1.50 — config-routes closed (VPN per hive) and what was left of OPA

- **Date:** 2026-10-01 (ART) · **Previous version:** 0.1.49 · **Commit:** `aa206ac` (FINDINGS
  A-18, A-21; DTAP panel D-12). The router (`rt-gateway`), the admin and the system policy change.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - A single surface for routes, VPNs and taps: `/hives/{h}/routes|vpns|taps`. `/routes`, `/vpns`
    and `/taps` with `?hive=` were deleted.
  - Rule 5 of `system.rego` no longer names `SY.config-routes`; `system.wasm` and
    `system_route.wasm` recompiled.
  - The router rereads the routing config on every heartbeat, with version control (D-12).
  - A router without a user policy reports `ok`, as after a clear (A-21).
  - `OPA_RELOAD` is no longer forwarded to every local node: only the routers use it.
- **Build:** 23 min (the router's library changed: everything recompiles). **Publish:** 50 packages.
  Snapshots `pre-config-routes-0-1-50` on all 4 VMs (`pre-opa-blockB-0-1-47` was deleted first).
- **Install:** motherbee 00:04 UTC on 02/10; spokes 00:05–00:07.
- **Live verification:**
  - The 4 hives with the same binaries; 0 `failed`.
  - Freshly started and without a policy, the 4 routers report `load=0 ok` (before, `load=1 error`).
  - `/routes`, `/vpns` and `/taps` → `UNKNOWN_ROUTE`; `/hives/{h}/...` answers.
  - A test route on worker1 and a test tap on the motherbee through the per-hive routes: they are listed, and
    each one shows up in the other hive's LSA. Deleted at the end.
  - Global apply and clear: 4.9 s and 5.0 s, the 4 hives. `OPA_RELOAD` applied on each router and
    no "forwarded to local nodes". The heartbeat refresh only logs when the config
    changes (motherbee: 4 updates in the window; logs at a normal rate).
  - 0 gate rejections and 0 drops by `routing.src`.
  - Seen in passing: a tap DELETE with `@` encoded as `%40` did not find the tap (the admin did not
    decode the query). Fixed in 0.1.51.
- **Rollback:** snapshot `pre-config-routes-0-1-50` or `apt install fluxbee=0.1.49`.

## 0.1.49 — architect: a single admin, the handbook in the `.deb`, the vault awaited at startup

- **Date:** 2026-10-01 (ART) · **Previous version:** 0.1.48 · **Commit:** `c921d1a`
  (FINDINGS A-24, A-25, A-26). `sy-architect` and `build-deb.sh` change.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - The snapshot and the plan compiler's `query_hive` query ask `SY.admin@motherbee`
    with the hive as destination (before, `SY.admin@<hive>`, which only exists on the motherbee).
  - The `.deb` installs `/etc/fluxbee/handbook_fluxbee.md`, like `install.sh`.
  - At startup, the architect retries reading its AI key and the URL of its message database until
    the vault answers (like SY.storage and SY.identity), both at once.
- **Build:** 10 min. **Publish:** 49 packages (the wait in `pve.py` dropped on a connection error; the repo
  ended up with 0.1.49). Snapshots `pre-architect-fixes-0-1-49` on all 4 VMs (`pre-opa-blockA-0-1-46`
  was deleted first).
- **Install:** motherbee 21:59 UTC; spokes 21:59–22:01.
- **Live verification:**
  - The 4 hives with the same binaries; 0 `failed`.
  - Handbook at `/etc/fluxbee/handbook_fluxbee.md` (30940 bytes).
  - The architect's startup: both vault reads gave `vault not reachable yet; retrying` and
    1.2 s later it was listening with `messages_db_configured=true` (in 0.1.48 it started at `false` and
    the vault's notice rescued it). `ai_configured=false`: the 8.x vault has no AI key.
  - The motherbee's admin answers the five snapshot reads for worker1. On ingress1 and
    egress1, `runtimes` and `wf-rules` fail (by role): noted in A-24.
  - Global apply and clear: 5.1 s and 5.3 s, the 4 hives. 0 gate rejections and 0 drops by
    `routing.src`; no user policy at the end.
- **Rollback:** snapshot `pre-architect-fixes-0-1-49` or `apt install fluxbee=0.1.48`.

## 0.1.48 — the architect declares the global OPA policy (one per system, the last one wins)

- **Date:** 2026-10-01 (ART) · **Previous version:** 0.1.47 · **Commit:** `1486807`
  (FINDINGS A-23). Only `sy-architect` changes.
- **Scope:** motherbee + the three spokes (core-update; the architect runs only on the motherbee).
- **What changed:**
  - A solution declares `desired_state.opa` (one policy, no hive) instead of per-hive
    `opa_deployments`. A policy with `hive` does not pass validation.
  - The snapshot reads the running policy once, from the motherbee. If the rego is the same, there is no
    change.
  - A single `opa_compile_apply`. `OPA_REMOVE` is now `opa_clear`.
  - If two solutions declare OPA, the last one wins. The owner of the running policy is the solution
    whose stored manifest declared it most recently. A solution only does a clear if the policy
    is still its own, and the plan (`user_policy` in the confirmation) says whose policy it
    replaces.
  - The plan compiler receives the declared rego and the plan's OPA steps are pinned to it (exact rego and
    entrypoint, no hive).
- **Build:** 12 min. **Publish:** 48 packages. Snapshots `pre-architect-opa-0-1-48` on all 4 VMs
  (`pre-rule3-0-1-45` was deleted first).
- **Install:** motherbee 21:19 UTC; spokes 21:19–21:21.
- **Live verification:**
  - The 4 hives with the same binaries; 0 `failed`; the architect active and `/api/status` ok.
  - The real response of the motherbee's `opa_get_policy`, with and without a policy, was captured and kept as a
    test for the snapshot parser.
  - Global apply and clear: 6.2 s and 4.9 s, the 4 hives. 0 gate rejections and 0 drops by
    `routing.src`; no user policy at the end.
  - **Not tested live:** the full pipeline (the designer and the plan compiler are AI, and the
    architect started without AI: A-26). Covered by tests: validation, snapshot, owner rules,
    step pinning and confirmation.
- **Rollback:** snapshot `pre-architect-opa-0-1-48` or `apt install fluxbee=0.1.47`.

## 0.1.47 — OPA, block B of the DTAP panel: a stuck hive says why, half-done installs are repaired, no race in the state, no old rego

- **Date:** 2026-10-01 (ART) · **Previous version:** 0.1.46 · **Commit:** `652a9c9` (DTAP panel
  2026-10-01: P-6, P-7, P-13, P-15, D-9, D-10). Only `sy-opa-rules` (Go) changes.
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - SY.opa.rules's `get_status` says what the hive runs, what it got published
    (`published_version`/`published_hash`), what the router has loaded (`region_hash`), whether it is
    `in_sync`, and whether it is waiting for something: `waiting {version, hash, since, reason}`. After a minute
    stuck, it logs it once.
  - Each 5 s check (motherbee included) rewrites the router's region if it does not have the policy
    installed (an install cut off halfway).
  - The state has its own lock (before, `lastError` without a lock between two goroutines).
  - Installing without a rego deletes the `policy.rego` that a previous policy had left.
  - The published files keep the owner of `dist/policy`, not a fixed `fluxbee`.
- **Build:** 7 min. **Publish:** 47 packages. Snapshots `pre-opa-blockB-0-1-47` on all 4 VMs
  (`pre-src-binding-0-1-44` was deleted first).
- **Install:** motherbee 19:55 UTC; spokes 19:56–19:57.
- **Live verification:**
  - The 4 hives with the same binaries; 0 `failed`.
  - Without a policy: all 4 with `in_sync: true` and the region zeroed. Global apply: 4.7 s, all 4 with
    `in_sync: true` and the region equal to the published hash.
  - On the motherbee, `dist/policy/opa/*` is `fluxbee:fluxbee` and only the current wasm remains. The
    replicas have no `policy.rego`.
  - Stuck hive, forced on egress1: Syncthing cut (nft on :22000), apply of v18 (12 s,
    `pending: [egress1]`, `unreachable: []`) and the motherbee's manifest copied by hand into the local
    copy (the manifest arrives and the wasm does not). Status: `in_sync: false`,
    `waiting {version: 18, reason: "the wasm the manifest names has not arrived"}`; the log at
    1m0s; on unblocking, Syncthing reconnected in ~1:40 and egress1 installed on its own
    (`installed after waiting 2m51s`). Revert of the folder (`/rest/db/revert`) →
    `receiveOnlyTotalItems: 0`.
  - With the sync cut entirely (without the hand copy), egress1 shows `in_sync: true`: it compares against
    what it got. What says it is behind is the `pending` in the admin's report.
  - Final clear: 5.3 s, all 4 without a policy. 0 gate rejections and 0 drops by `routing.src` (the
    only "matches" in the journal were the guest-agent's log with the counting command itself).
  - Not tested live (covered by tests): the repair of the half-written region and the race
    (`-race`).
- **Rollback:** snapshot `pre-opa-blockB-0-1-47` or `apt install fluxbee=0.1.46`.

## 0.1.46 — OPA, block A of the DTAP panel: architect, honest report, bounded waits, re-publishing, a single write path

- **Date:** 2026-10-01 (ART) · **Previous version:** 0.1.45 · **Commit:** `7371ca1` (DTAP panel
  2026-10-01: D-1…D-8, P-4, P-5, P-12, P-14, A-1…A-3, T-5, T-11, T-13)
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - The architect translates the global OPA writes (`/opa/policy*`, including clear) and gives them 80 s.
  - The admin waits 10 s for the hives' answers (before, 30) and at most 5 s for the scan; if the
    motherbee does not answer the step that changes the policy, it answers `TIMEOUT` (before, `200 ok`).
  - The report waits for every `connected` hive in the registry (reachable or not), marks
    `unreachable` and only counts the answer from `SY.opa.rules@<hive>` with the announced hash.
  - The motherbee re-publishes if the manifest does not name the running policy.
  - `CONFIG_SET` on SY.opa.rules is read-only.
- **Build:** 11 min. **Publish:** 46 packages. Snapshots `pre-opa-blockA-0-1-46` on all 4 VMs
  (`pre-sync-hint-0-1-43` was deleted first).
- **Install:** motherbee 18:47 UTC; spokes 18:48–18:52.
- **Live verification:**
  - The 4 hives with the same binaries; 0 `failed`.
  - `CONFIG_SET` with `compile_apply` → `UNSUPPORTED_OPERATION`; the policy did not change.
  - Global apply and clear: ~5 s, the registry's 4 hives in `running_hives`, `unreachable` empty.
  - With egress1's Syncthing cut (nft on :22000): the write came back in 13.6 s (before, ≥32 s)
    with `pending: [egress1]` and `unreachable: []`; on unblocking, egress1 caught up in 7 s.
  - The motherbee's manifest altered by hand (fake hash): re-published in 5.0 s.
  - 0 gate rejections and 0 drops by `routing.src` on the 4 routers; no user policy at the
    end.
  - Not tested live (covered by tests): the architect's translation, the change step's `TIMEOUT`
    and the `PUBLISH_FAILED` answer.
- **Rollback:** snapshot `pre-opa-blockA-0-1-46` or `apt install fluxbee=0.1.45`.

## 0.1.45 — the orchestrator can only send what it forwards; the dead config signals are gone

- **Date:** 2026-10-01 (ART) · **Previous version:** 0.1.44 · **Commit:** `d6505ff` (FINDINGS A-20)
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - Rule 3 of the system policy: an `SY.orchestrator` can only send the 16 actions that it
    really forwards to another hive (before, all the protected ones except edge). Left out are `CONFIG_SET`,
    `CONFIG_GET`, `CONFIG_CHANGED`, `SYSTEM_UPDATE`, `SYSTEM_SYNC_HINT` and `INVENTORY_REQUEST`.
  - Deleted what nobody used: the orchestrator's `CONFIG_CHANGED {node_config}` and the
    `notify` flag; the extra `CONFIG_RESPONSE` after `set_storage`; sy-wf-rules's CONFIG_CHANGED handler
    (it applied workflows without checking origin); the io-slack branch. CONFIG_CHANGED is left
    with one sender (SY.admin) and one receiver (SY.opa.rules).
- **Build:** 17 min. **Publish:** 45 packages. Snapshots `pre-rule3-0-1-45` on all 4 VMs
  (`pre-opa-fixes-0-1-42` was deleted first).
- **Install:** motherbee 15:22 UTC; spokes 15:25–15:28.
- **Live verification:**
  - The 4 hives with the same binaries; 0 `failed`.
  - WF on worker1 (what exercises the most forwarding between orchestrators): created in 15 s, node HEALTHY;
    clean deletion on the motherbee and worker1 (no directory, no entry in the manifest, no unit).
  - Reads across hives (`nodes`, `versions`) on worker1, ingress1 and egress1: OK.
  - **0 gate rejections and 0 drops by `routing.src`** on the 4 routers since the deploy.
  - OPA: apply, apply and clear `converged` (10 / 6 / 4 s).
- **Rollback:** snapshot `pre-rule3-0-1-45` or `apt install fluxbee=0.1.44`.

## 0.1.44 — a node only sends as itself; the routes notice is gone; faster OPA

- **Date:** 2026-10-01 (ART) · **Previous version:** 0.1.43 · **Commit:** `005d0ad` (FINDINGS A-22,
  A-18; logbook 2026-09-30)
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - Security: the router drops a frame whose `routing.src` is not the UUID that socket
    did its HELLO with. Before, a node that knew another's UUID could send as it (A-22).
  - The CONFIG_CHANGED that the admin sent after each add/remove of a route, VPN or tap was deleted
    (nobody used it), and so was the router's special handling of CONFIG_CHANGED.
  - After an OPA notice, each hive rechecks its copy every 1 s for 30 s.
- **Build:** 17 min. **Publish:** ~6 min (44 packages). Snapshots `pre-src-binding-0-1-44` on all 4
  VMs (`pre-opa-global-0-1-41` was deleted first).
- **Install:** motherbee 03:03 UTC; spokes 03:05–03:08.
- **Live verification:**
  - The 4 hives with the same binaries; 0 `failed`; 0 nodes down.
  - Impersonation probe on the motherbee: with the admin's UUID, the router now drops it
    (`routing.src is not the sending node`) and SY.config.routes answers nothing; with 0.1.43 it answered.
    No drops of legitimate traffic on the 4 hives.
  - OPA: apply A, apply B, rollback and clear in ~5 s each (before, 8–10 s), all `converged`.
  - Routes without the notice: one from the motherbee shows up in worker1's LSA region after ~11 s and goes away after
    ~7 s; one from worker1 reaches the motherbee's LSA after ~6 s, without touching the motherbee's
    config.
  - Final state: no user policy and no test routes.
- **Rollback:** snapshot `pre-src-binding-0-1-44` or `apt install fluxbee=0.1.43`.

## 0.1.43 — the sync hint accepts a specific dist folder (closes the policy scan)

- **Date:** 2026-09-30 (ART) · **Previous version:** 0.1.42 · **Commit:** `2b3abb2` (FINDINGS A-19)
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:** the sync hint's `dist` channel accepts any `fluxbee-dist*`; that way the
  scan of `fluxbee-dist-policy` that the admin requests right after publishing a policy works.
- **Build:** 9 min. **Publish:** ~6 min (43 packages). Snapshots `pre-sync-hint-0-1-43` on all 4 VMs
  (`pre-purge-order-0-1-40` was deleted first).
- **Install:** motherbee 01:43 UTC; this time it waited for the admin to answer before the
  spokes (01:45–01:48).
- **Live verification:**
  - The 4 hives with the same binaries; 0 `failed`.
  - apply A (v9) 8.4 s · apply B (v10) 10.5 s · rollback (back to A) 8.4 s · clear 8.3 s: all
    four `converged: true`, with the right policy on the 4 hives and their routers. The motherbee
    scans ~0.5 s after applying; the rest is the transfer and each hive's 5 s check.
  - config-routes: add and remove of a route on worker1 (v12, v13) with the motherbee untouched (v8); worker1's
    router picks up the change from its SHM instantly.
  - Final state: no hive with a user policy.
- **Rollback:** snapshot `pre-sync-hint-0-1-43` or `apt install fluxbee=0.1.42`.

## 0.1.42 — OPA: scan on publish; only the primary admin writes

- **Date:** 2026-09-30 (ART) · **Previous version:** 0.1.41 · **Commit:** `ac301d9` (FINDINGS
  A-19, A-20)
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:** the admin asks the motherbee's orchestrator to scan `fluxbee-dist-policy`
  before the notice; SY.opa.rules takes CONFIG_CHANGED only from `SY.admin@motherbee`;
  SY.config.routes no longer applies lists that arrive through CONFIG_CHANGED; the answer of
  `POST /opa/policy` says `compile_apply`.
- **Build:** 8 min. **Publish:** ~9 min (42 packages). Snapshots `pre-opa-fixes-0-1-42` on all 4 VMs
  (`pre-identity-baseline-0-1-39` was deleted first).
- **Install:** motherbee 01:16 UTC; spokes 01:18–01:21. A first core-update went out without a hash because
  the admin was still starting: they rejected it, with no effect.
- **Live verification:** identical binaries on the 4 hives, 0 `failed`. **The scan did not work:** the
  orchestrator rejects `folder_id: fluxbee-dist-policy` (`INVALID_REQUEST`, it only accepts
  `fluxbee-dist`) and without a scan the watcher also held back the apply: after 33 s only the motherbee
  was running v8, and the following clear merged with it into a single scan at 64 s → fixed
  in 0.1.43.
- **Rollback:** snapshot `pre-opa-fixes-0-1-42` or `apt install fluxbee=0.1.41`.

## 0.1.41 — a single user OPA policy on every hive, distributed by Syncthing

- **Date:** 2026-09-30 (ART) · **Previous version:** 0.1.40 · **Commit:** `72891be` (FINDINGS A-7;
  logbook 2026-09-30; [`docs/opa-distribution.md`](../docs/opa-distribution.md))
- **Scope:** motherbee + the three spokes (core-update) + `hive.yaml` of the motherbee, ingress1 and
  egress1 (SY.opa.rules on ingress/egress; recipe in HANDBOOK §12).
- **What changed:**
  - SY.opa.rules runs on the 4 hives. Only the motherbee compiles: it applies and publishes the wasm in the
    Syncthing folder `fluxbee-dist-policy`; the other hives install it if the sha256 matches.
  - The OPA writes are global (`/opa/policy*`); those for a specific hive are rejected. The
    answer says which hives already run it and which are still pending.
  - CONFIG_CHANGED travels through normal routing and crosses hives;
    `PUT /config/routes|vpns|taps` were removed.
- **Build:** 17 min. **Publish:** ~9 min (`dpkg-scanpackages` over 41 packages). Snapshots
  `pre-opa-global-0-1-41` on all 4 VMs (`pre-wf-mirror-0-1-38` was deleted first).
- **Migration (00:25–00:39 UTC):** the motherbee's `hive.yaml` edited before the `apt install` (backup
  `hive.yaml.pre-opa-0-1-41`); the orchestrator built `dist/core/ingress` with 5 components and
  `dist/core/egress` with 4, and the binary reached ingress1/egress1 in under a minute; then
  the `hive.yaml` of those two spokes and a core-update to the three (TIMEOUT as always; the binaries
  ended up with the same hash as on the motherbee).
- **Live verification:**
  - SY.opa.rules `running` on the 4 hives (on ingress1/egress1 the unit was created by the orchestrator at
    startup). Folder `fluxbee-dist-policy`: `sendonly` with 4 devices on the motherbee (inherited
    from vendor) and `receiveonly` with 2 on each spoke. 0 `failed` services.
  - `POST /opa/policy` (v6): `converged: true` in 16 s; the 4 routers with v6 loaded.
  - Catching up: with egress1's Syncthing cut (temporary nft on :22000), v7 answered
    `pending: [egress1]` and egress1 stayed on its v6; on unblocking it installed v7 on its own, in 27 s.
  - Clear: the motherbee cleared instantly, but the spokes took ~63 s (Syncthing's watcher
    holds a change made only of renames and deletions for 60 s) and the answer went out with all three
    pending → fixed in 0.1.42.
  - Write for a specific hive → 400; old route `/hives/{h}/opa/policy/clear` → 404. On
    ingress1/egress1: `/var/lib/fluxbee/opa` `700`, SHM `600`.
  - Factory reset in dry-run: it now lists the policy of the 4 hives.
- **Rollback:** snapshot `pre-opa-global-0-1-41`, or `apt install fluxbee=0.1.40` + restore
  `hive.yaml.pre-opa-0-1-41` on the motherbee, ingress1 and egress1.
  - **Note (2026-10-01, DTAP panel P-9):** before the `apt install`, `POST /opa/policy/clear` and
    wait for `converged: true`; otherwise, the last global policy stays applied on the workers and in
    memory in the ingress/egress routers. See HANDBOOK §12, "The rollback".

## 0.1.40 — WF purge on the motherbee in order and tolerant of "no longer exists"

- **Date:** 2026-09-30 (ART) · **Previous version:** 0.1.39 · **Commit:** `43dd174` (FINDINGS A-15)
- **What changed:** on a worker, wf-rules purges on the motherbee leaving the current version for last
  (the admin does not let it be deleted while there are others) and treats `RUNTIME_NOT_FOUND` /
  `RUNTIME_VERSION_NOT_FOUND` as done.
- **Build:** 7 min. **Install:** motherbee 23:03 UTC; spokes 23:05 (only wf-rules changed).
- **Live verification — full cycle of a WF on worker1:**
  - create → node HEALTHY on 0.0.1 (18 s: publishes, waits for the sync, spawns);
  - re-apply with a change → HEALTHY on 0.0.2 (18 s);
  - delete → OK; the motherbee is left without versions or a manifest entry; worker1's mirror
    with 0 local changes (19/19).
  - Cleanup: `AI.vaultprobe@worker1` deleted with its ILK purged (the primary goes back to 24 ILKs,
    as at the start of the day); no test nodes or scripts on the VMs.
- **Rollback:** snapshot `pre-purge-order-0-1-40` or `apt install fluxbee=0.1.39`.

## 0.1.39 — identity does not delete the ILKs of a hive that started without a primary; targeted WF purge

- **Date:** 2026-09-30 (ART) · **Previous version:** 0.1.38 · **Commits:** `2248612`, `85c3244`
  (FINDINGS A-15, A-17). A first build of 0.1.39 without `85c3244` was redone before being published.
- **What changed:** the identity replica does not publish its set of self-owned ILKs until it has a baseline
  from the primary; wf-rules targets `remove_runtime_version` at the motherbee (`target`).
- **Install:** motherbee 22:38 UTC; spokes 22:41 (only identity and wf-rules were restarted, which were
  the changed binaries).
- **Live verification:**
  - Worker1's replica started with the primary blocked (temporary nft): it logged
    `holding its self-owned snapshot` while it had no baseline; when it came back, a snapshot of 25 ILKs. The
    primary **stayed at 25, with the ILK of `AI.vaultprobe@worker1`** (with 0.1.38 it had gone from
    25 to 24).
  - Workflow on worker1: created in 3 s, node HEALTHY. Delete: the motherbee deleted 0.0.1 and the
    sync cleaned worker1 without local changes, but the purge stopped at 0.0.2 (`RUNTIME_NOT_FOUND`:
    orphan directory) → fixed in 0.1.40.
- **Rollback:** snapshot `pre-identity-baseline-0-1-39` or `apt install fluxbee=0.1.38`.

## 0.1.38 — wf-rules does not touch a worker's `dist/` mirror; the orchestrator does not spawn without files

- **Date:** 2026-09-30 (ART) · **Previous version:** 0.1.37 · **Commits:** `676732d`, `20baec3`
  (FINDINGS A-15)
- **What changed:** on a hive that is not the motherbee, wf-rules purges by asking the admin for
  `remove_runtime_version` instead of deleting its mirror; the orchestrator answers
  `RUNTIME_NOT_PRESENT` until the package's `package.json` is on the hive.
- **Build:** 4 min (incremental). **Install:** motherbee 22:07 UTC; spokes 22:10–22:12.
- **One-off repair:** worker1's `dist-runtimes` mirror restored with Syncthing's `POST /rest/db/revert`
  (15 → 0 local changes). Afterwards `WF.w1probe@worker1` was HEALTHY: **the first WF
  running on a worker**.
- **Live verification:** the delete left the mirror clean (0 local changes), but the request to the
  admin went out without `target` (`INVALID_REQUEST`) → fixed in 0.1.39.
- **Rollback:** snapshot `pre-wf-mirror-0-1-38` or `apt install fluxbee=0.1.37`.

## 0.1.37 — what came out of validating 0.1.36: WF spawn on workers, the vault's verdict, OPA permissions

- **Date:** 2026-09-30 (ART) · **Previous version:** 0.1.36 · **Commits:** `3055a40`, `6c708a6`,
  `c09f9af` (FINDINGS A-12 to A-14)
- **Scope:** motherbee + the three spokes (core-update).
- **What changed:**
  - wf-rules waits (up to 20 s) for the freshly published package to reach the hive before spawning.
  - SDK: `VaultClient::get`/`list` keep the vault's verdict (`KEY_NOT_FOUND` no longer arrives
    as a JSON error).
  - SY.opa.rules: SHM `0600` (it was `0666`), policy `0600`, dirs `0700`.
- **Build:** VM110 `build-deb.sh 0.1.37` (22 min) → publish (37 packages). Snapshots
  `pre-followups-0-1-37` on all 4 VMs (the oldest one on each was deleted first).
- **Install:** motherbee 21:37 UTC; spokes 21:40–21:42 (TIMEOUT as always; `rt-gateway` with the
  same hash on the 4 hives).
- **Live verification:**
  - `/dev/shm/jsr-opa-motherbee` and `…-worker1` → `rw-------`; `/var/lib/fluxbee/opa` → `700`.
  - Workflow on worker1: `RUNTIME_NOT_AVAILABLE` at 21:43:39/40/42/46 and spawn OK at 21:43:51,
    when the manifest arrived. **The node ended up in a crash-loop** because of missing package files:
    wf-rules had deleted them from the local mirror (A-15) → fixed in 0.1.38.
- **Rollback:** snapshot `pre-followups-0-1-37` or `apt install fluxbee=0.1.36` + core-update.

## 0.1.36 — config with hive scope, origin control, vault/admin from workers, identity catches up

- **Date:** 2026-09-30 (ART) · **Previous version:** 0.1.35 · **Commits:** `ad48ecb`, `15d53cc`,
  `d24e3aa`
- **Scope:** **motherbee + the three spokes** (core-update to worker1, ingress1 and egress1: the
  router, SY.config.routes, identity, cognition and the Go nodes change).
- **What changed** (logbook 2026-09-30; FINDINGS A-8 to A-11). The four bugs were confirmed
  live on 0.1.35 before touching anything:
  - `ConfigChangedPayload.hive`: an `add_route` on worker1 no longer overwrites the motherbee's routes, and
    an OPA operation targeted at worker1 no longer runs on the motherbee.
  - CONFIG_CHANGED becomes a protected action (gate in the router). SY.config.routes and
    SY.opa.rules only accept mutations and commands from `SY.admin@motherbee`.
  - `VaultClient::for_primary` and wf-rules → `SY.admin@motherbee`: AI, cognition, IO and WF on a
    worker reach the vault and the admin.
  - The identity replica resyncs with a snapshot on every subscription.
- **Build:** VM110 `build-deb.sh 0.1.36` (22 min: the SDK changed) → publish (36 packages).
  Snapshots `pre-config-scope-0-1-36` on all 4 VMs (the oldest one on VM100 was deleted first).
- **Install:** motherbee 20:42→20:43 UTC; core-update of the spokes → `TIMEOUT` as always, but
  applied: `rt-gateway` with the same hash on the 4 hives and services restarted 20:44–20:46.
- **Live verification** (the same steps that confirmed each bug):
  - Two additions on worker1 → the motherbee stays at 0 routes; its SY.config.routes logs
    `config changed addressed to another hive; ignoring`.
  - Fake node: CONFIG_CHANGED → `router dropped CONFIG_CHANGED from unauthorized origin`;
    direct `add_route` → `FORBIDDEN`; `rollback_policy` → `UNAUTHORIZED`. Routes intact.
  - The orchestrator's legitimate `node_config` on worker1 → passes the gate.
  - AI and cognition on worker1 query `SY.vault@motherbee` (before, `NODE_NOT_FOUND` against
    `SY.vault@worker1`).
  - Workflow on worker1 → **publishes** (before, `PACKAGE_PUBLISH_FAILED: UNREACHABLE`). The first
    spawn still collides with Syncthing's sync (~11 s): fixed in 0.1.37.
  - Worker1's replica started with the primary unreachable (temporary nft rule) → DEGRADED →
    when the connection came back, `identity full sync applied (delta stream subscribed)` on its own.
  - OPA clear targeted at worker1 → the motherbee ignores it; the admin reports `pending worker1`
    (reaching the spokes is the pending design, A-7).
- **Rollback:** snapshot `pre-config-scope-0-1-36` (all 4 VMs) or `apt install fluxbee=0.1.35` +
  core-update of the spokes.

## 0.1.35 — admin: the orchestrators can mark a node's ILK again (fix for 0.1.34)

- **Date:** 2026-09-28 (ART) · **Previous version:** 0.1.34 · **Commit:** `5f1cd71`
- **Scope:** **motherbee** (VM100). Only SY.admin and the reset script change; the spokes stay
  on the 0.1.34 core (the admin does not run on them).
- **What changed:** the **0.1.34 regression**. When `delete_ilk` was added to Cloud's surface, the
  relay gate (EDGE-06) started rejecting the **orchestrator's** teardown. The kill with
  `purge_instance` answered ok but left the node's ILK **active and orphaned**. Fix: any
  `SY.orchestrator@*` can use `delete_ilk` (and only that), following the sibling rule of
  `vault_put`. Also, the reset verifies OPA by result, hive by hive.
- **Build:** VM110 `build-deb.sh 0.1.35` (7 min) → publish (35 packages).
- **Live verification:**
  - e2e 27/31. The 4 FAILs are earlier, explained findings: the replica rejects the admin's
    listing and the OPA write does not reach the spokes.
  - The kill+purge was verified: the node's ILK ends up purged and the name free.
  - **Reset installed by the `.deb`** with real data: 1 node, 2 ILKs, 2 secrets, **8 tenants**
    and OPA policy v5, with RC=0 and 0 problems. The second dry-run comes out empty.
  - Cloud's door at 200, with 13 actions.
- **Rollback:** snapshot `pre-teardown-fix-0-1-35` (VM100) or `apt install fluxbee=0.1.34`, which
  reintroduces the regression.

## 0.1.34 — identity: mark / restore / purge · `opa_clear` · full factory reset

- **Date:** 2026-09-28 (ART) · **Previous version:** 0.1.33 · **Commits:** `13f426f`, `60331c5`
  (plus `f47dcb2` and `c1fee0d` for the reset).
- **Scope:** **motherbee + the 3 spokes**, with core update, because the router,
  SY.identity and SY.opa.rules change, and they also run on the spokes.
- **What changed:**
  - **Identity:** deleting an ILK or a tenant only **marks** it. The mark is reversible, hides the
    record and keeps its keys reserved; the **purge** is physical and requires the mark. The tenant
    marks its ILKs in cascade.
  - **DB migration v2.**
  - **Cloud:** marks and restores, always scoped to the tenant that claims; it **cannot purge**.
  - **`opa_clear`:** the routers unload the user policy (before, they ignored it until
    restarted).
  - **`fluxbee-factory-reset`:** ships in the `.deb`.
- **Build:** VM110 `build-deb.sh 0.1.34` (24 min) → publish (34 packages). Snapshots
  `pre-lifecycle-0-1-34` on all 4 VMs.
- **Live verification:**
  - the v2 migration was applied;
  - the spokes did the core update (hash `e67a8815…`) and their services restarted;
  - e2e 26/30 through the real public door: mark, hide, reserve, scope by tenant, cascade,
    restore and purge;
  - the router unloaded the OPA policy (seen in its log).
- **Regression found in the e2e:** the orchestrator's teardown got blocked. It was fixed in
  0.1.35.
- **Rollback:** snapshot `pre-lifecycle-0-1-34` (all 4 VMs) or `apt install fluxbee=0.1.33`. Note:
  the v2 migration only **adds** columns, so 0.1.33 ignores them without a problem.

## 0.1.33 — io.cloud: email-ONLY get_ilk (cross-tenant) — the website login

- **Date:** 2026-08-28 (ART) · **Previous version:** 0.1.32 · **Commit:** `01b6266`
- **Scope:** **motherbee** (VM100). io.cloud + fluxbee_sdk (seqlock warn in the listing reader) + docs.
- **What changed:** the website (Google OAuth) has ONLY the email at first login; `params.tenant_id` is now
  OPTIONAL with `params.email`. With a tenant: the 0.1.32 O(1) probe, unchanged. WITHOUT a tenant: cross-tenant scan of the
  `cloud` channels (`list_ich_options_from_hive_id`, propagates errors) → `{exists, ilk: only if unique match,
  matches:[ONE subset PER tenant where the email exists — each one with its tenant_id]}`. From `matches` the website
  gets the tenant; 0 matches → create_tenant+register_human; N matches → company selector.
- **Review hardening (2 MEDIUM pre-ship):** (1) `tenant_id` PRESENT but malformed (number, empty,
  invalid format) = hard error — it never silently widens a probe with a tenant into a global scan (branch
  on key presence, not on parse); (2) `matches` guarantees one-per-tenant: transient identity states
  (merge-alias window, address takeover) can leave 2 active ilks in the same (cloud,email,tenant) — the
  ambiguous tenants are re-tested with the authoritative O(1) resolver (same answer as the probe with a tenant).
  + LOW: warn on the listing reader's seqlock-timeout (first-login outages diagnosable).
- **Accepted semantics (documented):** disabled channels match (register_human creates them enabled:false — a
  strict filter would break the first-login); the scan is O(hive) per request (accepted at the current size; list_ilks
  already scans the same way). Email→tenants oracle exposed ONLY to the already trusted caller class (the edge's bearer,
  default-deny with no edge configured).
- **Build:** VM110 `build-deb.sh 0.1.33` → publish to the :8900 repo (33 packages).
- **E2E verification (7/7 from the internet):** 1-company login (`ilk` populated with the tenant), register in a 2nd company,
  2-company login (`ilk:null, matches:[2]` one per tenant), unknown email (`exists:false`), `tenant_id:42`
  → hard error (no scan), and email+tenant / ilk_id regressions intact.
- **Rollback:** `apt install fluxbee=0.1.32` (additive; the 0.1.32 selectors did not change).

## 0.1.32 — io.cloud: selector by EMAIL in get_ilk / get_ilk_details

- **Date:** 2026-08-27 (ART) · **Previous version:** 0.1.31 · **Commit:** `90fd64a` (branch `daily_onworking_coa`)
- **Scope:** **motherbee** (VM100). Touches `io.cloud` + `fluxbee_sdk` (identity readers + cloud catalog) + docs.
- **What changed:** Cloud has the human's email (Google login) but not the `ilk_id`; both reads accept
  `params.email` + `params.tenant_id` as an alternative selector to `params.ilk_id` (exactly-one, in both ops).
  The email IS the address of the ilk's `cloud` channel (register_human provisions it that way) → the local `get_ilk`
  resolves with an O(1) probe of the SHM index `(channel, address, tenant)` — io.api inbound pattern; cheaper than
  the by-id path itself (O(n)). `get_ilk_details` pre-resolves email→ilk_id locally and relays the admin's `get_ilk`
  unchanged (admin/identity do not gain an email selector; `ILK_NOT_FOUND` without touching the admin if it does not match).
  `tenant_id` REQUIRED with email (uniqueness by `(channel, address, tenant)` — the same email can be two
  ilks in two companies) and validated as canonical `tnt:<uuid>`, fail-loud. SDK: variants
  `resolve_identity_option_*_strict` (unreadable SHM = `Err`, never a silent miss — the F-09 laundering is for
  io.api's degrade, not for an authoritative read API).
- **Adversarial review (2 lenses) pre-ship:** both MEDIUMs fixed BEFORE the deploy (strict SHM semantics;
  exactly-one in the op with PII); cross-tenant oracle ruled out (the tenant is part of the index key); the request_id
  LOW was a false positive (the run_loop wrapper injects it). Tests: SDK 6/6+6/6, io-cloud 9/9, admin 3/3.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.32` → 245 MB · publish to the :8900 repo (32 packages).
- **Deploy:** reboot (qga down, known pattern) → `apt install fluxbee=0.1.32` → the manual restart of io.cloud
  came before the unit existed, and **0.1.30's liveness-reconcile respawned it on its own with the new
  binary** (~60s) — the previous fix auto-deployed this feature.
- **E2E verification (10/10 from the internet):** hit by email (`exists:true` + subset), case-insensitive, miss,
  wrong tenant → `exists:false` (isolation), exactly-one in both ops, email-without-tenant clear error,
  details by email → full record, `ILK_NOT_FOUND` with request_id, and the regression by `ilk_id` OK.
- **Rollback:** `apt install fluxbee=0.1.31` (additive feature; the by-id selector did not change).

## 0.1.31 — SDK/rpc: fix for the edge poison (family-wide response_only) — the real crearPersona bug

- **Date:** 2026-08-26 (ART) · **Previous version:** 0.1.30 · **Commit:** `862c5e4` (branch `daily_onworking_coa`)
- **Scope:** **motherbee** (VM100). Touches `crates/fluxbee_sdk/src/rpc.rs` — statically linked into ALL the binaries (full rebuild of the .deb; reboots were needed to respawn the runtimes with the new SDK).
- **The bug (prod, deterministic):** `register_human` in io.cloud waits for the frontdesk's verdict with
  `send_with_matcher(frontdesk_reply_matcher())` whose success matcher is `any_msg_type(user)` (unavoidable: the
  verdict is a `user` frame with `meta.msg=None`). `send_with_matcher` registered the success shapes in the
  **PERMANENT** `response_only` registry (insert-only, filled in WHEN SENDING). `classify` checks that registry by
  SHAPE (no trace) BEFORE io.cloud's `Any→Command` catch-all → after ONE `register_human` (success OR timeout),
  EVERY incoming user-kind frame (all of the edge's Cloud requests) was dropped as an "orphaned response"
  silently (debug-level) → edge 504 → Cloud "crearPersona falló (FRONTDESK_REJECTED)" ("crearPersona failed (FRONTDESK_REJECTED)"). Only a restart cured it.
  **Proven 3 ways:** code, a panel of 4 mappers, and a controlled experiment in prod (baseline 200×3 →
  1 register SUCCESS → probe 6s later, permanent timeout). Refutes the "presence decay ~6min" framing:
  12min lists-only monitor = 36/36 OK; VM100 healthy in the middle of the failure (cpu 0%, mem 15%) — it was NOT Proxmox/memory.
- **The fix:** `register_response_only` skips `AnyMsgOfType` (same reason as the `Any` skip: global drops).
  Exact/OneOf are still registered — the orchestrator's AF-P2b protections intact. Family-wide late replies
  stay covered by pending (trace) + stale table (30s TTL) + each node's own gates (io.cloud ignores
  src≠edge, fail-closed). **Defuses 3 bombs: io.cloud (detonated), io.api (identical handoff) and
  `sy_admin::send_admin_request` (`any_msg_type(admin)`).** Dead helper removed; poisonous test inverted into the
  regression `family_wide_success_matcher_never_poisons_command_traffic`; RPC multiplexing doc updated.
- **Review:** 39/39 SDK tests green · adversarial, 3 lenses (blast radius of every caller, protection
  windows, test coherence): **0 HIGH / 0 MEDIUM**, only LOW about metrics/logs drift.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.31` → 234 MB. **Publish:** `apt-repo-publish.sh` → repo :8900
  (note: the `:8900` server is a transient `systemd-run` unit that does NOT survive VM110 reboots — it had to be brought back up).
- **Live verification (E2E from the internet, post-reboot):** **4 consecutive `register_human`** (3 fresh +
  1 idempotent repeat, including the operator's real flow) all `status=ok success=True reg=complete`, with
  `list_cloud_actions` 200 interleaved after EACH one + 200×3 stability — with 0.1.30 the first register killed
  all the later edge traffic.
- **Rollback:** snapshot `pre-rpc-poison-fix-0-1-31` (VM100) or `apt install fluxbee=0.1.30` + reboot (reintroduces the poison).

## 0.1.30 — orchestrator: self-heal of dead managed runtimes + goodbye `is_packaged_singleton`

- **Date:** 2026-08-26 (ART) · **Previous version:** 0.1.29 · **Commit:** `af1166a` (branch `daily_onworking_coa`)
  (+ `9e81687` reword docs "singleton"→"managed runtime").
- **Scope:** **motherbee** (VM100). Touches `sy_orchestrator` + `fluxbee_sdk::managed_node`.
- **The bug (prod):** `IO.cloud@motherbee` was found DEAD (transient unit inactive) with the MB up for 19.8 days, and
  nothing brought it back: the managed runtimes run with `systemd-run --property Restart=always`, but if they exhaust the
  start-limit nobody recreates them — `reconcile_persisted_custom_nodes` ran ONLY at bootstrap, and the 5s watchdog
  only LOGGED them ("node disconnected") while it DOES restart SY.*/rt-gateway → edge 504 until the next boot.
- **The fix:** `run_managed_runtime_reconcile_loop` — its OWN 60s task (NOT the 5s watchdog's hot path: a
  slow systemctl never holds back the self-heal of SY.*) that re-runs the persisted reconcile; the reconcile now does
  `try_lock_node` (SO-04) per node — if an admin-op holds the lock, it skips and retries on the next cycle. It only
  revives `relaunch_on_boot=true` nodes. **Operational note:** `kill_node` without purge of a boot node now revives in
  ≤60s (consistent with SY.*; a durable take-down = purge). Also: deleted the dead mechanism
  `is_packaged_singleton`/`PACKAGED_SINGLETON_NODES` (empty list; the double-owner guard is held by `kind=="SY"`/RT.gateway).
- **Review:** adversarial, 3 lenses — the major finding (the inline reconcile blocked the watchdog) was fixed
  BEFORE the deploy (own task + try_lock).
- **Build:** fb-build (VM110), `build-deb.sh 0.1.30` → 234 MB. **Publish:** repo :8900.
- **Live verification:** boot-reconcile relaunched the 9 runtimes (`started=9 skipped=0 failed=0`); the periodic
  loop runs exactly every 60s (`reconcile completed started=0 skipped=9`); Cloud endpoint 200 post-boot.
- **Rollback:** snapshot `pre-liveness-fix-0-1-30` (VM100) or `apt install fluxbee=0.1.29`.

## 0.1.29 — io.cloud: Cloud's READ path (phase 2) — fast SHM reads + get_ilk_details relay

- **Date:** 2026-08-25 (ART) · **Previous version:** 0.1.28 · **Commit:** `a0bc25d` (branch `daily_onworking_coa`)
- **Scope:** **motherbee** (VM100). Touches `fluxbee_sdk::cloud` + the runtime node `IO.cloud` + the core node `SY.admin`.
- **What changed:** gives Fluxbee Cloud a way to **read** what it created (io.api pattern: existence/subset from the
  local SHM without a round-trip; full data by relay to the admin).
  - **SDK (single source):** `CLOUD_LOCAL_OPS += get_ilk/get_tenant/list_ilks` (SHM reads that io.cloud serves alone);
    `CLOUD_OP_ACTIONS += get_ilk_details → get_ilk` (relay). `CLOUD_EXPOSED_ACTIONS` gains `get_ilk` → the admin's
    `authorize_cloud_relay` + the Cloud catalog auto-allow/publish it (advertised==enforced). `cloud_action_catalog` documents the 4.
  - **io.cloud:** `handle_shm_read` reads the identity SHM via `fluxbee_sdk::identity` (`list_ilks_from_hive_id`/
    `tenant_exists_in_hive_id`, by `config.hive_id`) — the same readers as io.api, **without a round-trip**. Subset
    `{ilk_id, ilk_type, registration_status, tenant_id, display_name}`. `get_ilk_details` → the admin's `get_ilk` (identification PII + channels + tenant).
- **Build:** fb-build (VM110), `build-deb.sh 0.1.29` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Live verification (E2E from ingress):** the **4 ops OK** — `get_ilk` `{exists,ilk:subset}`; `get_tenant`
  `{exists,ilk_count:4}`; `list_ilks` `{count:4, ilks:[…pepito…]}`; `get_ilk_details` full record
  (`identification{email,phone,company,attributes}` + `channels` + `tenant{pepito}`). SDK 5/0 · io.cloud 9/0 ·
  admin catalog test updated. **0 units failed**, io.cloud `NRestarts=0` (an initial transient 502 because of the ICH
  re-registering after the restart). **Contract for Cloud dev in `docs/io-cloud-api.md` §4.6–4.9.**
- **Note (owner-deferred):** tenant ownership is still MVP-trusted — a holder of the bearer can read any id
  it names (same posture as the writes).
- **Rollback:** VM100 snapshot `pre-cloud-readpath-0-1-29`, or `apt-get install -y --allow-downgrades fluxbee=0.1.28`.

---

## 0.1.28 — frontdesk: reconcile CONFIG_GET/CONFIG_SET with the autonomous model (Model D', like architect)

- **Date:** 2026-08-25 (ART) · **Previous version:** 0.1.27 · **Commit:** `1438737` (branch `daily_onworking_coa`)
- **Scope:** **motherbee** (VM100). Touches the core node `SY.frontdesk.gov` (`ai_node_runner`).
- **What changed:** a panel of 4 agents confirmed that architect AND the admin's executor are autonomous **and
  KEEP** a reconciled CONFIG_GET/CONFIG_SET — so conforming = **reconcile, not delete** the plane.
  - **`refresh_ai_gate()`**: the single resolve-token-and-set-gate seam (analogous to `refresh_architect_ai_runtime`),
    now used by `boot_self_configure`, `handle_vault_secret_changed` **and** CONFIG_SET.
  - **CONFIG_SET (Fork B, owner-confirmed):** it was a config injector; now the architect model — **rejects EVERY**
    config/secret/behavior field (`frontdesk_rejected_config_field`) and is a **trigger to re-resolve the vault
    token**; it persists nothing.
  - **CONFIG_GET reconciled:** `ok` now reflects the token gate (Configured), not the presence of config;
    `required_fields`/`optional_fields` → `[]`; adds `config.ai {default_provider, model}` (hive-wide); renames
    `secrets[]` → `resources[]` (+required) mirroring architect; autonomous notes.
  - Removed the 5 injection orphans (`ok_response`, `persist_dynamic_config`, `write_json_atomic`,
    `first_secret_bearing_config_field`, `parse_effective_config_doc`; their test repointed). 27/0 tests.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.28` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Live verification:** `0.1.28`; **autonomous boot still OK** (degraded → VAULT_SECRET_CHANGED → Configured);
  **deterministic handoff regression** → HTTP 200 `success:true complete`; **0 units failed**. (Note: the operator route
  `GET /nodes/.../config` gives `NODE_CONFIG_NOT_FOUND` because it reads a persisted config FILE that the autonomous node
  no longer has — same as architect; the live CONFIG_GET is served over the mesh channel, verified in code + tests.)
- **DEFERRED (catalogued cleanup pass, flagged by the panel):** the `--config`/`run_one_config` route (YAML,
  DEAD in prod — the systemd unit does not pass `--config`) + its helpers + the YAML input types + the pre-existing
  `with_jitter`/`parse`.
- **Rollback:** VM100 snapshot `pre-frontdesk-configplane-0-1-28`, or `apt-get install -y --allow-downgrades fluxbee=0.1.27`.

---

## 0.1.27 — frontdesk: AUTONOMOUS bootstrap (resolves the vault token at boot + acts on the broadcast)

- **Date:** 2026-08-25 (ART) · **Previous version:** 0.1.26 · **Commit:** `000de90` (branch `daily_onworking_coa`)
- **Scope:** **motherbee** (VM100). Touches the core node `SY.frontdesk.gov` (`ai_node_runner`).
- **What changed:** the frontdesk conforms to the canonical SY.* system node pattern (verified with a panel against
  `SY.architect` `build_architect_ai_runtime`/`refresh_architect_ai_runtime` and the `SY.admin` executor — same
  mechanism, same seam). It is an **autonomous** node: its AI runtime is **baked** (always ai_chat — prompt
  `frontdesk_default_instructions` + engine `load_hive_ai_engine` hive.yaml/fallback), and the **only external input
  is the vault token**.
  - **Boot:** builds the baked behavior, `boot_self_configure` resolves the token (`resolve_ai_api_key`, Model D' root)
    → `Configured` if it is there, otherwise `Unconfigured` (degraded — the deterministic handoff still works). Baked build
    fails (broken hive.yaml) → `FAILED_CONFIG`.
  - **Broadcast:** `handle_vault_secret_changed` was a no-op probe+log (THE bug — it never changed state); now it
    **acts** like `refresh_architect_ai_runtime` — re-resolves and sets `Configured`/`Unconfigured` live.
  - Reuses only existing seams (no new builder or vault path). 27/0 tests.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.27` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Live verification:** `0.1.27` installed; **the frontdesk moved to `Configured` from the vault token** at
  startup (degraded boot 00:52:19 → `VAULT_SECRET_CHANGED` op=put openai_api_key 00:52:20 → **Configured, LLM
  path live**) — without CONFIG_SET. **Deterministic handoff regression OK** (register_human from ingress → HTTP 200
  `success:true registration_status:complete`; an initial transient 502 because io.cloud was re-registering its ICH after
  the restart). frontdesk + io.cloud `active`, `NRestarts=0`, **0 units failed**.
- **DEFERRED (EDIT 3, owner-confirmed, to a catalogued cleanup pass):** remove the CONFIG_SET/spawn plane
  (`apply_config_set` + `load_persisted_dynamic_config` in BOTH boot routes + ~6 helpers) — a tangled cascade;
  under the "no borrar a ciegas" ("do not delete blindly") rule it is left for a dedicated pass. `apply_config_set` still works (nobody
  sends CONFIG_SET to the frontdesk in practice). Pre-existing warnings `with_jitter`/`parse` also go to that pass.
- **Rollback:** VM100 snapshot `pre-frontdesk-autonomous-0-1-27`, or `apt-get install -y --allow-downgrades fluxbee=0.1.26`.

---

## 0.1.26 — frontdesk: the deterministic handoff (JSON) runs without the Configured/LLM gate (register_human fix)

- **Date:** 2026-08-24 (ART) · **Previous version:** 0.1.25 · **Commit:** `cb2d192` (branch `daily_onworking_coa`)
- **Scope:** **motherbee** (VM100). Touches the core node `SY.frontdesk.gov` (`ai_node_runner`).
- **Root cause (found with 0.1.25's observability):** `register_human` never completed because the frontdesk
  **is UNCONFIGURED on every boot** (it was never seeded nor received CONFIG_SET) and `on_message` rejected
  **EVERY** `user` message with `node_not_configured` **before** looking at the payload → io.cloud saw it as
  `FRONTDESK_REJECTED` → the ilk stayed `temporary`. (Confirmed by the line `Cloud op completed error_code=FRONTDESK_REJECTED` + the frontdesk without handoff logs.)
- **What changed:** human sign-up has TWO methods, distinguished by the **method**, not by being human: **auto**
  (a `frontdesk_handoff` JSON arrives → deterministic, ILK_REGISTER, no LLM) and **conversational** (the human chats
  and the LLM gathers the data). The deterministic method is handled **before** the Configured gate → `register_human`
  registers even if the frontdesk has no LLM. The conversational/LLM path still requires Configured.
  `handle_frontdesk_handoff` is config-independent. New test `frontdesk_handoff_runs_deterministically_even_when_unconfigured`.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.26` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Live verification:** `fluxbee 0.1.26` installed; **frontdesk restarted (18:40) + `active`**, io.cloud `active`
  `NRestarts=0`, **0 units `failed`**. Pre-deploy: sy-frontdesk-gov 27/0 tests green. **Pending:** E2E of
  register_human from the screen (it should register now → ilk `complete`).
- **Topic 2 (separate):** the openai secret was NOT lost; the frontdesk is UNCONFIGURED only because it was never
  configured (peer AI.chat has config v2). The conversational/LLM path needs the frontdesk configured — a separate matter.
- **Rollback:** VM100 snapshot `pre-frontdesk-handoff-0-1-26`, or `apt-get install -y --allow-downgrades fluxbee=0.1.25`.

---

## 0.1.25 — observability: outcome of every Cloud op + the frontdesk's verdict (phase 1 of the tenant/ILK consolidation)

- **Date:** 2026-08-24 (ART) · **Previous version:** 0.1.24 · **Commit:** `69b1c46` (branch `daily_onworking_coa`)
- **Scope:** **motherbee** (VM100). Touches the runtime node `IO.cloud` + the core node `SY.frontdesk.gov` (`ai_node_runner`). **Logging only, zero behavior change.**
- **What changed (operational impact):** make the round-trip visible to diagnose why a `register_human` does not complete (today the frontdesk processes+answers but logs nothing at INFO → the verdict is invisible).
  - **io.cloud:** the missing **egress/outcome** line — every Cloud op logs `{status, error_code, registration_status, ilk_id, tenant_id, elapsed_ms}` at INFO, with the **same `trace_id`** as the ingress → the full round-trip edge→io.cloud→admin/identity/frontdesk becomes greppable by `trace_id`. (Detail: inside the `tracing::*` macros a bare `Value` resolves to `tracing::Value`, so the serde reads use closures.)
  - **frontdesk (Gov):** promote to INFO/WARN the **handoff decision** at each exit: parsed-as-handoff vs fell-to-conversational (**WARN** when a handoff-shaped payload does NOT parse — the silent failure we are hunting), unsupported operation, incomplete→needs_input (ilk NOT registered), REGISTERED (complete), or register FAILED.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.25` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Live verification:** `fluxbee 0.1.25` installed; **io.cloud + sy-frontdesk-gov + sy-admin + sy-identity `active`**, io.cloud `NRestarts=0`, **0 units `failed`**. Pre-deploy: io-cloud 8/0 + sy-frontdesk-gov 26/0 tests green.
- **Next:** reproduce `register_human` from the Cloud screen → the new logs show LIVE why it does not complete (handoff parsing or missing fields) → fix at the source. Then, phase 2 = reads (SHM E/!E + `get_ilk_details` relay).
- **Rollback:** VM100 snapshot `pre-observability-0-1-25`, or `apt-get install -y --allow-downgrades fluxbee=0.1.24`.

---

## 0.1.24 — security: reserve the vault's infra namespaces against Cloud's `put_token` relay

- **Date:** 2026-08-23 (ART) · **Previous version:** 0.1.23 · **Commit:** `35349ef` (branch `daily_onworking_coa`)
- **Scope:** **motherbee** (VM100). Touches `fluxbee_sdk::vault` + the core node `SY.admin` (systemd) + the runtime node `IO.cloud`.
- **What changed (operational impact):** closes the **MEDIUM** from the audit of io.cloud's external surface.
  `put_token`→`vault_put` had no key namespace guard: a semi-trusted (or compromised) Cloud relay
  could **overwrite any key** in the vault by charset — including `edge_channel_secret:<ich>` (the bearer
  that protects an externalized endpoint, its own included), `edge_tls`, or `ssh:<hive_id>` (the spoke's
  recovery key). That is a **DoS/takeover** surface, not "storing a provider token".
  - Single-source + defense-in-depth fix: `fluxbee_sdk::vault::CLOUD_RESERVED_VAULT_KEY_PREFIXES`
    (`edge_channel_secret:`, `edge_tls`, `ssh:`) + `is_cloud_reserved_vault_key`. The peer-auth keys
    (mesh-HMAC / WAN-mTLS) live in the **filesystem**, not in the vault, so they were already out of reach.
  - **Authoritative server-side** enforcement in `SY.admin::enforce_cloud_relay_content` (only `IO.cloud@hive` origin,
    right after `authorize_cloud_relay`, no bypass) + mirror in `io.cloud::translate_cloud_op` (clean early
    error). Only the Cloud relay origin is bound; the internal SY.* nodes keep writing those keys as usual.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.24` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Live verification:** `fluxbee 0.1.24` installed; **io.cloud + sy-admin + sy-identity + sy-vault `active`**,
  io.cloud `NRestarts=0`, **0 units `failed`**. Pre-deploy: tests green in all 3 layers (SDK vault, io.cloud
  translate, admin enforce) + wiring verification (enforce always runs after authorize, no bypass).
- **Pending (not from this deploy):** diagnosing the `create_tenant` error from Cloud (to be looked at **tomorrow with
  the dev** — the sign-up works on the backend side; the error is in the cross-hive edge↔io.cloud response round-trip).
- **Rollback:** VM100 snapshot `pre-vault-guard-0-1-24`, or `apt-get install -y --allow-downgrades fluxbee=0.1.23`.

---

## 0.1.23 — io.cloud: first-class Cloud actions framework (relay + local) + `register_human` with the tenant at the root

- **Date:** 2026-08-21 (ART) · **Previous version:** 0.1.22 · **Commit:** `4abad1b` (branch `daily_onworking_coa`)
- **Scope:** **motherbee** (VM100). Touches `fluxbee_sdk::cloud` (the shared Cloud vocabulary) + the runtime node `IO.cloud`.
- **What changed (operational impact):**
  - `register_human` stops being an ad-hoc branch (`op == "register_human"`) and becomes a **first-class Cloud
    action**, dispatched by the **declared set** in the SDK (`CLOUD_LOCAL_OPS`), not by a magic string.
    Two categories: **relay** (the usual 3 → `SY.admin`) and **local** (`register_human`, `list_cloud_actions`
    → io.cloud resolves them, they **never** touch the admin), disjoint by design (a test pins it — a local op cannot
    leak into the `authorize_cloud_relay` gate).
  - **New action `list_cloud_actions`** (local): returns the action catalog (relay + local) with help,
    so that Fluxbee Cloud **discovers the surface** without depending on the doc. Closes the "no discovery API".
  - **`register_human` envelope aligned:** the `tenant_id` now goes at the **root** of the envelope (like
    put_token/provision_node), no longer in `params`; io.cloud injects it into the `frontdesk_handoff` for the frontdesk.
    It is **breaking** vs 0.1.22, but Cloud has nothing firm built yet and adapts.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.23` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Live verification:** `fluxbee 0.1.23` installed; **io.cloud `active/running`, `NRestarts=0`, `ExecMainStatus=0`**
  (connected to the router, own ilk, ICH `ich:14b66389…` enabled — no FAILED_CONFIG); `sy-admin` +
  `sy-orchestrator` + `sy-frontdesk-gov` + the 13 `sy-*` + `rt-gateway` `active/running`; the 9 `fluxbee-node-*`
  `active/running`; **0 units `failed`**. Pre-deploy: SDK cloud tests + io workspace green; adversarial review
  (2 lenses + verify) → **0 confirmed defects** (+ 1 catalog drift hardening applied).
- **Pending:** **functional E2E** of `register_human` from Fluxbee Cloud dev (tomorrow) — not runnable live
  from here (no bearer). Final contract for Cloud in `docs/io-cloud-api.md` §4.4 (`register_human`) / §4.5
  (`list_cloud_actions`).
- **Rollback:** VM100 snapshot `pre-cloud-actions-0-1-23`, or `apt-get install -y --allow-downgrades fluxbee=0.1.22`.

---

## 0.1.22 — frontdesk: fix for the conversational bug + extension of the human ilk's data

- **Date:** 2026-08-20 (ART) · **Previous version:** 0.1.21 · **Commit:** `15fc77f` (branch `daily_onworking_coa`)
- **Scope:** **motherbee** (VM100). Touches the core node `sy-frontdesk-gov` (systemd) + `io_common` (comment).
- **What changed (operational impact):**
  - **BUG fixed:** the frontdesk's conversational path (reachable — the 0.1.20 force rule sends the
    plain message of a first-contact human to the frontdesk) reported `REGISTERED`/`complete`
    **without having registered** when the LLM's turn did not write `thread_state` (a chat turn, or
    register-and-delete, ambiguous). Now: `None` → `needs_input`/`IN_CONVERSATION` (no false REGISTERED),
    and the prompt persists `status=completed` on success (instead of deleting) to disambiguate. The
    deterministic path (io.cloud `register_human`) **is not touched** (it returns the direct registration result).
  - **Human ilk schema extended (additive, ZERO change in identity — `identification` is free JSONB
    stored verbatim):** `company_name` (typed) + `attributes` (free) flow end-to-end to
    `ILK_REGISTER` through the shared tool → it serves both paths. `handle_frontdesk_handoff` used to
    **drop** `company_name`; now it forwards it + merges multi-turn `attributes` (symmetric with company_name).
- **Build:** fb-build (VM110), `build-deb.sh 0.1.22` → 245 MB, 118 entries.
- **Publish:** `apt-repo-publish.sh` → repo :8900 (22 versions).
- **Live verification:** `fluxbee 0.1.22` installed; **`sy-frontdesk-gov` restarted + `running`**
  (02:31 UTC, with the fix+schema); rt-gateway + the 13 `sy-*` `active`; io.cloud `active`; **0 `failed` nodes**.
  Pre-deploy: `sy-frontdesk-gov` tests green (26/0, including the test that encoded the bug, corrected) +
  adversarial review (clean bug-fix; 1 LOW about multi-turn `attributes` symmetry, fixed).
- **Rollback:** VM100 snapshot `pre-frontdesk-0-1-22`, or `apt-get install -y fluxbee=0.1.21`.
- **Pending / known:** the conversational path depends on the LLM following the prompt (same
  reliability as the whole flow); a free `attributes` without a size bound could put large blobs
  into the JSONB (size validation = future improvement if needed). Closes the two follow-ups from 0.1.21.

## 0.1.21 — io.cloud `register_human`: automatic registration of humans, Cloud→frontdesk

- **Date:** 2026-08-20 (ART) · **Previous version:** 0.1.20 · **Commit:** `2f60403` (branch `daily_onworking_coa`)
- **Scope:** **motherbee** (VM100). io.cloud is a motherbee-only managed runtime; the spokes do not run it.
- **What changed (operational impact):**
  - New inbound op `register_human` in io.cloud (it is NOT a relay to SY.admin): Fluxbee Cloud sends the human's
    data as `frontdesk_handoff` JSON; io.cloud **provisions** a temporary human ilk (same
    `strict_provision_ilk` as io.api) and **Unicasts** the handoff **verbatim** to the configured frontdesk
    (`government.identity_frontdesk`). The frontdesk (deterministic path) registers (temporary→complete) and
    answers; io.cloud **relays the structured verdict** to Cloud, stamped with the `ilk_id` it minted.
  - **Unicast, not the router's force rule**: an explicit handoff reaches the frontdesk with ANY state
    of the ilk, so a re-registration of an already `complete` human still lands (`ILK_REGISTER` idempotent).
    io.cloud does not choose the target (it uses the frontdesk from the config); the 0.1.20 force stays as a containment net for
    implicit senders (io.slack/io.wapp).
  - Strict gate (type + schema_version + operation + canonical tenant `tnt:<uuid>` + real email) and
    `response_envelope` (so that the frontdesk emits the structured verdict, not plain text).
  - De-diverge: `frontdesk_response_contract` shared in io_common; io.api de-diverged.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.21` → 245 MB, 118 entries.
- **Publish:** `apt-repo-publish.sh` → repo :8900 (21 versions indexed).
- **Live verification:** `fluxbee 0.1.21` installed; runtime **io.cloud moved 0.1.20→0.1.21 + restarted
  + healthy** (connected to the router as `IO.cloud@motherbee`, self-ilk, ICH enabled); rt-gateway routing;
  13 `sy-*` + rt-gateway `active`; **0 `failed` nodes**. Pre-deploy: io workspace green (rust 1.92) +
  **double adversarial review** (10 findings in the 1st pass, all fixed at the source; clean re-review).
- **Rollback:** VM100 snapshot `pre-register-human-0-1-21`, or `apt-get install -y fluxbee=0.1.20`.
- **Pending / known:** (a) **functional E2E test** of `register_human` (Cloud really calling it) —
  the binary runs healthy; the trigger from Fluxbee Cloud is missing. (b) 🔖 bug in the frontdesk's conversational
  path (it reports REGISTERED without registering). (c) 🔖 `company_name`/`attributes` that the deterministic path drops
  today (extension of the human ilk's schema). (b)+(c) are the next step.

## 0.1.20 — router: routing to the frontdesk as an OPA policy (no Rust fallback)

- **Date:** 2026-08-20 (ART) · **Previous version:** 0.1.19 · **Commit:** `1297ba7` (branch `daily_onworking_coa`)
- **Scope:** **motherbee** (VM100) only. The spokes (worker1/ingress/egress) stay on
  **0.1.19** — the `.deb` only updates the motherbee and the core-update to spokes is optional (same authority
  wasm ⇒ identical decisions; the routing to the frontdesk happens on the motherbee, where identity lives).
- **What changed (operational impact):**
  - The "unidentified sender → frontdesk" routing stopped being a hardcoded `if` in the router
    (`apply_identity_pre_resolve`) and became a **visible** rule in `policy/system.rego`
    (new entrypoint `fluxbee/system/frontdesk_route`). Now **both** the `temporary` ilks **and** the
    messages **without an ilk** fall to the frontdesk (before, the no-ilk case was lost in `OpaError::NotLoaded`).
  - The Rust `authority()` fallback was **removed** (it duplicated the SYSTEM authority policy):
    **single source = the rego**, and the router **fails closed at startup** if a baked `.wasm` does not
    load (`ensure_system_policy_loaded` → `RouterError::Startup`).
  - The orchestrator's `SO-05` re-check now uses `authorize_system` (same policy, not a Rust twin).
- **Build:** fb-build (VM110), `packaging/build-deb.sh 0.1.20` → `fluxbee_0.1.20_amd64.deb`, 245 MB,
  118 entries; the `.wasm` files go **baked inside `rt-gateway`** (`include_bytes!`), not as files.
- **Publish:** `scripts/apt-repo-publish.sh --deb …0.1.20….deb` → `/var/lib/fluxbee-apt` served at
  `10.10.10.50:8900` (with `-m`, keeps versions for rollback).
- **Live verification (post-`apt install`):**
  - `rt-gateway.service` `active (running)` — **started fail-closed OK** (`ensure_system_policy_loaded`
    passed: both baked wasm files loaded) and **routes real traffic** (ADMIN_COMMAND / INVENTORY / VAULT_GET).
  - 13 `sy-*` + `rt-gateway` `running`; **0 `failed` nodes**.
  - Router `/status` **without a user-OPA catchall** ⇒ None→frontdesk is a strict improvement (it does not redirect existing traffic).
  - Pre-deploy: lib+bins tests green (rust **1.92.0**) + **adversarial review** (4 lenses + verification): **0 confirmed defects**.
- **Rollback:** VM100 snapshot `pre-router-0-1-20`, or `apt-get install -y fluxbee=0.1.19`.
- **Pending / known:** the frontdesk still **cannot MINT** an ilk for a fully
  ilk-less sender (`ILK_PROVISION` is IO-only; `ILK_REGISTER` only COMPLETES an existing temporary) — the
  ilk-less sender reaches the frontdesk but is not onboarded yet. Separate fast-follow.

---

<!-- TEMPLATE for the next entry (copy above this line, most recent first):

## 0.1.N — <short title of the change>

- **Date:** YYYY-MM-DD (ART) · **Previous version:** 0.1.N-1 · **Commit:** `<sha>` (branch `<branch>`)
- **Scope:** motherbee | + spokes (worker1/ingress/egress)
- **What changed (operational impact):** <1-3 bullets, in terms of what the system does differently>
- **Build:** fb-build (VM110), `build-deb.sh 0.1.N` → size / entries / preflight OK
- **Publish:** `apt-repo-publish.sh` → repo :8900
- **Live verification:** <services `running`, 0 `failed`, functional check specific to the change>
- **Rollback:** snapshot `pre-<something>` · `apt install fluxbee=0.1.N-1`
- **Pending / known:** <what is left>

-->
