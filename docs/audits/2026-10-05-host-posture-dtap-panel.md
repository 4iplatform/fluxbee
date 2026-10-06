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
