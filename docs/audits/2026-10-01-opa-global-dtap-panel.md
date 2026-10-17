# One global user OPA policy: DTAP review panel and its resolution

**Date:** 2026-10-01 · **Releases reviewed:** 0.1.41–0.1.45 · **Resolved in:** 0.1.46–0.1.50 ·
**Status:** closed. Every finding is fixed, documented, accepted with a reason, or postponed by
the operator (the security package).

Canonical design: [`docs/opa-distribution.md`](../opa-distribution.md). Day-by-day narrative:
`lab/logbook/2026-09-30.md`. Ledger: `lab/DEPLOYMENTS.md`. Findings register:
`lab/logbook/FINDINGS.md`.

## What was reviewed

Releases 0.1.41–0.1.45 moved the user OPA policy (routing for messages without a destination) to
one global policy on every hive:

- the motherbee's SY.opa.rules compiles, applies and publishes the wasm through Syncthing;
- every other hive installs it when the sha256 matches;
- CONFIG_CHANGED became an ordinary protected action, sent only by `SY.admin@motherbee`;
- system policy rule 3 was narrowed to the actions an orchestrator forwards;
- the router drops a frame whose `routing.src` is not the sending socket (A-22).

The panel had four reviewers: Development, Test, Acceptance and Production. An adversarial verifier
checked each finding against the code. The result was 50 findings: 47 confirmed and 3 refuted
(D-11, D-12, P-11). Production did not pass on P-1 (critical).

## How it was resolved

The operator went through the findings by cost and benefit (2026-10-01):

- **Postponed: the security package.** *"Today it costs more than it returns."* It covers P-1, the
  direction of rule 3 (D-2, P-2), manifest signing (P-8), `get_policy` exposing the rego (P-16) and
  encrypting the synced copy. It is tracked in FINDINGS A-22 and A-20; the documents now state
  these limits instead of promising more.
- **Block A, high benefit (0.1.46, `7371ca1`):**
  - the architect translates the global write paths;
  - writes have bounded waits;
  - the rollout report is honest about which hives converged;
  - the motherbee re-publishes if a publish failed;
  - there is a single write path.
- **Block B, visibility and robustness (0.1.47, `652a9c9`):**
  - a hive that is stuck says why;
  - a half-finished install is repaired;
  - the status data race is gone;
  - no stale rego is left behind;
  - the Syncthing owner comes from the folder.
- **Block C, documentation (`9b367cb`, `d2ec18b`):** texts that were stale or promised too much.
- **Block D, tests (`1503570`, 0.1.50 tests):**
  - the shared contracts and the admin's write sequence are covered;
  - Go tests run in CI (`go-tests.yml`), and so do Rust tests (`rust-tests.yml`, `802faf5`).
- **Block F, leftovers (0.1.50, `aa206ac`):**
  - the router reports `ok` when it has no policy (A-21);
  - OPA_RELOAD is no longer fanned out to every node;
  - system rule 5 no longer names a node that does not exist.

## Finding by finding

