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
