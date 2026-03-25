# Core Report - Spawn Env Contract Gap (`IO.slack`)

Date: 2026-03-19  
Owner target: Core (`SY.orchestrator` / spawn runtime contract)  
Scope: why spawned `IO.slack` fails to load existing `config.json` and exits with missing token.

---

## 1) Que paso

### Observed sequence

1. Runtime publish/update succeeded.
2. `SPAWN_NODE` returned `status=ok` and created:
   - `/var/lib/fluxbee/nodes/IO/IO.slack.T123@motherbee/config.json`
3. The created config contains valid Slack credentials (`slack.app_token`, `slack.bot_token`).
4. Node process started from transient unit:
   - `fluxbee-node-IO.slack.T123-motherbee.service`
   - unit only had `ExecStart=.../start.sh`
   - no `Environment=` entries for `NODE_NAME`, `ISLAND_ID`, `NODE_CONFIG_PATH`
5. Runtime resolved defaults:
   - `NODE_NAME=IO.slack.T123`
   - `ISLAND_ID=local`
6. Runtime tried wrong config path (derived from defaults), did not load the spawned file, then failed with:
   - `missing Slack app token (...)`
   - process exit `status=1/FAILURE`

### Evidence already collected

- `config.json` exists and includes tokens + tenant.
- `systemctl cat fluxbee-node-IO.slack.T123-motherbee` shows transient unit without env injection.
- journal shows token-missing error immediately at startup.

---

## 2) Que deberia haber pasado

For spawn compliance, runtime must receive enough identity/context to load the effective config created by orchestrator.

At least one of these must be true:

1. `NODE_CONFIG_PATH` points to the exact effective file:
   - `/var/lib/fluxbee/nodes/IO/IO.slack.T123@motherbee/config.json`
2. Or both:
   - `NODE_NAME=IO.slack.T123@motherbee`
   - `ISLAND_ID=motherbee`
   so runtime can derive canonical path correctly.

With correct env contract:

- runtime loads config from spawned node path,
- reads Slack tokens from `config.json`,
- starts normally (or reports non-fatal degraded state if config is truly invalid).

---

## 3) Que hay que hacer para solucionarlo

## 3.1 Core/platform fix (primary)

Update spawn implementation (`run_node` path) so generated systemd transient unit injects canonical runtime environment.

Minimum required:

- `NODE_NAME=<full node name with hive suffix>`
- `ISLAND_ID=<target hive id>`

Recommended (stronger and less error-prone):

- `NODE_CONFIG_PATH=<absolute effective config.json path>`

Why this is the correct layer:

- orchestrator is the authority that creates the effective config file and knows node identity + hive.
- without this handoff, runtime cannot reliably resolve the file in multi-hive/FQDN naming.

## 3.2 IO runtime hardening (secondary, not replacement)

`IO.slack` should not terminate process on missing/invalid credentials.

Expected runtime behavior:

- stay alive in `degraded/not-ready` state,
- expose operational error (status/logs/metrics),
- remain controllable by orchestrator/admin for config repair.

This improves operability but does not solve the spawn contract gap by itself.

---

## 4) Responsibility split

- Core responsibility (blocking root cause for this incident):
  - spawn env contract not providing required node/hive/config context to runtime process.
- IO responsibility (product hardening):
  - avoid fatal `exit 1` on config/secrets missing and move to degraded mode.

Both should be addressed, but the config-path mismatch after successful spawn is primarily a core/orchestrator contract issue.

---

## 5) Acceptance criteria for core fix

1. Spawned transient unit includes either:
   - `NODE_CONFIG_PATH` (preferred), or
   - valid `NODE_NAME` + `ISLAND_ID`.
2. `IO.slack` spawned via orchestrator reads the just-created `config.json` without manual env overrides.
3. Same behavior is reproducible across hives (`@motherbee`, `@sandbox`, etc.).
4. No false "missing token" when tokens are present in effective config.