| ID | Sev. | Verdict | Resolution |
|---|---|---|---|
| A-1 | medium | confirmed | 0.1.46: the architect translates `/opa/policy*` |
| A-2 | medium | confirmed | 0.1.46: the expected hives come from the registry (`connected`); `unreachable` is reported |
| A-3 | low | confirmed | 0.1.46: CONFIG_SET on SY.opa.rules is read-only |
| A-4 | low | confirmed | Block C: 06-regiones, 02-protocolo and opa-distribution corrected |
| A-5 | low | confirmed | Block C: seven specs and three code comments corrected |
| A-6 | low | confirmed | Block C: HANDBOOK §12 names only `fluxbee-dist-policy` |
| A-7 | low | confirmed | 0.1.46: the motherbee re-publishes when the manifest is not what it runs |
| D-1 | medium | confirmed | 0.1.46: a partitioned hive is pending, not converged |
| D-2 | medium | confirmed | **Postponed** (security package; FINDINGS A-20) |
| D-3 | medium | confirmed | 0.1.46: the architect runs the OPA paths |
| D-4 | low | confirmed | 0.1.46: sync answers are filtered by the announced hash |
| D-5 | low | confirmed | 0.1.46: the hive comes from the router-stamped name, not from the payload |
| D-6 | low | confirmed | 0.1.46: an unanswered change step returns 504 TIMEOUT |
| D-7 | low | confirmed | 0.1.46: re-publish, and the answer is PUBLISH_FAILED |
| D-8 | low | confirmed | 0.1.46: CONFIG_SET is read-only |
| D-9 | low | confirmed | 0.1.47: status behind its own lock; clean under `-race` |
| D-10 | low | confirmed | 0.1.47: an install without rego removes the old one |
| D-11 | low | refuted | The Syncthing-user part was fixed anyway (P-13) |
| D-12 | low | refuted | Latent (needs several routers per hive); fixed anyway in 0.1.50: config is re-read on the heartbeat |
| D-13 | low | confirmed | Block C: README and the e2e script |
| P-1 | critical | confirmed | **Postponed** (security package; FINDINGS A-22) |
| P-2 | medium | confirmed | **Postponed** (as D-2) |
| P-3 | medium | confirmed | 0.1.46 (as A-2/D-1) |
| P-4 | medium | confirmed | 0.1.46: the write waits 10 s for hives and at most 5 s for the scan |
| P-5 | medium | confirmed | 0.1.46 (as A-7) |
| P-6 | medium | confirmed | 0.1.47: the router region is repaired on every check |
| P-7 | medium | confirmed | 0.1.47: `waiting {version, hash, since, reason}` in the status; logged after a minute |
| P-8 | medium | confirmed | Block C documents it; signing **postponed** (A-22) |
| P-9 | medium | confirmed | Block C: clear before going below 0.1.41 (HANDBOOK, 0.1.41 ledger entry) |
| P-10 | low | confirmed | 0.1.46 (architect) and block C (e2e script) |
| P-11 | low | refuted | — |
| P-12 | low | confirmed | 0.1.46: the scan hint is bounded to 5 s |
| P-13 | low | confirmed | 0.1.47: published files go to the owner of `dist/policy` |
| P-14 | low | confirmed | 0.1.46 (as A-3) |
| P-15 | low | confirmed | 0.1.47 (as D-9) |
| P-16 | low | confirmed | Block C documents it; restriction **postponed** (A-22) |
| T-1 | medium | confirmed | 0.1.50 tests: the sender check is driven through a real socket and verified to fail without the drop |
| T-2 | medium | confirmed | Block D: a valid wasm of another policy is refused |
| T-3 | medium | confirmed | Block D: the admin's write sequence, with the SDK harness and a mutation check |
| T-4 | medium | confirmed | Block D: the sync notice is a contract file shared by Rust and Go |
| T-5 | medium | confirmed | 0.1.46: every OPA example in the admin catalog translates |
| T-6 | low | confirmed | 0.1.50 tests: a replica refuses every write; the publication is the wasm and the manifest only |
| T-7 | low | confirmed | **Accepted**: `orchestrators_get_exactly_what_they_forward` already checks completeness over the 27 protected actions; tying them to the call sites would cost more than it returns |
| T-8 | low | confirmed | 0.1.50 tests: the notice reaches only SY.opa.rules, and only from the primary admin |
| T-9 | low | confirmed | **Accepted**: testing `policySyncLoop` needs a stoppable loop; every live validation runs it |
| T-10 | low | confirmed | **Accepted**: the folder inheritance was validated live on every hive |
| T-11 | low | confirmed | 0.1.46: PUBLISH_FAILED path tested |
| T-12 | low | confirmed | Block D: Go tests in CI and rootless; Rust tests in CI |
| T-13 | low | confirmed | 0.1.46: write labels fixed in every builder |
| T-14 | low | confirmed | Block D: shared ILK vectors; Go no longer trims |

## Found along the way (outside the panel)

- **A-23:** the architect modelled OPA per hive. Since 0.1.48 a solution declares one global
  policy, and the last one applied wins. Composing several solutions' rules is pending review.
- **A-24:** the architect snapshot asked `SY.admin@<hive>`. Fixed in 0.1.49. Still open: workflow
  definitions do not reach the plan compiler, and on DMZ hives two snapshot reads do not exist.
- **A-25:** the `.deb` did not ship the architect handbook. Fixed in 0.1.49.
- **A-26:** the architect read its vault secrets once at boot. Fixed in 0.1.49.
- **A-27:** the admin catalog guard in CI had been red. Fixed in `802faf5`.
- **A-28:** three Go binaries were tracked in git. Untracked and ignored in `3eeadf2`.
- **A-18:** config-routes closed in 0.1.50 with VPN per hive by decision: one route/VPN/tap API
  surface, system rule 5 cleaned, config refresh on the heartbeat. Accepted: a cut-off hive's routes
  and taps stop applying elsewhere during the cut; taps match exact names; deleting a hive loses its
  config.

## Decisions on record (operator, 2026-09-30 / 2026-10-01)

- One user OPA policy for every hive; each hive keeps its own copy and runs on it while cut off.
- Syncthing plus a sha256 check for transport; no encryption yet; eventual convergence.
- VPN, taps and static routes stay in config-routes, not in OPA. VPN is per hive.
- In the architect, two solutions declaring OPA: the last one applied wins (for now).
- Security package postponed: *"today it costs more than it returns."*
