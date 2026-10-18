# PENDING-BUGS — open code tasks from the integrated test in PROD alpha

**What this is.** [FINDINGS.md](FINDINGS.md) is the record of *findings* — what was observed and with
what evidence. This document is the other half: the list of **pending code tasks** that come out of
those findings, with the proposed fix and whether it blocks or not.

Entry rule: only what **requires touching the product** goes here. Infrastructure findings (the
`B-*` series in FINDINGS) do not go here unless they imply a change in fluxbee.

**States:** 🔴 open · 🟡 mitigated (it works, but the correct fix is pending) · ✅ closed

> **⚠️ Adversarial audit of 2026-08-03**
>
> The 12 open findings went through a systematic attempt at **refutation** (rule 0d of
> [METHOD.md](METHOD.md)). Result: **only 2 survived intact** (U-3, U-9); the other 10 had
> something true inside, but the **framing, the cause or the fix was wrong** — and **three of the
> proposed fixes were dangerous to implement** (PB-6, PB-8, U-4).
>
> Pattern: *in 6 of 12, the real bug was in the same file, a few lines away — and it was not seen
> because the hypothesis was already written.* The entries below are the **rewritten** ones; each
> one says what was deleted and why.

| id | state | topic | blocks |
|---|---|---|---|
| [PB-1](#pb-1) | 🟡 mitigated | SY.edge does not reload the TLS cert when it rotates in the vault | no |
| [PB-2](#pb-2) | ✅ **closed and VALIDATED live** | Completeness of the TLS chain · `chain_len=4` served, `verify=0` from the internet | — |
| [PB-3](#pb-3) | 🟢 cosmetic | The `ResourceType` enum drifted · **no functional impact** | no |
| [PB-4](#pb-4) | ✅ **CLOSED** | The cold start **looked** like a failure: the edge blamed the vault for no reason | — |
| [PB-5](#pb-5) | 🟡 rewritten | The egress reports `nat_applied: true` without verifying its LAN leg | no |
| [PB-6](#pb-6) | 🟡 rewritten | `ssh_password` does not close by itself · **⛔ inverting the default leaves boxes without access** | no |
| [PB-6b](#pb-6b) | ✅ **CLOSED** | `harden_ssh_applied` was an echo of the request: it reported `false` with password auth already off | — |
| [PB-7](#pb-7) | ✅ **CLOSED** | `add_hive` (>6 min) overflows the admin's window (180 s) **and blocks the channel** | no |
| [PB-8](#pb-8) | ✅ **closed and VALIDATED live** | Egress for motherbee · **only reports**, does not touch the route (`EGRESS_MOTHERBEE_BYPASS`) | — |
| [PB-9](#pb-9) | ✅ **CLOSED** | Every restart of `sy-vault` recycles a healthy edge → blip in the public HTTPS | no |

### Series U — the update system (audited 2026-07-30, before using it)

| id | state | topic | blocks |
|---|---|---|---|
| [U-1](#u-1) | ✅ **closed and VALIDATED live** | The core update reported success without restarting anything | — |
| [U-2](#u-2) | ✅ **closed** | Ingress and egress had no core channel · solved by splitting `dist` by content + core by role | — |
| [U-3](#u-3) | ✅ **closed and VALIDATED live** | The `.deb` overwrote `dist/runtimes/manifest.json` · snapshot in `preinst` + a seed that restores it | — |
| [U-4a](#u-4a) | ✅ **closed and VALIDATED live** | The `/versions` verdict works, with `runtimes: n/a` on ingress and egress | — |
| [U-4b](#u-4b) | ✅ **closed and VALIDATED live** | The drift-alerts were never written · now there is a writer, and the synthesis was deleted | — |
| [U-5](#u-5) | ✅ **closed and VALIDATED live** | `core_rollback` with sealed generations, a health gate and a cross-hive forward | — |
| [U-5b](#u-5b) | ✅ **CLOSED** | Core backups piled up without GC **and were created empty on every no-op** | — |
| [U-6](#u-6) | 🟡 mitigated | The race was **the orchestrator against itself**: a non-atomic 35 MB write | — |
| [U-7](#u-7) | ✅ **closed** | The watchdog restarted syncthing every ~7 s because of a nonexistent folder | — |
| [U-8a](#u-8a) | ✅ **closed and VALIDATED live** (`9f86c12`, with PB-7) | The admin API had no contract for long operations | — |
| [U-8b](#u-8b) | ✅ **closed** (0.1.55) | `update category=core` returned TIMEOUT: now it replies before restarting | — |
| [U-8c](#u-8c) | ✅ **CLOSED** | The architect's `timeout_unknown` was **dead** because of an impossible substring | — |
| [U-9](#u-9) | ✅ **CLOSED** | The catch-all broke the envelope **and returned 404 where 405 was due** | — |
| [U-10](#u-10) | ✅ **closed and VALIDATED live** | A `.deb` upgrade left every runtime node orphaned | — |
| [U-11](#u-11) | ✅ **closed and VALIDATED live** | IO.cloud could not self-publish its channel | — |
| [U-12](#u-12) | 🟡 corrected | The race exists, but **there is an atomic call that avoids it** and I did not find it | no |
| [U-13](#u-13) | ✅ **closed and VALIDATED live** | `sy-storage` lost the startup race against the vault and **did not retry** | — |

---

<a id="pb-1"></a>
## PB-1 🟡 — `SY.edge` does not reload the TLS certificate when it rotates in the vault

**State:** mitigated with the house pattern. The correct fix (hot-reload) is still pending,
and it is a **decision already taken by the operator**: it ships like this now because it is not blocking.

### What happens

The TLS material is read **only once, at boot** ([sy_edge.rs:271-290](../../src/bin/sy_edge.rs#L271-L290))
and stays in an immutable `Arc<rustls::ServerConfig>` already moved into the listener. The
`VAULT_SECRET_CHANGED` handler existed and worked, but it only called `resolve_secrets()`, which
refreshes the **channel secrets** — nobody touched the TLS.

Neither the broadcast nor the handler was missing: what was missing was for the handler to do
something when the key that changed was the TLS one.

### Evidence in production (2026-07-30)

Reproduced live with the real cert:

```text
vault_put edge_tls_fluxbee_ai  ->  status: ok, version: 2, changed: true   (chain of 3 certs)
broadcast VAULT_SECRET_CHANGED emitted
openssl s_client against the edge  ->  certs served: 1                     (version 1, the old one)
systemctl restart sy-edge
openssl s_client against the edge  ->  certs served: 3                     (version 2)
```

Worse still: the symptom is **silent and misleading**. A lenient client (a browser, curl with the
CA cached) completes the handshake anyway by fetching the intermediate via AIA, so
`tls_verify=0` and "everything works fine" — while a strict client (Meta/Slack webhooks, Go,
Python) fails. It looks fine exactly where it does not matter.

### What was done

The **contract the house already uses** was applied, for vault material that can only be consumed
at startup: `exit(0)` and let systemd restart. It is literally what `sy.identity` and
`sy.storage` do for the postgres secret, and it is documented as Model D' VA-J'-13:

> *"The reaction is `exit(0)` so systemd reboots the node and identity re-resolves vault with the
> secret cleanly."*

Details of the fix:

- It reuses the *fail-closed* origin check that `sy_edge` **already had** (only the `SY.vault@<vault_hive>`
  this edge uses can trigger the restart; otherwise any node forging the broadcast restarts it
  in a loop).
- **It matches by key**, deliberately departing from the `(resource_type, tenant_id, ilk)` guidance that
  `VaultSecretChangedPayload` documents. That guidance is for consumers that resolve *by interest*;
  the edge is configured with **an explicit `tls_vault_key`** and looks up that exact key, so
  key equality is exactly the condition that invalidates what it loaded.
- **A `delete` does NOT restart.** Restarting without a cert is *fail-closed* → the edge is left without
  an HTTPS frontend. It would turn a mistaken call to the vault into a public outage. It keeps serving
  the cert it already has and shouts in the log.
- The decision was extracted into `tls_secret_change_action()`, a pure function, so that it is testable
  (with `process::exit` inline it was not). 4 regression tests.

### What is missing (the correct fix)

Real hot-reload: rustls's `ResolvesServerCert` reading from an `ArcSwap`, so that the broadcast
swaps the cert **without touching the socket**. Zero downtime. It is the idiomatic rustls mechanism,
not an invention. The edge is the public door: it is the only node where the house `exit(0)` has
a cost that is visible from outside.

**Why it was not done now:** the operator's decision — it has to ship working, and the house pattern
does not block. The cert rotation is yearly (Sectigo), so the brief restart is tolerated.

---

<a id="pb-2"></a>
## PB-2 ✅ CLOSED and VALIDATED live — Nobody verifies the **completeness** of the TLS chain: a truncated chain goes into the vault and the edge serves it without complaint

> **Rewritten on 2026-08-03 after the adversarial audit.** The original finding (*"`vault_put` accepts
> TLS material without validating it"*) proposed a validator that **would not have caught the
> operator's broken file**. This was verified by running the real case.

**How it showed up.** The full-chain `.crt` that the operator delivered was **malformed**: the
leaf's `END CERTIFICATE` and the intermediate's `BEGIN CERTIFICATE` were **stuck together on the same
line**, plus mixed CRLF. `openssl` reads **zero** certificates from that file.

**What is true.** `vault_put` does not inspect `value` (`sy_vault.rs:752-798`).

**What was false and was deleted:**

- *"The TLS material is not validated."* It is validated, on resolve: a non-empty chain, a parseable key and
  a key/cert match by SPKI, via `CertifiedKey::from_der` (`sy_edge.rs:1642-1652`). The whole
  family validates **on resolve, not on write** — it is the vault's deliberate contract (*"The vault
  does not interpret it"*, `docs/history/sy-vault-spec.md:177`).
- *"The failure shows up much later."* Post-PB-1 the `put` triggers `exit(0)` and the material is validated
  within seconds (`sy_edge.rs:461-475`).
- *"The symptom does not mention the vault."* `sy_edge.rs:276/288` name it.
- *"`postgres` also lacks a validator."* It exists (`sy_storage.rs:2463-2467`).

**Why the proposed fix did not work — tested, not reasoned.** With `END`/`BEGIN` stuck together,
`rustls_pemfile::certs` returns **`Ok` with 1 certificate and no error** (openssl: 0). So "non-empty
chain + key parses + modulus matches" gives **OK on the broken file**, and the edge brings HTTPS up
serving **only the leaf**. It is not the door going down: it is a **silent** failure that breaks
strict clients.

**The fix that does work:**

- **(a)** In `sy_edge.rs::tls_config_from_pem`: count the PEM's `BEGIN CERTIFICATE` markers and
  compare them with the certificates the parser returned. If they do not match → fail hard (the PEM
  was silently truncated). If the chain has a single cert → an explicit WARN *"leaf without intermediates:
  strict clients are going to fail"*. It catches the real case, and it holds both for the vault and for
  disk.
- **(b)** *Optional, the operator's decision* — the same check as a pre-flight of the `put`, or better a
  seam in the SDK that both call so that they cannot diverge. **It breaks the vault's declared opacity**,
  which is why it is not done without an OK.
- **(c)** Note the **fail-closed asymmetry**: a `delete` is deliberately protected from bringing down the
  public door (`sy_edge.rs:476-481`), but an invalid `put` **does bring it down**, and post-PB-1 immediately:
  `exit(0)` → boot → `Err` in main → crash loop. Either the boot degrades like the `delete`
  (keep the last good material), or it is documented that the recovery is `vault_rollback`.

---

<a id="pb-3"></a>
## PB-3 🟢 — The `ResourceType` enum drifted from what the product itself writes

> **Downgraded from 🔴 to cosmetic/documentation on 2026-08-03.** It has no functional impact.

**Fact.** At least four types that the product itself produces or consumes are not in `ResourceType`
(`crates/fluxbee_sdk/src/vault.rs:40-87`) nor in the `vault_put` help: `tls`, `ssh`
(`sy_orchestrator.rs:20649`), `whatsapp` (`io-wapp/src/main.rs:62`) and `linkedhelper_adapter`. The
help calls them an *"escape hatch"* while the house uses them as the normal path.

**What it is NOT.** There is no functional impact: `known_wire_values()` **has no callers**, the vault
filters by equality of the normalized string (`sy_vault.rs:1102-1110`), and a `Custom` behaves
identically to a named variant. In particular, **nobody consumes `tls`**: SY.edge binds by
`tls_vault_key` and matches **by key**, not by `resource_type` — the deliberate decision that PB-1
already documents.

**The link to PB-2 was deleted:** PB-2's validator hangs off the normalized string and can be
implemented today with `Custom("tls")`. That `tls` is not in the enum **blocks nothing**.

**Fix (pick one).** (a) Promote the four to variants and make `known_wire_values()` stop being
dead code — e.g. a WARN when admin falls into `Custom`; or (b) rewrite the help so that "normalized
custom" is a first-class path, and document the *value-shape* of each type (`tls` is missing
the `{cert, key}` row).

---

<a id="pb-4"></a>
## PB-4 ✅ CLOSED — The ingress cold start **looks** like a failure: the edge says "fix the vault" when there is nothing to fix

> **Rewritten on 2026-08-03.** The causal title was removed: what the finding asked to review **already
> exists and is deliberate**.

**Observed** after the host reboot of 2026-07-30: `restart counter is at 9`, and it ended up coming up
by itself, without intervention.

**What it is NOT.** Waiting for the vault is not missing: the wait **is** the retry. `Restart=always` +
`RestartSec=5` was chosen because the ingress hive has no local `sy-vault` and systemd cannot order
a cross-hive dependency (`docs/edge-ingress-spec-v6.md:381-386`). Backoff is not missing either:
`RestartSec=5` is exactly what keeps the unit **under systemd's rate limit** (5 starts/10 s),
which is why 9 restarts heal by themselves and never latch into `failed`. Total window ~45 s.

**The real defect, small and concrete.** `sy_edge.rs:304-307` prints the same sentence — *"Fix the vault
secret / cert files and restart."* — both when the vault is not reachable yet (transient, it heals
by itself) and when the cert is really broken (the operator is needed). On a cold start that instruction
**is false**, and it is the only source of the noise.

**✅ Fixed (2026-08-03).** `fetch_tls_config_from_vault` no longer flattens the error to a string at the point
where it still knows the type: it returns `TlsFetchFailure`, which splits the failure into **`VaultUnavailable`**
(`VaultError::Node` / `ActionTimeout` — no reply arrived, systemd retries) and
**`MaterialInvalid`** (the vault answered, or the PEM is no good — a human is needed). Startup logs
a `warn!` with *"vault not reachable yet; systemd retries in ~5 s (normal on cold boot)"* in
the first case, and the `error!` with the instruction to the operator only in the second. The final
fail-closed message forks the same way. 23/23 `sy_edge` tests green.

**Not done, optional:** retry in-process for ~15 s before the first `exit`, copying the pattern of
`sy_identity.rs:6626-6651` — it would bring 9 restarts down to 1-2. It is cosmetic on a path that already heals
by itself; I did not do it, so as not to touch the startup contract without need.

**What the finding did not see (and it is worse).** After 98adab7 the restart counter **mixes
`exit(0)` with `exit(1)`**. `sy_vault` emits `VAULT_SECRET_CHANGED op=put` for **every** secret on its
own startup (`sy_vault.rs:246-260`) and `tls_secret_change_action` does not look at `action="bootstrap"`
(`sy_edge.rs:1690-1715`): **every restart of `sy-vault` restarts a healthy edge** → a ~5 s blip in the
public HTTPS. → see **PB-9**.

**Rigor.** The cause *"waiting for the vault"* was **inferred from the code**, not from a line of the
edge's journal, and it was propagated as fact to `HANDBOOK.md:515`. The inference holds, but next time
the cold-start `journalctl -u sy-edge` must be captured before writing it down as fact.

---

<a id="pb-5"></a>
## PB-5 🟡 — The egress reports `nat_applied: true` without verifying its LAN leg

> **Rewritten on 2026-08-03.** The previous title (*"the orchestrator does not configure secondary
> NICs"*) asked for something that is **explicitly against the contract**.

**Corrected scope: only `role=egress`.** Ingress leaves the finding — `add_hive` does not ask it, and
cannot ask it, for interface names (`IngressSection:210-221`); its two NICs are a topology decision
of the v6 spec, not a gap. *(Also correct `HANDBOOK.md:403-408`, which today says "pass it
the interface names" for ingress, and that is false.)*

**The "breaks the empty-machine promise" framing was removed:** the *clean box* contract
(`07-operaciones.md:435-447`) already puts the network on the operator's side **for all roles**, and the
product does not address anyone's primary NIC either. There is no divergence between siblings: there is a
uniform prerequisite.

**"Have the orchestrator configure the NICs" is not requested.** It is against the contract
(`edge-egress-nat-spec.md:378` decides **not** to write netplan/NM) and a vNIC cannot be created from
inside the guest. If the operator wants it anyway, it is a **new feature to discuss**.

**The real bug (fail-loud, small, house style).** `reconcile_egress_nat` must run a preflight
before accepting the NAT as good: that `lan_iface` and `wan_iface` **exist**, and that `cfg.edge_ip` is
actually assigned to `lan_iface`. If not → a fatal error in the bootstrap with an actionable message, and
a persistent WARN on every drift tick, the same as is done with `internet_reachable`. Add `lan_leg_ok`
to `EgressVerification` and **stop emitting `egress_nat_applied: true` without it** — today the comment at
`:19194-19199` declares that field *"trustworthy"* and it is not.

**Why the failure is silent (a missing technical detail):** `iifname`/`oifname` match by
name **at runtime**, so the absence of the NIC does **not** make `nft -f` fail — unlike
`iif`/`oif`.

---

<a id="pb-6"></a>
## PB-6 🟡 — The `ssh_password` bootstrap channel does not close by itself — and plain `harden_ssh` is **not** the answer

> **Rewritten on 2026-08-03.** The original remediation (*"invert the default"*) **would leave boxes without
> any access path**. Partial duplicate of SO-2026-06-30-02.

**What is true.** `resolve_add_hive_harden_ssh` defaults to `false` (`sy_orchestrator.rs:20113-20118`),
with no env fallback, the same in the three flows (worker/egress/ingress) — that is, **a family contract,
not a divergence**. With the `ssh_password` channel, when the join finishes the box keeps
`PasswordAuthentication yes`.

**The original evidence was wrong.** The hardening has **two** callers:
`apply_add_hive_ssh_controls_after_finalize` (gated by `harden_ssh`, `:20820`) and
`finalize_spoke_key_persist` (**unconditional**, `ssh_access=key_only_persist` branch, `:20711`).

**Impact reframed.** `add_hive` **never turns on** password auth; the channel before the join is the
operator's prerequisite. On the recommended path (`ssh_key` on a cloud image) the server already comes
with password OFF and the default `false` leaves nothing open. The hole only appears when the
operator opened password by hand — the case the doc already covers by passing `harden_ssh:true`.

**"Invert the default" was withdrawn.** `harden_ssh=true` + the default `ssh_access=revoke` = a box **with no
password, no MB key and no sudoers** → recoverable only through the hypervisor console. It collides
head-on with the invariant the code itself declares: *"verify-before-revoke that never strands a
spoke"* (`:20121-20132`).

**Two sound options, for the operator to decide:**

- **(a)** Make `ssh_password` imply `ssh_access=key_only_persist` — it hardens **and** leaves a verified
  per-spoke key in the vault. It closes the hole **with no lockout risk**, and it invents nothing:
  it is machinery that already exists.
- **(b)** Return `warnings: ["bootstrapped via ssh_password and PasswordAuthentication left
  enabled"]`. The state **can already be queried**; what is missing is the warning.

**Correct the HANDBOOK:** the slogan *"Goes in open, comes out locked"* (`:524`) and trap 10 (`:532`)
**lead to the error** — combined with the default revoke, it is exactly what left the boxes of the
2026-07-29 deployment without any SSH path.

<a id="pb-6b"></a>
### PB-6b ✅ **CLOSED** — `harden_ssh_applied` was an echo of the request, not a measured fact

A real defect **in the opposite direction** to the finding, found while refuting it. The three flows
(`worker`/`egress`/`ingress`) literally returned `"harden_ssh_applied": harden_ssh` — the requested
flag, not what happened. With `ssh_access=key_only_persist`, which **hardens unconditionally**, a join
with `harden_ssh:false` left the box **with password auth off and verified** while telling
the operator that it was still on. A security fact, wrong.

**Fixed:** `harden_ssh_applied` is now a measured field that travels in
`AddHiveSshControlsResult` / `SpokeKeyPersistResult` from where the work actually happens — in
the gated branch, after propagating the `disable` + `verify` with `?`; in the persist branch, `true` on **both**
return paths, because the harden already happened before the verify-before-revoke decision point.
*(The `socket_only` path still reports `false`, which is correct there: it does not touch SSH.)*

**The live check is missing:** confirming it end to end requires a new `add_hive`. It compiles and
the change is local to the report, but **I have not seen it against a real box yet.**

---

<a id="pb-7"></a>
## PB-7 ✅ **CLOSED** — The real `add_hive` lasts longer than the admin's response window (180 s) — and during that time the hive's control plane is **blocked**

**What happens.** `JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS` defaults to 180 s (`sy_admin.rs:13648-13653`) against an
`add_hive` **measured at >6 min**. The client receives `{"error_code":"TIMEOUT"}` on an operation that
keeps running and usually finishes well.

**Where the time goes (corrected).** **Not** in the gates: they cap at ~150 s on any failure
path, because the LSA gate only runs if the WAN one passed. The dominant cost is the **earlier SSH
phase**: `sync_core_to_worker` + `sync_vendor_to_worker` (~35 MB) + dozens of `ssh` calls with
`ConnectTimeout=10` but **no total timeout** and with retries. **That phase has no ceiling.**

**What it is NOT.** It is neither a divergence nor an oversight: the 180 is deliberate, it is the **longest** window in the
family (update 60 / sync_hint 45 / generic 30). It is a contract that reality overflowed.

**The mitigation that was written down is half false.** *"It stays pending and the retry is
idempotent"* holds **only** if the cut falls after `write_hive_info` (`:18362`). During the SSH
phase **there is no `info.yaml`**: the retry **deletes the directory and re-bootstraps from zero**.

**And "retry to see the state" is impossible during the join.** Every `ADMIN_COMMAND` goes to a
**single serial channel** that `add_hive` occupies entirely, also **blocking the thread with
`std::thread::sleep` in async code** (`:21726`). `get_hive`/`list_hives`/`hive_status` queue up,
expire after 30 s on the client side, and **then run late**.

**Options (none of them is "raise the default to 240"):**

1. Raising the default is a **placebo**: covering the measured case would need a ~600 s HTTP window.
2. **The right thing:** split `add_hive` into **acceptance + background work** — reply `202` with
   the hive in `joining` state, run the flow in its own task (which also takes the
   `std::thread::sleep` off the executor) and **leave the admin channel free**. It closes PB-7 and U-8a together:
   they are the same gap seen from two ends.
3. **Cheap and honest in the meantime:** write `info.yaml` with `status: joining` **at the start**
   of the flow, and have the TIMEOUT return a body that says so. Without (2) it still cannot be queried
   until the worker frees up — say so explicitly.
4. Document in the HANDBOOK that **an `add_hive` TIMEOUT does not mean failure**.

---

<a id="pb-8"></a>
## PB-8 ✅ CLOSED and VALIDATED live (only reports) — The egress model **has no story for motherbee** — and motherbee is where the nodes that call the internet run

> **Rewritten on 2026-08-03.** The fix the previous title suggested (*"propagate `gateway_ip` to
> MB"*) **is a control-plane lockout**.

**What it is (not what it said).** `egress.gateway_ip` in MB's hive.yaml means, by explicit
and documented contract, *"where the **workers** go out"* (`sy_orchestrator.rs:181-184`). It is not a
value that "does not propagate": **it was never for MB**. The exclusion is an explicit arm next to Ingress
(`:5604`), not an oversight.

**The real gap.** `base-nodes.json` boots `IO.slack.default@motherbee`, `IO.wapp.default@motherbee`
and `AI.chat@motherbee`. That is: the only hive the egress model does **not** route is exactly the one that
hosts everyone who calls OpenAI/Slack/Meta. *(And `handbook:156` says "internal hives can reach
the internet through it" when the code only covers workers → correct it to "workers".)*

**Correct consequence** (it replaces *"it would go out through the admin network"*): MB goes out today through the
hypervisor's SNAT, **a temporary exception marked to be closed**. When it is closed, MB loses internet and
there is no managed mechanism to give it back. The admin network is on **both** paths — the egress's own
WAN leg is `192.168.8.240` — so the problem is not "it goes out through admin", it is **"it goes out
outside the managed chokepoint"**, where spec §11 reserves an allow-list and logging.

**⛔ Forbidden fix:** adding `HiveRole::Motherbee` to the `Worker` arm of `reconcile_egress`.
It would set the **control plane** to rewrite its own default route on every boot and every watchdog tick,
from a yaml field, with no rollback, on the box that provisions the egress. For a worker the failure is
non-fatal **by design**; on MB it is a **lockout**.

**Options (none is implemented without an OK):** (a) the operator configures MB's route at the
OS/netplan level and Fluxbee only **verifies** it and shouts if it does not match (read-only, no risk); (b) a
separate, explicit field `egress.motherbee_route: true` (opt-in, **never** inherited from the workers'
field), with a reachability gate and no re-application in the watchdog; (c) move the nodes that call
the internet to a worker — the only option that already works today.

**The most immediate trap, which was missing:** today **nobody routes** through the egress — MB does not
declare the section yet, worker1 joined before, and the injection only reaches workers provisioned **afterwards**.
And `packaging/hive.yaml.example` **does not even have the root-level `egress:` section**, so
declaring it is an undocumented hand edit.

---

<a id="pb-9"></a>
## PB-9 ✅ **CLOSED** — Every restart of `sy-vault` restarts a **healthy** `SY.edge` → blip in the public HTTPS

> Discovered by the adversarial audit of 2026-08-03, while PB-4 was being refuted. It was not in
> any finding.

`sy_vault` emits `VAULT_SECRET_CHANGED op=put` for **every secret it holds** during its own
startup (`sy_vault.rs:246-260`, `530-596`) — it is its bootstrap, not a real change. And
`tls_secret_change_action` (`sy_edge.rs:1690-1715`) **does not look at `action="bootstrap"`**: if the key
matches `tls_vault_key`, it triggers the reload `exit(0)`.

**Consequence:** every time `sy-vault` restarts (`.deb` upgrade, reboot, manual restart), the
edge — which was perfectly fine — gets recycled. In the public HTTPS that is a ~5 s blip.

**Fix:** have `tls_secret_change_action` return `NoAction` when the event comes marked as
bootstrap, or have `sy_vault` not emit `op=put` for pre-existing secrets on its startup. The first one
is smaller and local; the second one also fixes any other subscriber. Who else reacts to
`VAULT_SECRET_CHANGED` (io.slack does) must be checked before choosing.


---
---

# Series U — the update system

> Audited on **2026-07-30**, before using it, with adversarial verification and checks on the
> production machines. The trigger was the operator's decision: *"para resolver los problemas en
> prod sería mejor tener el update andando bien, y después ir por los bugs"* (to solve the problems
> in prod, it would be better to have the update working well, and then go after the bugs).
>
> **What is built works better than one would expect** — there is a manifest with sha256, an atomic
> copy with `rename()`, a backup of the previous binaries, a local rollback on failure, a staleness
> gate by hash, and for *runtimes* even a fanout with `sync_to`/`update_to`. The problem is not missing
> machinery: it is **two specific defects that make it inoperable**, and both are silent.

<a id="u-1"></a>
## U-1 ✅ CLOSED — The core update reported success **without restarting anything**

The function that runs after swapping the binaries is called
`restart_local_core_services_with_health_gate`… and it calls **`systemctl start`**:

```rust
// src/bin/sy_orchestrator.rs:3248
systemd_start(&service)?;          // fn systemd_start -> Command::new("systemctl").arg("start")
wait_for_service_active(&service, …).await?;
restarted.push(service);           // <-- and reports it as "restarted"
```

`systemctl start` on a service **that is already running** is a no-op: systemd returns success and does
nothing. The process keeps executing the old binary (the previous inode, still mapped). But the
name is pushed onto `restarted` and the API replies `status: ok` with the full list.

**There is no `systemd_restart` anywhere in the file.** The remote paths (over SSH, lines 6361/6512/6678
and 16168) do use `systemctl restart` — so **the local path is the divergent one**, which, by the rule
of looking at the peers, is exactly the signal of where the bug is.

**Why it is the worst kind of failure:** the binaries *are* left swapped in `/usr/bin`. So the update
"works"… on the next reboot. Someone can run the update, see `ok`, reboot the
machine for some other reason days later, and conclude that the mechanism works fine.

### The innocent explanation was ruled out

Before touching anything I checked whether the flow **stopped** the services before swapping the
binaries — in that case `start` would have been correct. **It does not stop them:** the binaries are swapped
hot, and the code's own comment states the intent,
*"re-render units and daemon-reload BEFORE the restart, so the new binaries start under the new
units"*. Also, the rollback path calls the same function again, expecting the services to
come up with the restored binary — which would not happen with `start` either.

### Fix applied

`systemd_start` → `systemd_restart` (a new function, aligned with the remote path). `systemd_start`
**is kept** because it is still correct in the 5 places where something that is not running
gets started.

### ✅ VALIDATED LIVE — `update category=core` against the production worker

An ideal scenario, and it came about by itself: the worker was running the `sy-orchestrator` from the original join (17:11)
while syncthing had already delivered the new binary to it in `dist/core/bin`. That is, exactly
the state that U-1 left frozen.

```text
BEFORE  pid=4027   since 17:11:10   /usr/bin/sy-orchestrator = 0716f1f1…
        dist/core/bin/sy-orchestrator = 1e2726ed…            <- real delta

AFTER   pid=54775  since 22:25:27   exe=/usr/bin/sy-orchestrator   (NO "(deleted)")
        /usr/bin = 1e2726ed… = dist                          <- promoted
```

**The `/proc/PID/exe` assert passes on the three services tested.** Before the fix, the update
would have answered `ok` with the services untouched and `exe` pointing to a deleted inode.

The worker ended up with 10 services running, 0 failed, and `connected` in the mesh.

**What the same test uncovered**: the call returned `TIMEOUT` after 60 s even though the operation
**did complete**. It is to be expected —the update restarts the target's own `sy-orchestrator`, which
is the one that had to reply— but it gives the operator an `error` on a successful operation. It is added to
[U-8](#u-8).

**Missing test** (today it has to fail, and that is the proof):

```bash
p=$(systemctl show -p MainPID --value sy-config-routes)
readlink /proc/$p/exe        # if it says "(deleted)" -> it runs the old binary
systemctl show -p ActiveEnterTimestamp --value sy-config-routes   # earlier than the update
```

---

<a id="u-2"></a>
## U-2 ✅ CLOSED — Ingress and egress had no core channel

The `hive.yaml` that motherbee writes for those two roles comes with the channel **turned off by hand**, with no
parameter:

```
src/bin/sy_orchestrator.rs:18211  (egress)   dist:\n  path: "…"\n  sync:\n    enabled: false
src/bin/sy_orchestrator.rs:18839  (ingress)  dist:\n  path: "…"\n  sync:\n    enabled: false
```

The **worker** has it parameterized (line 17440) and in prod it is `true`. **Verified on the
production machines:**

```text
ingress1  dist.sync.enabled: false      worker1  dist.sync.enabled: true
```

`POST /hives/ingress1/update {category:"core"}` stays at `202 sync_pending` forever, and the
message does not say that the channel **does not exist**.

### The machinery is already there — what is missing is sharing the folder

**fluxbee's software channel is syncthing, not SSH.** SSH exists only for the `add_hive`
bootstrap; that it gets revoked when the join closes is the correct and desired behavior, not a second
problem. Verified on the production machines:

| syncthing folder | worker1 | ingress1 |
|---|---|---|
| `fluxbee-blob` → `blob/active` | `sendreceive` | **absent** ← correct, invariant P5 |
| `fluxbee-blob-public` → `blob/public` | — | `receiveonly` |
| **`fluxbee-dist` → `dist/`** | **`receiveonly`** | **ABSENT** ← *this* is the bug |
| paired devices | 8 | 6 |

That is: **syncthing already runs on the ingress, is already paired with motherbee, and already receives a folder
in receive-only.** There is no need to set up new infrastructure or reopen any channel: what is missing is sharing
`fluxbee-dist` with it, with exactly the same `receiveonly` posture the worker already has.

**And it does not clash with the DMZ invariant.** `docs/io-blob-spec-v1.md:53` (P5) and line 278 are
specific to **blob**: *"DMZ never receives `active/`… never add ingress as a device of the
`fluxbee-blob` folder"*. `fluxbee-dist` is software distribution, not blob content; the invariant does not
reach it. (An automated audit of this finding confused the two folders and added a caveat
that does not apply — it is noted here so that it does not spread.)

### The real root cause: `public_only` does two jobs

The `hive.yaml` was the visible half. The real gate is in `reconcile_syncthing_peer_xml`:

```rust
if !public_only && dist.sync_enabled && dist_sync_tool_is_syncthing(dist) {
    // creates fluxbee-dist + adds the device
}
```

For the ingress `public_only = true`, so **the folder is skipped regardless of `sync_enabled`**, and
there is a test that pins that behavior on the motherbee end
(`ingress_public_peer_profile_never_attaches_private_folders`).

So the flag bundles two things: **(1)** not sharing `blob/active` — invariant P5, correct —
and **(2)** not sharing `dist/`, which is what leaves the ingress with no update path. The invariant
only requires the first one.

### Why removing the gate is not enough

`dist/` is not just the core binaries: it contains `core/` (389 M), **`runtimes/` (64 M)** and
`vendor/` (35 M). `runtimes/` is where `SY.wf-rules` publishes the workflow packages — that is,
potentially customer content. Sharing the whole `dist/` with a DMZ box leaves it a copy of
**all** the runtimes, including those that node is never going to execute.

### Design agreed with the operator (2026-07-30): **a dist folder per role**

> *"me parece perfecto que sea carpeta dedicada por role de hive… así se sincroniza solo esa carpeta
> según el role y minimizamos el tema de seguridad"* (it seems perfect to me for it to be a dedicated
> folder per hive role… that way only that folder is synced, according to the role, and we minimize
> the security issue)

Each role receives **only what that role executes**. It relies on two things that **already exist**:

1. **`core_component_names_for_role(&manifest, role)`** already computes the set of binaries per role, from
   `system_nodes.<role>` in the `hive.yaml`.
2. **The SSH bootstrap already does exactly this:** `sync_core_to_worker` uses that same set in
   `worker-bootstrap-minimal` mode. ⇒ **scoping by role is already implemented on one path and missing on the
   other** — once again the divergence between peers pointing to the answer.

And the scoped-folder pattern also exists: `fluxbee-blob` (private, complete) vs
`fluxbee-blob-public` (one-way, scoped to a subdirectory).

**Proposed shape:**

```text
MOTHERBEE                                   SPOKE
dist/roles/<role>/core/bin/<role's set>   ->  dist/core/bin/...     (same folder id,
dist/roles/<role>/core/manifest.json      ->  dist/core/manifest.json  different path per device)
dist/roles/worker/runtimes/               ->  dist/runtimes/        (only roles that spawn)
```

**What was verified that makes this cheap:** `local_core_bin_source_path` is fixed and
`validate_core_manifest_for_bins` validates **only the binaries passed to it** (the role's), it does not
require the whole manifest to exist on disk. ⇒ if each spoke gets its folder mounted at `dist/`,
it sees `dist/core/bin/…` as always and **the update's apply path is not touched**.

### Implemented — the final shape, and why it differs from the first idea

Before writing code, the whole surface was mapped and the design was attacked adversarially.
**Three objections turned out to be correct and fatal to the design as it was stated**, each
one verified by hand in the code:

1. **Scoping "only the role's binaries" broke the worker.** There is no `sync_runtime_to_worker`:
   `runtimes/` travels **only** over syncthing, and it is the only delivery path for published
   packages. A tree with only `core/bin` left `publish --sync_to worker` broken forever.
2. **Removing `vendor/` froze syncthing everywhere.** `update category=vendor` takes the hash
   of the syncthing binary from there; without it, a permanent `MANIFEST_INVALID`.
3. **A manifest trimmed per role broke the staleness gate.** The gate compares the sha256 of the
   `manifest.json` between motherbee and the target; a per-role manifest differs by construction, so
   **every update would be rejected as stale forever.**

**The final shape** splits `dist` by **content** (the three subdirectories already existed on disk and are
siblings, with no nesting) and scopes **only the binaries by role**:

| folder | motherbee | spoke | who |
|---|---|---|---|
| `fluxbee-dist-core-<role>` | `dist/core/<role>` | `dist/core` | the hives of that role |
| `fluxbee-dist-vendor` | `dist/vendor` | `dist/vendor` | every hive with syncthing |
| `fluxbee-dist-runtimes` | `dist/runtimes` | `dist/runtimes` | **worker only** |

**The manifest travels whole**, byte for byte the same as motherbee's, so that the hash gate keeps
working. It is metadata (names, versions, hashes) — not executables — so the DMZ box
still has on disk **only the binaries it runs**: the ingress receives 4 instead of 17, without the
181 MB of `sy_architect`, which is exclusive to motherbee.

**The path asymmetry is what avoids touching anything on the spoke side:** motherbee serves
`dist/core/<role>` and the spoke receives it as its own `dist/core`, so it keeps reading
`dist/core/bin/…` and `dist/core/manifest.json` with the same constants as always.

### Details that came out of the analysis and stayed in the code

- **The legacy `fluxbee-dist` folder is REMOVED, never re-pointed.** `ensure_syncthing_folder_in_config_xml`
  rewrites the path of an id that matches, and a `receiveonly` spoke applies remote deletions —
  the exact mechanism of the incident already recorded in this repo
  (`CORE_DIAG_runtime_deletion_by_fluxbee_syncthing.md`, syncthing deleted `runtimes/ai.common/0.1.2/**`).
  The **folder-deletion primitive had to be written, since it did not exist**: the whole reconcile was additive.
- **Folders created in isolated mode** (`ensure_isolated_...`), so that a new folder does not inherit
  another one's peer list and end up shared with hives it does not belong to.
- **Copies, never hardlinks:** `build-deb.sh` and `install.sh` write with `install -m0755`, which does
  unlink+create — a hardlink would be left pointing to the old inode while the manifest announces the new one.
- **Atomic write** (temp + rename) of the manifest and of each binary: a reader cannot observe
  a half-written file.
- **Pruning:** a role that stops running a service stops receiving its binary.
- **Pre-flight on motherbee:** if a role claims a component that the build does not bring, it fails there, loud
  and visible, instead of producing a short tree that on the spoke would be a crash-loop.
- **The `sync-hint` is resolved by the target hive.** The requester names the *channel*, not the layout;
  motherbee and the e2e scripts keep sending `folder_id: "fluxbee-dist"` unchanged and the target
  expands it to the folders its role actually has, waiting for **all** of them (a partially
  converged channel is not converged).
- **The egress joins the mesh.** It had syncthing turned off by role in two hard-coded places; now the
  config decides, as for everyone else. The operator's rationale, correct: syncthing is not a flat port —
  the device ID **is** the hash of the peer's TLS certificate, and only explicitly paired devices
  connect.

**Tests:** 8 new ones, including the rewrite of the DMZ invariant's guard, which now asserts
something sharper: the ingress stays **out** of `blob/active` (P5) but **in** its core and
vendor, and **out** of runtimes. Plus one that proves that no dist folder overlaps another one on disk.
**140/140 green.**

### Known residual

Motherbee materializes the set from its `system_nodes.<role>`, and the spoke validates against the copy that
motherbee wrote to it at join time. They match by construction unless motherbee's `hive.yaml` is edited
**after** the join — which is [U-6](#u-4) (the `hive.yaml` is never re-emitted to the spokes).
Before, that drift was harmless because the tree brought everything; now it would be a crash-loop at the
spoke's boot. Under the operator's model that change is a **major** (reinstall from scratch), so it stays
bounded — but it is the reason U-6 goes up in priority.

---

<a id="u-3"></a>
## U-3 ✅ **CLOSED** — The `.deb` upgrade deletes the hot-published runtimes · **IT SURVIVES (and it is worse)**

> **It survived the audit with its core intact**, but it was **miscalibrated in three things and
> underestimated the damage in two.** Rewritten on 2026-08-03.

`dist/runtimes/manifest.json` is a **package file** (`dpkg -L fluxbee` lists it **since
23c4175 / 2026-07-22**; earlier `.deb`s do not ship `dist/runtimes` at all), and the only
declared `conffile` is `/etc/fluxbee/hive.yaml.example`. An upgrade **overwrites it**.

**Correction 1 — the GC waits for nobody.** The text said *"the next `update category=runtime`
collects them"*. **No operator action is needed:** the retention runs by itself in the watchdog
every 300 s (`RUNTIME_VERIFY_INTERVAL_SECS`), and since the `postinst` restarts `sy-orchestrator`, the
pass happens **a few minutes after the upgrade**.

**Correction 2 — the deletion splits into two cases.** The retention also keeps what the persisted
configs reference (`persisted_runtime_keep_versions_with_root`):

- **with a persisted instance** → the directory survives the GC and the process keeps running, but
  it becomes **unstartable**: any `restart_node`/`run_node`/reboot gives `RUNTIME_NOT_AVAILABLE`;
- **without an instance** (a freshly published workflow package, a preloaded runtime) → **the whole
  directory is deleted**.

In both cases the manifest lies.

**Correction 3 — and this is the serious one: the [U-10](#u-10) fix does NOT cover this.**
`reconcile_persisted_custom_nodes` resolves the runtime's **key** against the manifest first, and
only then the version. With the key deleted, the *"that `current` means current"* **never gets to run**
and the node is abandoned. That is: after the upgrade, **every node mounted on a
hot-published runtime is left orphaned just as in U-10, and with no self-heal**.

**That raises it from "does not block (today)" to "blocks the day the first hot-published runtime
exists" — and that day has already come in the lab (`wf.router 0.0.4` on hive 240).**

**Correction 4 — the merge does NOT need to be built, it already exists.** `scripts/publish-runtime.sh` merges
(it reads the previous manifest) and the sibling installer already uses it that way against the live root
(`install.sh:895-960`, `--dist-root /var/lib/fluxbee/dist`). **The divergence is that the `.deb` runs it
at BUILD time against the staged tree instead of at INSTALL time against the live tree.** The
correct framing is *"de-diverge the two installers by calling the merger that already exists from the
postinst"*, not *"three open options"*. **The `conffile` path is discarded:** on a generated JSON it
leaves `.dpkg-dist`/prompts and merges nothing.

**→ It touches packaging. To discuss with the operator before touching it.**

---

<a id="u-4"></a>
## U-4 — Compatibility verification between motherbee and spokes · **split in two**

> **The absolute "zero verification" is false** and the obvious remediation (rejecting the peer by version)
> **self-fences the mesh**. Rewritten on 2026-08-03.

**What DOES exist, and the original finding denied:** the spoke rejects the `SYSTEM_UPDATE` with
`VERSION_MISMATCH`/`sync_pending` (exercised in `orchestrator_system_update_stale_e2e.sh`);
motherbee verifies the sha of the manifest and of each binary after the push; and the runtime manifest has a
real cross-hive compatibility contract, with the supported range `1..=2` and the write gate
`FLUXBEE_RUNTIME_MANIFEST_WRITE_V2`.

**And the consequence was wrong:** a **hard** protocol break **is not silent** — the peer
becomes `stale` and every action toward that hive fails with *"target hive not reachable in LSA"*. What does
degrade quietly is a **semantic** change, because serde tolerates default/unknown fields. The
mitigation (a payload with no protocol changes, a short window) is still the correct one, but **for that
reason**, not because "there is no safety net at all".

**The house contract, so that it is not reread as an omission:** in fluxbee **the hash is the gate and the
version is telemetry**. It looks the same in the OPA reload: different version → warn, different hash →
reject.

<a id="u-4a"></a>
### U-4a ✅ CLOSED and VALIDATED live — The HELLO carries a protocol version and nobody looks at it

`WanHelloPayload` carries `protocol: "fluxbee/1.16"` and `WanRejectPayload` exists (it is used for
`HIVE_NOT_AUTHORIZED`), but **`peer_hello.protocol` is never compared**, and the
`negotiated.protocol` that is returned is the local constant, rewritten. The same in the siblings
(`NodeHelloPayload.version`, `RouterHelloPayload.version`). **The field exists and is dead.**

**⛔ Watch out for the obvious fix:** rejecting the peer would be **self-fencing** — the cross-hive
`SYSTEM_UPDATE` travels over the mesh and requires LSA visibility of the target, so **cutting off the old peer cuts
the only channel able to fix it**. The right move is (a) to use the field only to **report skew**, or
(b) to **delete it from the contract** so that it does not simulate a guarantee it does not give. **Design decision → talk it over.**

<a id="u-4b"></a>
### U-4b ✅ CLOSED and VALIDATED live — The drift-alerts **are never written**

They are not "informational": `drift_alerts_path()` (→ `orchestrator/drift-alerts.jsonl`) **only appears on
the read path**. There is not a single writer in the whole repo — I verified it by function name and
by file name.

What `GET /drift-alerts` returns are **synthetic entries** (`local_current_state`,
`severity=info`) that `enrich_drift_alert_history_entries` fabricates **from the current snapshot**
when the local hive has no entries in the file — that is, **always**. The endpoint that the README
documents as *"Historical drift alert entries"* returns, in practice, **always** that
snapshot.

> **My own correction (2026-08-05).** Here I had written *"a live snapshot disguised as history,
> worse than returning empty"*. **It is unfair to the code and I withdraw it.** The synthetic rows
> literally say *"No historical drift alert entry was recorded for this category"*, and they travel with
> `severity: info`, `kind: local_current_state` and `synthetic: true` — a marker documented in
> `README.md:1002`. They do not deceive: they warn. Also, the synthesis **only fires if the local hive
> has no real rows**, so it retires by itself as soon as a writer exists.
>
> That is: the synthesis **is not touched**, there is no schema change and there is no pending product
> decision. The bug is **only** the missing writer.

It is the real bug behind U-4, **and it is independent of versions**. Also missing: `GET /versions`
aggregates snapshots **without issuing a verdict** (it should flag which hives differ from motherbee's), and the
WAN reject counters are a private `HashMap` with no query surface.

**→ Writing the emitter is design** (when it emits, with what severity, how it deduplicates, retention).
**To discuss.** What can be closed on its own is the lie: either the endpoint declares that what it returns
is a snapshot, or it stops synthesizing.

---

<a id="u-5"></a>
## U-5 ✅ CLOSED and VALIDATED live — There is no core rollback as a command · **gap, not bug**

> Downgraded from 🔴 on 2026-08-03: **"automatic-only" is the family contract** — vendor does not
> have a command either, and runtime does not need one because it is versioned.

> **How it ended up:** `core_rollback` exists (`POST /hives/{hive}/core/rollback`): it returns a spoke to the
> sealed generation that its last `update category=core` replaced, with a health gate and a forward over
> the mesh; the motherbee has no generations (dpkg installs its core). Recipe in the HANDBOOK,
> "The rollback". What follows is the original problem.

There is an **automatic** rollback (local, on a detected failure) and there is a backup of the previous binaries in
`/var/lib/fluxbee/orchestrator/core-bin.prev.local/update-<ms>/` *(corrected path)*. There is no core
rollback action: the only `*_rollback` ones are for vault, opa and wf_rules.

**The sentence about U-1 was deleted** (it is closed). What does hold: the automatic rollback
fires **only if the unit is not `active` within 30 s** — it does not detect a **functional** regression (a broken
mesh, incompatibility with spokes) — **and it also excludes `sy-orchestrator`**, which restarts itself
with no verification: if the orchestrator's new binary crash-loops, **there is no gate and no rollback**.

**Half the machinery already exists:** `rollback_remote_core_to_prev` restores the full previous generation over
SSH. If the command is built, it is built **on top of that** for spokes, not from zero.

**And downgrading spokes is already a supported flow:** the previous `.deb` on motherbee → dist →
`update category=core`, because the core path is **by hash** and has no monotonicity gate. The only
external part is keeping the `.deb`.

**⛔ Mandatory warning against the obvious fix.** A `core_rollback` that *"restores the most
recent backup"* is **unsafe with the current store**: (a) the directories are created **even when there are no
changes**, so the most recent one is usually **empty**; (b) `rollback_local_core_binaries` returns
`Ok()` when it **does not find** the backup — that is, the command would answer `ok` without restoring anything,
**exactly U-1's failure mode**; (c) the directories carry no `manifest_hash` or version, so
*"rollback to version X"* **is not even expressible**.
*Prerequisites:* tag the backup with the `manifest_hash` it replaced · do not create a directory on the
no-op · fail hard if the backup does not have the binaries · retention (reuse the runtimes pattern).

**A structural limit, not a shortcoming:** an in-band action is routed to `SY.orchestrator@{hive}`; **it
cannot rescue the hive whose orchestrator is the broken component.** For that case the external path
(snapshot / downgrade via apt) **is the correct answer**.

<a id="u-5b"></a>
### U-5b ✅ **CLOSED** — Core backups piled up without GC, and were created empty

Two defects, both prerequisites of any future `core_rollback`:

1. **The directory was created before knowing whether there was anything to back up** — `fs::create_dir_all` ran
   unconditionally, so **every no-op update left an empty `update-<ms>/`**. That is precisely why
   *"restore the most recent backup"* would have restored **nothing**. Now the creation is **lazy**:
   it happens only at the first binary that is actually copied.
2. **It was never cleaned up.** Now there is retention (`CORE_BACKUP_GENERATIONS_KEPT = 3`), applied **only on
   the success path** — a rollback still needs its generation on disk. It is *best-effort*: if
   the cleanup fails, it logs `warn!` and does not turn a good update into a reported failure.

`prune_core_backup_generations_in(root, keep)` takes the root as a parameter so that it is testable without touching
the real state dir. Two tests: that it keeps the newest ones and respects what is not a generation, and that it
**never empties the store** (below the threshold it touches nothing, and a nonexistent root is not an error).


---

<a id="u-6"></a>
## U-6 🟡 MITIGATED — `add_hive` runs a race between the vendor push over SSH and the syncthing folder

**Found live** while rebuilding prod (2026-07-30), on the first `add_hive role=ingress`:

```text
status: error
SYNC_SETUP_FAILED  motherbee public Syncthing setup failed:
  vendor manifest size mismatch for syncthing: expected=35806960 actual=22364160
```

**22364160 / 35806960 = 62 %** — a half-written file.

**Cause, and it is a consequence of [U-2](#u-2):** until now the ingress had no dist channel, so
`sync_vendor_to_worker` (push over SSH) was the **only writer** of `dist/vendor` on the hive that was
joining. With the `fluxbee-dist-vendor` folder added, **syncthing writes the same file at the same
time**, and the size validation —which is fail-closed, and rightly so— lands on the intermediate
state.

**Verified that it was transient:** on inspecting afterwards, motherbee and the ingress both had
exactly `35806960`, and the retry of the join passed without touching anything. But **the first attempt fails**, and
that is not acceptable in an `add_hive`.

**Proposed fix (order, do not retry):** seed vendor over SSH **and only afterwards** add
the device to `fluxbee-dist-vendor`. The order also matters for a deeper reason: syncthing needs
its own binary to start, so the SSH push has to come first anyway —
today it is simply not awaited before the folder is enabled.

**To discuss:** whether the validation should also tell "still converging" apart from "corrupt",
the same as the update's staleness gate does.

---

<a id="u-7"></a>
## U-7 ✅ **CLOSED** — The watchdog looks for a folder that is never declared

On motherbee, every 5 seconds:

```text
WARN failed to verify syncthing folder health; scheduling runtime reconcile
     folder=fluxbee-dist-core-motherbee
     error=syncthing db status ... 404
```

**My bug, introduced with [U-2](#u-2).** The watchdog calls
`dist_sync_folders_for_role(dist, state.role, is_motherbee)` with its **own** role; on motherbee that
produces `fluxbee-dist-core-motherbee`. But motherbee **does not declare that folder** — it declares the
worker/ingress/egress ones (`ROLE_CORE_DIST_ROLES`), because it does not sync with itself.

**Real damage, measured before getting alarmed: none.** `NRestarts=0`; `ensure_blob_sync_runtime` only
restarts on a real config change, so it is a noisy warn, not a destructive loop. But it
**masks real problems** — which is exactly what a warn every 5 s does.

**Fix:** have the watchdog use the same criterion as the declaration — if it is motherbee, iterate
`ROLE_CORE_DIST_ROLES`; if not, its own role.

---

<a id="u-8"></a>
## U-8 — Long operations with no queryable progress · **split in three**

> Rewritten on 2026-08-03. U-8b's diagnosis pointed to a **false** cause, and in U-8c the audit
> uncovered the best finding of the batch: **the machinery U-8 asked to build already exists and
> is dead because of a one-line bug.**

<a id="u-8a"></a>
### U-8a ✅ CLOSED (`9f86c12`, with PB-7) — The admin API has no contract for long operations

> **Closed together with [PB-7](#pb-7):** `add_hive` replies `202`, the join runs in the background and
> its phase is kept in `info.yaml` (`GET /hives` shows it). Validated live on 2026-08-05: the control
> plane answered in ~100 ms during a join.

An `add_hive` takes **more than 6 minutes** and there is no way to ask it which step it is on: the audit log
is written **at the end**, there is no operation-status action in the registry, and the new hive's
`info.yaml` (`status: pending`) is written **only after the 60 s + 60 s gates**, so during the
window `GET /hives` shows nothing.

**Mitigations that ALREADY exist and must be documented in the HANDBOOK before coding anything:** (i) the
timeouts are knobs — `JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS` / `JSR_ADMIN_UPDATE_TIMEOUT_SECS`, **not
set in `packaging/`**; (ii) the hive stays `pending` and retrying `add_hive` is idempotent
*(with the [PB-7](#pb-7) caveat: it only holds if the cut fell after `write_hive_info`)*.

It closes together with [PB-7](#pb-7) option 2 — they are the same gap seen from two ends.

<a id="u-8b"></a>
### U-8b ✅ CLOSED (0.1.55) — `update category=core` returned TIMEOUT after 60 s on a successful operation

> **The operator's decision (2026-10-02): path 1.** With `SYSTEM_UPDATE category=core`, the target
> replies when the new binaries and units are in place (`phase: "restarting"`). The
> restarts, with the health gate, the rollback and the orchestrator's self-restart, run afterwards in
> a task that holds the lifecycle lock from the preparation on, so no other update
> interleaves. They start 2 s later, so that the reply goes out before the router goes down. The
> result is kept in `core-update-last.json` and `/versions` shows it in `core.last_update`. The
> `add_hive` finalize stays synchronous (`prepare_core_update` + `restart_core_after_update`).
>
> **Validated live with 0.1.56 (2026-10-03):** the three spokes answered `status=ok
> phase=restarting` and finished with `core.last_update` at `done ok`. The deploy's first send had
> not arrived (`TRANSPORT_ERROR`, B-16) and it was repeated by hand.

**The cause that was written down is false.** It is *not* that the target's orchestrator restarts
itself: the code **explicitly avoids it** — it excludes itself and **defers its restart to a 2 s
timer precisely so as not to kill its own reply**.

**Update 2026-10-02 (measured in the 0.1.53 deploy, worker1):**

- **Cause 2 does not apply:** the full update, with the restarts, took ~2 s.
- **What happens:** the reply (922 bytes) is written over the old connection to the just-restarted
  router (`tx_loop write_frame failed: Broken pipe`) and the SDK reconnects a millisecond later.
- **Why the SDK does not find out earlier:** the `rx_loop` does not read the EOF while the orchestrator processes
  the update, because it does not consume what it receives and the queue fills up.
- **It is not an SDK bug:** the protocol (§10.6) defines that on reconnect the TX queue is emptied and that
  resending is the node's responsibility.
- **Paths:**
  1. reply after applying binaries and units, and before restarting; the restarts, with their
     health gate and rollback, run afterwards and their result is recorded for `/versions`;
  2. keep the reply synchronous, waiting for the reconnection without blocking reception.
- **Recommended:** 1, the same pattern the orchestrator already uses for its own restart. It is the
  operator's decision, because it changes what the reply's `ok` means.

**The earlier analysis:** (1) the target restarts its own `rt-gateway` bus **first**,
which is the way the reply must come back, **and the send failure is swallowed** (`let _ =`);
(2) the **serial** restart with a 30 s health gate per service over 8 services can exceed
60 s on its own. *(Aside: if the target is motherbee, the one that restarts in the middle of the request is
`sy-admin`.)*

<a id="u-8c"></a>
### U-8c ✅ **CLOSED** — The architect's `timeout_unknown` was dead because of an impossible substring

> **The most valuable finding of the batch, and it was not in any findings.** The attempt to
> refute U-8 uncovered it.

The semantics of *"in progress / it may have finished fine / go and verify"* **already existed** in
SY.architect: the `timeout_unknown` state, and `reconcile_timeout_unknown_operation`, which for `add_hive`
queries `get_hive` and marks `succeeded_after_timeout`.

**It was dead.** The classifier matched the substring `"timeout waiting ADMIN_COMMAND_RESPONSE"`
against a `Display` that **since d04cce8** says:

```text
timeout waiting response trace_id=… target=… request_msg=… response_msg=ADMIN_COMMAND_RESPONSE …
```

The words `waiting` and `ADMIN_COMMAND_RESPONSE` **are never adjacent**. The predicate was
**permanently false**: every expired operation was persisted as `failed` (terminal), the
reconciliation **never ran**, and the operator saw an `error` on a probably
successful operation — **exactly the damage U-8 describes**.

**Fixed:** `RpcError::is_timeout()` / `timeout_response_msg()` in the SDK, and the architect matches
the **typed variant** via `downcast_ref` walking the `source()` chain, instead of sniffing
strings. Three new tests, including the one that was missing: **one that asserts that the real `Display` does not
contain the old substring**, so that it cannot happen silently again.

**Method note:** the `sy_orchestrator` sibling **already had it right** —a comment documenting the
shape of the `Display`, the correct substring and a test—. The architect was the outlier. Once again the rule
*"check the siblings"*.

---

<a id="u-9"></a>
## U-9 ✅ **CLOSED** — Unknown routes returned a bare `not_found`

`POST /hives/motherbee/hives` (my mistake: the correct route is `POST /hives`) returned exactly
`{"error":"not_found"}`.

**The primary defect was one of ENVELOPE, not of friendliness.** Every other admin reply is
`{status, action, payload, error_code, error_detail}`; the catch-all emitted `{"error": …}`, **the only
key like that on the whole surface** — a client reading `error_code` got `null`.

**And the worse case, which the finding did not see:** the same catch-all swallowed **the wrong method on
a route that DOES exist** (`PUT /hives`), returning *"does not exist"* for something that exists. More harmful
than the lack of hints.

**Fixed, as reuse and not as a new system:**

- the family envelope + `error_code: UNKNOWN_ROUTE`;
- **405 `METHOD_NOT_ALLOWED`** with `allowed_methods` when the route exists with another verb, deriving
  the table from `admin_action_path_patterns` over `INTERNAL_ACTION_REGISTRY` so that it cannot
  diverge from the catalog;
- a `see: "GET /admin/actions"` pointer — **a pointer, not a dump**: the default bind is loopback,
  but it can be overridden and there is a documented public reverse proxy, and that endpoint already has its own
  exposure decision;
- the echoed path is **truncated to 256 and stripped of control characters** (it comes from the client).

Exact scope: SY.admin's catch-all. It does **not** include `/modules/*`, `sy_edge` or `io.api`, which already
reply typed. **SY.architect's twin was left as it is on purpose:** there `{"error": …}` is
what its neighbors use, so **it is not the outlier** — de-diverging it would break its own family.

**Residual noted, not fixed:** `externalize` / `unexternalize` / `list_externalized` have real REST
routes but **are not in `INTERNAL_ACTION_REGISTRY`**, so `GET /admin/actions` does not
list them and the pointer lies for the `/channels/*` family. They were added by hand to the diagnostic
table (`ADMIN_ROUTES_OUTSIDE_ACTION_REGISTRY`) so that the 405 works, but **putting them in the
registry touches dispatch and authz → the operator's decision.**

---

<a id="u-10"></a>
## U-10 ✅ **CLOSED** — A `.deb` upgrade leaves every runtime node orphaned · BLOCKING

**Reproduced in production on 2026-07-30**, on the first real upgrade (`0.1.1 → 0.1.2`), which was
precisely the path this test came to validate. The `.deb` installed cleanly
(`Unpacking fluxbee (0.1.2) over (0.1.1)`, 0 failed units)… and **the four runtime nodes
ended up in a crash-loop**:

```text
fluxbee-node-IO.api-motherbee.service  activating auto-restart
  ExecStart=/var/lib/fluxbee/dist/runtimes/io.api/0.1.1/bin/start.sh
  Main process exited, code=exited, status=203/EXEC
```

### The exact sequence

1. The runtime directories are versioned by the package version. The upgrade
   **installs `0.1.2` and deletes `0.1.1`** — only `0.1.2` is left on disk, and the manifest says
   `current=0.1.2`.
2. But the **systemd unit** stores the **absolute path with the old version**, and nobody
   regenerates it. `203/EXEC`: the executable no longer exists.
3. **`restart_node` does not recover:** `RUNTIME_NOT_AVAILABLE: version '0.1.1' not available for
   runtime 'io.api'`. The node's persisted config was also left pinned.
4. **`PUT /nodes/{n}/config {"runtime_version":"0.1.2"}` returns `ok`** (`config_version: 2`)
   **but does NOT rebind** — it stores it as just another config key. The next
   `restart_node` fails identically. This is [U-8 of the previous analysis], confirmed: **the `ok` is a lie**,
   and that is worse than rejecting.

⇒ **Over HTTP there is no way at all to recover a runtime node after an upgrade.**

### The detail that makes it more serious

`fluxbee-firstboot` spawns the nodes with **`"runtime_version":"current"`** — that is, the operator
explicitly asks *"follow the current pointer"*. The system **resolves `current` only once** and
bakes the concrete version into the unit's path. It is not that the operator pinned: **they
asked for tracking and got a silent pin.**

### Recovery that does work (for the HANDBOOK, until it is fixed)

```bash
DELETE /hives/{h}/nodes/{n}            # kill_node — kills the process
DELETE /hives/{h}/nodes/{n}/instance   # remove_node_instance — without this: NODE_ALREADY_EXISTS
POST   /hives/{h}/nodes                # run_node
  {"node_name":"IO.api","runtime":"io.api",
   "runtime_version":"current",
   "tenant_id":"tnt:00000000-0000-0000-0000-000000000001"}
```

Two traps found along the way: the order `kill` → `remove_instance` is mandatory, and
**`tenant_id` is mandatory** even though the error does not say so until it fails
(`IDENTITY_REGISTER_FAILED: tenant_id is missing`). **And this recovery loses the node's
configuration**, which in prod (`UNCONFIGURED` nodes) cost nothing, but in a deployment with loaded tokens
it would be a real loss.

### Fix applied — the operator's decision: **that `current` means current**

*"Que siga al puntero"* (let it follow the pointer): when starting after an upgrade, the node rebinds to the new version and
restarts by itself. It is what the operator asked for when spawning, and it is what makes a *minor update*
actually arrive.

It relies on a pattern that **already existed for the core** — `regen_local_core_units`, with the comment
*"Spoke self-heal: re-render them from THIS binary's templates on every boot"*. It was applied to
the core services and not to the nodes.

**Two halves:**

1. **Persist the intent, not just its resolution.** `build_node_system_block` now stores
   `requested_runtime_version` next to `runtime_version`. (The field already existed as patchable in the
   code; what was missing was for `run_node` to write it.) A config without the field —all the
   earlier ones— is read as `current`, which is correct because they are precisely the ones that
   `fluxbee-firstboot` spawned asking for `current`.
2. **Resolve by the intent at boot.** `reconcile_persisted_custom_nodes` resolved against
   `node.runtime_version` —the old answer— and that is why it died with *"version not available"* and
   abandoned the node in the middle of a crash-loop. Now it resolves against what was requested: a `current` node
   lands on the new version and **that same boot step repairs it**. An explicit pin stays
   pinned and **fails hard** if its version disappeared, which is correct for a deliberate pin.

When the pointer moves, it is logged in plain terms (`runtime 'current' pointer moved; rebinding node
from X to Y`) and the new resolution is persisted — so the config on disk reflects what runs.

**4 regression tests**, including the one for the old config without the field. 147 green.

### ✅ VALIDATED LIVE — and it took three rounds

It was tested with real chained upgrades in production. **Each attempt failed differently, and that is
what made it possible to get to the bottom of it:**

| upgrade | result | what it taught |
|---|---|---|
| 0.1.2 → 0.1.3 | ❌ `failed=4` | The re-resolution by intent **did work** (`pointer moved from=0.1.2 to=0.1.3`), but the relaunch died: *"Unit … was already loaded or has a fragment file"*. The nodes are **transient** units and systemd does not reuse a name that is still loaded — and a node in a crash-loop is in `auto-restart`: not active, but loaded, holding on to its name with the old `ExecStart`. |
| 0.1.3 → 0.1.4 | ❌ **identical error** | That sameness was the clue: if the cleanup had run and failed, the message would have changed. `systemd_unit_exists()` **appends `.service` to the name**, and it was being passed one that already had it → it asked for `…service.service` → always "not-found" → the guard exited early and **cleaned up nothing**. |
| 0.1.4 → 0.1.5 | ✅ **`started=4`** | The four nodes repaired themselves **on their own, in ~25 s, with no intervention at all**. |

The full sequence in the log of the last one:

```text
21:26:10  reconcile completed  started=0  skipped=0  failed=4      <- the previous upgrade
21:43:28  runtime 'current' pointer moved   from=0.1.4  to=0.1.5
21:43:28  freed stale transient unit name before relaunch
   (×4)
21:43:30  reconcile completed  started=4  skipped=0  failed=0      <- ALL
```

Units pointing to `runtimes/<rt>/0.1.5`, configs with `rv=0.1.5 requested=current`.

**Method lesson:** an error that repeats **identically** after a fix almost never means "the
fix was not enough" — it means that **the fix did not run**. If it had run and failed, the
message would be different.

### The residuals, resolved

- ✅ **The rebind over HTTP no longer lies.** The route wrapped **the whole body** under `"config"`,
  while `set_node_config` reads the five binding fields at the **top level** — so
  `{"runtime_version":"…"}` landed as an inert config key and the call answered
  `ok` without rebinding. Now the route lifts them and logs it.
  **And it also validates**: testing it live, it accepted `9.9.9`, a nonexistent version, with `ok` — the
  same sin with another face. Now a rebind to a version that is not installed returns
  `RUNTIME_NOT_AVAILABLE` at request time, not at the next restart.
- ✅ **The SSH noise went quiet** *(2026-10-06: obsolete, the TLS reconciliation over SSH was
  deleted with D32)*. The TLS reconcile emitted
  *"failed to distribute mesh TLS material … Permission denied (publickey)"* for every hardened
  spoke, on every pass. A hive joined with `ssh_access=revoke` **has no** SSH channel on
  purpose: that is the hardening working, and reporting it as a warn trains people to ignore the log.
  Now, when there is no recovery key and the failure is an authentication one, it drops to `debug` with a
  message that says why. **Verified: 0 warns since the new startup.**
- ⚪ **`tenant_id`: discarded.** It IS documented (`sy_admin.rs:10520`, *"AI.* and IO.* managed
  nodes require root-level tenant_id on first spawn"*). My mistake was not reading it.
- 🔴 **One remains**: the order `kill` → `remove_instance` is mandatory and is not stated
  anywhere (`NODE_ALREADY_EXISTS` if it is skipped). It goes to the HANDBOOK.


---

<a id="u-11"></a>
## U-11 ✅ CLOSED and VALIDATED live — `IO.cloud` cannot self-publish its own channel

**Found on 2026-07-30** while configuring IO.cloud in production to connect Fluxbee Cloud.

When starting with `IO_CLOUD_EDGE_NODE` set, the node creates its ICH and asks to publish it. The hive
rejects it:

```text
INFO  IO.cloud own ICH ensured  ich_id=ich:14b66389-…  owner=IO.cloud@motherbee  enabled=true
WARN  IO.cloud -> SY.admin externalize REJECTED  error_code=UNAUTHORIZED
      IO.cloud may relay only ["create_tenant","vault_put","run_node"] over the mesh,
      not 'externalize' (Fluxbee Cloud provisioning gate)
```

### It is a clash between two controls that are each fine on their own

**`authorize_cloud_relay`** (`sy_admin.rs:5401`) is *default-deny* for `IO.cloud@<hive>`: it can
only relay the three actions in `CLOUD_EXPOSED_ACTIONS`. That is FIX-1 and it is deliberate — the
comment explains that it exists so that a compromised IO.cloud does not become a *confused deputy*.
Correct.

**`authorize_channel_command`** (`sy_admin.rs:5374`) is the gate made **specifically** for this:
only `IO.*` nodes, and **only on their own channel** (`owner == caller`). It would have authorized this
call with no problem: caller `IO.cloud@motherbee`, owner `IO.cloud@motherbee`.

**The problem is that the first one swallows the case before the second one can weigh in.** The
default-deny does not tell apart two different things:

- **relaying** — acting *on behalf of Cloud*. That is what must be contained, and the three
  allowed actions are exactly that.
- **acting on its own channel** — the node acting *for itself*, on a resource it
  owns. It is not a relay, and it already has its gate with the ownership restriction.

IO.cloud's own code says it expected the second one
(`nodes/io/io-cloud/src/main.rs:118-123`): *"the same node→admin ADMIN_COMMAND path a real deploy
uses; it is self-service (requester owns the ICH). SY.admin authorizes this by router-stamped IO.*
origin plus `requester == ICH owner`"*.

### Impact

`IO.cloud` **is not reachable from the internet** until an operator publishes the channel by hand. And
it has to be repeated on every reinstall of the hive. Documented as a mandatory step in
[`docs/io-cloud-api.md`](../../docs/io-cloud-api.md) §5.

**Workaround verified in production** (`POST /channels/externalize` from the admin, where
`caller=None` is a trusted internal path):

```json
{"ich":"<the one from the log>","edge_node":"SY.edge@ingress1","inbound_family":"user",
 "auth_mode":"shared-secret","secret":"<IO_CLOUD_SECRET>","methods":["POST"]}
```

### Proposed fix — to discuss

Have `authorize_cloud_relay` **delegate the channel commands** to `authorize_channel_command` instead
of denying them outright. It keeps the default-deny for the relay intact (which is what contains a
compromised IO.cloud) and returns the decision about its own channels to the gate that already knows how to make it,
with its `owner == caller` restriction.

### ✅ Fixed and VALIDATED live

The delegation was applied: `authorize_cloud_relay` lets `externalize`/`unexternalize`
(`CHANNEL_SELF_SERVICE_ACTIONS`) through to the handler, where `authorize_channel_command` still
requires `owner == caller`. **It does not widen the relay**: a compromised IO.cloud still cannot
touch someone else's channel or relay anything outside the allowlist.

**Tests that pin the boundary, not the happy path:** the two channel commands pass; `add_hive`,
`vault_get`, `publish_artifact` and `sync_hint` are still denied; and a channel command from a
**non-IO** caller passes this gate —it has no relay privilege to contain— so that the channel gate
rejects it by rule I1.

**Validation in production** (0.1.6): the channel was unexternalized by hand and the node was restarted, without
touching anything else.

```text
INFO  IO.cloud own channel ICH enabled — ready to externalize on SY.edge
WARN  externalize rejected by a retryable edge condition; retrying        attempts=1
INFO  externalize OK (authenticated Cloud URL published on the edge)      attempts=2
```

There is no more `UNAUTHORIZED`. The first attempt hit a momentarily unresolvable edge and **the
node's own retry resolved it** — the retry logic working as designed. Then,
from the internet: without a credential `401`, with a bearer a complete `create_tenant` with a new `tenant_id`.

⇒ **The operator no longer has to publish the channel by hand.** Step 2 of the checklist in
[`docs/io-cloud-api.md`](../../docs/io-cloud-api.md) §5 is obsolete as of 0.1.6.

---

<a id="u-12"></a>
## U-12 🟡 — The race exists, but the atomic operation IS ALREADY BUILT

**Corrected on 2026-08-04, the same day I reported it.** I framed it wrong.

### What is true

Chaining `kill_node` and then `remove_node_instance` **is** a race: the kill replies
`status: ok` **before** systemd takes the unit down, and the immediate remove fails with
`NODE_INSTANCE_RUNNING`.

And so is the consequence: `remove_node_instance` is the only thing that deletes
`/var/lib/fluxbee/nodes/<KIND>/<node>/`, so if it fails and nobody looks at the reply, the config
stays and `reconcile_persisted_custom_nodes` **relaunches the node** on the next startup. It happened to me: I took
two test nodes as cleaned up and they reappeared after the next upgrade.

### What was wrong

I proposed as the fix *"an atomic purge that does both"*. **It already exists.** `kill_node` accepts
`purge_instance`, and the self-documentation itself says so:

> *"Stop a node in a hive. Optional **purge_instance** also removes its persisted instance directory."*

with the example included:

```bash
curl -X DELETE /hives/<hive>/nodes/<node>@<hive> -d '{"purge_instance":true}'
```

**Tested in production**, a single call:

```text
before:   unit active    · config exists
DELETE {"purge_instance": true}  ->  status: ok
after:    unit inactive  · config DELETED
```

No race, no second call, no wait.

### What remains as a real finding

1. **The two-call sequence is still a trap** for whoever uses it — and I had
   written it in the HANDBOOK as the recovery recipe. **Corrected there**: it now uses
   `purge_instance`.
2. **`instance_removed` comes back `null`** even though the deletion happens. The field is in the reply and
   is not filled in, so a client that checks it cannot confirm the purge.

### The lesson, which is the third time in this session

Before proposing to build something, **ask the system whether it already has it**
(`GET /admin/actions/<action>`). It would have saved me a badly framed finding and a wrong
recipe in the handbook.

---

<a id="u-13"></a>
## U-13 ✅ **CLOSED** — `sy-storage` lost the startup race against the vault and did not retry

**Observed in production**, six times in the same journal, in two forms:

```text
SY.storage vault lookup for postgres_url_ref failed; starting degraded
  error="... vault unreachable: reason=NODE_NOT_FOUND original_dst=SY.vault@motherbee"
  error="... node error: disconnected"
```

Losing the race **is not unusual**: it is the normal result of systemd restarting everything together in a
`.deb` upgrade. And it was not a verdict from the vault ("I do not have that secret") — it was that `sy-vault`
had not announced itself to the router yet. Nobody answered.

**Why the existing rescue was not enough.** It is **push**: reacting to a later `VAULT_SECRET_CHANGED`.
The vault's bootstrap broadcast is a **one-time** event, and a storage that
was not listening yet does not get a second notice. It stays degraded until a person restarts it,
with the hive's admin plane down in its wake, behind `STORAGE_NOT_READY`.

Ordering the units does not fix it: `sy-storage` and `sy-vault` are both `Type=simple`, where
"started" means "the process was executed", not "it announced itself on the bus". `After=` would be decoration.

**The fix: the pull is missing.** It goes in the SDK (`resolve_resource_awaiting_vault`), not in each binary,
because `sy-identity` **already retried** — storage was the outlier. A single definition of the cut:
`Unreachable`/`TtlExceeded`/`Node`/`ActionTimeout` are retried; `Service`/`EmptyValue`/
`InvalidVaultRef` are not, because the vault already gave a verdict and it is going to repeat it.

**Validated live three times**, in the exact failure mode:

| event | observed error | outcome |
|---|---|---|
| upgrade 0.1.11→0.1.12 | `node error: disconnected` | retried, `storage_ready=true` in **1 s** |
| ACPI reboot | `NODE_NOT_FOUND` ×3 | retried ~3 s → resolved |
| hard reset | `NODE_NOT_FOUND` ×2 | retried ~4 s → resolved |

`STORAGE_NOT_READY` in the orchestrator: **0** in both startups. Before: 70+.

### Residual noted, not fixed — to discuss

The push rescue fires anyway **even if the consumer has already resolved through the pull**. The
`exit(0)` is unconditional once origin and interest pass: it does not compare the incoming secret
against the one already running. Measured on the cold start: storage and identity resolve by themselves
and ~7 s later they restart when the broadcast arrives.

It is not a bug that this fix introduced, and it is **harmless** (`Restart=always`, they come back in
~5 s). But the comment in `sy_vault.rs` said it was idempotent "re-resolving the same value
and continuing", which is not what happens — that is already corrected. Really refining it (exiting only if the
secret **changed**) touches a safety mechanism that is still needed for a real
rotation, so it is left to decide with the operator.
