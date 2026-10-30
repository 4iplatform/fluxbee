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
| **0.1.90** | 2026-10-29 | `e6b07fc` | motherbee + spokes (core) | ✅ live | snap `pre-wfcase0-0-1-90` (all 4 VMs) · `apt install fluxbee=0.1.89` + core-update of the spokes |
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
