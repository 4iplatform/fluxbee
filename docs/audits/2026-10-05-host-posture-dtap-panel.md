# DTAP panel — host posture and exposure design, revision 1 (2026-10-05)

**Reviewed:** `docs/host-posture-and-exposure-spec-v1.md`, revision 1.

**Panel:** four agent reviewers, each followed by an adversarial verifier that tried to refute
every finding against the repository:

- Development: does the design hold against the code?
- Test: what proves each stage?
- Acceptance: does it do what the operator asked?
- Production: operations and attacker view.

**Verdict:** all four lenses failed.

| Lens | Findings | Blockers |
|---|---|---|
| Development | 18 | 1 |
| Test | 17 | 2 |
| Acceptance | 21 | 1 |
| Production | 19 | 1 |

Verifiers: 51 confirmed, 24 partial, 0 refuted. A "partial" usually means the claim held but the
verifier corrected a detail or the recommendation; the corrected form is what went into the
document.

**Disposition:**

- Every finding is folded into revision 2, or tracked as an operator decision (O1–O8, spec §2.2)
  or a separate finding (A-48, A-49).
- The raw panel output (claims, evidence with file:line, recommendations, verifier notes) stayed in
  the session scratchpad. This file is the durable record.
- Revision 3 (same day): the operator answered O1–O4, recorded in the spec as D16–D19, and set
  the order of work, D20: core, then the Cloud connection, then the implementation nodes.

---

## Blockers

