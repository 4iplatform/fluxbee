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
