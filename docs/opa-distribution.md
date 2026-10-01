# User OPA policy — one global policy on every hive

**Status:** implemented (0.1.41). Supersedes the "compile on every hive" description in
`SY_nodes_spec.md` §3 and the OPA part of `02-protocolo.md` §CONFIG_CHANGED.
Decisions: operator, 2026-09-30 (lab logbook of that day).

There are two OPA layers. This document is about the **user** policy — the operator-editable
routing policy the routers evaluate for messages without a destination (`dst: null`,
entrypoint `router/target`). The **system** policy (authority: who may send protected SYSTEM
actions; frontdesk routing) is baked into the router binary (`policy/system.rego` →
`policy/system*.wasm`), changes only with a core update, and is not touched by anything below.

## Model

- **One policy for every hive.** The same compiled policy runs in every router of every role
  (motherbee, worker, ingress, egress). A policy that needs hive-specific behaviour branches on
  the sender (`input.routing.src_l2_name` is `role@hive`, stamped by the router).
- **SY.opa.rules runs on every hive** (`system_nodes.<role>` in `hive.yaml`). It derives its own
  ILK from its L2 name (Model D'), so it needs no SY.identity (ingress and egress have none).
- **Only the motherbee compiles.** Its SY.opa.rules compiles the rego, applies it locally and
  **publishes the compiled wasm** — never the rego — for the other hives. The other hives refuse
  compile/apply/rollback/clear (`NOT_PRIMARY`) and only install what the motherbee published.
- **Each hive keeps its own copy.** `/var/lib/fluxbee/opa/{current,backup,staged}` (root, 0700;
  files 0600) and the router's SHM region `/jsr-opa-<hive>` (0600). A hive starts on its last
  policy whether or not the motherbee is reachable, and keeps running on it while cut off.
- **Eventual convergence.** A write succeeds when the motherbee applied and published it; the
  other hives converge on their own and the admin reports which ones are still pending.

## Transport

- **The artifact travels by Syncthing:** folder `fluxbee-dist-policy`, path
  `/var/lib/fluxbee/dist/policy` on every hive — `sendonly` on the motherbee, `receiveonly`
  everywhere else (declared by the orchestrator for every role, like `fluxbee-dist-vendor`,
  and shared with the same peers as vendor — so a hive that joined before the folder existed
  gets it on its next orchestrator start, without re-joining).
  Layout under `opa/`:
  - `manifest.json` — `{schema_version, version, hash, entrypoint, wasm_file, compiled_at,
    published_at}`; `hash` is `sha256:<hex>` of the wasm, `""` means "no user policy".
  - `policy-<hash16>.wasm` — the compiled policy the manifest names (older ones are removed).

  The motherbee writes the wasm first, then the manifest (atomic rename). A compiled wasm is
  ~150 KB: it does not fit in a mesh message (128 KiB frame cap), which is why it rides Syncthing.
- **The notice travels by the mesh:** after the motherbee applied and published, SY.admin
  broadcasts `CONFIG_CHANGED {subsystem: "opa", action: "sync", version, config: {hash}}` with
  `meta.target = "SY.opa.rules@*"` — one broadcast, delivered only to those nodes, on every hive.
  Only `SY.admin@motherbee` may send CONFIG_CHANGED across hives (system policy rule 2).

## Installing on a hive (not the motherbee)

SY.opa.rules checks its local copy of the folder **at start, on every notice, and every 5 s**
(a local file check — nothing is asked over the network). When the manifest names a policy that
is not the one running:

1. read the wasm the manifest names; if it is missing or its sha256 differs from the manifest,
   the sync is still carrying it — try again on the next check;
2. install it as the current policy (previous → backup), write the SHM region, reload the local
   routers (`OPA_RELOAD`, local only);
3. answer any pending notice for that hash with `CONFIG_RESPONSE {action: "sync", status: "ok"}`
   (a notice still unsatisfied after 5 minutes is dropped — e.g. its policy was superseded).

A manifest with `hash: ""` clears the policy. A hive that was down or partitioned catches up as
soon as Syncthing brings the folder up to date — no notice needed.

## Admin

- Writes are global: `POST /opa/policy` (compile + apply), `/opa/policy/compile`,
  `/opa/policy/apply`, `/opa/policy/rollback`, `/opa/policy/clear`, `/opa/policy/check`, and the
  `opa_*` internal actions. A per-hive write (`hive` other than the motherbee) is rejected; the
  `/hives/{hive}/opa/policy*` write routes no longer exist.
- The response of a change reports `hash`, the motherbee's answer (`responses`), the hives that
  answered the notice (`hives`, `running_hives`), those still converging (`pending`) and
  `converged`. It waits up to 30 s for the notices (the file takes ~11 s to sync).
- Reads stay per hive: `GET /hives/{hive}/opa/status` and `/opa/policy`.

## Security

- **Integrity:** a hive installs only a wasm whose sha256 matches the manifest. Syncthing is
  receive-only on the spokes, so a spoke cannot publish; the notice is origin-gated to the
  primary admin. The check guards against a partial transfer, not against a local writer: a
  local change to the synced copy on a hive (it needs root or the Syncthing user) affects that
  hive only and is never sent back.
- **Confidentiality:** only the compiled wasm leaves the motherbee. On each hive the installed
  policy is root-only; the synced copy is readable by the Syncthing service user (`fluxbee`).
  Encrypting the synced copy with a key held in SY.vault is a possible later step (not done).
- Root on a running hive can always read the policy its routers enforce.