| Id | Finding | Where it went |
|---|---|---|
| D-1 | `add table` + `flush table` + define does not replace named sets or chains. A removed hive would stay admitted and a removed chain would keep its policy. | §3.6: render `table {}` + `delete table` + full definition in one `nft -f`. A-48 for the credentials `remove_hive` leaves valid. |
| T-1 | "Static addresses first, discovery off later" holds only if the two stages ship and are validated one at a time. A skip-upgrade cuts worker1's sync. | §3.5: the order is enforced in code, with pure predicates on the spoke and the motherbee. Stage 3 gate: skip-upgrade test. |
| T-2 | Rollback covers only the live ruleset. A failed ruleset survives in the boot file, the watchdog or a reboot. | §3.6: persist only after verify, `.prev`, a commit-confirm timer, quarantine of a failed hash, hold-down, errors never abort bootstrap. |
| A-1 | The O1 default (the motherbee's subnet) admits the DMZ ingress to SSH and locks out VPN operators. Absent sources were undefined. | §3.3: explicit CIDRs only; hive addresses subtracted; no sources means no enforce. O1 asks the operator for the data. |
| P-1 | A compromised ingress can mint and use an Archi session by itself. The edge is the only bearer check and terminates TLS. | §1.5 states today's trust path. O5 offers (a) end-to-end (Cloud-signed mint, encrypted code, SNI passthrough) or (b) accepted risk. |

## Firewall mechanics (Part A)

| Id | Finding | Where it went |
|---|---|---|
| D-7, A-8, P-7 | nftables is not a dependency outside egress. | §1.2, §3.6, §3.7: `.deb` Depends, `add_hive` preflight on every role, `firewall: unavailable` without failing. |
| P-10, A-8 | Boot unit ordering vs `nftables.service`; its `flush ruleset`; interface names. | §3.6: `After=nftables.service`, `iifname`/`oifname` only, the conflict reported, drift repaired. |
| D-14, T-6, T-7, P-11 | Observe mode cannot produce evidence and drops invalid packets. Counters reset. | §3.6: a dynamic would-drop set keyed by tuple, accumulated and split by source class; invalid only counted in observe; enforce = observe + final verdict. |
| D-15, A-15 | Egress would have two inbound owners. | D2: `fluxbee_host` owns inbound everywhere; `fluxbee_egress` keeps forward and NAT. |
| P-18 | Conntrack flood on the ingress public port. | §3.3: the public port runs untracked; conntrack fill is reported. |
| D-8 | DEV uses DHCP. Detection was undefined. | §1.1, §3.3: DHCPv4 is unaffected (packet sockets); DHCPv6 rule only where used; client socket expected in the audit. |
| P-13 | 443 was hard-coded. | §3.1: the edge port comes from `edge.listen`; non-443 or plaintext is reported. |
| A-9, P-6 | Removing old ufw rules breaks a host where ufw is active. There was no way to declare customer ports. | §3.8: remove only when ufw is inactive; an active third-party firewall keeps the host in observe with the list of ports to open. §3.2: `extra_inbound`. |
| T-17 | No test for third-party detection or cleanup. | §3.8 classification; stage 4 gate. |
| T-8 | Correctness depends on nft and kernel behaviour that string tests cannot see. | Stage 4: CI job applying every role and shape inside a network namespace, plus a veth connect test. |

## Sources, admission, settings for spokes

| Id | Finding | Where it went |
|---|---|---|
| D-2 | Nothing delivers posture settings to spokes. | §3.2: the motherbee pushes them over a new protected SYSTEM action; the spoke stores them; with no settings it stays in observe. |
| D-3, P-5, T-14, A-18 | The registry `address` is an SSH target, can be a name, and NAT changes the source. | §3.2: per-hive `source_ips` reported at join and backfilled from WAN sessions; mismatches reported and blocking enforce. D3: no NAT between hives in v1. |
| P-3 | Which registry states are admitted, and when the sets are applied. | §3.2: every entry until `remove_hive`; applied in the join-accept path before `spawn_background_join`. |
| D-5, T-10, P-9 | SSH rules unsafe both ways; port hard-coded; no operator-path check. | §3.3: ports from `sshd -T`, hive addresses subtracted. §3.6: break-glass file and the enforce precondition that SSH was seen from a declared source. |

## Verify and rollback

| Id | Finding | Where it went |
|---|---|---|
| D-4, P-2, A-7, T-11 | Probes revert on correct rulesets (9100 from ingress and egress), offline spokes block every apply, and joining hives fail. | §3.6: probes per role from the same declaration; a baseline before the apply; offline and joining hives skipped; negative probes report only; timeouts never pass; targets derived locally. |
| A-7 (verifier) | An unlisted SYSTEM verb is delivered ungated, not refused. | §3.6: the probe action joins `PROTECTED_SYSTEM_ACTIONS` + Rego/wasm in the same commit. |

## Lifecycle

| Id | Finding | Where it went |
|---|---|---|
| D-9, T-12, P-8 | Nothing removes or downgrades the posture. | §3.7: `REMOVE_HIVE_CLEANUP` teardown with an offline warning, prerm on removal, mode `off` before a downgrade, a format version, conntrack flush, and lifecycle steps in the stage 4 and 5 gates. |

## Syncthing

| Id | Finding | Where it went |
|---|---|---|
| D-6, T-4 | Stage 2 could not repair joined spokes. A finalize without an address rewrites `dynamic`. | §3.5: addresses reconciled at boot and watchdog; `AcceptOnly \| Static`; finalize without an address refused. |
| D-10, T-16 | A template with public infrastructure off exists, but only the dev path seeds it. | §1.4, §3.5: the orchestrator owns `<options>`; the template is aligned; validation only through the runtime REST API. |
| D-11 | No options editor; three unlocked writers; the precheck passes over a relay; "internal address" undefined; UDP unused. | §3.5: one serialized writer (REST preferred); `tcp-server` check; listen addresses defined per role; TCP only. |
| A-17, P-12 | Matrix vs listen addresses; binding at boot; add_hive wording. | §3.3, §3.5: TCP only, spokes on loopback, `After=network-online.target` on the motherbee, "derive; refuse only if impossible". |
| T-5 | The stage 3 gate named no measurement. | Stage 3 gate: REST checks, `ss` sampling, reconnection times, CI pair test. |

## Binds (stage 1)

| Id | Finding | Where it went |
|---|---|---|
| D-17 | A non-configurable host means changing Archi's code; the code default is already loopback. | O2: (a) config only, recommended; (b) code. |
| D-16, A-20, P-19 | Missed dependencies: `lab-install.sh` rewrites admin; Archi's UI text; the Docker lab's published ports. | §3.4: lab reaches admin and Archi through exec or a tunnel, mappings removed, mode `off` there, never `network_mode: host`. |
| T-3 | Stage 1 could not be tested and its drift had nowhere to go. | §3.4: drift through the existing drift store; stage 1 gate; CI guard. |

## Listener audit, declarations, report

| Id | Finding | Where it went |
|---|---|---|
| T-9 | The audit missed missing listeners and contradicted the matrix. | §3.1: one declaration renders everything; §3.9: unexpected and missing, fixtures from PROD. |
| D-12, A-16 | `system_nodes` misses listening managed runtimes such as IO.web. | §3.1: core IO listeners come from the managed-node inventory. |
| A-10, P-4 | io.linkedhelper's direct listener was missing from the matrix. | §1.6; O3: (a) behind the edge, recommended; (b) declared exception. |
| D-13 | The posture report had nowhere to live. | §3.9: `local_versions_snapshot` → `/versions`, and `ops.py health`. |

## Stage gates and tests

| Id | Finding | Where it went |
|---|---|---|
| T-13 | Break tests existed only as names. | Stage 5 gate as a versioned probe script; canary order; 8.x is the authorized testbed (2026-09-28). |
| T-15 | The stage 6 gate checked only DNS and the certificate. | Stage 6 gate expanded; Host routing tests. |

## Web exposure and naming (Parts B and C)

| Id | Finding | Where it went |
|---|---|---|
| A-3, A-6 | The DMZ sees codes and cookies; the Cloud bearer becomes root-equivalent; edge v6 I2 and io-cloud bounds were silently broken. | O5; D8: the operator mint is its own action and is logged; §4.3: I2 exception and io-cloud spec updates recorded. |
| A-4, P-15 | Same-site CSRF against Archi (which has none and accepts any Content-Type); shared-domain conditions. | §4.2: Origin and Sec-Fetch-Site checks and Archi's CSP profile. D13: host-only cookies on Cloud and the brand site, Origin-based CSRF, no installation certificate covering Cloud. |
| A-5, P-17, T-15 | The edge routes by path on any Host. | §4.4: Host-first exact table, default 404, before stage 7; edge invariants listed. |
| A-11 | IO.web scope ("no quiero complicarla"). | D7: v2 = session-gated Archi plus artifacts only. |
| A-13 | "Passes the subject to Archi for its logs" would need an Archi change. | §4.2: IO.web logs the subject. |
| A-2 | "Every edge" did not match single-edge publication. | §1.5, §5.3; O7: one ingress in DNS until multi-edge publication exists. |
| A-12, P-16 | Manual DNS-01 every ~60 days, expiry, DNS credentials, takeover. | O6: (a) 1-year certificate, recommended; (b) CNAME-delegated ACME. §5.3: runbook, expiry alert, CAA, record cleanup, no DNS credentials on hosts. |
| A-14 | What of FC.edge-manager / edge-control v2 is superseded was not said. | §5.4. |
| A-19 | No registration step exists; where the ID lives. | D11: the operator picks it; it lives in `admin.public_base_url`. |
| P-15 | The brand wildcard sits on the DMZ until stage 6. | §5.3: move PROD off it early, independently of stages 1–5. |

## Residuals and facts

| Id | Finding | Where it went |
|---|---|---|
| P-14 | Source allowlists are not authentication on a shared L2; the apt repo is unsigned; outbound is open. | §0 residuals; O4 (ingress segment, anti-spoofing, outbound filter); A-49 (sign the repo). |
| A-21 | "Stages 1–5 close A-46" overstated. | §6: they close the inbound and HTTP side; §0 lists what stays open. |
| D-18 | §1 imprecisions: only the motherbee gets core ufw rules; the egress re-applies every ~60 s; no 501 stub; Archi's catch-all route. | Corrected in §1.2, §1.3, §1.5. |

---

# Round 2 — Part A of revision 3 (2026-10-05)

**Scope:** sections 0–3 and stages 0–5 of the spec, plus A-48 and A-49. Same four lenses, each with
an adversarial verifier.

**Verdict:** all four lenses failed again.

| Lens | Findings | Blockers |
|---|---|---|
| Development | 23 | 2 |
| Test | 22 | 0 |
| Acceptance | 16 | 1 |
| Production | 23 | 1 |

Verifiers: 47 confirmed, 37 partial, 0 refuted.

**The common cause.** Most findings come from mechanisms revision 2 added to filter the mesh ports
per hive IP and to steer spokes from the motherbee:

- `source_ips`, their backfill, and the comparison of session addresses;
- the settings push;
- verify probes, commit-confirm, quarantine and hold-down;
- `operator_sources` and `extra_inbound`.

Each brought its own failure modes:

- admitting a joining hive was circular (D2-2, A2-2);
- a re-addressed hive was cut off for good (A2-1, P2-11);
- spokes never get the registry (A2-3, P2-13);
- the push could be sent by any orchestrator (D2-1, P2-15);
- verify could pass with zero probes after an upgrade (P2-4);
- a widening change could stall `add_hive` (P2-5).

The Acceptance lens found that safety needs none of these (A2-1, A2-7, A2-8), which is also the
operator's "no la compliques".

**Disposition: revision 4 simplifies Part A** instead of patching each mechanism.

| Change | Findings resolved |
|---|---|
| Ports and interfaces per role, with no per-hive IPs, no registry input to the ruleset and no settings from other hives. Mesh ports are already authenticated. Removed hives are kept out at the protocol layer (A-48). | D2-1, D2-2, D2-3, D2-4, D2-23 (partly), A2-1, A2-2, A2-3, A2-7, P2-6, P2-11, P2-13, P2-15, T2-6, T2-7 |
| Automatic mode with no configuration: observe until a clean window fixed in code, then enforce; a changed ruleset goes back to observe. Two local files for us: break-glass, which the orchestrator also honours, and hold. | D2-5, D2-10, A2-6, P2-2, P2-8, T2-9, T2-10 |
| No probes, commit-confirm or quarantine: a ruleset is enforced only after it ran clean in observe. | D2-9, A2-8, P2-4, P2-5, T2-12 |
| Would-drop accounting never carries the verdict (chain policy), records only unicast to the host on internal interfaces, and keeps INVALID separate. | P2-1 (blocker), A2-9, T2-5 |
| SSH only on internal interfaces, on every role, with no configuration. Report-only sshd warnings. `operator_sources` and `extra_inbound` dropped from v1. | A2-4, P2-12, P2-14, P2-21, P2-22, T2-4 |
| Report-only TCP listener check from stage 1 (the live bind, not the config); a scoped CI guard. | T2-1, T2-16, D2-13, A2-11 |
| Syncthing: the motherbee on `0.0.0.0`, spokes on loopback, IP literals only, reconnection interval owned; ordering predicates taken from spoke reports over the existing GET_VERSIONS, over linked devices only; the whole public set gated together. | D2-6, D2-17, D2-18, T2-2, T2-3, P2-19, P2-20, A2-16 |
| nftables: `.deb` Depends on the motherbee; `add_hive` installs it on spokes; a failure means `unavailable`, never a refused join. | D2-12, A2-5 |
| Third-party firewalls: a generic check, and Fluxbee stops writing ufw/firewalld rules without touching existing ones. | A2-10, T2-17 |
| Lifecycle: the outgoing code tears the posture down on a downgrade or rollback; REMOVE_HIVE_CLEANUP runs before the registry entry goes and stops sy-edge on an ingress. | D2-8, P2-7, P2-10, P2-17, T2-14, T2-20, T2-22 |
| A-48 detailed: leaf pinned, a router registry view, sessions closed, REMOVE_HIVE_CLEANUP refused on the motherbee, SSH host keys recorded. | D2-11, A2-12, A2-13, P2-16 |
| A-49 detailed: non-interactive signing, the key shipped in the `.deb`, the postinst switches the source. | D2-22, A2-14, P2-18, T2-15 |
| §1 facts and residuals corrected. | D2-14, D2-21, A2-15, P2-23, T2-11, T2-13, T2-18, T2-19, T2-21 |

**Operator decisions.** Two simplifications narrow decisions the operator already took, so they
need his OK: D3 (filtering by interface instead of per-hive IP) and D16 (SSH only on internal
interfaces).

**Next.** A third round on revision 4, before stage 1.

---

# Round 3 — Part A of revision 4 (2026-10-05/06)

**Scope:** sections 0–3, core stages 0–5, A-48 and A-49. The prompts told the lenses that the
round-2 machinery had been removed on purpose. Same four lenses, each with an adversarial verifier.

**Verdict:** all four lenses failed.

| Lens | Findings | Blockers |
|---|---|---|
| Development | 17 | 1 |
| Test | 18 | 1 |
| Acceptance | 13 | 1 |
| Production | 16 | 3 |

Verifiers: 39 confirmed, 25 partial, 0 refuted.

**Blockers — none in stage 1:**

- **A-48 cuts every pre-existing hive** (D3-1, T3-16, A3-1, P3-2): they have no pin, and the
  motherbee cannot compute one.
- **The observe window is blind on non-internal interfaces** (P3-1), so the automatic switch would
  close SSH there with no evidence.
- **`ADD_HIVE_FINALIZE` on the motherbee is a kill switch** (P3-3), the twin of the
  `REMOVE_HIVE_CLEANUP` guard that A-48 already planned.

**Stage 1 findings:**

| Findings | Where they went |
|---|---|
| D3-2, T3-5, A3-10, P3-15 | Moot: the Docker lab was removed (D23). |
| D3-8, T3-1 | Fixtures for `*`, `[::]`, the scope before and after the brackets, several owners or none, mapped loopback; one alert kind per process. |
| D3-9, T3-3 | The migration is a separate, non-fatal script that runs before the orchestrator starts, with 19 checks in CI. |
| D3-10, T3-4 | The guard checks each URL on its own, runs on every push, fails on a missing input, and keeps history out of scope. |
| T3-2 | A positive control in the gate: a dummy `sy-admin` listener must produce the alert. |
| T3-6 | The SSE item is dropped, and `lab/posture-probe.sh` is versioned. |
| D3-3, P3-3 | The spoke-only guards ship with stage 1 (D24). |
| A3-13 | D3 and D16 are marked approved. |

**An adversarial review of the stage 1 diff** found no blocker or major. Its minors are fixed (one
alert per process, guard hardening, the scope parse order, a warning when `ss` stops parsing, the
migration on symlinks and CRLF, a tunnel carrying 8080) or became moot with the Docker lab's
removal. The theoretical upgrade race, where Archi restarts on its own between unpack and postinst,
is left: the listener check would report it.

**Everything else** (stages 2–5, A-48, A-49) is listed in spec §8. It becomes revision 6, with a
DTAP round 4, before any of those stages is built.

---

# Round 4 — revision 6: core stages 2–5, A-48, A-49 (2026-10-06)

**Verdict:**

| Lens | Verdict | Findings | Blockers |
|---|---|---|---|
| Development | fail | 19 | 1 |
| Test | fail | 18 | 0 |
| Acceptance | **pass with observations** (first time) | 14 | 0 |
| Production | fail | 17 | 1 |

Verifiers: 52 confirmed, 16 partial, 0 refuted.

**About the Test reviewer:** the safety classifier timed out while reviewing its run. Its transcript
was checked afterwards: 58 calls, all read-only (`grep`, `wc`, `Read`), no writes and no remote
commands.

**Blockers:**

- **D4-1:** revision 6's "explicit `delete chain`" for `fluxbee_egress` fails on the second apply.
  It would leave the egress without NAT at its next boot, and crash-loop its orchestrator.
- **P4-1:** A-48 admitted unpinned entries whatever their status. A removed box could come back in
  while its `hive_id` was re-added, and stay.

**The idea that simplified the most:** under D3 enforce can only cut three things. Those are SSH
through non-internal interfaces, undeclared listeners (cut on purpose) and special UDP/ICMP. So the
switch is gated by SSH logins plus preconditions, and packet accounting is report-only (D28). That
dissolves P4-6, P4-7, P4-8, A4-6, A4-7, T4-4, T4-16, D4-11 and P4-12.

**Also found, in today's code:** the remove cleanup script dies when it stops sy-orchestrator
(D4-2). Everything after it in the loop never runs, so removed spokes keep fluxbee-syncthing up and
enabled. Revision 7 fixes it under A-48.

**Disposition:** everything is in revision 7; spec §8, "Round 4 → revision 7", maps each finding.
D25 and D27 still await the operator; Acceptance found D26 consistent with what he approved.
**Next:** stage 2, then round 5 on stages 4–5, A-48 and A-49.

---

# Round 5 — revision 7: core stages 4–5, A-48, A-49 (2026-10-06)

**Verdict:**

| Lens | Verdict | Findings | Blockers |
|---|---|---|---|
| Development | pass with observations | 16 | 0 |
| Test | fail | 20 | 1 |
| Acceptance | fail | 15 | 1 |
| Production | fail | 14 | 1 |

Verifiers: 48 confirmed, 17 partial, 0 refuted.

**About the Test reviewer:** the safety classifier timed out while reviewing its run. Its transcript
was checked afterwards: 81 calls, all read-only (`grep`, `wc`, `ls`, `sed -n`, `Read`), no writes and
no remote commands.

**Blockers:**

- **A5-1, P5-1:** read literally, §3.5's orphan rule removes the motherbee's own Syncthing device. The
  stage-2 code never touched it (the local id is excluded); the wording now says "remote devices
  only", and a fixture holds the local device.
- **T5-1:** the stage-3 skip-upgrade test needs a 0.1.57 `dynamic`↔`dynamic` worker, which can only be
  built before stage 2 is deployed. Done on 2026-10-06: VM 104 joined as `worker2` on 0.1.57 and was
  stopped. Its first join failed because cloud-init's first-boot dist-upgrade restarted sshd mid-join
  (A-52); it was retried after cloud-init finished and a reboot.

**For the operator, before revision 9:** A5-2 finds that D28 (gate only on SSH logins; packet
accounting report-only) changes what the approved D21 and D22 meant, so it must be marked proposed
and approved explicitly. D25 and D27 are still open.

**Disposition:** A5-1, P5-1 and T5-1 are in revision 8. Every other finding is in revision 9 (spec §8,
"Round 5", maps each one), with the operator's D25–D28 of 2026-10-06; A5-3 became D29, proposed.
**Next:** round 6 on revision 9, before stage 4.

| Finding | Severity | Verifier | Claim | Disposition |
|---|---|---|---|---|
| D5-1 | major | confirmed | On an egress without nft, the background install cannot finish and can leave dpkg interrupted. The egress bootstrap aborts when nft is missing, the orchestrator exits … | rev 9 |
| D5-2 | major | confirmed | 'Closes the sessions that no longer match' cannot be built on today's wan_peers. It keeps one entry per hive_id. A second session for the same hive_id overwrites the … | rev 9 |
| D5-3 | minor | confirmed | The ss filter `dport = :<wan port>` matches any established connection to that remote port, not only the uplink. On a multi-NIC host, a node talking to some service on … | rev 9 |
| D5-4 | minor | confirmed | The hash depends on a template version constant bumped by hand. Re-apply happens only on drift, so a template change shipped without the bump is never applied: the hash … | rev 9 |
| D5-5 | minor | confirmed | Two legitimate setups would keep a host in observe and failing health forever. A host without sshd (no binary, no ssh.service or ssh.socket) is a permanent derivation … | rev 9 |
| D5-6 | minor | confirmed | Three gaps. (a) Nothing says the cursor advances only after every entry in the batch was classified, so a login read while the internal interface or `ip route get` was … | rev 9 |
| D5-7 | minor | confirmed | The firewall writes run only at bootstrap and on a Syncthing config change. Today they also land in an inactive ufw, which pre-stages the mesh ports for a customer who … | rev 9 |
| D5-8 | minor | confirmed | The transient-unit command as written starts the script at once. Today's cleanup starts with `sleep 1` so the REMOVE_HIVE_CLEANUP reply leaves before rt-gateway stops. … | rev 9 |
| D5-9 | minor | confirmed | The dated note in the egress spec still says the input chain 'is deleted'. That is revision 6's design, which round 4 found to be a blocker (D4-1). §3.7 claims that note … | fixed now: the dated note in edge-egress-nat-spec |
| D5-10 | minor | confirmed | Three gaps. (a) 'Existed when A-48 first booted' needs a durable one-shot marker. If 'no view file' serves as that marker, deleting the view (as the A-48 gate does) and … | rev 9 |
| D5-11 | minor | confirmed | The TLS push is best-effort in all three join flows, on the stated grounds that the WAN degrades to plaintext. That no longer holds: spokes are rendered with `mtls … | rev 9 |
| D5-12 | minor | confirmed | Step 2 deletes the registry entry and then rewrites the view. If that write fails, remove_hive fails, but a retry answers NOT_FOUND, and the router keeps admitting the … | rev 9 |
| D5-13 | minor | partial | Not every registry write is under the per-hive lock. The join's first and last update_join_state calls run outside it, and write_file_atomic creates the directory. A … | rev 9 |
| D5-14 | minor | confirmed | A host-key mismatch is treated as an auth failure, because any ssh exit 255 counts as one. For hives joined in the default revoke mode (all of PROD's spokes), the … | rev 9 |
| D5-15 | minor | confirmed | Swapping InRelease after Packages cannot guarantee that no client sees a mismatched pair. A client that fetches the old InRelease before the swap and Packages after it … | rev 9 |
| D5-16 | minor | confirmed | Rotation contradicts the postinst rules. After the first switch there is nothing left to switch, so 'touches the keyring only together with a verified switch' forbids … | rev 9 |
| T5-1 | blocker | confirmed | The stage-3 skip-upgrade precondition cannot be produced once stage 2 is on 8.x. The gate says: 'before the stage-3 deploy, join the throwaway VM as a worker on the … | rev 8, §6 (done before the stage-2 deploy) |
| T5-2 | major | partial | The pure mode function is not specified completely enough for a table-driven test: 1. No outputs. The new state, the action (apply observe, apply enforce, remove the … | rev 9 |
| T5-3 | major | confirmed | The render hash covers the declaration plus a rules-template version constant bumped by hand, not the rendered rules. Suppose a template change is merged without a bump … | rev 9 |
| T5-4 | major | partial | The stage-4 unit list has fixtures for the sshd ports and the SSH logins, but none for the derivations and classifiers whose errors 8.x cannot show: - (a) … | rev 9 |
| T5-5 | major | confirmed | The prerm teardown has no test. dpkg runs the old package's prerm at the next upgrade, so a teardown placed where `upgrade` reaches it cannot be fixed by the next … | rev 9 |
| T5-6 | major | confirmed | Under D28, the only protection for DHCP, DHCPv6, ICMPv6 ND/RA and echo in enforce is that their explicit allows are correct, and the gate checks only their order. 8.x is … | rev 9 |
| T5-7 | major | confirmed | The stage-5 probes cannot tell enforce from observe on worker1, the first host released, or on the ingress' internal side. '22 open' is true in both modes, and the … | rev 9 |
| T5-8 | major | confirmed | The A-48 gate has only infra steps. Most of the new logic has no unit or CI coverage, and one VM cannot reach it: - the admission predicate: not in view; in view without … | rev 9 |
| T5-9 | major | confirmed | The No-view step leaves the 8.x motherbee (PROD) admitting any CA-valid peer, and nothing restores the view: - The view is rebuilt only at orchestrator boot or on … | rev 9 |
| T5-10 | minor | confirmed | The re-add has no working credentials as written: - `key_only_persist` turns password login off in every case and removes the motherbee's key; - `add_hive` never reads … | rev 9 |
| T5-11 | minor | confirmed | With one VM, 'the old box is never admitted ... the new box is admitted by its pin' cannot be checked: - the old and new box share the VM, the address and the `hive_id` … | rev 9 |
| T5-12 | minor | partial | A-48 is feasible with one VM and at most 3 snapshots, but two things are wrong. - (a) Base state undefined. The throwaway's state before each join is not defined, and … | rev 9 |
| T5-13 | minor | confirmed | 'Within 10 s `ss` shows no established 9000 or 9100' is a single sample with no defined start. The box keeps redialing: the router every 5 s after each successful TCP … | rev 9 |
| T5-14 | minor | confirmed | - (a) 'After confirming `admin.public_edge_node`' names no check and no fallback. If neither the setting nor the env override is set, a second connected ingress makes … | rev 9 |
| T5-15 | minor | partial | The expected result is only a log line, and the step proves detection only for `key_only_persist`. In the default `revoke` mode, `ssh_error_is_auth_failure` matches any … | rev 9 |
| T5-16 | minor | partial | Starting the clock at `uptime -s` includes the motherbee's userspace boot. FINDINGS B-14 measured it at 108 s, with no known cause and a 53–108 s spread across hosts. … | rev 9 |
| T5-17 | minor | confirmed | Health is to fail on a derivation failure, yet the spec expects one after every uplink drop: a motherbee upgrade or reboot, or a spoke restart. The router redials with a … | rev 9 |
| T5-18 | minor | confirmed | Several stage-4 infra items have no defined result or lack a cheap control: - they do not say where drift and third-party detections are read; - the new … | rev 9 |
| T5-19 | minor | confirmed | - (a) The public check from the operator's Mac cannot reuse `posture-probe.sh`: macOS has no `timeout`, so every port prints `error:`. 'Everything else' also names no … | rev 9 |
| T5-20 | minor | confirmed | - (a) The first verified `apt-get update` is the one after the postinst switch. The deploy's own update still runs under `[trusted=yes]`, and the gate does not say so. - … | rev 9 |
| A5-1 | blocker | confirmed | As worded, the orphan rule deletes the motherbee's own Syncthing device. A hive's device is defined by its registry entry, and 'a device with no registry entry' is … | rev 8, §3.5 |
| A5-2 | major | confirmed | D28 sits in the table of decisions agreed with the operator and is not marked proposed. But it changes what approved D21 ('clean window') and D22 ('ran clean in … | rev 9 |
| A5-3 | major | partial | Under D28, going back to observe on every render change protects nothing, and it periodically undoes D6 and D16. The window can only detect SSH logins through … | rev 9 |
| A5-4 | major | confirmed | The evidence classifies a login by the route back to its source (`ip route get`). Enforce filters by the interface the packet arrived on (`iifname`). So the claim that … | rev 9 |
| A5-5 | major | confirmed | The dated note in the egress spec, which revision 7 points to as recording the supersession, still says the egress input chain 'is deleted'. That is the instruction … | fixed now: the dated note in edge-egress-nat-spec |
| A5-6 | major | confirmed | The spec never says how 'entries that existed when A-48 first booted' is determined. If it is inferred from a missing view file or from an entry without a pin, the P4-1 … | rev 9 |
| A5-7 | minor | confirmed | D28 says undeclared listeners are 'cut on purpose, and reported', but the listener check reports TCP only. Undeclared UDP listeners are cut and show up only in the … | rev 9 |
| A5-8 | minor | confirmed | D25 writes into an active ufw or firewalld, which contradicts approved D2 ('Fluxbee stops writing ufw/firewalld rules ... Third-party firewalls are reported, never … | rev 9 |
| A5-9 | minor | partial | Any foreign input base chain with rules counts as a third-party firewall. Tools that only ban addresses, such as fail2ban's standard actions, would hold a host in … | rev 9 |
| A5-10 | minor | confirmed | The documented repurpose command leaves /etc/fluxbee/posture.disabled in place. If the box is later joined again, its posture starts off instead of in observe, which … | rev 9 |
| A5-11 | minor | partial | 'Observed time starts at 0 with the stage-5 release' has no mechanism. Observed time grows regardless of the release switch, and the hash changes only if the template … | rev 9 |
| A5-12 | minor | partial | Under D28 packet accounting gates nothing, yet the report still parses the kernel log into top tuples. That is a log parser and an aggregation that no gate uses. | rev 9 |
| A5-13 | minor | confirmed | The rotation path cannot work under the postinst rules. The postinst touches the keyring only together with a verified switch and never replaces a key. After the first … | rev 9 |
| A5-14 | minor | confirmed | A motherbee with a second network, office or public, accepts SSH and the mesh ports there, so D6/D16 do not hold on it. Only a public address gets a warning, and §0 does … | rev 9 |
| A5-15 | minor | confirmed | Some status lines contradict revision 7. | rev 9 |
| P5-1 | blocker | partial | Read literally, the orphan rule removes the motherbee's own Syncthing device. Every config.xml lists its local device, and the motherbee has no registry entry, so its … | rev 8, §3.5 |
| P5-2 | major | confirmed | The SSH-login evidence can be forged, so an attacker can hold a host in observe. (a) Remotely, without credentials: sshd logs the username verbatim on failed attempts … | rev 9 |
| P5-3 | major | confirmed | Enforce cuts UDP listeners and other IP protocols, and no check reports them beforehand. D28 says undeclared listeners are 'cut on purpose, and reported', and §3.2 says … | rev 9 |
| P5-4 | major | partial | The one-time, stored legacy mark cuts legitimate hives, and the spec never says whether the pin or the mark wins. (1) Downgrade, then upgrade again: a pre-A-48 build … | rev 9 |
| P5-5 | major | confirmed | remove_hive is a one-shot sequence, so a failure or crash after step 2 leaves the hive's identity key behind, and nothing finishes the job. Once the entry is gone, a … | rev 9 |
| P5-6 | major | confirmed | A-49 can break deploys in four ways the spec does not exclude. (1) Failure handling: the postinst runs under set -e, so a failing switch step aborts it before `systemctl … | rev 9 |
| P5-7 | minor | partial | Observe turns conntrack into a resource the internet can fill on the ingress. The edge port is untracked for exactly this reason, but in observe every other connection … | rev 9 |
| P5-8 | minor | confirmed | The evidence has silent blind spots, and in each one enforce cuts an SSH path that was never reported. (1) With sshd LogLevel QUIET, FATAL or ERROR, or journald storing … | rev 9 |
| P5-9 | minor | confirmed | The derivation takes any established TCP connection whose destination port is the WAN port, from any process to any address, and its result decides which interface keeps … | rev 9 |
| P5-10 | minor | confirmed | Three edges of the view admit more than intended. (1) The view is written only at boot and on add_hive/remove_hive. If the file is lost, the router fails open at its … | rev 9 |
| P5-11 | minor | partial | Today a host-key failure looks like an auth failure. Reconcile treats an ssh exit 255 on a revoke-mode hive as 'no SSH channel' and logs it at debug. So on the 3 PROD … | rev 9 |
| P5-12 | minor | partial | The 3 PROD spokes will stay unpinned indefinitely, because the only way out, a re-add, needs console work. They were joined with harden_ssh:true: password login off, the … | rev 9 |
| P5-13 | minor | partial | The third-party rule ('any non-Fluxbee base chain on the input hook with … rules') also catches tools that only add drops or accepts: fail2ban, Tailscale, LXD/Incus … | rev 9 |
| P5-14 | minor | partial | Section 0 should still name eight residuals. (a) SSH host keys are trust on first use, not closed. The first contact with every new spoke, and with each existing spoke … | rev 9 |

**Also found while building stage 2** (code reviews, not the panel): the `config.xml` writer
followed symlinks planted in the Syncthing user's directory; the folder rule followed hive.yaml's
enable flags; and on PROD the motherbee shared `blob/active` with the egress (A-50). All fixed in
stage 2.

# Round 6 — revision 9: core stages 4–5, A-48, A-49 (2026-10-06)

**Verdict:**

| Lens | Verdict | Findings | Blockers |
|---|---|---|---|
| Development | pass with observations | 18 | 0 |
| Test | fail | 18 | 0 |
| Acceptance | pass with observations | 13 | 0 |
| Production | pass with observations | 21 | 0 |

Verifiers: 61 confirmed, 8 partial, 1 refuted.

**No blocker.** All four lenses found that revision 9 had pasted its stage-4 and stage-5 gates into
the History table, so §6 still carried revision 7's gates. Most majors were about the mode decision
list (missing and unreachable rows, logins lost under "first match wins"), the SSH-login evidence
(a journald suppression could hold a host in observe), D29 (no guard for a host already enforcing),
A-48's pins and revocation, and A-49's publish order.

**The operator, after round 6 (2026-10-06):**

- D30: no observe phase (*"eso de esperar 24hrs es un total parche que no sirve"*) and no check on
  the host before applying (*"demasiado frágil"*); CI proves the rules.
- D31: one SSH door on the egress, for now.
- D32: SSH only inside `add_hive`; the boot-time TLS reconcile and `key_only_persist` go (A-54).
- O9 opened: how software enters an installation, keeping apt.

**Disposition:** revision 10 (spec §8, "Round 6", maps each finding). The findings that were only
about the mode machinery or SSH outside `add_hive` are withdrawn with it. **Next:** a focused
re-check of revision 10 (§3.1–§3.3, §3.6–§3.9, §6), then stage 4.

| Finding | Severity | Verifier | Claim | Disposition |
|---|---|---|---|---|
| D6-1 | major | confirmed | Revision 9's stage-4 and stage-5 gates were written into the History table instead of §6. §6 still carries the revision-7/8 gates, which contradict §3, and the History … | rev 10 |
| D6-2 | major | confirmed | Read as 'first match wins', the list has transitions with no row and rows that can never be reached, so it cannot become the one-row-per-rule table the gate asks for. | withdrawn with the mode machinery (D30) |
| D6-3 | major | confirmed | A login read in a check where an earlier row matches is used up without resetting the window. A host can then enforce after an SSH login through a non-internal interfa … | withdrawn with the mode machinery (D30) |
| D6-4 | major | confirmed | Rewriting the view while the registry entry still exists works against the watchdog's restore and the boot rebuild. A crash or failure between the two steps admits the … | rev 10 |
| D6-5 | major | confirmed | The pin lifecycle has three holes: first-use pins have no trigger; a failed pin write silently turns an A-48 join into a legacy entry; and a box holding an older leaf … | rev 10 |
| D6-6 | minor | partial | D29 cannot be decided from the persisted state, its test misses SSH changes that come from the template, and its rationale relies on a precondition that is not checked … | withdrawn with the mode machinery (D30) |
| D6-7 | minor | confirmed | Switching the live stage-1 check to `ss -H -tulnp` blinds it. One alert kind for undeclared listeners hides all but the first for an hour. | rev 10 |
| D6-8 | minor | confirmed | Four details of the evidence rules would lose real logins or hold hosts in observe for nothing. | withdrawn with the mode machinery (D30) |
| D6-9 | minor | refuted | Untracking the sshd ports does not stop the internet from filling conntrack while a host observes. In observe the policy accepts every packet to any closed port on a n … | refuted by its verifier; no change |
| D6-10 | minor | confirmed | The posture check is put in watchdog_tick, the single-flight self-heal tick, with subprocesses that have no time bound. One hung command would stop rt-gateway restarts … | rev 10 |
| D6-11 | minor | confirmed | Three details decide whether pinned admission and view reloads work as described. | rev 10 |
| D6-12 | minor | confirmed | `<hives>/known_hosts.d/` puts a directory that is not a hive in the registry root. It would show up as a phantom hive. | rev 10 |
| D6-13 | minor | confirmed | 'A hive that has a pin and whose certificate goes missing is reported' can work only where the motherbee has SSH. Two gate expectations cannot be met as written. | withdrawn with SSH outside add_hive (D32) |
| D6-14 | minor | confirmed | The refusal needs no sender check, because nothing legitimate ever sends SYSTEM_CORE_ROLLBACK to the motherbee. | rev 10 |
| D6-15 | minor | confirmed | Once the cleanup runs outside sy-orchestrator's cgroup, the orchestrator stays alive until the loop reaches it. Meanwhile it undoes the loop: its 5 s watchdog restarts … | rev 10 |
| D6-16 | minor | confirmed | 'Publish without the key fails and leaves the previous InRelease in place' leaves the repo broken, because by then Packages has already been swapped. | rev 10 |
| D6-17 | minor | confirmed | The helper's way of finding the Fluxbee source, and what it does when a new key is added, are undefined. | rev 10 |
| D6-18 | minor | confirmed | For a third-party firewall, §3.6's health verdict contradicts the operator's D25, and the classifier rule names a chain policy nftables does not have. | rev 10 |
| T6-1 | major | confirmed | Revision 9's stage-4 and stage-5 gates went into the History table instead of §6. The rows for revisions 4 and 5 now hold the gates: three cells in a two-column table, … | rev 10 |
| T6-2 | major | confirmed | The decision list is still not complete enough for a table-driven test: (a) No rule takes a host out of `off` once the break-glass file is removed, or out of `unavaila … | withdrawn with the mode machinery (D30) |
| T6-3 | major | confirmed | Posture alerts from spokes cannot be read where the spec and the gate read them. The motherbee's own orchestrator serves `GET /hives/<h>/drift-alerts`, reading its loc … | rev 10 |
| T6-4 | major | partial | From stage 5, health fails a host that is not held and not in enforce 48 hours after the later of its last render change and switch_enabled_since. The stage-5 gate hol … | withdrawn with the mode machinery (D30) |
| T6-5 | major | confirmed | Since D28, the netns job is the only check of what the render does on a real kernel (§3.2:500-502). Revision 9 does not say what it checks per role and per interface. … | rev 10 |
| T6-6 | major | confirmed | The stage-4 infra checks only what the orchestrator reports about itself: the posture in /versions, the mode and the reasons. It never checks, on each real host, the i … | rev 10 |
| T6-7 | major | confirmed | The A-48 steps do not say which join (hive_id, ssh_access mode) each runs on, or where the 'clean' rollbacks fall. As written, several steps cannot work or cannot fail … | rev 10 |
| T6-8 | major | confirmed | The A-48 unit list misses the new logic most likely to regress, and the infra steps cannot reach most of it: (a) The per-hive session map: admitting B closes A; A exit … | rev 10 |
| T6-9 | major | confirmed | The publish-without-key test checks the wrong property and runs where it can break the repo. Today the publish renames Packages and Packages.gz before Release. If sign … | rev 10 |
| T6-10 | minor | confirmed | The SSH-login test cannot run in the order written, and it lacks the attacker-side control: (a) The negative control (a login through 10.10.10.40) comes before the key … | withdrawn with the mode machinery (D30) |
| T6-11 | minor | confirmed | The rejoin times cannot be measured live as written. After a motherbee reboot, the guest agent comes back after about 2.5 minutes. That is around when the spokes, whic … | rev 10 |
| T6-12 | minor | confirmed | §0 says enforce keeps established connections, including an SSH session opened through a path it now closes. Revision 9 untracks the sshd ports on non-internal interfa … | rev 10 |
| T6-13 | minor | confirmed | Two decisions disagree with the tests: - D25 says a third-party firewall makes health fail; §3.6 and the stage-4 injection say health only warns. - D29 is only propose … | rev 10 (D25 kept as decided; D29 withdrawn by D30) |
| T6-14 | minor | confirmed | One stage-4 check cannot fail because of the posture, and one has no defined check: - In observe the policy is accept, so `curl -sk https://10.10.10.30/` succeeds what … | rev 10 |
| T6-15 | minor | confirmed | The prerm test is not specified well enough to test what it claims: - No postrm exists, and build-deb.sh installs only preinst, postinst and prerm, so a script-level t … | rev 10 |
| T6-16 | minor | confirmed | The guard bans two literal strings but does not prove what §3.8 requires: accept-new and a per-address known_hosts file at every call site. All of these pass it: - `-o … | rev 10 |
| T6-17 | minor | confirmed | The tamper test has no positive control and one wrong expectation, and nothing tests the first install or rotation: (a) The gate never shows that the untouched copy up … | rev 10 |
| T6-18 | minor | confirmed | Rolling VM 104 back to 'clean' and joining right away can repeat A-52. The apt timers in the snapshot are persistent and fire after boot, and unattended-upgrades resta … | rev 10 |
| A6-1 | major | confirmed | Revision 9's stage-4 and stage-5 gates were written into the History table instead of §6: - They sit as a third cell of rows 4 and 5 of a two-column table, overwriting … | rev 10 |
| A6-2 | major | confirmed | D29 is consistent with D28. Observe can only produce SSH-login evidence, so a change that does not move SSH gains nothing from a new window. But as written D29 has thr … | withdrawn with the mode machinery (D30) |
| A6-3 | major | confirmed | "First match wins" stops the evaluation at rules that should not stop it, so the stage-4 table test would encode wrong behaviour: - D29's in-place branch skips the hol … | withdrawn with the mode machinery (D30) |
| A6-4 | major | confirmed | A journald suppression of ssh.service counts as unread evidence. That lets anyone who can open TCP connections to sshd hold a host in observe indefinitely, which break … | withdrawn with the mode machinery (D30) |
| A6-5 | minor | confirmed | D25, recorded as the operator's decision, says that an active ufw or firewalld makes health fail. §3.6 makes a third-party firewall only a warning. | rev 10 |
| A6-6 | minor | confirmed | Over-engineered under D25. Besides ufw and firewalld, §3.6 classifies foreign nft base chains by their policy, with its own fixtures and a worker1 injection: - drop or … | rev 10 |
| A6-7 | minor | confirmed | The stage-5 health rule fails hosts that the planned staggered release has just freed: - The 48 hours run from switch_enabled_since. - But a hold resets the window on … | withdrawn with the mode machinery (D30) |
| A6-8 | minor | partial | Two A-48 statements have no trigger behind them: - Legacy hives are said to be pinned by their first session after A-48, but nothing rebuilds the view when the router … | rev 10 |
| A6-9 | minor | confirmed | The A-48 gate joins the throwaway in the default revoke mode, then runs two steps that need key_only_persist: - the missing-certificate report needs an SSH channel to … | withdrawn with SSH outside add_hive (D32) |
| A6-10 | minor | confirmed | A-49's helper keeps three parts that are unneeded or undefined: - The first install has two paths, and one of them is a step for the user. - "The Fluxbee URI" is undef … | rev 10 |
| A6-11 | minor | partial | §0 contradicts itself on SYSTEM_CORE_ROLLBACK. It also omits a residual that stage 4 creates: while the ingress observes, the internet can fill its conntrack table. | rev 10 (§0); its conntrack part refuted |
| A6-12 | minor | confirmed | §3.7's "the orchestrator installs nothing" rests on a fresh template clone having nft. Nothing in the repo records that check, and the HANDBOOK still tells operators t … | rev 10 |
| A6-13 | minor | confirmed | "Set elements" is left over from the removed dynamic sets. Revision 9 renders no named set. A normalizer that strips set elements would also strip the anonymous sets i … | rev 10 |
| P6-1 | major | confirmed | Revision 9's stage-4 and stage-5 gates landed in the History table and overwrote the history rows for revisions 4 and 5. §6 still carries revision 7's gates, so the ga … | rev 10 |
| P6-2 | major | confirmed | 'When the TLS reconcile re-issues a leaf for an unpinned hive, it writes the pin too' turns the named first-contact residual into a takeover, and it can also cut a leg … | rev 10 |
| P6-3 | major | partial | A-48 cuts a live pre-A-48 hive whose registry entry says `failed` or `interrupted`. After the upgrade it can be recovered only over SSH, which means console work for r … | rev 10 |
| P6-4 | major | partial | A registry read error becomes a revocation. Read literally, a failed listing of the hives directory at boot yields an empty view, so every spoke is refused as not_in_v … | rev 10 |
| P6-5 | major | confirmed | An internet scanner can hold the ingress in observe. The same works for an office host against the egress, and for any flat-L2 host against the motherbee. The attacker … | withdrawn with the mode machinery (D30) |
| P6-6 | major | confirmed | No check stops enforce from cutting a core port when the declaration is wrong. The expected listeners come from the same pure function as the render. A port missing fr … | withdrawn with the mode machinery (D30) |
| P6-7 | minor | confirmed | Four gaps in pinning legacy hives on first use. (a) Two different CA-valid leaves of the same legacy hive_id can connect before the pin is written, for example a box r … | rev 10 |
| P6-8 | minor | confirmed | 'Rewrite the view, only then delete the entry' is redundant and opens a window. Revision 9 also made revocation a reconcile, and a remove on a missing entry runs it, w … | rev 10 |
| P6-9 | minor | confirmed | `<hives>/known_hosts.d/<address>` puts a non-hive directory in the registry root. `list_hives` returns every directory there, so GET /hives gains a hive named `known_h … | rev 10 |
| P6-10 | minor | confirmed | Any successful `ip route get <uplink>` answer is adopted and persisted. On the ingress and the egress, the default route goes through the external NIC. When the intern … | rev 10 |
| P6-11 | minor | confirmed | Three gaps, for the table-driven test and for safety across transitions. (a) The 5 → 4 rollback rule sits outside the numbered list. Its place relative to rule 4 (a de … | withdrawn with the mode machinery (D30) |
| P6-12 | minor | confirmed | (a) 'The source comes from its fixed tail, `from <ip> port <n> ssh2`' fails on publickey and certificate logins, which continue after 'ssh2:' with the key type and fin … | withdrawn with the mode machinery (D30) |
| P6-13 | minor | confirmed | 'sshd-owned lines in ss' also match per-session listeners. Ubuntu's sshd_config enables X11 forwarding by default, and the session's sshd then owns a listener on 127.0 … | rev 10 (SSH fixed at tcp 22) |
| P6-14 | minor | confirmed | §0 says enforce keeps established connections, 'including an SSH session opened through a path it now closes'. But SSH on non-internal interfaces is untracked in both … | rev 10 |
| P6-15 | minor | partial | 'Admitting a session for a hive closes the previous one' makes a spoke with two wan.uplinks entries flap. The router dials every uplink, and after A-48 both can reach … | rev 10 |
| P6-16 | minor | confirmed | Loading fluxbee_host turns conntrack on for the motherbee, which then tracks its mesh ports and SSH on every interface, in both modes. §0 says only that 'a host on the … | rev 10 |
| P6-17 | minor | confirmed | 'Publish without the key fails and leaves the previous InRelease in place' can break every deploy. The publish swaps Packages first and Release last. If signing fails … | rev 10 |
| P6-18 | minor | confirmed | (a) The append-only rule undoes key removal. 'Every run adds a shipped key that is missing', and the rotation steps never say to stop shipping the old key before the r … | rev 10 |
| P6-19 | minor | confirmed | D25 says a third-party firewall makes health fail. §3.6 and the revision-9 gate say it only warns. The stale §6 gate says it fails. | rev 10 |
| P6-20 | minor | confirmed | The 'set elements' exclusions are left over from the removed dynamic sets, and they are now harmful. An implementation may well put the sshd or edge ports in a named s … | rev 10 |
| P6-21 | minor | partial | Section 0 must still name or correct the following. (1) The sshd-flood suppression (P6-5), either as a hold or as a missed login, whichever rule is kept. (2) SSH sessi … | rev 10 |
