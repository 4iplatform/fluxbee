# FINDINGS — findings from the integrated deployment in PROD alpha

> **What this is.** The cumulative record of what the integrated test in production brings to
> light. The daily logbook (`YYYY-MM-DD.md`) tells *the journey*; this document gathers *the
> findings* so that a well-founded **code-change plan** can come out of them later.
>
> **The rule that governs it** (`METHOD.md` §3, rules 0b/0c): during the deployment **no code is
> touched and nothing is patched to make it work**. A failure is **data**, not an obstacle.
> Everything that shows up here is discussed with the operator **before** it becomes a change.

**States:** 🔴 confirmed in code, open · 🟡 partial, parked or observed (the heading says which) ·
🔵 to evaluate · ✅ resolved (the heading names the version and how it was validated) · 🟢 lab note or
no change required

---

## The board: open items by track

> **Since 2026-11-04 (the operator):** the work is organized by track: **WF**, **io.web**,
> **Archi**, **IO** (the family; each IO node is a track of its own inside it) and **General**
> (the system's bugs, and what fits no track). Section B is the infrastructure and the lab.
> Each open item of section A carries its track in brackets after its id (`A-136 [IO/io.slack]`),
> and a new finding gets one when it is written. An item leaves the board when it closes; the
> sections below keep their order.
>
> **Since 2026-11-07 (the operator):** a sprint runs several tracks at once, in releases of
> changes that touch different binaries, each with its own gate, simplest first
> ([METHOD](METHOD.md) §9).

### This sprint (2026-11-07), by releases, simplest first

🧠 marks what closes cognition (A-137).

1. **First release, 0.1.106** ✅ live and checked on 8.x (2026-11-07); its gate found A-167:
   1. PENDING-BUGS' six August rows: checked, then closed or brought here (docs only). ✅
      2026-11-07: PB-1 and PB-5 were closed already, U-12 needed no change, and PB-3, PB-6 and U-6
      came here as A-164, A-165 and A-166.
   2. A-80 [General/identity]: a restore looks the email up in the person's own tenant
      (SY.identity). ✅ by its test.
   3. 🧠 A-163 [WF]: the episode gate says which rule turned a proposal down, with its numbers
      (SY.cognition). ✅ on 8.x.
   4. A-103 [General/agent tools]: the tool cards stop restating the code's values by hand
      (ai.generic). ✅ on 8.x.
   5. A-120 [Archi]: Archi takes a page down, through its confirmation (SY.admin). ✅ on 8.x.
2. **Second release, 0.1.107** ✅ live and checked on 8.x (2026-11-07):
   1. 🧠 Cognition's numbers, on A-163's output: whether a quarrel between two people is news, and
      whether the gate's thresholds become configuration. ✅ The operator's call (2026-11-07): the
      gate stays as it is, a quarrel it passes is news, and the thresholds wait for an automatic
      mechanism, not configuration (cognition-hands-v1 §12.6). A-167's fix agreed: the narrator's
      rule.
   2. 🧠 A-167 [WF]: an episode names its people, and says "the agent" only of the AI agent (the
      narrator's rule, SY.cognition), with A-160 [General/ops]: the factory reset clears the
      workers' cognition too (SY.admin and SY.cognition). One branch: the same binary. ✅ on 8.x.
   3. A-150 [WF]: first, whether the provider can enforce a response contract inside the tool
      loop (ai.generic; alone if the fix reaches the shared AI SDK). Both providers can; the fix
      reaches the shared AI SDK, so it went alone: ✅ 0.1.108, checked on 8.x (2026-11-07).
3. **Third release, 0.1.109** (SY.admin, the orchestrator and the package's seed: one branch,
   since every item touches SY.admin) ✅ live and checked on 8.x (2026-11-07); its gate found
   A-172:
   1. A-122 [General/upgrade]: `reset_base_runtime`, reachable by Archi behind the operator's
      CONFIRM, returns a base runtime to its package's version, which the seed records as
      `package_version` and `list_runtimes` shows.
   2. A-165 [General/add_hive]: a join that may log in over a password chooses `harden_ssh`, or
      is refused (the operator left the call to me, 2026-11-07).
   3. A-164 [General/vault]: the vault help's types come from the SDK's list, with the three the
      product's nodes use and their values.
   4. A-166 [General/add_hive]: ✅ verified in the code, nothing to change.
   5. From the sweep of pending items (lab/logbook/2026-11-07.md): A-169, an identity read that
      names a spoke was refused, and A-170's three small ones.
   6. 🧠 What stays open of A-137 ([cognition-hands-v1](../../docs/cognition-hands-v1.md) §12.6),
      talked first, one at a time:
      1. An agent taking a conversation back from another sees none of its turns. ✅ The
         operator's call (2026-11-07): it stays so, so roles do not mix; what matters crosses
         only through the WF, or the person tells it. Nothing to build; Archi's handbook states
         the rule.
      2. A conversation among people that later reaches an agent. The operator's first call
         (2026-11-07) was not now. Asked whether that took Juan's part back too, the operator
         turned it around the same day: the lines reach the agent, its role decides whether to
         use them, and the first agent on a thread takes what people said before it (A-173).
         Built, held back by its review, rebuilt in the sixth release.
      3. A message and its copy split in two batches on a new thread.

      Then A-137's case closes: gate0206 run once from zero, and taken down.
4. **Fourth release, 0.1.110** (each item on a binary of its own) ✅ live and checked on 8.x
   (2026-11-07):
   1. A-172 [Archi], with its family: Archi removes a runtime version and a hive, deletes, restores
      and purges a tenant and lists its nodes, rolls a hive's core back, reads the global
      versions, deployments and drift alerts; `set_node_config` names its binding fields; a CI
      check and a test hold the executor's fields to what each route stores (SY.admin).
   2. A-176, three texts (SY.timer, SY.wf-rules, SY.architect).
   3. The egress's internet alert and A-174, a worker reads its IPv6 block back (SY.orchestrator);
      A-175 found: an egress's alerts never reach the motherbee.
   4. The docs and FINDINGS texts that drifted, from the sweep (docs only, already on daily).
   5. A-173 held back to a release of its own.
5. **Fifth release, 0.1.111, the router alone** ✅ live and checked on 8.x (2026-11-08):
   1. A-177 [General/router]: a second rt-gateway started by hand refuses and exits, and leaves the
      live router's sockets, shared memory and stream alone.
   2. A-178 [General/router]: `wan.mtls` fails closed: absent or unknown is `required`.
   3. A-179 [General/CI]: CI compiles the system policy's Rego and compares the baked wasm (no
      binary).
6. **Sixth release, 0.1.112** ✅ live and checked on 8.x (2026-11-08); its gate found A-185:
   1. 🧠 A-173 [WF]: rebuilt, each take recorded under the held state's lock (SY.cognition).
   2. A-180 [General/orchestrator]: a second sy-orchestrator refuses and exits; and A-175's
      interim, a hive's drift alerts say they hold only what the motherbee recorded
      (SY.orchestrator, and Archi's line on it).
7. **Then each SDK alone, from the sweep:**
   1. 0.1.113: A-182, the Rust SDK's handshake bounded ✅ live and checked on 8.x (2026-11-08).
   2. 0.1.114: A-183, the Go SDK's one HELLO and its deadline ✅ live and checked on 8.x
      (2026-11-08).
   3. 0.1.115: A-184, the AI SDK's provider errors: typed, OpenAI's retries, cognition's count,
      and the person's one fixed English text ✅ live and checked on 8.x (2026-11-08).
8. **Design first, one at a time, the largest:**
   1. A-181, a hive's shared config travels in the update
      ([hive-config-update-v1](../../docs/hive-config-update-v1.md)), by stages: stage 1 ✅
      0.1.116; stage 2 ✅ 0.1.117, the release's settings in the code; stage 3a ✅ 0.1.120 and
      3b ✅ 0.1.121, the installation file in use; 3c ✅ 0.1.122, the operator's action; stage 4
      ✅ 0.1.123, SY.admin's installation update with its automatic step. Next, stage 5: the blob
      folders' members (A-181's first half) and A-188.
   2. Archi's chapter: the Cloud token (A-124) and the chat that answers at its turn's end (A-142).
   3. The IO standard (A-149), which unblocks A-91, io.slack's revamp (A-136, A-148), A-114 and
      io.blob's tenant (A-106).
   4. io.web's stage 2, on the operator's definition of its security.
   5. A-159: deleting an ILK that other data names, the episodes now among them; a chapter of
      its own.
   6. A-96: how the SY nodes' tokens are measured per message.
9. **Found by 0.1.117's gate:** A-190 [General/join], a fresh worker could not join ✅ 0.1.118,
   checked on 8.x with a throwaway worker, timed.
10. **Found by 0.1.118's timed gate:** A-194 [General/orchestrator], the Syncthing watchdog's
    restart during a finalize ✅ 0.1.119, checked with a throwaway worker; A-193 and A-195, to
    evaluate; A-196, latent.
11. **Found by 0.1.122's review round:** A-197 [Archi], Archi's catalog of SY.admin's actions did
    not fit the mesh's frame ✅ 0.1.122, checked on 8.x; A-198 [General/admin], other SY.admin mesh
    replies that can outgrow it ✅ 0.1.123, checked on 8.x.
12. **Found by 0.1.123's review round:** A-199 [General/orchestrator], the motherbee orchestrator's
    admin worker runs one request at a time, to evaluate. **Found by A-198's build:** A-200
    [General/vault], the vault's boot re-announcement covers only its newest 200 secrets (latent);
    A-201 [Archi], Archi could not read `list_recent_commands`, which its handbook tells it to read
    first ✅ 0.1.123, checked on 8.x. **Found by A-198's review round:** A-202 [General/router], a message just
    under the frame passes the SDK and the router drops it ✅ 0.1.124, checked on 8.x.
    **Found by 0.1.123's deploy gate:** A-203 [General/router], a WAN session that ends on a read
    error leaves its hive in the peer table, and frames to that hive are lost without UNREACHABLE
    ✅ 0.1.124, checked on 8.x. **Found by 0.1.123's gate:** A-204
    [General/admin], the installation update's record gives every `set_installation_ai` hive the
    slowest one's time; A-205 [General/admin], a background read taken before a hive applied a
    config change can mark it pending after it did ✅ both 0.1.124, checked on 8.x.
13. **Found by the sweep's re-check (2026-11-09):** A-206 [General/SY.timer], a cron spec that
    starts with `TZ=` and has no space crashes SY.timer ✅ 0.1.124, checked on 8.x.
    **Found by A-203's builder:** A-207 [General/router], a WAN peer that vanishes without
    closing keeps its session for up to about 15 min ✅ 0.1.124, checked on 8.x; A-208 and A-209,
    low.
14. **The sweep's calls, the operator's 2026-11-10:** A-210 to A-219 (lab/logbook/2026-11-10.md),
    in releases by binary after the router's; A-218 alone (the SDKs); A-219 with its shape written
    first. **Found by A-206's builder:** A-220 [General/SY.timer], low. **Found by A-207's
    builder:** A-221 [General/router], low; A-222 [General/router], latent. **Found by A-216's
    builder:** A-223 [IO/io.api], low. **Found by A-219's design:** A-224 [General/orchestrator]
    and A-225 [General/egress], low.
    **Found by A-215's and A-217's builder:** A-226 [IO/io.slack] and A-227 [WF], low.
    **Found by A-202's builder:** A-228 [General/Go SDK], low.
15. **The router's release, 0.1.124** ✅ live and checked on 8.x (2026-11-10): A-203, A-207 and
    A-202 (the router, with the links between routers and to nodes), with A-204 and A-205
    (SY.admin, the operator's call to ride along), A-206 (SY.timer), A-211 (packaging) and A-181's
    leftover texts. **Found by its gate:** A-229 [General/admin], low. **Found by 0.1.125's
    builders:** A-230 [Archi], A-231 [General/admin], A-232 [General/shared], low.
16. **0.1.125** ✅ live and checked on 8.x (2026-11-10): the sweep's A-212 to A-217, A-210 (mostly),
    A-213's and A-224's orchestrator, A-229 and A-230, and from its review round A-235 (Archi's
    redeploy that lost each node) and A-236 (a removed ingress's SY.edge). Its deploy saw A-199
    again. **Found:** A-237 [General/SDK], low. **Next:** A-219 (built, in review, for 0.1.126).
17. **0.1.126, A-219 alone** ✅ live and checked on 8.x (2026-11-10): a worker goes behind the egress
    and back by an explicit action (with A-225); worker1 left behind egress1, as the operator decided.
18. **0.1.127, one branch per binary** ✅ live and checked on 8.x (2026-11-10): the orchestrator
    (A-199's step 1, reads apart and 4 per target; hive-config stage 5, the blob folders following
    their dist folder, and A-188; A-196), SY.admin (A-233 and its in-flight race, A-231, A-234,
    A-232, A-171), SY.vault (A-200), SY.timer (A-220, `LINK_BUSY` retried). **Found by its review
    round:** A-245 to A-249, low. **Found by its gate:** A-250 [General/admin], the update's follow
    loses a read to each spoke's self-restart; A-251 [General/admin] and A-252 [General/join], low.
    **Next:** the Rust SDK alone (0.1.128), then the router alone (0.1.129).
19. **0.1.128, the Rust SDK alone** ✅ live and checked on 8.x (2026-11-10): A-218 (the dead identity
    surface, SY.identity's migration v5; the identity shared memory exactly as it was), A-237 (an
    intermediate answer marked accepted), A-248 (an AI node used as a tap target answers nothing,
    counted). Its deploy saw A-250 again (systematic). **Next:** the router alone (0.1.129: A-222,
    A-209 with a node's own `via_tap` cleared, A-221).
20. **0.1.129, the router alone** ✅ live and checked on 8.x (2026-11-10): A-222 (a bounded queue per
    link, the newest refused `LINK_BUSY` with an answer, never a disconnect), A-209 (no router answers
    a tap copy; a node's own `via_tap` cleared), A-221 (an agent's turn reaches cognition only once its
    link took it). **Found by its gate:** A-253 [General/storage], low. A-250 seen a third time.
21. **0.1.130, the Go SDK alone** ✅ live and checked on 8.x (2026-11-10): A-228 (the RPC error carries
    the router's reason), A-239 (`TimerInfo` keeps `canceled_reason`; SY.timer serializes the SDK's
    type, its bytes unchanged), A-218's ECHO leftovers (the notice rules match the AI nodes'). Its
    update closed in 6 s with no orchestrator restarting, which confirms A-250's cause. **Found by its
    builder:** A-254 [WF], low.
22. **0.1.131, one branch per binary, from the sweep (ultracode)** ✅ live and checked on 8.x
    (2026-11-11): SY.admin (A-242, no removal refused for pages, each naming them; A-249 closed with
    it; A-251; A-250's option (a), the update closed 38 s after its call), SY.timer 1.5 (A-238, A-244),
    IO (A-91, A-226, A-223's spec), WF (A-254 with the 366-day bound) and the orchestrator (A-252).
    **Found by its builders and reviews:** A-255 to A-265. **Found by its gate:** A-266
    [General/SY.timer] and A-267 [General/orchestrator], low. **Next:** the router alone (0.1.132:
    A-245, A-195, A-261, A-262, A-246, A-247, A-208, A-253, A-265), then 0.1.133 (A-191, A-240,
    A-241, A-255, A-258, A-259, A-260, A-266, A-267).
23. **0.1.132, the router alone** ✅ live and checked on 8.x (2026-11-11): A-245 (a far router's answer
    reaches a sender on another spoke), A-195 (a new sender's and a new link's first frames are no
    longer dropped; A-261, A-262 with it), A-265 (a new node reached by name from another spoke in
    0.33 s, 5.98 s before), A-246, A-253 (each dropped record named), A-208 (A-247 closed into it).
    **Found by its review:** A-268, A-269; **by its gate:** A-272, low. **Next:** 0.1.133 (A-191,
    A-240, A-241, A-255, A-258, A-259, A-260, A-266, A-267, A-270, A-271).
24. **The sweep's end (the operator, 2026-11-11):** 0.1.133, one branch per binary (A-191 with A-232,
    A-240, A-241, A-255 checked live, A-258, A-259, A-260, A-266, A-267, A-270, A-271, A-274, A-279
    with A-288, A-281, A-285's texts); 0.1.134, the router alone (A-272, A-268); 0.1.135, the AI SDK
    alone (A-257); 0.1.136, one branch per binary (A-243, A-263, A-273, A-280 in SY.admin; A-282,
    A-283, A-311 in the orchestrator; A-277 in SY.identity; A-284 in the WF; A-286, A-287, A-298 in SY.architect;
    A-292 in SY.cognition; A-293 in IO.cloud); then A-278 alone (the SDKs and the WF). **From
    0.1.136 a new finding enters only if it breaks operation; the rest is recorded for later.** The
    SDKs' lows (A-264, A-290, A-294, A-295, A-285's catalog text) and the router's A-269 go with each
    one's next change.
    **0.1.133 deployed 2026-11-11, and its gate found A-314 (🔴, no node can be launched): 0.1.134 is
    the orchestrator's hotfix alone, so the router is 0.1.135, the AI SDK 0.1.136 and the sweep's
    binaries 0.1.137 (the cut-off above moves with them).**
25. **0.1.133, one branch per binary, and 0.1.134, the orchestrator's hotfix** ✅ 0.1.134 live, both
    checked on 8.x (2026-11-11): 0.1.133 shipped A-191 (system ILKs retired, never deleted) with
    A-232, A-240 (a status read writes nothing), A-241 (a worker cleans its own blob staging:
    its first pass on 8.x deleted the 2 old put leftovers of 4 planted files), A-255 (the edge applied to the running IO.cloud, built: a new
    edge's apply by its tests, and on 8.x a rotation of the Cloud's token on the edge IO.cloud
    already trusted), A-258, A-259 and A-266 (SY.timer 1.6), A-260 and A-274 (the publication lock
    free while an edge hangs), A-267 and A-271 (a check with no answer acts on nothing), A-270,
    A-279 with A-288 (Archi's SCMD reads), A-281 (no version removed while a hive's use of it was
    not read) and A-285's texts; what needed a node launched was checked from 23:33Z, over 0.1.134.
    **Found by its gate:** A-314 (🔴, on the hives' systemd 255 a node unit systemd does not have
    read as no answer: no node could be launched), fixed forward the same evening by 0.1.134, the
    orchestrator alone, with its review's guard (`kill_node` and `remove_node_instance` refuse an
    `SY.*` or `RT.*` name); A-315 [Archi], low; A-308 seen live. **Next:** 0.1.135, the router alone
    (A-272, A-268). A-314's leftover, SY.admin's help that offers `SY.admin@motherbee` as the
    `node_name` of `kill_node` and `remove_node_instance`, is built for SY.admin's 0.1.137
    (`coa/admin-136`, `b0bfce41`).
26. **0.1.135, the router alone** ✅ live and checked on 8.x (2026-11-11): A-272 (split horizon: a
    broadcast that came over the WAN never goes back to the peer it came from; 32 broadcasts from
    each hive reached every receiver exactly once and no router logged a drop, where on 0.1.134 the
    echoes gave 7 drop lines on the motherbee and 6 on each spoke) and A-268 (FIB builds stored in
    the order they began; its race by its unit test, and A-265's check reached a new node on
    worker1 from ingress1 0.05 to 0.15 s after its connect, 0.33 s on 0.1.132). The smoke after
    A-314: a stopped node whose unit was gone was started (ok in 1.3 s, `RUNNING` at 11 s), and a
    core node's kill was refused. Health clean on every hive with `wandrops=0`, which `ops.py
    health` checks from now on.
    **Next:** 0.1.136, the AI SDK alone (A-257, `coa/aisdk-136`).
27. **0.1.136, the AI SDK alone** ✅ live and checked on 8.x (2026-11-12): A-257 (the AI runtime
    reads every message while its 4 workers run LLM turns: status, help, config and a well-formed
    handoff run at once beside them, and the turn past the 128 waiting is answered
    `ai_runtime_busy` at once, never run). With both nodes' workers held by an nft drop of their
    HTTPS, AI.pruebas answered its status in 0.924 s and `CONFIG_GET` in 0.328 s and the frontdesk
    a handoff in 6 ms, where on 0.1.135 the status was the orchestrator's inference at 2.449 s,
    `CONFIG_GET` timed out at 25.168 s and the handoff got no answer; a turn past the queue
    answered busy in 7 to 12 ms; the waiting turns drained within 0.165 s, with no restart. 2 model
    calls.
    **Found by its review:** A-324 and A-325, low; and the known limit written into A-257 (the
    handoff rule applies to ai.generic too, until the frontdesk's `AiNode` override).
    **Next:** 0.1.137, the sweep's binaries, one branch per binary (`coa/admin-136`,
    `coa/orch-136`, `coa/architect-136`, `coa/identity-136`, `coa/wf-136`, `coa/cognition-136`,
    `coa/iocloud-136`), integrated with their texts moved to 0.1.137.
28. **0.1.137, the sweep's seven binaries, one branch per binary, with A-322 and A-323** ✅ live and
    checked on 8.x (2026-11-12): SY.admin (A-243, each publisher takes its own pages down; A-263,
    the update's reads at once; A-273, its help, the failure not provoked; A-280, a failed publish
    answers within its budget; A-311's and A-314's help), the orchestrator (A-282, a
    `systemctl show` with no answer by its tests; A-283, the bounded Syncthing restart, and its
    drift read at +24 h: no warning opened unprovoked; A-311, with the integration's readers), SY.identity
    (A-277), the WF (A-284), SY.cognition (A-292), IO.cloud (A-293, a retryable frontdesk error by
    its tests) and SY.architect (A-286, A-287, A-298, A-311's reconcile, A-322, A-323). On 8.x: a
    WF node's 1000 running instances counted in 0.50 to 1.04 s, where 0.1.136's count failed past
    the frame; a removed worker's reconnects refused *"not in the hive registry"*, its key deleted
    or pinned in place; worker1's frozen Syncthing restarted at +329 s and not again within the
    hour; a refused turn dropped from cognition 0.037 to 0.255 s after the busy answer; IO.cloud
    refusing a malformed registration before anything is created; Archi's publish moving its nodes
    in place; and after each CONFIRM Archi's own words on what each step answered.
    **Found by A-322's review:** A-327 and A-328, low; A-124 unchanged. **Found by its gate:** A-329
    [General/Go SDK], medium (on 0.1.136: the Go SDK dropped 344 of a burst of 1000 messages);
    A-330, A-331 and A-332, low; B-18, the lab's template. A-326 is A-278's, on `coa/a278`.
    **Pending:** the gate's `drift` read at +24 h (2026-11-13, about 11:00Z) for A-283. (The Cloud
    team's page, outside the repo, says A-243 and A-298 are live since its Version 4, 2026-11-12.)
    **Next:** A-278 alone (built on `coa/a278`, `711cda24`, for 0.1.138); then A-191's leftover:
    a purge of the system ILKs 0.1.133's gate retired stays the operator's explicit word.
29. **0.1.138, A-278 alone (the Go SDK, the Rust SDK and the WF engine)** ✅ live and checked on 8.x
    (2026-11-12): a WF reconciles all its pending timers: the SDKs say where the timers past a list
    start, the WF reads every page of SY.timer's list, and when the list stops before its end a
    timer past its time and past the last one listed is fired. On 8.x a WF restarted with 3,000
    pending timers listed them all in 13 pages in 0.931 s, took none as missing and was never
    `DEGRADED`, `HEALTHY` at 3.670 s, where on 0.1.137 it took 2,000 as missing, sent 2,030 again
    and ran `DEGRADED` 72 polls, `HEALTHY` at 94.453 s. No model call. Not provoked on 8.x, so by
    its tests: a list that stops before its end (the review's fire of a timer past its time and its
    cancel, the 200-page bound, a page of no timer that gives a cursor, a cursor given again, an
    SY.timer before 1.6), and the Go SDK's `List` and the Rust SDK's `list` and `list_page` (no
    caller on 8.x).
    **Found by its gate:** A-333 [WF], low (the reconcile of 3,000 pending timers takes 3.6 s on
    worker1, and the receive loop starts after it); A-326's live data (a walk of any status, its
    slowest page 7.204 s); A-302 seen live. **In its deploy's timeline:** A-334 (worker1's
    Syncthing failed to delete some directories of its receive-only mirror), closed: a later pass
    deleted them, nothing left.
    **The 0.1.137 gate's `drift` read for A-283** (2026-11-13, 14:35Z, 29 h from 0.1.137's
    deploy): no `api_unanswered` opened unprovoked on any hive (A-283).
    **Then:** A-191's leftover, the operator's call of 2026-11-12 (*"Purge the 61 from removed
    workers"*): the 61 retired system ILKs of removed hives purged, each first read free of history;
    SY.policy's 2 stay retired (A-191).

30. **The bug push (the operator, 2026-11-12: *"cerraría con un empujón más, resolviendo todo lo que
    tiene que ver con nodos SY. RT. WF. AI ... y al menos resolver los más graves"*; IO only where a
    test needs it; then the IO family, applied to io.slack to close it).** From the state review of
    2026-11-12 (66 open items, a top 10 verified in the code). The operator's calls of the day:
    A-329, A-304, A-307, A-124, A-313 (left as it is) and A-317 (recorded in each entry); and, on
    0.1.139's open points, A-317's two (the old door, as built; the vault's rollback, built at the
    integration), A-332 (the grace 5 s), A-326 (a 16 MiB page cache), A-304, A-335 and A-303 (each
    as built), recorded in each entry as *"The operator's call (2026-11-12)"*. Releases,
    one per binary group, the SDKs each alone: 0.1.139 the binaries (SY.admin: A-312, A-297, A-304,
    A-305, A-306, A-316, A-317 (built in SY.admin alone: SY.edge is unchanged), A-318, A-319, A-324,
    A-328; the orchestrator: A-296, A-299, A-300, A-301, A-308, A-310, A-330, A-331; Archi: A-124,
    A-315, A-320, A-321; SY.frontdesk.gov: A-303; SY.cognition: A-332; SY.timer: A-326; the WF
    engine and SY.wf-rules: A-302, A-307, A-333); then the router alone (A-269); the Go SDK alone
    (A-329 with SY.timer's resend, A-264); the AI SDK alone (A-294, A-290, A-325, `tool_choice` for
    A-327); the Rust SDK alone (A-295, A-309, A-285's `cloud.rs` text). Lab: B-18, the template
    rebuilt (✅). Out of it, by decision or as large topics: A-159/A-275/A-276 (deletion),
    A-137/A-227 (cognition), A-142 (with io.web), A-168, the parked security items, A-289, A-199
    step 2, A-250 (b), A-210 (after 2026-11-22).
    **0.1.139, the binaries, one branch per binary** ✅ live and checked on 8.x (deployed 2026-11-12
    22:30 ART, gate to 2026-11-13): the Cloud's door recorded before its token, a refused
    `edge_node` and a failed record changing nothing, and a door that did not open rolled back to
    the Cloud's token, which still opened it (HTTP 200); a secret an answer brings shown once in
    Archi and kept masked; `BUSY` within SY.admin's 75 s; a WF's reconcile of 3,000 timers in
    0.042 s (4.214 s on 0.1.138); the Cloud's `list_nodes` in 12.5 s behind a frozen worker
    (`TIMEOUT` at 30.0 s on 0.1.138); `identity_unavailable` in about 0.77 s; a refused turn with no
    cognition call; `BOX_RUNS_ANOTHER_HIVE` in 29 s (191 s and a timeout on 0.1.137); SY.timer 1.7's
    list indexes. 423 PASS, 57 SEE and 37 FAIL, each FAIL read, none a fault of 0.1.139's changes.
    By their tests: A-318, A-319 and A-328 (✅ BUILT), and A-317's move to another edge and its
    close.
    **The hazard S4 left, found and fixed:** step 44.1's fake record went to a stray top-level
    `config` key of IO.cloud's config.json, which IO.cloud would have taken as its whole config at
    its next start, dropping every Cloud request; it did not restart, and the key was removed on 8.x
    at about 05:44Z on 2026-11-13 (A-346).
    **Found by its gate:** A-340 [General/orchestrator], low (a join refused at its key probe says
    the motherbee's key is still on the box); A-341 [General/orchestrator], low (`start_node` within
    about 10 s of a node's exit answers `already_running` and launches nothing); A-342 [WF], low (a
    WF's resends of its timers land up to about 2 s late); A-343 [General/orchestrator], medium (a
    hung Syncthing freezes its hive's orchestrator about 10 s of every 15; a local fix, to ask the
    operator); A-344 [WF/wf-rules], low (a change that could not rebind its node answers with no
    `retryable`; a question for the operator); A-345 [General/orchestrator], low (`get_node_status`
    cannot tell an unconfigured node from any other `DEGRADED` one); A-346 [General/admin], medium
    (`set_node_config`'s help describes a body its HTTP route does not take; the fix to decide);
    A-347 [General/orchestrator], low (`remove_hive`'s cleanup races the box's own orchestrator).
    **0.1.140, the router alone** ✅ live and checked on 8.x (2026-11-13): A-269's router part: a
    node name that is not an L2 name refused at HELLO in 1.1 to 3.0 ms with no line forged in
    rt-gateway's journal, where 0.1.139 announced a name with a newline and took 3 forged lines;
    the deploy refused no live node (every node list as before it); 50 PASS, no model call.
    **0.1.141, the Go SDK alone** ✅ live and checked on 8.x (2026-11-13): A-329 (a burst to a WF:
    every message created or answered `NOT_TAKEN node_busy`, retryable, none dropped unseen, where
    0.1.140 dropped 1,717 of 2,000; SY.timer sent again 1,943 fires a WF could not take, none lost,
    where 0.1.140 lost 867 of 1,500), A-264 (a WF's 140 KiB text as a blob, where 0.1.140 refused
    it), A-325's Go sibling (SY.timer leaves `VAULT_SECRET_CHANGED` unanswered); A-338's window seen
    (a blob reached the motherbee 11.2 s after its message); 152 PASS, no model call.
    **0.1.142, the AI SDK alone** ✅ live and checked on 8.x (2026-11-13): A-294 (a reply over the
    frame fails alone, the runtime goes on, where 0.1.141's process exited and restarted), A-290 (a
    saturated AI node reads `DEGRADED` `ai_runtime_saturated` and turns one more turn away at once,
    status reads under 1 s), A-325 (ai.generic and the frontdesk leave `VAULT_SECRET_CHANGED`
    unanswered: closed with its Go sibling), A-339 (they leave a `NOT_TAKEN` and an error answer
    unanswered); A-327's `tool_choice` by its tests; 97 PASS, no model call.
    **Next:** the Rust SDK alone (0.1.143, `coa/rustsdk-143`, with A-269's call (b)), built and
    pushed, not released; then the IO family, with io.slack.
    **Numbers given on the push's branches** (the operator's call of 2026-11-12, where 0.1.139's
    integration review had given the orchestrator's item A-339): A-335 is the orchestrator's (a box
    `remove_hive` did not reach, 0.1.139); A-336 is the Rust SDK branch's (`coa/rustsdk-143`: the
    orchestrator reads `NOT_TAKEN` as unknown); A-337 is Archi's plan-review item (SY.admin's plan
    review model, found by A-124's review, 0.1.139); A-338 is the Go SDK branch's [General/blob]
    item (`coa/gosdk-141`); A-339 is the AI SDK's item (*an AI node answers a `NOT_TAKEN` with
    `unknown_system_msg`*), written A-335 on `coa/aisdk-142` and `coa/rustsdk-143`, to rename at
    their integration. The next new number is A-348.
    **0.1.139's deploy:** the motherbee ran it before the workers did, and until every hive's core
    ran 0.1.139 no runtime version was removed and no workflow deployed or deleted (their purges
    remove versions): a worker on 0.1.138 listed no `RELAUNCH_DUE` node (A-300), so a version a
    crash-looping node there was pinned to could be removed under it; its SY.wf-rules answered a
    purge refused `UNAUTHORIZED` with the old warning, which offered a removal (A-304), never
    answered `BUSY` (A-307), still moved its slots before a refused publish, and waited 5 s for a
    publish. Every hive runs 0.1.139 since the deploy's update closed (22:34 ART on 2026-11-12), so
    the hold is lifted.

### The sprint of 2026-11-04 (done; A-137 goes on in the sprint of 2026-11-07)

1. **Small, from the tests** (1 to 3 ✅ validated on 8.x with 0.1.99, 2026-11-04; 4 and 5, found
   on the way, ✅ with 0.1.100, 2026-11-05; 6 and 7, from 5's talk, ✅ validated on 8.x with
   0.1.101, 2026-11-05):
   1. A-146 [General/router]: the VPN warning gone.
   2. A-147 [Archi]: the model's text shown with a staged plan.
   3. A-115 [General/SY.timer]: a fire sent again while its owner is unreachable; a one-shot never
      fired twice; after a reboot, an overdue fire waits for its owner; caps on the delay, the
      count and the payload; listing and reading only for the owner and SY.admin; and the target
      rule, the operator's call (2026-11-04: a node schedules only for itself, a SY node for any).
   4. A-152 [Archi], found checking A-147: the confirm sentence said once.
   5. A-151 [General/SY.timer], found on the way to A-115: `TIMER_LIST` in pages (SY.timer 1.3),
      checked with a WF's start over 350 timers.
   6. A-153 [WF], from A-151's talk with the operator: at most 10 timers in a WF definition.
   7. A-154 [WF], from A-153's talk: a quiet WF conversation closes on its own, after 4 days by
      default.
2. **The 🔴, simple and important** (✅ 0.1.99, 2026-11-04: A-100, A-89 and A-90 validated on 8.x;
   A-84 and A-85 by tests):
   1. A-100 [General/router]: protected verbs gated on any `msg_type`, in any case.
   2. A-84 [General/SY.admin]: `bundle_upload` with no symlinks, regular files only, caps and its
      own modes.
   3. A-85 [IO/io.slack]: the file fetch streamed under a byte cap, with timeouts and no redirects.
   4. A-89 [io.web]: slots counted past the handshake, a handshake budget per source, a shorter
      timeout.
   5. A-90 [io.web]: a share of the edge's slots per client, connections to io.web reused or closed
      by io.web, a deadline for a stalled body.
3. **WF, design first, one topic at a time:**
   1. A-138 [WF]: the conversation's trace in the message: design agreed and built 2026-11-04
      ([message-trace-v1](../../docs/message-trace-v1.md)); ✅ 0.1.100, its WF gate on 8.x
      (2026-11-05).
   2. A-137 [WF]: the chat on cognition across hands. Its first part, agreed and built 2026-11-05
      ([cognition-hands-v1](../../docs/cognition-hands-v1.md)): who wrote each turn, which turns
      cognition analyzes, and how the immediate memory names its speakers. ✅ 0.1.101, its WF gate
      on 8.x (2026-11-05). Its second topic, several people on one thread and the feelings (§10),
      agreed and built 2026-11-05 with A-155, A-157 and two of A-156's three: ✅ 0.1.102, its
      gate on 8.x (2026-11-06). Where cognition keeps its state, reviewed whole and agreed on
      2026-11-06 (§11), built as A-158 with A-156's first: ✅ 0.1.103, its gate on 8.x
      (2026-11-06). The rest, agreed in the chat of 2026-11-06 (§12), built the same day with
      its review round's fixes, went out in 0.1.104 (2026-11-07); its gate on 8.x found A-161,
      fixed with A-162: ✅ 0.1.105, the same gate on 8.x (2026-11-07), which found A-163.

Blocks 1 and 2 went out in 0.1.99 (2026-11-04), checked on 8.x. Block 3's A-138, with A-151 and
A-152, went out in 0.1.100 (2026-11-05), checked on 8.x. A-153, A-154, A-137's first part and the
empty-block fix went out in 0.1.101 (2026-11-05), checked on 8.x. A-137's feelings, with A-155,
A-157, two of A-156's three and the fix of a person with no name, went out in 0.1.102
(2026-11-06), checked on 8.x. Where cognition keeps its state, A-158 with A-156's first, went out
in 0.1.103 (2026-11-06), checked on 8.x. The rest of A-137 went out in 0.1.104 (2026-11-07), and
A-161 with A-162 in 0.1.105 (2026-11-07), each checked on 8.x.

### WF

- A-137 🔵 Cognition follows only the conversations an agent is in. *Sprint: its first part ✅
  0.1.101, the feelings ✅ 0.1.102, where cognition keeps its state ✅ 0.1.103 (A-158, with
  A-156's first); the rest agreed in the chat of 2026-11-06 and out in 0.1.104
  (cognition-hands-v1 §12: the handover's lines, people with no agent, episodes as public
  news), and its gate's A-161 ✅ 0.1.105 and A-163 ✅ 0.1.106.*
- A-168 🔵 A WF's menu has no schema, so no provider holds an agent's answer to it (from A-150).
- A-342 🔵 A WF's resends of its timers land up to about 2 s late (low; the WF engine; found by 0.1.139's gate; there before it, since A-254).
- A-344 🔵 A change that ran and could not rebind its node answers 502 `WF_NODE_NOT_DEPLOYED` with no `retryable` (low, a question for the operator; SY.wf-rules; found by 0.1.139's gate).

### io.web

- Stage 2 ([io-web-v2](../../docs/io-web-v2.md)): sessions, Cloud's one-time code, Archi behind
  io.web. It waits for the operator's definition of its security.
- A-142 (Archi) bites here once stage 2 proxies Archi.

### Archi

- A-142 🟡 Archi's chat answers only when its turn ends, and the edge waits 30 s.
- A-307's Archi part 🔵 Archi's pipeline does not know `BUSY`: a step SY.wf-rules refused so, nothing changed and `retryable` true (A-307), blocks the run as *"Pipeline execution failed"*, not as a refusal that may pass asked again (low; SY.architect: a deterministic rule in `classify_failure_deterministic` and `BUSY` beside `SERVICE_FAILED` in the prompt facts; left by A-307's review, see its entry; the handbook already tells Archi's model, as it does of SY.admin's own `BUSY` for a Cloud door publish while another runs, since 0.1.139's integration review).
- The solutions chapter ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md), its status and
  §5.5.3): where a solution lives, with its manifest and its deployment; Archi keeps no definition
  across chat turns; the atoms become a document of their own. **A large topic (the operator,
  2026-11-11), to think about first:** a repository that keeps the solutions as sources (scripts),
  with their versions, since several solutions can live in one tenant.

### IO

- A-149 🟡 The family has no written standard, and its nodes answer health and config each their
  own way.
- A-114 🟡 No IO node writes first to a person; io.api delivers no answer at all.
- A-256 🔵 io.cloud accepts `io.inbound_family` and ignores it (low; with A-149).
- **io.slack:**
  - A-136 🟡 One interlocutor's conversation is split by where they write in Slack.
  - A-148 🟡 What it lacks against the standard, starting with its revamp's architecture.
- **io.blob:**
  - A-106 🟡 The blob store has no tenant.
  - A-107 🔵 A published page cannot be updated under the same link (pending by the operator's
    choice).
- **io.wapp, io.linkedhelper:** test-grade (the operator, 2026-10-15). Their tracks start from the
  standard.

### General

- A-96 🔵 How the SY nodes' tokens are measured per message.
- A-159 🔵 An ILK is deleted for good while data elsewhere refers to it: a chapter of its own, for
  later (the operator, 2026-11-06).
- Security, parked by decision: A-22, A-49, A-51, A-55, A-60.
- A-175 🟡 A spoke's drift alerts stay on the spoke: with the system's log, later (the operator,
  2026-11-08; router-stream-v1 §8).
- A-193 🔵 Where a deploy and a join spend their time: the motherbee's 65 s with its orchestrator
  and admin down, 25 s of hashing per start, a join's 67 s SSH copy.
- The sweep's calls, decided by the operator on 2026-11-10: A-211 ✅ 0.1.124; A-212 to A-217 ✅
  0.1.125; A-219 ✅ 0.1.126 (worker1 behind egress1); A-210 mostly (SY.edge's part after
  2026-11-22, and a few the builder found); A-218 ✅ 0.1.128 (the dead identity surface, the Rust SDK alone). Cron descriptors wait for the talk on a WF that starts on a schedule.
- A-227 🔵 A relay that names no one is attributed to the last other person who wrote through that
  IO node (low).
- A-269 🟡 The router takes any node name and logs it unescaped (low; the router's part ✅ 0.1.140; the operator's call (b), the Rust SDK's logging for every Rust binary, built on `coa/rustsdk-143` for 0.1.143).
- A-275 🔵 A removed hive's managed nodes keep their ILKs active and their vault secrets (low; the operator's rule of 2026-11-11 keeps them; with A-159).
- A-276 🔵 A replica cut off during a purge can bring back a purged node ILK (low; with A-159).
- A-289 🔵 Several routers on a hive (not supported): a broadcast from a router that is not the gateway needs ttl 4 to cross the WAN (low; a known limit).
- A-291 🔵 io.slack posts an agent's error answer as *invalid_text_payload* (low; io.slack's revamp).
- A-295 🔵 The Rust SDK's command channels are unbounded (low; the Rust SDK; built on `coa/rustsdk-143`, for 0.1.143, planned).
- A-309 🔵 The Rust SDK's `send_with_matcher` keeps its pending entry when its caller drops the call (low, latent; the Rust SDK; built on `coa/rustsdk-143`, for 0.1.143, planned).
- A-313 🔵 A Syncthing that cannot come up holds the whole watchdog about 35 s a tick (low; the orchestrator; to decide) (see A-343: a hung Syncthing freezes the hive's orchestrator, not only its watchdog).
- A-327 🔵 Archi's explanation turn after a CONFIRM never reads the prompt cache: about 40,000 tokens written to it at each run (low; the AI SDK's `tool_choice` ✅ 0.1.142, by its tests; SY.architect's use of it open, for its next release).
- A-335 🔵 A box `remove_hive` did not reach is taken again over the socket by a join under its hive's name, whatever address it names, with no identity key and no new certificate (low; the orchestrator; found by A-301's builder and review; the texts built on `coa/orch-139` for 0.1.139; the operator's call of 2026-11-12: left as documented, the texts warn, and the real guard comes with A-48's leaf pinning, parked security).
- A-336 🔵 The orchestrator reads SY.admin's `NOT_TAKEN` as an unknown outcome, and a spoke orchestrator's as unreachable (low; the orchestrator's next change after 0.1.143; found by the review of `coa/rustsdk-143`).
- A-337 🔵 SY.admin's plan review gives its model each step as written, a secret the operator wrote into its args included (low; SY.admin; found by A-124's review, Archi's branch, 0.1.139).
- A-338 🔵 A text sent as a blob can fail at a reader on another hive: the message comes before Syncthing copies the blob (low; A-264's option (c), a topic of its own by the operator's call of 2026-11-12).
- A-340 🔵 A join refused at its key probe says the motherbee's key is still on the box, when the key does not open it (low; the orchestrator; found by 0.1.139's gate; misleading only).
- A-341 🔵 `start_node` within about 10 s of a node's exit answers `already_running` and launches nothing (low; the orchestrator; found by 0.1.139's gate; the router writes its LSA region on its 10 s hello).
- A-343 🔵 A hung Syncthing freezes its hive's orchestrator for about 10 s of every 15 (medium; the orchestrator; found by 0.1.139's gate; its local fix, `block_in_place`, to ask the operator).
- A-345 🔵 `get_node_status` cannot tell an unconfigured node from any other `DEGRADED` one (low, observability; the orchestrator; found by 0.1.139's gate).
- A-346 🔵 `set_node_config`'s help describes a body its HTTP route does not take (medium; SY.admin; found by 0.1.139's gate; the hazard it left on IO.cloud fixed on 8.x; the fix to decide).
- A-347 🔵 `remove_hive`'s cleanup on the box races the box's own orchestrator, which starts again the core services the cleanup stopped (low; the orchestrator; found by 0.1.139's gate).
- The sweep of 2026-11-07 ([logbook](2026-11-07.md)): about 30 small items off the board, those
  that need the operator's call listed apart.
- [PENDING-BUGS.md](PENDING-BUGS.md)'s August rows were checked on 2026-11-07: PB-1 and PB-5 were
  already closed, U-12 needed no change, and the other three came here.

### Infrastructure and the lab (section B)

- B-14 🟡 The PROD VMs boot slowly (lab only).

---

## A. The fluxbee product — candidates for a code change

### A-1 ✅ VALIDATED ON 8.x (0.1.81, 2026-10-20) — The orchestrator does not configure secondary network cards

- **Fixed (0.1.81, `4e9f91e`):** the egress join reads `edge_ip` from `lan_iface` on the host
  (an explicit one must be on it); the LAN leg, IPv4 forwarding and the IPv6 block are checked at
  boot (fatal), on every drift tick (alert) and by the join on the box (`EGRESS_NAT_NOT_READY`,
  `EGRESS_LAN_LEG_INVALID`). PROD's egress1 had `edge_ip` 10.10.10.1, add_hive's guess, on a leg
  that is 10.10.10.40: corrected by hand before the upgrade. Validated: `lan_leg_ok=true`.

> **Reframed in the 2026-08-03 audit ([PB-5](PENDING-BUGS.md#pb-5)):** addressing the cards is the
> operator's job by contract, in every role. What stays open is that the egress reports
> `nat_applied: true` without checking its LAN leg.

- **What happens:** `add_hive role=ingress|egress` **requires** the interface names (`wan_iface`,
  `lan_iface`) and assumes they **already exist and are addressed**. Not a single line in the
  orchestrator assigns IPs to interfaces.
- **Evidence:** `src/bin/sy_orchestrator.rs` — `resolve_add_hive_egress_section` validates
  `lan_cidr`/`wan_iface`/`lan_iface`; `reconcile_egress_nat` applies nftables **on interfaces it
  assumes are configured**. Zero address handling.
- **Impact:** every deployment with ingress/egress needs **manual or external** network
  configuration (in this case, cloud-init). On bare metal without cloud-init, someone does it by
  hand → a non-reproducible step, outside the recipe.
- **Detected by:** the operator anticipated it; confirmed by reading the code.
- **Status:** a real product gap. **To discuss:** should the orchestrator accept the configuration
  of the secondary legs in the `add_hive` payload and apply it?

### A-2 ✅ RESOLVED (0.1.81: the join warns; PB-6 option b) — `harden_ssh` defaults to `false`

- **Fixed (0.1.81, `4e9f91e`):** when the join's password fallback ran and the box was not hardened,
  the join answers a `warnings` entry saying password logins stay on and how to close them (PB-6
  option b; option a went with D32). Covered by tests; no join ran in the gate.

> **Rewritten in the 2026-08-03 audit ([PB-6](PENDING-BUGS.md#pb-6)):** flipping the default
> would leave boxes with no way in at all. What stays open is that the `ssh_password` channel does
> not close by itself.

- **What happens:** with an `ssh_password` bootstrap, if `harden_ssh:true` is not passed
  explicitly, when the join ends `add_hive` **removes its key and its sudoers entry but leaves
  `PasswordAuthentication yes`**. The machine is left with password login open.
- **Evidence:** `resolve_add_hive_harden_ssh` → default `false`. The hardening
  (`disable_remote_password_auth_with_access` + verification) only runs if it is `true`.
- **Impact:** the correct mental model is *"the box opens for a few seconds and `add_hive` closes
  it"*. With the current default **that does not hold** unless the operator remembers the flag.
- **Status:** **to discuss.** Options: flip the default, or make it loud (warn in the response
  when the bootstrap used a password and no hardening ran).

### A-3 ✅ CLOSED (→ PB-7, `9f86c12`) — The admin timeout (180 s) can fall short for `add_hive`

> **Closed with [PB-7](PENDING-BUGS.md#pb-7):** `add_hive` answers `202` right away and the join
> runs in the background, with its phase in `info.yaml`. Validated live on 2026-08-05.

- **What happens:** `JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS` defaults to **180 s**, but the flow's
  internal waits can add up to more (30 s health + 60 s WAN + 60 s LSA + finalize).
- **Evidence:** `src/bin/sy_admin.rs` (timeout) vs. the gates of `add_hive_flow` in
  `sy_orchestrator.rs`.
- **Existing mitigation:** the hive stays in `status: pending` and **retrying is idempotent**.
- **Impact:** on slow boxes the client sees a *timeout* even though the join carries on and ends
  well → it confuses, and it can lead someone to "fix" something that was working.
- **Status:** **to discuss.** Raise the default, or have the response say explicitly
  "in progress, retry to see the status"?

### A-4 ✅ CLOSED (→ PB-8) — `egress.gateway_ip` propagates to the workers, but **not to motherbee**

> **Closed with [PB-8](PENDING-BUGS.md#pb-8):** it was not an oversight. `egress.gateway_ip` is, by
> contract, the workers' way out, and propagating it to the motherbee would leave it without a
> control plane. The motherbee reports it with `EGRESS_MOTHERBEE_BYPASS` and does not touch its
> route. Validated live.

- **What happens:** when MB declares `egress.gateway_ip`, **each worker** routes its default
  through the egress (`reconcile_worker_egress` → `ip route replace default via <gw>`). **MB is not
  on that list.**
- **Impact:** the outbound traffic of **motherbee** —which is precisely the one that calls the
  external APIs (OpenAI, Slack, Meta)— **does not go out through the egress node**, but through its
  own default route. In this deployment that means it would go out through the management network,
  which is exactly what the design wants to avoid.
- **Status:** **observed in the code, not yet confirmed live.** It is one of the points to look at
  once the egress is working. **To discuss:** is it intentional (MB must have its own way out) or
  is it a gap?

### A-5 ✅ RESOLVED (0.1.60 with A-52, validated 0.1.61) — A freshly booted clone **is not ready** for `add_hive`

- **Status (2026-10-19 review):** `wait_for_host_to_settle` runs in the three join flows; a fresh clone waited 8 minutes (ledger 0.1.61) and joined again on 0.1.62.

- **What happens:** the first boot of a VM cloned from the cloud image triggers automatic updates
  (`unattended-upgrades`), **reboots by itself**, and during that time the `qemu-guest-agent`
  (and potentially sshd) become intermittent.
- **Measured:** `netin` 37 MB → 52 MB, `diskwrite` 2.5 GB, a spontaneous reboot, the agent down for
  several minutes.
- **Impact:** running `add_hive` "as soon as the VM boots" can hit a box in the middle of an
  upgrade → an intermittent failure, hard to diagnose, and on top of that combined with **A-3**
  (timeout).
- **Status:** observed in this deployment. **To discuss:** should `add_hive` have an explicit
  *readiness gate* (wait until the box is quiet), or is documenting it in the recipe enough?

### A-6 ✅ RESOLVED — `build-deb.sh` **never compiled `ai_node_runner`** (divergence from `install.sh`)

- **What happens:** the `[1/5] build rust` step of `packaging/build-deb.sh` does:
  ```bash
  cargo build --release --bins
  cargo build --release -p sy-frontdesk-gov --bin sy-frontdesk-gov
  cargo build --release --manifest-path nodes/io/Cargo.toml -p <io crates>
  ```
  and a comment claims: *"ai.generic (nodes/ai) is a root workspace member already built by
  `--bins`"*. **That is false.**
- **Why:** the root `Cargo.toml` has **`[package]` (json-router) AND `[workspace]`**. With a root
  package present, `cargo build --bins` **without `--workspace`** compiles **only the root
  package's bins**. `nodes/ai/ai-generic` (package `fluxbee-ai-nodes`, bin `ai_node_runner`) is a
  **member** → **it is never compiled**. Note that `sy-frontdesk-gov` —another member— **does** have
  its explicit line: the correct pattern is already applied there; `ai-generic` was missed.
- **Symptom:** the build reaches `[3/5] stage files` and aborts:
  `Error: binary does not exist: target/release/ai_node_runner` → **no `.deb` is produced**.
- **Why it was never noticed:** on the **lab** build box the binary already existed in
  `target/release/` because an earlier manual `build2.sh` **does** compile it explicitly
  (`cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner`). `build-deb.sh` found it and
  carried on. **On a clean box —like prod— it fails.**
- **Impact:** the **canonical** build path is not self-sufficient. Any new build box (or a
  `cargo clean`) breaks the `.deb` build.
- **Proposed fix (one line, pending approval):** add it next to the `sy-frontdesk-gov` one:
  ```bash
  cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner
  ```
- **What makes it a divergence (not a design decision):** **`scripts/install.sh` line 246 DOES
  have** `cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner`. And `base-nodes.json`
  states explicitly that it is *"the single source of truth read by BOTH packaging/build-deb.sh and
  scripts/install.sh (**they must not diverge**)"*. In other words: `install.sh` was right and
  `build-deb.sh` was the outlier → **de-diverge, don't invent**.
- **✅ RESOLVED** (approved by the operator): `build-deb.sh` got the **same line** that
  `install.sh` already had, and **the two false comments were corrected**, the ones that claimed
  `--bins` built it (those comments are the reason the bug survived). **The two scripts ended up in
  exact parity** on the three explicit builds.
- **Shortcut rejected:** the error itself suggests `-buildvcs=false` for B-8, and here compiling
  the binary by hand would have been enough — both would have made the build pass **while hiding
  the problem**, leaving the next clean box just as broken.

### A-7 ✅ RESOLVED (0.1.41–0.1.42) — User OPA did not reach the spokes (CONFIG_CHANGED never left the motherbee)

- **What happens:** the admin sends OPA writes as CONFIG_CHANGED; the router intercepts it before
  routing, ignores the destination and only distributes it inside the host. A clear aimed at
  worker1 was run by the motherbee (2026-09-28). No spoke has anywhere to get the policy from;
  ingress and egress do not even run SY.opa.rules. And the compiled wasm (140–155 KB) does not fit
  in a message (128 KiB cap).
- **Fix (the operator's design, logbook 2026-09-30; `docs/opa-distribution.md`):** SY.opa.rules on
  every hive; only the motherbee compiles and publishes the wasm in the Syncthing folder
  `fluxbee-dist-policy`; a CONFIG_CHANGED `{opa, sync}` announces it; each hive installs it if the
  sha256 matches; one single global policy; eventual convergence; no encryption. CONFIG_CHANGED now
  travels over normal routing (it crosses hives).
- **Validated on 8.x:** apply on the 4 hives in 16 s; a hive cut off from Syncthing caught up by
  itself, 27 s after reconnecting; global clear (slow until 0.1.43: A-19).

### A-8 ✅ RESOLVED (0.1.36) — One hive's config applied on another: `add_route` on a spoke overwrote the motherbee's routes

- **Confirmed live** on 8.x (logbook 2026-09-30): two adds on worker1, and the motherbee ended up
  with those routes. The broadcast after each add/remove carried the spoke's list, and the admin's
  counter ran one ahead of SY.config.routes'.
- **Fix:** `ConfigChangedPayload.hive`; the admin fills it in, and SY.config.routes / SY.opa.rules
  ignore another hive's config.

### A-9 ✅ RESOLVED (0.1.36) — Any node could rewrite its hive's routes, taps or OPA

- **What happened:** CONFIG_CHANGED was not a protected action, and the router's intercept
  distributed it without looking at the origin; SY.config.routes accepted `add_route`/`add_tap`
  from anyone (the `admin` type does not go through the router's gate); SY.opa.rules accepted
  `compile/apply/rollback_policy` from anyone. A user node could, for example, add itself a tap and
  mirror traffic.
- **Fix:** CONFIG_CHANGED protected (router + `system.rego`, current rules, no recompilation);
  SY.config.routes mutations and SY.opa.rules commands only from `SY.admin@motherbee`.

### A-10 ✅ RESOLVED (0.1.36) — Nodes outside the motherbee pointed at `SY.vault@<their hive>` / `SY.admin@<their hive>`

- **What happened:** vault and admin run only on the motherbee. Cognition, the AI runners and the
  IO nodes derived the vault's destination from their own hive; SY.wf-rules published via
  `SY.admin@<their hive>`. On a worker, none of that exists. SY.edge had already solved it with
  `vault_hive`.
- **Fix:** `VaultClient::for_primary` and `PRIMARY_HIVE_ID` in the SDK (the router re-exports it);
  `PrimaryAdminNode` in the Go SDK for SY.wf-rules.

### A-11 ✅ RESOLVED (0.1.36) — The identity replica did not catch up on reconnect

- **What happened:** the full sync ran only at startup. If the delta stream was cut, or the worker
  started with the motherbee down, the replica kept old data (or only the system ILKs) until the
  next restart, even though the log said it would converge on reconnect.
- **Fix:** each subscription starts with a full snapshot; a sequence gap reconnects and resyncs
  instead of `exit(0)`.

### A-12 ✅ RESOLVED (0.1.37) — A WF's first spawn on a worker ran before Syncthing

- **What happened:** with the publish fixed (A-10), the deploy reached the spawn before the package
  had synced to worker1 (~11 s measured) → `RUNTIME_NOT_AVAILABLE`, a single attempt.
- **Fix:** wf-rules retries (backoff 1→4 s, up to 20 s) while the orchestrator answers
  `RUNTIME_NOT_AVAILABLE` / `RUNTIME_NOT_PRESENT` / `BASE_RUNTIME_NOT_AVAILABLE`.

### A-13 ✅ RESOLVED (0.1.37) — The SDK masked the vault's verdict

- **What happened:** `VaultClient::get`/`list` parsed the response as a success before looking at
  the `status`; a `KEY_NOT_FOUND` arrived as `json error: missing field key`, and the
  `KEY_NOT_FOUND` branches of io-slack and io-wapp never ran.
- **Fix:** read the verdict from the raw JSON before parsing.

### A-14 ✅ RESOLVED (0.1.37) — OPA's SHM was `0666`

- **What happened:** SY.opa.rules created `/dev/shm/jsr-opa-<hive>` `rw-rw-rw-` (and set it back to
  that if it existed): any process could read the policy or write one for the router. Its Rust
  peers use `0600`. Policy files `0644` in `0755` dirs.
- **Fix:** region `0600` (corrected at startup), files `0600`, dirs `0700`.

### A-15 ✅ RESOLVED (0.1.38–0.1.40) — wf-rules deleted packages from a worker's `dist/` mirror

- **What happened:** after apply/rollback/delete, wf-rules purged versions and rewrote the manifest
  in its **local** `dist/runtimes`. On a worker that is a receive-only Syncthing mirror: the local
  deletion is never restored. worker1 was left with 15 local changes (13 deletions); the
  `wf.w1probe` package disappeared from the mirror and the WF node went into a crash-loop because
  `flow/definition.json` was missing. On top of that, the orchestrator accepted the spawn with the
  manifest already synced but without the package's files.
- **Fix:** on a hive that is not the motherbee, the purge asks the admin for
  `remove_runtime_version` (it is deleted at the origin and the sync carries that over); the
  orchestrator answers `RUNTIME_NOT_PRESENT` until the package's `package.json` is on the hive. In
  0.1.38 the request went out without a `target` (`INVALID_REQUEST: missing target`); 0.1.39 aims
  it at the motherbee; 0.1.40 purges leaving the current version for last and takes
  `RUNTIME_NOT_FOUND`/`RUNTIME_VERSION_NOT_FOUND` as done.
- **Validated (0.1.40):** create → re-apply → delete a WF on worker1: node HEALTHY on 0.0.1 and
  0.0.2 (18 s each), delete OK, motherbee with no versions and no manifest entry, worker1's mirror
  with 0 local changes.
- **One-off repair:** worker1's mirror was restored with Syncthing's `POST /rest/db/revert`
  (15 → 0 local changes); after that `WF.w1probe@worker1` became HEALTHY: the first WF running on
  a worker.

### A-22 [General/security] 🟡 PARTIAL (0.1.44 + 0.1.83: sockets 0600 with a peer-uid check) · binding a name to its process (P-1) PARKED with A-60 — A local node could send messages as another node

- **What happened:** the router looked up a frame's sender by the `routing.src` the frame declared,
  and from there stamped its L2 name, which is what the gate uses to decide protected actions.
  Confirmed live on 8.x: a test node on the motherbee sent `NODE_STATUS_GET` (protected) with
  SY.admin's UUID, and SY.config.routes answered the admin; with its own UUID, the gate dropped it.
  UUIDs travel in every message (a tenant node sees its orchestrator's in every `node_config`
  signal), so any local node could act as the motherbee's admin or as its hive's orchestrator,
  which under rule 3 can perform every protected action on any hive.
- **Fix:** the router drops the frame if `routing.src` is not the UUID that socket used in its
  HELLO, which is what `02-protocolo.md` already required ("the router stamps `src_l2_name` from
  the authenticated session"). The Rust, Go and IO senders were reviewed: all use their own UUID.
- **Validated:** the same probe is now dropped (`routing.src is not the sending node`); no
  legitimate traffic dropped on the 4 hives.
- **What stays open (DTAP panel 2026-10-01, P-1):** 0.1.44 closed the case of a node using
  another's UUID inside its own connection, but the **name** is declared by the process itself in
  the HELLO and the router accepts it: socket `0666`, no reserved names, no rejection of duplicates
  or of a foreign `@hive`. Any local process can connect with a new UUID as `SY.admin` and write
  the global policy, or as `SY.orchestrator` on a spoke.
- **The operator's decision (2026-10-01): DEFER** the security package (today it costs more than it
  yields). It includes: authenticating the name in the HELLO (P-1), the direction of rule 3
  (A-20 / D-2), signing the published manifest (P-8), `get_policy` exposing the rego (P-16), and
  encrypting the synced copy (already deferred in the design).

### A-23 ✅ RESOLVED (0.1.48) · composing several solutions' policies and a registered owner PARKED — The architect modeled OPA per hive; there is one single global policy

- **Status (2026-10-19 review):** what stayed "to review" is still not built: the last solution's policy wins and its owner is deduced, not stored. That was the operator's decision of 2026-10-01, not a defect.

- **What happens (seen in code, 2026-10-01):** a solution declares `opa_deployments` per hive
  (`hive`, `policy_id`, `rego_source`) and the plan emits one `opa_compile_apply` for each.
  - If the hive is not the motherbee, the admin rejects it with 400.
  - If it is the motherbee, the policy becomes the one for every hive. Two deployments overwrite
    each other.
  - The snapshot looks for `policy_id`/`rego_hash` in the state, which does not have them: the
    reconciler never sees the policy that is running and re-applies it on every run.
  - Removing it is blocked (there is no remove action).
  - The plan compiler (AI) receives only the `delta_report`: nobody passes it the declared rego.
- **The operator's decision (2026-10-01):**
  1. The solution declares one single global policy, with no hive.
  2. It is compared with the rego running on the motherbee; if they are the same, nothing is done.
  3. It is applied with one single `opa_compile_apply`; a `pending` hive is not a failure.
  4. When the owning solution stops declaring it, `opa_clear`.

  If two solutions declare OPA, **the last one wins**. The "owner" is the solution whose policy is
  running now; it is recognized by comparing the rego with the stored manifests. A solution only
  does a clear if the running policy is its own, and the plan says whose policy it replaces.
- **Fix (0.1.48, `1486807`):** `desired_state.opa` (one policy, no hive); the snapshot reads the
  motherbee's policy with `opa_get_policy`, and the owner from the stored manifests; one single
  `opa_compile_apply`, and `OPA_REMOVE` = `opa_clear`; the plan compiler receives the rego, and the
  plan's OPA steps are pinned to it; the confirmation says whose policy is being replaced.
- **Validated:** tests (177 of the architect's, including the real response from the 8.x admin).
  The full pipeline was not run live: it needs the architect's AI (A-26).
- **Pending review (a deliberate simplification, like the security package):** composing the rules
  of several solutions into one single policy, instead of the last one winning; and an owner
  registered together with the policy, instead of deducing it by comparing the rego.

### A-24 ✅ RESOLVED (0.1.49 + 0.1.81, the WF half) — The architect's snapshot asked an admin that does not exist for each hive's state

- **The WF half (0.1.81, `4e9f91e`):** the snapshot reads wf-rules' real answer (`workflows`,
  `current_hash` reproduced exactly); a DMZ hive is not asked for runtimes or workflows; workflow
  ownership comes from executed manifests (external by default) and removal reaches every hive
  the solution used; the plan compiler gets the declared definitions and the steps are pinned.
  The review found more in the same reconciler: see A-77.

- **What happens (seen in code, 2026-10-01; not reproduced live):** `build_actual_state_snapshot`
  sends each read to `SY.admin@<hive>`. Only the motherbee has an admin (on worker1, `sy-admin` is
  inactive), so in a solution with more than one hive those reads fail, the snapshot is left
  incomplete and the reconciler blocks the whole run.
- **Fix (0.1.49):** the snapshot and the plan compiler's `query_hive` query ask
  `SY.admin@motherbee`, with the hive as the target (the admin is, and will be, only on the
  motherbee; operator, 2026-10-01). A source-code test prevents naming a per-hive admin again.
- **Open after 0.1.49, both closed in 0.1.81 (the WF half above):**
  - The plan compiler does not receive the definition of the declared workflows
    (`wf_deployments`) either: the same gap A-23 had with the rego.
  - On ingress and egress, `list_runtimes` answers `RUNTIME_MANIFEST_MISSING` and there is no
    SY.wf-rules (that is how it is, by role). A solution that names a DMZ hive still ends up with
    an incomplete snapshot, and that blocks the run. On worker1 the five reads come back ok.

### A-25 ✅ RESOLVED (0.1.49, the handbook found on the motherbee) — The `.deb` did not install the architect's handbook (`install.sh` did)

- **Status (2026-10-19 review):** `packaging/build-deb.sh` installs `docs/archi/handbook_fluxbee.md`; the 0.1.49 gate found it on the motherbee.

- **What happens (2026-10-01):** the architect appends `/etc/fluxbee/handbook_fluxbee.md` to its
  prompts if it exists. `install.sh` copies it, `build-deb.sh` does not: it is not on the 8.x
  motherbee, so the PROD architect works without a handbook. The same class of divergence as A-6.
- **Fix (0.1.49):** `build-deb.sh` installs it in the same place (a static document: it is
  replaced on every upgrade, it is not a conffile).

### A-26 ✅ RESOLVED (0.1.49, validated) — The architect read its secrets from the vault only once, at startup

- **Status (2026-10-19 review):** `resolve_resource_awaiting_vault` waits for the vault at boot (`VAULT_BOOT_WAIT`); re-reads after a vault notice or a `CONFIG_SET` are still one-shot, as in the sibling nodes.

- **What was seen (2026-10-01, when installing 0.1.47 and 0.1.48):** at startup, the architect
  could not read its AI key or the URL of its message database (`vault unreachable`): during an
  upgrade the whole hive restarts, and the vault was not answering yet. It stayed listening with
  `ai_configured=false` and `messages_db_configured=false`.
- **Correction of the diagnosis:** the vault's startup notice (`VAULT_SECRET_CHANGED`, one per
  secret) **did** arrive: the message database reconnected 0.4 s later. The AI stays off because
  the 8.x vault has no AI key (it has 3 secrets: edge, TLS and postgres). Even so, that notice goes
  out only once, and the SDK documents that a node that does not hear it in time stays degraded
  (that is what happened to SY.storage).
- **Fix:** like SY.storage and SY.identity, the two startup reads retry until the vault answers
  (`resolve_resource_awaiting_vault`, up to `VAULT_BOOT_WAIT`, both at once). Re-reads on a notice
  or on `CONFIG_SET` are still one-shot.

### A-27 ✅ RESOLVED (`802faf5`) — The parity guard of the admin's catalog (CI) had already been red before

- **What happens (2026-10-01):** `scripts/admin_action_catalog_parity_check.sh` runs on GitHub
  Actions on every push that touches `sy_admin.rs`, and it fails: its expressions do not recognize
  how 20 actions that do have an HTTP route, or are internal-only, are dispatched today:
  `get_runtime`, `list_cloud_actions`, `publish_artifact`, `publish_cloud_endpoint`,
  `unpublish_artifact`, the 8 `timer_*` and the 8 `wf_rules_*`. Every push in the OPA stretch that
  touched the admin set it off red.
- **Fix:** the script also follows `handle_admin_query_with_payload`, the timer RPCs, the generic
  internal dispatch, the wf-rules handlers and the cloud endpoint one, and it declares the
  exceptions on purpose: actions that only arrive over the mesh (`publish_artifact`,
  `list_cloud_actions`) and actions with a dedicated arm (`externalize`, `unexternalize`,
  `list_externalized`); both lists fail if they go stale. In CI it passes since `802faf5`.

### A-28 ✅ RESOLVED (`3eeadf2`) — Three compiled Go binaries were versioned in git

- `go/sy-opa-rules/sy-opa-rules`, `go/sy-timer/sy-timer` and `go/nodes/wf/wf-generic/wf-generic`.
  They dirtied every `git status` after a local build and were one `git add -A` away from a commit.
- **Fix:** they leave the index (the files stay on disk) and `.gitignore` ignores the four that the
  build writes (also `sy-wf-rules`). Approved by the operator in the small-topics batch
  (2026-10-02).

### A-29 ✅ RESOLVED (0.1.51) — sy-timer and wf-generic: SQLite waited for the lock on only one connection

- **What happened:** the first Go CI showed a flaky sy-timer test (20 concurrent `TIMER_SCHEDULE`
  with the same `client_ref`): it failed 10 out of 15 times on fb-build with
  `TIMER_STORAGE_ERROR: database is locked (SQLITE_BUSY)`. The cause was in the product:
  `busy_timeout` (and `synchronous` in wf-generic) is a **per-connection** pragma, and it was
  applied with `db.Exec` on the `database/sql` pool. Only one connection waited for the lock; the
  others failed instantly. In sy-timer the scheduler writes while requests are being served, so a
  scheduling or a firing could fail intermittently in production.
- **Fix:** the pragmas go in the DSN (`path?_pragma=...`), which modernc.org/sqlite applies to
  every connection it opens. New tests check three connections at once; the sy-timer suite passed
  15 times in a row with `-race`.

### A-30 ✅ RESOLVED (0.1.51) — The admin did not decode the query's `%xx`

- **What happened (while validating 0.1.50):** a `DELETE /hives/{h}/taps` with `@` sent as `%40`
  (the way Python's `urlencode` sends it) answered `NOT_FOUND`: the admin compared the value
  without decoding it.
- **Fix:** query keys and values are decoded; `+` stays literal (raw `+` signs are more common
  here, like `Etc/GMT+3`) and a stray `%` is kept.

### A-31 ✅ RESOLVED (0.1.57) — The services write ANSI color codes into the journal

- **What happens:** levels come out as `\x1b[33m WARN\x1b[0m` in `journalctl`. A `grep ' WARN '`
  finds nothing, and while validating 0.1.52 an error count gave a false 0 until the codes were
  stripped with `sed 's/\x1b\[[0-9;]*m//g'`.
- **Why:** each binary builds its own `tracing_subscriber` (36 files), and none turns color off
  when the output is not a terminal.
- **Proposed fix:** a tracing init in the SDK with `with_ansi` only if the output is a terminal,
  used by every binary. It is a mechanical change in 36 files: it goes with the mass-formatting
  pass (item 8 of the batch), at the operator's decision.
- **Fix:**
  - `fluxbee_sdk::logging::fmt()` and `ansi()` only add color when stdout is a terminal.
  - The 36 tracing startups go through there: the core binaries, the IO/AI/gov/test nodes, the
    diags and the SY.edge layer.
  - The mass formatting stays separate; it is the operator's decision.
- **Verified:** a diag with its output to a pipe went from 48 ESC sequences to 0, and its levels
  come out as ` WARN `.
- **Validated live (0.1.57, 2026-10-06):** zero ANSI codes in the new processes on the 4 hosts.
  Before the upgrade there were hundreds per service on the motherbee (rt-gateway: 564).


### A-32 ✅ RESOLVED (0.1.53) — Personal data in logs and responses (frontdesk and identity)

- **What happened:**
  - SY.frontdesk.gov wrote to the log, at INFO level, the `ILK_REGISTER` payload (name, email,
    phone, company), the company name the person typed (`TNT_CREATE`), the first 240 characters of
    each message and the sender's id (on WhatsApp, their phone number).
  - SY.identity's errors traveled with their free text. For a duplicate email that text carries the
    Postgres DETAIL with the email.
  - SY.identity logged the `ILK_PROVISION` payload (the person's address) and returned the Postgres
    DETAIL to the caller.
- **Fix:**
  - For a registration, it logs `ilk_id`, `tenant_id` and which fields came in, never their values.
  - For an identity error, only its code.
  - SY.identity logs `ich_id` and `channel_type`, and answers with the Postgres message without the
    DETAIL.
  - Three tests capture the logs of the frontdesk's three entry points; with the old fields they
    fail.

### A-33 ✅ RESOLVED (0.1.53) — SY.frontdesk.gov: definitive rejections from identity reported as retryable

- **What happened:** errors were classified by searching their text. TENANT_PENDING, ILK_DELETED,
  SYSTEM_ILK_PROTECTED, DUPLICATE_* and others ended up as `IDENTITY_UNAVAILABLE` with
  `retryable: true`.
  - **Also:** the fallback target's response was not checked. A `status:"error"` came out as
    `registered: true`, a false REGISTERED. It could only happen with
    `GOV_IDENTITY_FALLBACK_TARGET` configured.
- **Fix:**
  - The bridge returns the SDK's `IdentityError`, just like the orchestrator, and SY.identity's
    code is kept as is.
  - Only transient errors are retryable: no response, NOT_PRIMARY, DB_NOT_READY and
    DB_WRITE_FAILED.
  - Both responses go through the same status check.

### A-34 ✅ RESOLVED (0.1.53) — Cognition: the cold start never ran, and it rebuilt wrongly

- **What happened (seen in PROD and in a code review):**
  - **The rebuild never ran.** The Postgres credential arrives from the vault after startup, and
    the rebuild was only attempted at startup.
  - **It rebuilt with other keys.** It indexed by entity id while the live path indexes by label.
    After a restart, the next turn duplicated contexts, reasons and memories with the same id.
  - **The live state was never pruned.**
  - **A scope was never cut:** a sustained topic change renamed it instead of cutting it.
  - **`storage.enabled: true` was hardcoded** in CONFIG_GET.
  - **The deterministic text of the episodes was never sent.**
- **Fix:**
  - **Rebuild:**
    - It waits for the vault at startup (the A-26 pattern, up to 60 s).
    - If it still does not load, the first turn retries it, only once and inside the turn task.
    - It builds the state with the same keys as the live path, and it only installs over an empty
      state.
    - Episodes get their real instance back.
  - **Pruning:** the live state is trimmed to the same set that goes into the `jsr-memory` SHM.
  - **Scopes:** a sustained topic change cuts the scope; one that still binds is drift, and it
    relabels the scope. The dominant one is chosen deterministically.
  - **CONFIG_GET:** reports the real `storage.db_configured`.
  - **Episodes:** their text is the AI summarizer's.
  - New tests for each case; each one was verified to fail without its fix.

### A-35 ✅ RESOLVED (0.1.53) — ai.generic asked for the AI key by name

- **What happened:** since July (`ai-engine-selection.md` D2/D3), an `AI.*` node had to name its
  key (`behavior.vault_key`). Without that key, its config was rejected.
- **The operator's decision (2026-10-02, D4):** *"La clave de IA la debería tomar del vault con la
  clave general (sin asignación de nombre)."* ("It should take the AI key from the vault, with the
  general key (no name assignment).")
- **Fix:**
  - The provider is the hive's (`hive.yaml` `ai`), and the key is that provider's general key in
    SY.vault: the one of the node's tenant and, if there is none, the root's. It is the same thing
    the SY.* nodes and the frontdesk do.
  - `behavior.vault_key` is rejected with a message that names it.

### A-36 ✅ RESOLVED (0.1.53) — The base install brought up a test instance, and Archi knew of runtimes that do not exist

- **What happened:**
  - `AI.chat@motherbee`, the example node from March, started with every install as if it were
    base.
  - The cookbook seeds told Archi that `config_only` packages should use
    `runtime_base: "AI.common"`, which does not exist.
  - The handbook that Archi loads said that `ai.chat` is a runtime.
  - An architect example launched a node with no runtime, which would have been derived to
    `ai.chat`.
  - An architect help text had double-encoded UTF-8 ("catÃ¡logo").
- **Fix:**
  - `ai.generic` stays baked in, with no startup instance. Instances are created for something
    concrete; test ones are ephemeral (operator, 2026-10-02).
  - Everything points to `ai.generic`: admin, architect, handbook, seeds, docs and scripts.
  - `install-ia.sh` and `ai-nodectl.sh` (March's per-node systemd unit model) were deleted, along
    with the deprecated flags of the publishing scripts.
- **PROD:** the `AI.chat@motherbee` instance that already exists stays in FAILED_CONFIG, because its
  config has `vault_key`. It gets deleted or reconfigured when the operator asks.

### A-37 ✅ RESOLVED (0.1.54 + 0.1.55) — SY.frontdesk.gov: the tenant path and the merge by email

**The operator's decisions (2026-10-02):**

- On tenants: *"no da para crear un tenant por un mensaje que llegue de un humano que no esté
  registrado; ahora el tenant se crea desde el cloud y tiene un proceso"* ("it makes no sense to
  create a tenant because of a message from a human who is not registered; now the tenant is
  created from the cloud and it has a process").
- On the merge: *"haría merge sobre datos que no están… si no coinciden los datos y mandan un update
  queda lo último"* ("I would merge over data that is missing… if the data doesn't match and they
  send an update, the latest stays").

**The case's tenant:**

- It is the temporary ILK's, which SY.identity registered when it provisioned it under the IO
  node's tenant. The frontdesk reads it from identity's SHM.
- A handoff's `tenant_id` only has to match; if it does not, `INVALID_REQUEST`.
- With no readable tenant it answers `TENANT_UNRESOLVED` and does not register.
- The frontdesk no longer creates tenants: SY.identity took away its permission for `TNT_CREATE`,
  `TNT_UPDATE` and `TNT_SET_SPONSOR`.
- Neither the person nor the LLM chooses the tenant.

**The tenant's state:** `ILK_REGISTER` on a suspended tenant is now rejected (`TENANT_SUSPENDED`),
just like on a pending one.

**Merge by email**, inside SY.identity's `ILK_REGISTER`:

- **When:** the email already belongs to another active human of the same tenant, and the one
  registering is a temporary of that tenant.
- **What it does:** the channels are moved (before, they were copied: a bug) and the temporary
  stays as an alias.
- **The data:** only empty fields are filled in; a value is never overwritten.
- **The response:** `merged:true`; the frontdesk answers `MERGED`.
- **The latest wins:** an `ILK_REGISTER` from the ILK itself, for example a repeated
  `register_human`, replaces the identification.
- **io.cloud:** `register_human` reports the final ILK, `merged` and the source temporary.

**Also:**

- Email lookups are per tenant, like the database index.
- A failed handoff keeps the case's tenant (G7).
- The tests of logs without personal data were flaky; they became stable.

**Closed in 0.1.55:**

- **Root tenant:** nobody registers there. The operator's decision (2026-10-02): *"nadie se puede
  registrar en tenant raíz"* ("nobody can register in the root tenant").
  - SY.identity rejects a person's `ILK_REGISTER` in the root tenant with
    `TENANT_ROOT_NOT_REGISTRABLE`, no matter who it comes from. Node ILKs (`agent`) are still
    registered there.
  - The frontdesk cuts it off earlier, with no LLM and no SY.identity, and answers
    `TENANT_NOT_REGISTRABLE`. The person stays temporary.
  - IO nodes launched from the root tenant remain as A-43.
- **The frontdesk gate of io.slack and io.wapp** (`io-common`) built the handoff as a `data`
  message, but the frontdesk only takes handoffs from `user` messages. The handoff never reached
  its deterministic path. Now it goes as `user`, like io.cloud's.
- **The identity lab e2e tests:** four diags created their tenants with the frontdesk's name and
  got `UNAUTHORIZED_REGISTRAR`. They were `identity_merge`, `identity_negative`,
  `identity_provision_complete` and `identity_replica_sync`.
  - Now the script creates the tenants through SY.admin, passes them to the diag and purges them at
    the end. The shared parts went into `scripts/lib/identity_e2e.sh`.
  - The replica one also checks through SY.admin that the tenant and the ILK reach the replica.
  - The `gov_frontdesk_identity_e2e.sh` wrapper no longer brings old hives (`sandbox`,
    `worker-220`).
- **The diags impersonate the frontdesk:** they connect with its name and with the UUID the
  frontdesk persisted.
  - While they run, the router delivers the frontdesk's traffic to them.
  - When they leave, the real frontdesk stays connected but with no route until it reconnects
    (A-44).
  - The scripts now stop `sy-frontdesk-gov` while the diag runs, and start it again afterwards.

**Still open:**

- 🔴 **Security:** there is no proof that the email belongs to whoever writes it (A-42, deferred).
- **G6 and G8:** the §12 counters and the conversational retries.

### A-38 ✅ RESOLVED (0.1.54) — Cognition: design questions

The operator's answer (2026-10-02): *"sí a todo"* ("yes to everything").

- **Thresholds:** `context_open`/`reason_open` are renamed after what they do,
  `config.thresholds.context_close` / `reason_close`, with a default of 0.25 (the same effect as
  before). CONFIG_SET rejects the old keys by name, and any other unknown key. The persisted PROD
  config with the old ones starts with the defaults, which give the same behavior, and leaves a
  warning.
- **Full region:** the recent wins. The current turn's thread is never dropped, and after it comes
  the most recently seen.
- **Scope cut:** it stays at the 6th sustained divergent message (5th if the scope has one turn,
  7th if it is heavily reinforced), written in the spec §8.3 and pinned by a test.
- **Still not assembled** (a separate chapter): cross-thread memory, LanceDB, getting what is on
  the workers to reach the motherbee, and readers of the tables.

### A-39 ✅ RESOLVED (0.1.54, to be validated in the deploy) — On every release, the motherbee's managed nodes went down for 20 to 80 s (`203/EXEC`)

- **What happens (seen while validating 0.1.53, and pre-existing: AI.chat racked up 104 failures
  in 30 days):**
  1. When unpacking the new version, dpkg deletes the old package's files, and the
     `dist/runtimes/<rt>/<old version>` directories came in that package.
  2. During the update, the managed nodes' units restart with the old `ExecStart`, which points to
     a directory that no longer exists: a `status=203/EXEC` loop.
  3. Only when the new orchestrator starts does it see that `current` moved, and it repoints them.
- **Evidence 0.1.53:**
  - AI.chat, IO.api, IO.blob, IO.cloud and IO.slack.default: 5 failures between 20:32:10 and
    20:32:31. Repointed at 20:32:34 ("runtime 'current' pointer moved; rebinding node").
  - IO.wapp.default: 17 failures, until 20:33:33; the reconcile had skipped it as "visible".
- **Impact:** IO.api and IO.cloud, the public-facing ones, are left without service on every
  upgrade.
- **Who restarted them:** `needrestart`, Ubuntu's apt hook. After dpkg, it restarts the services
  that run deleted binaries, and their old `ExecStart` points to the directory that no longer
  exists.
- **The other side of the bug:** the startup reconcile skipped the nodes that were running. One
  that nobody restarted stayed on the old binary indefinitely; it repaired them only because
  `needrestart` had knocked them down.
- **Fix (`779df4c`):**
  - The startup reconcile, which runs after every upgrade and every core-update, relaunches on the
    new version a node that follows `current` when the pointer moved: a controlled restart. The
    60 s loop does not do it, so a hot publish behaves as before.
  - The package installs `/etc/needrestart/conf.d/fluxbee.conf`, which leaves the
    `fluxbee-node-*` units to the orchestrator (verified against `needrestart`'s code).


### A-40 ✅ RESOLVED (0.1.55) — IO.slack.default did not have its ILK in identity (PROD)

- **What happened:** its config pointed to `ilk:9cd1bb44…`, but SY.identity answered `NOT_FOUND`,
  so every `CONFIG_SET` failed when registering its own channel, and the node stayed in
  FAILED_CONFIG. It was probably deleted by the first run of the factory reset on 28/09.
- **The operator's decision (2026-10-02):** *"io.slack es parte del sistema pero funciona
  instanciándose, no corre solo… corre con su propio tenant que lo lanza"* ("io.slack is part of
  the system, but it works by being instantiated, it doesn't run on its own… it runs with its own
  tenant, which launches it").
- **How it was closed:**
  - `io.slack` stays baked in, with no startup instance in `base-nodes.json`;
  - each binding is launched by its tenant;
  - the PROD instance, which ran in the root tenant, was deleted with a purge. Its config was saved
    outside the repo, to recreate the binding from a tenant.

### A-41 ✅ RESOLVED (0.1.54) — Purging a node left its UUID persisted

- **What happened:** `purge_instance` deleted the directory, the ILK and the mapping, but not
  `state/nodes/<name>.uuid`. In PROD there were 12, from nodes already deleted: AI.chat, the
  factory reset test ones and old Slack bindings. A new node with the same name inherited the old
  UUID.
- **Fix (`7c9f781`):** both purge paths delete it and report it (`uuid_file_removed`). The 12 in
  PROD were deleted.


### A-42 ✅ VALIDATED ON 8.x (0.1.83, 2026-10-22) — The merge by email does not verify that the email belongs to whoever writes it

- **Resolved (0.1.83, design §7, rule (b)):** only an email the channel vouches for (the IO
  node's handoff, Cloud, io.api's integrator) goes in `identification.email` and can merge; a
  typed one is `claimed_email`, never indexed or merged; the frontdesk takes handoffs only from
  IO nodes with the `frontdesk_handoff` mark. Live: Cloud's `register_human` registered a person
  with the vouched email. An explicit merge by the operator is not offered yet.

- **What happens (since 0.1.54, A-37):** if someone writes the email of another person in the same
  tenant, SY.identity does the merge, and their channel becomes associated with that person's ILK.
  They are routed as if they were that person, and they can fill in the fields that are empty.
- **The operator's decision (2026-10-02):** *"levántalo y dejamos para después"* ("raise it and
  we'll leave it for later"). It goes with the security package (A-22).
- **Planned fix:** a proof of ownership before the merge, for example a one-time code sent to the
  email.

### A-43 ✅ RESOLVED (0.1.56) — IO nodes launched from the root tenant

- **What happened:** the base install brought up channel IO nodes in the root tenant
  (`IO.api@motherbee`, `IO.wapp.default@motherbee`). A person who arrived through one of them
  stayed temporary, because since 0.1.55 nobody registers in the root tenant.
- **The operator's decision (2026-10-02):** *"algunos nodos IO tienen que correr con el tenant raíz,
  ej io.cloud io.blob los demás no"* ("some IO nodes have to run with the root tenant, e.g.
  io.cloud io.blob, the rest don't"); *"todos quedan para poder usarlos pero no corriendo default.
  Solo io.cloud y io.blob por ahora"* ("they all stay so they can be used, but not running by
  default. Only io.cloud and io.blob for now").
- **Fix (`4a9249f`):**
  - The SDK defines `ROOT_TENANT_IO_RUNTIMES` (io.cloud, io.blob).
  - The orchestrator rejects any other IO node in the root tenant with `TENANT_ROOT_NOT_ALLOWED`
    in the three places where it launches a node:
    - `run_node`, which covers admin, Cloud, Archi, firstboot and the forwards;
    - start/restart;
    - the relaunch at startup. If a node is already running, it leaves it running.
  - io.api and io.wapp stay baked in, with no base instance; firstboot only brings up IO.cloud and
    IO.blob.
  - Archi never picks the root tenant for an IO node.
  - Docs: `715143a`.
- **PROD:** the operator purged `IO.api@motherbee` and `IO.wapp.default@motherbee` before the
  0.1.56 deploy; from the session, the permissions classifier blocked it. In 30 days they had not
  been used, and their configs were saved outside the repo.
- **Validated live (0.1.56):** a `run_node` of io.api in the root tenant gives
  `TENANT_ROOT_NOT_ALLOWED` and leaves nothing created.
- **io.web (operator, 2026-10-03):** *"io.web también va en el core, la idea es que los IO que se
  requieren como recursos compartidos y no porque una solución lo requiera, corren en el core"*
  ("io.web also goes in the core; the idea is that the IO nodes that are required as shared
  resources, and not because a solution requires them, run in the core").
  - It went into `ROOT_TENANT_IO_RUNTIMES`, even though it does not exist yet: there is only a
    spec.
  - Its spec declares it in `system_nodes`; when it is built, it goes like IO.blob and IO.cloud
    (managed runtime, `boot=true`).
- **Noelia's LinkedHelper runbook** launches the node in the root tenant. The operator is looking
  into it.

### A-44 ✅ RESOLVED (0.1.55 + 0.1.56) — Two connections with the same UUID: the second one takes the route

Seen while fixing the A-37 e2e tests, reading `src/router/mod.rs` and `src/shm/mod.rs`.

- **What happened in the router:**
  - A HELLO with a UUID that already had a live connection replaced its entry in the node
    table.
  - When the new one closed, the old one stayed connected but with no route.
- **The operator's decision (2026-10-02):** *"rechazar la nueva, no veo porque deba aparecer una en el
  esquema normal"* ("reject the new one, I don't see why one should show up in the normal
  scheme").
- **What checking it first showed:**
  - **A pre-existing bug:** if a connection ended with an error (a read error, an invalid
    OPA_RELOAD, a forward to peers, `handle_message`), the router did not clean it up. The entry
    stayed in the table and the socket open: a live node stayed connected with nobody reading its
    messages. With "rechazar la nueva" ("reject the new one") that turned into a permanent block.
  - **Transient duplicates in the normal scheme:**
    - every Go node sends two HELLOs at startup;
    - a node can reconnect before the router processes the close of the previous connection.
- **Fix (`068014d`, 0.1.56):**
  - Every connection ends by cleaning up its entry; a message that cannot be processed is logged
    and skipped.
  - A HELLO with a UUID in use waits up to 1 s for the previous connection to go away; if it is
    still alive, it is rejected with no ANNOUNCE and with a WARN. The check and the insert run
    under the same lock.
  - The SDK logs the rejected handshake.
  - The negative identity diag uses one connection per name.
  - The identity e2e tests run with `timeout`: their retries waited for an error that no binary
    prints.
  - Three of the four new tests fail with the previous code.
- **The router's SHM (0.1.55):**
  - `register_node` duplicated a node that registered again.
  - The reader lost the last node when there was a gap.
- **Noted for later:**
  - the Go SDK's double HELLO: the wait absorbs it, but the SDK could reuse the first
    connection;
  - the Rust SDK has no handshake timeout: a router that accepts and does not answer leaves it
    hanging.

### A-45 ✅ RESOLVED AND MEASURED (0.1.57) — Every motherbee upgrade shuts down the whole core before unpacking

- **What happens (measured in the 0.1.55 deploy):**
  - The package's `prerm` (`packaging/deb-prerm`) stops every core service before unpacking, also
    on an upgrade.
  - The `postinst` starts only the orchestrator, which then brings up the rest.
- **Measured outage:** rt-gateway 70 s, sy-admin 76 s, sy-vault 78 s, sy-storage 84 s, sy-identity
  86 s.
- **Impact:** meanwhile the motherbee has no router, identity or admin.
  - IO.api and IO.cloud do not serve: the public sees 502.
  - The identity replicas retry every second (~2 warnings per second).
  - Nobody can be provisioned.
- **Why it is like this:** the `prerm` says "stop + disable services before removal", but it runs
  on an upgrade anyway.
- **Proposal:** have the `prerm` stop the services only on uninstall, and on an upgrade have the
  `postinst` restart them after unpacking. Linux keeps the old binary running until the restart,
  so the outage would shrink to what each restart takes.
- **To decide:** the order of the restarts, and whether the `postinst` or the orchestrator does
  them.
- **Operator (2026-10-02):** *"no me parece mal"* ("that seems fine to me").
- **Fix (`1a10cab`):**
  - On an upgrade the `prerm` stops only the ones that write the runtimes manifest:
    sy-orchestrator, sy-admin and sy-wf-rules. The `postinst` counts on that when it registers the
    runtimes.
  - The rest keep serving with the old binaries.
  - The new orchestrator's boot restarts each service that runs a replaced binary, in its startup
    order: rt-gateway first and SY.vault last. That way, ordering the restarts is handled by the
    orchestrator, not by a script.
  - The result is recorded in `/versions` as `core.last_update`, with `via: package`.
  - needrestart also leaves rt-gateway and sy-* to the orchestrator.
  - dpkg runs the old package's `prerm`: the upgrade to 0.1.56 still stops everything, and the
    improvement shows from 0.1.57 on.
- **Measured on the upgrade to 0.1.57 (2026-10-06):**

  | Service | 0.1.55 | 0.1.57 |
  |---|---|---|
  | rt-gateway | 70 s | 0 s (restart) |
  | sy-vault | 78 s | 1 s |
  | sy-storage | 84 s | 5 s |
  | sy-identity | 86 s | 5 s |
  | rest | — | 0 s |

  Only the ones that write the manifest stop: the orchestrator was down for 44 s.

### A-46 ✅ RESOLVED (0.1.57: the ingress no longer reaches Archi) · stage 4 minimal LIVE (0.1.83: SSH on the DMZ hosts) · the full stage-4 firewall and Archi's API auth PARKED — Archi's HTTP API has no authentication and is reachable from the ingress

- **Status (2026-10-19 review):** stage 1 (loopback binds) shipped in 0.1.57 and stages 2-3 closed with A-47; the stage-4 firewall and authentication for Archi's API are parked in the posture table (2026-10-07).

- **What happens (verified on 2026-10-03, in the node survey):**
  - SY.architect's axum router (`sy_architect.rs`, `Router::new()`) has no authentication layer at
    all. Open: `/api/chat`, `/api/executor/plan`, `/api/package/publish`,
    `/api/software/upload|publish`, `/api/agent-assets`, `/api/messages`, `/api/sessions`.
  - In PROD it listens on `0.0.0.0:3000`. The code's default is `127.0.0.1:3000`, but the packaged
    `hive.yaml.example` uses `0.0.0.0`.
  - The motherbee's firewall is inactive (`ufw status`: inactive).
  - From `fb-ingress`, which has a public interface (`eth1`), port `10.10.10.10:3000` is open. The
    admin's 8080 is not.
- **Impact:** whoever compromises the ingress can ask Archi's executor for plans and publish
  packages or software. Archi's identity can also write to the vault. It is a path from the DMZ to
  the control plane.
- **Options:**
  - Immediate: have Archi listen on `127.0.0.1` (and be used through a tunnel), or a firewall that
    lets 3000 through only from the operator's network.
  - Long-term: authentication on the API, with the security package (A-22).
- **Extension (2026-10-05): why 3000 was reachable.** It is not just Archi:
  - **No hive filters inbound traffic.** `ufw` is inactive on all 4. The input of the egress's nft
    table is at `policy accept` ("future hardening").
  - **The orchestrator's rules are inert.** `ensure_core_firewall_local` and the Syncthing one run
    `ufw allow` (9000, 9100, 22000, 21027, from any source) on a ufw that is off: they stay stored,
    with no effect.
    - It is a trap: if someone turns ufw on, SSH on every host and the ingress's 443 get blocked.
    - The core rules are never closed.
    - **Gone since 0.1.83** (docs/control-path-hardening-v1.md §6): no `ufw allow` remains in the
      orchestrator, which only reads `ufw status` to raise `host_firewall_active`.
  - **They listen on external interfaces:** the ingress's SSH and Syncthing, also on `eth1`
    (public), and the egress's on the office network.
  - **The admin also accepts any address** (`admin.listen` / `JSR_ADMIN_LISTEN`), and it has no
    HTTP auth either.
  - **Fixing the example is not enough.** The postinst only copies it if `hive.yaml` does not
    exist, so it does not fix PROD or DEV.
- **Decided with the operator (2026-10-05):**
  - the host posture (nft firewall per role, binds, Syncthing) is declared, applied and verified by
    the orchestrator;
  - Archi and the admin, on loopback only;
  - the only public thing is the ingress's 443;
  - from outside, Archi is used via Cloud → IO.cloud → IO.web, with a URL that Cloud hands out.
- **Design and staged plan:** `docs/host-posture-and-exposure-spec-v1.md`. Stage 1 = binds.
- **DTAP panel (2026-10-05):**
  - It failed revision 1 on all 4 lenses: 75 findings, none refuted (record in
    `docs/audits/2026-10-05-host-posture-dtap-panel.md`).
  - Revision 2 adds everything technical.
  - Operator decisions remain: O1–O4 before stages 1–5, O5–O8 before 6–7.
  - O1–O4 answered the same day (D16–D19 of revision 3):
    - SSH with nothing configured: open to everyone except the hives;
    - Archi on loopback only through config;
    - io.linkedhelper through the edge, at the end;
    - for the ingress, only what `add_hive` and the orchestrator handle.
  - Work order (D20): core → connection with Cloud → implementation nodes.
  - Panel round 2 (on Part A, revision 3): failed again, 84 findings. The cause was the per-hive
    IP filtering and everything around it.
  - Revision 4 simplifies: rules per port and interface according to the role, an automatic mode
    (observe for 24 h and then block), no configuration and no probes.
- **What stays open even if stages 1–5 are closed:**
  - the control mesh: any orchestrator, the ingress's included, can send SPAWN, KILL, etc.
    (A-20, A-22);
  - stage 7 reopens a path to Archi with a session: a compromised ingress can issue and use the
    session (O5).
- **Stage 1 (0.1.57, 2026-10-06), validated in PROD:**
  - Archi and the admin listen only on `127.0.0.1`. The motherbee's `hive.yaml` was migrated by
    itself during the upgrade (`fluxbee-migrate-config`).
  - From worker1, ingress, egress and fb-build, the motherbee's 3000 went from `open` to `refused`.
  - The orchestrator warns if any of them listens outside again. A positive control proved it: an
    alert at 33 s.
- **Status:**
  - Panel round 3 done.
  - The firewall (stages 4–5) and Syncthing (stages 2–3) are still in design: revision 6 and
    round 4 before building them (§8 of the doc).
  - Access from Cloud (stage 7) belongs to the Cloud phase.

### A-47 ✅ RESOLVED (0.1.59, validated on 8.x) — Syncthing used the public infrastructure, and worker1 depended on discovery

- **What happens (verified on 2026-10-05 on the 4 PROD hosts):**
  - `globalAnnounce`, `localAnnounce`, `relays`, `nat` and `crashReporting` are on, and the
    orchestrator never configures them.
  - Each hive announces its device ID and its IPs to the public discovery servers, can go through
    public relays, tries UPnP/NAT-PMP and sends crash reports.
  - Local discovery goes out on every interface: on the ingress through the public one, on the
    egress through the office network.
  - It listens on every interface.
- **The trap:** motherbee ↔ worker1 are set as `dynamic` on both sides, so today they sync only
  because discovery finds them. Ingress and egress have the motherbee's address fixed.
  - Cause: `add_hive` takes `syncthing_peer_address` as optional, and with no address it writes
    `dynamic` (`sy_orchestrator.rs:7340`).
  - Turning discovery off without fixing this cuts dist, blob and policy to worker1.
- **Decided:**
  - no public infrastructure at all;
  - listen only on the internal IP;
  - the side that dials always uses a fixed address (the spokes dial the motherbee, like WAN and
    identity);
  - `add_hive` does not create peers without an address;
  - the change goes in two steps: first the addresses, checking the connections, and only after
    that turn discovery off.
- **Design:** `docs/host-posture-and-exposure-spec-v1.md` §1.4 and §3.5 (stages 2 and 3).
  - The order is enforced in the code, so skipping versions cuts no one off (panel T-1).
  - The addresses are reconciled on every startup, not only at the join.
  - The orchestrator is the sole owner of the options.
- **Also found:** `vendor/syncthing/config.xml` already ships with the public parts off, but only
  the dev install path uses it. The `.deb` and the spokes run Syncthing's defaults.
- **Status:**
  - **Stage 2 (addresses and folders) validated in 0.1.58** (2026-10-06), after four adversarial
    code reviews (`docs/host-posture-and-exposure-spec-v1.md` §3.5, "as built"). The spokes end up
    with the motherbee at a fixed address, and the motherbee only accepts; it also removes each
    spoke from the folders its role must not have (A-50) and deletes the devices that no hive
    claims.
  - Before the deploy, `worker2` (VM 104) joined with 0.1.57 and was left powered off, for stage
    3's version-skipping test (T5-1).
  - **Stage 3 (Syncthing options: no discovery, relays, NAT, STUN or reports; listen only where
    needed) validated in 0.1.59** (2026-10-06), after two adversarial reviews. Each spoke switched
    on its own; the motherbee waited for `worker2`, which jumped from 0.1.57 to 0.1.59 and
    switched, and only then did the motherbee switch. Zero Syncthing UDP sockets on the 5 hives.

### A-48 ✅ VALIDATED ON 8.x (0.1.81, 2026-10-20) · leaf pinning PARKED — `remove_hive` revokes nothing: the removed hive is still a valid peer

- **Fixed (0.1.81, `4e9f91e`):** the registry (`/var/lib/fluxbee/hives/<id>/`) is the one list. The
  motherbee's router rejects a hive it does not list (`HIVE_NOT_REGISTERED`) and closes a removed
  hive's session within ~10 s; a rejected uplink backs off to 60 s. `remove_hive` deletes the
  hive's identity key (and restarts sy-identity when the box was not reached); a motherbee boot
  sweep deletes keys with no registry entry and refuses on an empty registry; the spoke's cleanup
  runs in its own transient unit, credentials first, and only from SY.orchestrator@motherbee.
  `wan.authorized_hives` is gone. Validated: the sweep deleted PROD's stale `worker2.key`; VM 104
  (worker2, which kept its certificate and key) was rejected on :9000 and :9100.
- **Parked:** leaf pinning (host-posture spec §3.8, superseded). A removed box that kept its leaf
  is admitted again if a hive with the same id is added later.

- **What happens (DTAP panel 2026-10-05, D-1, verified):**
  - `remove_hive_flow` (`sy_orchestrator.rs:10635`) deletes `hives/<id>`.
  - It unlinks the Syncthing device only if `info.yaml` has `syncthing_device_id`, and the egress's
    never has it (`:20989-20996`).
  - It does not delete the identity HMAC key, which stays in `/var/lib/fluxbee/identity/keys`
    (`src/mesh_hmac.rs:33`).
  - It does not revoke the mesh certificate: there is no CRL, and with `wan.authorized_hives` empty
    the router accepts any hive with a valid certificate from the CA (`src/router/mod.rs:2853`).
- **Impact:** a removed hive (or its disk) can reconnect to the mesh and to identity. Today the
  only brake would be the firewall, which does not exist.
- **Proposed fix:** have `remove_hive`:
  - revoke (or keep `authorized_hives` with the live hives);
  - delete the HMAC key;
  - unlink Syncthing in every role.

  It goes as a track separate from the posture design (§6 of the doc).
- **The same, on the motherbee's side (DTAP round 3, D3-3/P3-3):**
  - `REMOVE_HIVE_CLEANUP` and `ADD_HIVE_FINALIZE` sent to the motherbee were an off switch.
  - The policy lets any orchestrator send them, the ingress's included.
  - The first stops and disables the whole core; the second restarts the core and links to
    Syncthing a device chosen by whoever sends it.
- **Status:**
  - **0.1.57: the motherbee rejects both actions** (`FORBIDDEN`, with the origin in the log;
    covered by a unit test).
  - The rest of A-48 (certificate pinning, the router's view, HMAC keys, SSH host keys) is still
    in design: round 3 found that, as it was written, it cut off every existing hive (§8 of the
    doc).

### A-49 [General/security] 🟡 PARKED (waits for O9) — The apt repo is not signed (`[trusted=yes]`): whoever impersonates it is root on the motherbee

- **What happens (DTAP panel 2026-10-05, P-14, verified):**
  - The motherbee installs from `http://10.10.10.50:8900` with `[trusted=yes]`, because the repo
    is not signed (HANDBOOK:419, :434; `docs/packaging-and-build.md:157`).
  - The 10.10.10.0/24 network is flat: the ingress (DMZ) shares a segment with the motherbee and
    with fb-build.
- **Impact:** a compromised ingress that impersonates 10.10.10.50 serves its own `.deb`, and the
  next upgrade installs it as root on the motherbee.
- **Proposed fix:**
  - sign the `Release` (`InRelease`) and remove `[trusted=yes]` (the `.deb` does not change);
  - isolating the ingress at the network level waits until after the core: it needs infra config
    (D19).
- **Status:** recorded. Nothing was touched.

### A-50 ✅ RESOLVED (0.1.58, validated on 8.x) — The motherbee shared `blob/active` with the egress

- **What happens (verified on 2026-10-06 in PROD, read-only, with `lab/posture-check.py`):**
  - On the motherbee, the `fluxbee-blob` folder (`/var/lib/fluxbee/blob/active`, sendreceive: the
    live blobs of every tenant) is shared with worker1 **and with egress1**.
  - The egress never accepted it: its `hive.yaml` says `blob: enabled: false`, and it has it as a
    pending folder offered by the motherbee. No data traveled.
  - But the authorization exists on the motherbee's side: whoever has root on the egress (a host
    with an address on the office network) can accept the folder through its Syncthing's API and
    receive every active blob.
  - Side effect: the motherbee sees `fluxbee-blob` at 0% for egress1 forever.
- **Cause:** `d73687f` (2026-07-30, U-2) put the egress in the Syncthing mesh so it would receive
  the core, and the blob gate stayed at `public_only = spoke_role == HiveRole::Ingress`. From then
  on the motherbee shares `blob/active` with every spoke that is not an ingress, and the egress
  neither runs nodes (it does not receive `runtimes`) nor declares blob.
- **Fix (stage 2):**
  - The join shares `blob/active` only with workers.
  - The motherbee's reconciliation removes each spoke from every folder its role must never have
    (`blob/active` and `runtimes` outside the workers, `blob/public` outside the ingress, another
    role's core). The rule is the role alone, never the `hive.yaml` flags: the second adversarial
    review found that deriving it from the flags left every spoke without folders forever if
    someone turned a sync off for a while.
  - A test pins that the join never adds one of those folders, with any combination of flags.
- **A second way in, found in the third review:** creating a missing folder cloned another one
  with its members (creating `blob/active` after the spokes had joined shared it with every peer
  of the first folder), and the "isolated" folder was seeded with the first `<device id=>` of
  `config.xml`, which can be a peer. Now every new folder is born with no members (Syncthing adds
  the local device when it loads the config).
- **Known limit:** a folder created after the spokes joined (turning on the blob sync or the
  public one later) stays empty until `add_hive` is run again for them. The proposed fix is a
  backfill per role, like the policy one from vendor. Today it does not apply in PROD: blob has
  been on since the install.
- **Status:** validated in 0.1.58 (2026-10-06): the first round removed egress1 from
  `fluxbee-blob`, and the pending offer disappeared from the egress.

### A-51 [General/security] 🟡 FIXED ON THE MOTHERBEE (0.1.60) · the spokes PARKED with O9 (a signed manifest) — root wrote into the `fluxbee` user's directories following names that user controls

- **What happens (adversarial review of stage 2, 2026-10-06):**
  - `ensure_owned_tree` leaves all of `/var/lib/fluxbee/dist` owned by the Syncthing user
    (`fluxbee`), and `sync_data_dir` too.
  - `write_file_atomic` writes there (the core materialized per role, the manifest, the Syncthing
    binary in vendor) with a fixed-name temporary (`.<name>.tmp`) and `fs::write`, which follows
    symlinks.
  - `copy_exec_file` does a `chmod` by path after the rename, and `ensure_owned_file` does a
    `chown fluxbee:…` by path, which also follows symlinks.
- **Impact:** whoever holds the `fluxbee` user (Syncthing listens on the network) can plant a
  symlink and make root write to a root file, or change a file's owner (`chown` of
  `/etc/sudoers` or similar → root). It requires compromising the `fluxbee` user first; it is
  defense in depth. It has existed since before the posture work.
- **Proposed fix:** the same primitive the `config.xml` writer already uses (an `O_EXCL` temporary
  with a random name, owner and mode set by descriptor, rename, fsync of the directory) for
  `write_file_atomic`, `copy_exec_file` and `ensure_owned_file`. It is done in its own step, with
  its own review, because it touches the core update path.
- **Fix (0.1.60, four adversarial reviews):**
  - **Writing:** an `O_EXCL` temporary with a per-run token and the `.syncthing.` prefix (Syncthing
    never sends it), the mode set on the descriptor, fsync of the directory with `O_NOFOLLOW`.
    Temporaries from another run are swept right away; this run's, never.
  - **Managed directories:** they are opened `O_DIRECTORY|O_NOFOLLOW`, and the chmod/chown is done
    on the descriptor. Whatever is not a directory (a symlink or a planted file) is left as it is
    and reported as a posture alert, without bringing down the startup.
  - **`.stfolder`:** it is created exclusively; a symlink in its place is replaced; a folder that
    is a symlink gets no marker. The ingress join no longer chmods the blob directories.
  - The (dead) copy of the installed Syncthing into `dist/vendor` **was deleted**.
  - **The motherbee's `dist/` belongs to root** (`root:<Syncthing group>`, 0750, `g+rX,go-w`).
    Links inside it are deleted without following them, in the preinst and on every startup;
    Syncthing only reads it (on the motherbee it is send-only). The operator: *"Si root PERO!!!
    cuidado que hay muchas carpetas de syncthing, no todas pueden ser root"* ("Yes, root, BUT!!!
    careful, there are many syncthing folders, not all of them can be root"): only `dist/`;
    `blob/active` (send and receive), `blob/public`, Syncthing's home and the spokes' `dist/` stay
    `fluxbee`'s.
- **What stays open:**
  - **The spokes install and run whatever Syncthing delivers to them** (core, vendor, runtimes).
    Whoever holds the motherbee's Syncthing user (its `key.pem`) gets root on every spoke. The fix
    is a signed manifest, with a root key that the root of each spoke verifies.
  - **Root writes by path in the Syncthing user's `blob/`** on the motherbee: A-56.
  - **A re-join** installs with `install -m` inside the spoke's `dist/` by name (a race on a used
    box); the same class as the first point.
- **Status:** in 0.1.60.

### A-52 ✅ RESOLVED (0.1.60, validated on 8.x) — `add_hive` on a freshly created clone collides with cloud-init's `dist-upgrade`

- **What happened (2026-10-06, join of `worker2` on 8.x):** the first join failed with
  `CONFIG_FAILED` ("identity HMAC key distribution failed … port 22: Connection refused").
  - cloud-init, on the clone's first boot, runs an `apt-get dist-upgrade`. When it upgrades
    `openssh-server`, it stops `ssh.service` and closes `ssh.socket`, in the middle of the join.
  - After cloud-init finished and the VM rebooted (new kernel), the resumed join went fine.
  - HANDBOOK §3.4 already documents it ("A freshly booted clone is NOT ready": wait for the cycle
    and reboot before joining it). This time it was not followed; the error was an operational
    one, not the HANDBOOK's.
- **Also seen:** the throwaway VM (10.10.10.60) has internet access (it downloaded packages from
  `archive.ubuntu.com`). The 2026-07-29 logbook says the 8.x machines should not have it; it is the
  operator's network configuration.
- **Proposed fix:** have the join's bootstrap protect itself and wait for cloud-init
  (`cloud-init status --wait`, bounded) and for the dpkg lock before starting: the orchestrator
  owns the host's environment. Meanwhile, the HANDBOOK §3.4 recipe holds.
- **Fix (0.1.60):** as soon as it verifies the seeded key, and before installing the sudoers entry,
  the join waits while systemd has not finished booting (`is-system-running` in `starting`,
  `initializing` or `stopping`; it covers the gap between two cloud-init stages) or while a
  cloud-init stage or a daily apt job (unattended-upgrades) is `activating`. It does not use
  `cloud-init status`, which says "error" while stages are still running and "not started" forever
  if cloud-init does not run. Up to 15 min in total, retrying if the box reboots; the join shows
  the `waiting_for_host` phase. If it is still busy: `HOST_NOT_SETTLED` (retryable, the key stays,
  503); if the check itself fails: `HOST_SETTLE_CHECK_FAILED` (502).
- **Validated (2026-10-06):** a new clone of VM 104, joined as soon as it booted, waited 8 min in
  `waiting_for_host` while cloud-final ran, and then completed the join (ledger 0.1.61).

### A-53 ✅ RESOLVED (0.1.58; exercised by the joins of 0.1.61 and 0.1.62) — `local_syncthing_device_id` always failed with Syncthing v2 and fell back to the first device in `config.xml`

- **Status (2026-10-19 review):** the code no longer reads `config.xml` for the local device id; the worker2 joins of 0.1.61 and 0.1.62 linked the peer. PROD's motherbee id sorts first, so a join cannot tell the old fallback from the fix: the code is the proof.

- **What happens (verified on 2026-10-06 on the PROD motherbee, read-only):**
  - The orchestrator asked for the ID with `syncthing --home … --device-id`; the v2.0.14 binary
    answers `unknown flag --device-id` (in v2 it is the `device-id` subcommand).
  - So it always fell back to "the first `<device id=>` of `config.xml`". The journal since
    September shows the fallback 2 times.
  - That first device can be a peer: Syncthing sorts the folder members by ID. A join would have
    handed a spoke the wrong ID as "motherbee".
- **In PROD there was no damage:** the motherbee's ID (`BBJBKBB…`) is the lowest, so the fallback
  happened to give the local one; the 4 spokes have the motherbee's device right.
- **Fix (stage 2):** the `device-id` subcommand, only if `cert.pem` exists (without a certificate
  the binary would generate one as root); otherwise, the running Syncthing's `myID`. Never
  `config.xml`.
- **Status:** built; the next join exercises it (the A-48 one).

### A-347 [General/orchestrator] 🔵 low (found 2026-11-13 by 0.1.139's gate; read in the code, consistent with what the gate saw twice; there before it) — `remove_hive`'s cleanup on the box races the box's own orchestrator, which starts again the core services the cleanup stopped before it

- **Seen on 8.x (0.1.139's gate, VM 104, 2026-11-13 05:47–05:57Z):** after `remove_hive
  gate0239i` reached the box (`remote_cleanup` `socket_ok`, vm-step 2), a join of `gate0239i`
  again 10 s later failed `WORKER_SOCKET_UNREACHABLE` in 2 s, and the box still ran rt-gateway,
  sy-cognition, sy-config-routes, sy-opa-rules and sy-timer (the tool stopped those by hand) and
  sy-identity (still running then), so step 3 (A-299's `kept_for_other_hive`) was not run. After
  vm-step 11's removal (`socket_ok`), rt-gateway, sy-cognition, sy-config-routes, sy-opa-rules,
  sy-timer and sy-wf-rules still ran 35 s after the answer, and the box's hive.yaml still named
  gate0239i (it stays, by design), so `add_hive gate0239j` was refused `BOX_RUNS_ANOTHER_HIVE` in
  36 s: A-331's guard right, A-331 (c)'s two FAILs. Each service left running is one the cleanup's
  loop stops before sy-orchestrator. The box's journal went with VM 104, so its restart lines were
  not read.
- **Why:** the cleanup (`remove_hive_cleanup_script`, `src/bin/sy_orchestrator.rs` ~24421-24432,
  run in a transient unit 1 s after the answer, `remove_hive_cleanup_command` ~24516-24527) loops
  over rt-gateway, sy-config-routes, sy-opa-rules, sy-identity, sy-vault, sy-cognition, sy-timer,
  sy-wf-rules, sy-orchestrator and the rest, with `stop --no-block`, `disable`, `kill -s KILL` and
  `reset-failed` each. sy-orchestrator is ninth, and until it stops, its watchdog (every 5 s)
  starts any core service it finds inactive (`keep_core_service_running` ~3909-3931: *"service not
  active; attempting restart"*); a `disable` does not stop a `start`.
- **Effect:** the removed hive's services run on in the box, without their WAN TLS leaf and
  identity key, which the cleanup's first step deletes. A join of another hive id onto it is
  refused `BOX_RUNS_ANOTHER_HIVE` (A-331's guard: no harm, and its remedy names the steps), so a
  box is not reusable after a clean removal without them.
- **Fix, later (the orchestrator, local):** stop and disable sy-orchestrator first in the loop: the
  script runs in a unit of its own, so stopping the orchestrator does not end it, and the worker
  teardown already waits for it to be stopped.

### A-346 [General/admin] 🔵 medium (found 2026-11-13 by 0.1.139's gate, its step 44.1's S4; read in the code; there before it) — `set_node_config`'s help describes a body its HTTP route does not take

- **Seen on 8.x (0.1.139's gate, 2026-11-13 03:42Z):** the gate's tool wrote S4's fake record as
  `PUT /hives/motherbee/nodes/IO.cloud@motherbee/config` with the body `{"config": {"io": {...,
  "edge_node": "SY.edge@gate0239x"}}}`, as `set_node_config`'s help has it. The route answered 200
  and bumped `config_version`, and IO.cloud's config.json got a new top-level key `config` holding
  the fake, while its top-level `io.edge_node` still named `SY.edge@ingress1`. The publish then read
  the top-level `io` (A-317's record: `record_cloud_edge`, `cloud_edge_record_read`,
  `CloudEdgeRecord::previous_edge` and `moved_from`, `src/bin/sy_admin.rs` ~8690-9048) and saw a
  rotation on the same edge: no close, no `previous_*`, no warning, S4's two FAILs (A-317).
- **Why:** the help gives the body a required `config` and an optional `replace`, and the example
  `{"config": {"behavior": {"kind": "ai_chat"}}, "replace": false}` (`src/bin/sy_admin.rs`
  ~17467-17471, ~17894-17899, ~18351-18358). The HTTP route (~13378-13442) takes the body as the
  patch itself (`payload["config"] = config_payload`) and `replace` from the query string
  (`?replace=1`); Archi's SCMD translation (`src/bin/sy_architect.rs` ~14588-14602) takes the body
  as the patch too, with `replace` false. A caller who follows the help gets 200 and a bumped
  `config_version`, and plants top-level `config` (and `replace`) keys in silence.
- **Effect:** every reader of `get_node_config`, A-317's record included, reads the flat keys, but
  the node, at its next start, takes a top-level `config` as its whole config: the SDK's wrapped
  form (`extract_effective_config`, `crates/fluxbee_sdk/src/managed_control_plane.rs` ~158-167),
  which nothing writes today (a node's launch and the SDK's own save write flat files). On 8.x, any
  IO.cloud restart (a deploy, a reboot, a crash) from 03:42:24Z on would have made it trust
  `SY.edge@gate0239x` and drop every Cloud request through ingress1 while `HEALTHY`. IO.cloud did
  not restart meanwhile (pid 1949031 since 01:33:37Z).
- **Fixed on 8.x (2026-11-13, about 05:44Z):** IO.cloud's config read (keys `_system`, `config`,
  `io`, `tenant_id`; `config_version` 9), then put with `?replace=1`, its flat keys but `_system`
  and `config` (`io` naming `SY.edge@ingress1`, and `tenant_id`, built from the read), and read
  back: keys `_system`, `io` and `tenant_id`, `config_version` 10, `io.edge_node`
  `SY.edge@ingress1`, no stray key; IO.cloud `RUNNING` `HEALTHY` on the same pid. Only S4's write
  had that shape (the gate tools' one config `PUT`); the tool now writes a flat body, reads the
  fake back before the publish and refuses a stray key.
- **To decide (the operator):** (a) the help says the HTTP body is the patch and `?replace=1`
  replaces the config whole (the executor's args keep `config` and `replace`); (b) the
  orchestrator refuses a top-level `config` key in a patch, as it refuses `_system`
  (`src/bin/sy_orchestrator.rs` ~26327-26329); (c) the SDK drops the wrapped form. Recommended:
  (a) with (b).

### A-345 [General/orchestrator] 🔵 low, observability (found 2026-11-13 by 0.1.139's gate; seen on 8.x, read in the code; there before it) — `get_node_status` cannot tell an unconfigured node from any other `DEGRADED` one

- **Seen on 8.x (0.1.139's gate, A-332's G4, 2026-11-13 02:09–02:12Z):** `AI.gate0239cu@worker1`,
  an ai.generic with no config, answered a person's turn `node_not_configured` in 0.104 s, while its
  status read `RUNNING` `DEGRADED` (`NODE_REPORTED`) with no `last_error`: the gate's check for
  `UNCONFIGURED` in the status failed, the behaviour documented.
- **Why:** the status route keeps the unit's lifecycle and takes only the node's normalized health
  (`normalize_reported_health_state`, `src/bin/sy_orchestrator.rs` ~29120-29127; the node's answer
  read at ~29449-29467; the status built at ~29504-29548), while the node's own answer carries its
  config state as `lifecycle_state` (`managed_health_state` and
  `build_node_status_response_payload`, `crates/fluxbee_sdk/src/managed_control_plane.rs`
  ~342-367): an unconfigured node is `DEGRADED`, as any other.
- **Effect:** Archi and the operator read an unconfigured node as any degraded one; only the node's
  own answer to a message, or its journal, says why.
- **Fix, later:** the status carries the node's reported lifecycle state (for example
  `config_state`), so Archi tells them apart.

### A-344 [WF/wf-rules] 🔵 low, a question for the operator (found 2026-11-13 by 0.1.139's gate; seen on 8.x, read in the code) — A change that ran and could not rebind its node answers 502 `WF_NODE_NOT_DEPLOYED` with no `retryable`

- **Seen on 8.x (0.1.139's gate, `wf-busy`, worker1's Syncthing frozen, 2026-11-13 01:46–01:48Z
  and 05:34–05:36Z):** the changes that ran answered 502 `WF_NODE_NOT_DEPLOYED`, *"Package
  published, but sy.wf-rules could not rebind the existing node config: rpc timeout"* (or *"could
  not determine the node's current state"*), with no `retryable`.
- **Why:** `deployAnswer` (`go/sy-wf-rules/node/dispatch.go` ~572-605) answers the refusal with no
  `retryable`, which comes only from `WfRulesError.Retryable` (`go/sy-wf-rules/node/types.go`
  ~60-66, A-307's `BUSY`).
- **To decide (the operator):** whether such an answer says `retryable`: the package is published
  and the node not rebound, so asking again once the cause is gone may pass. Outside A-307's call.

### A-343 [General/orchestrator] 🔵 medium (found 2026-11-13 by 0.1.139's gate; seen on 8.x, read in the code: the mechanism confirmed, the cadence plausible; there before it) — A hung Syncthing freezes its hive's orchestrator for about 10 s of every 15

- **Seen on 8.x (0.1.139's gate, `wf-busy`, worker1's Syncthing SIGSTOPped, 2026-11-13
  01:46–01:48Z):** SY.admin's `get_node_config` and `set_node_config` to worker1, for SY.wf-rules'
  deploys, went unanswered within SY.wf-rules' 10 s about every 30 s (at about +2.5, +32 and
  +62.5 s; trace_ids `bf46dc65...`, `422f43e3...`, `d88b6406...`; r139-after.log:343-345); 0.1.138's
  run the same.
- **Why:** the watchdog's folder check runs `curl --max-time 10` through
  `std::process::Command::output()` (`syncthing_folder_status`, `src/bin/sy_orchestrator.rs`
  ~16028-16052, through `check_syncthing_folder` ~16113), from the tick's call of
  `check_syncthing_folders` (~20265; the function at ~19847), in the task spawned every 5 s
  (~1389-1408), not wrapped in `block_in_place`. A SIGSTOPped Syncthing still completes the TCP
  handshake, so the 2 s port check (`syncthing_api_port`, ~15851) passes and the curl waits its
  full 10 s. The orchestrator is one `#[tokio::main]` multi-thread runtime (~1133); in tokio 1.50
  one worker holds the I/O and timer driver, so a worker blocked in that curl stops every socket
  read and timer of the process for the 10 s (the router's messages included), whatever the worker
  count, until the tick's first `block_in_place` (~4106). Ticks during a run are skipped (~1407),
  so the runs come about every 15 s (A-283's 0.1.137 gate measured 14.9 s): commands are served
  only between curls, each up to about 10 s late. No lock is shared: `get_node_config` takes none
  (~28824-28921), `set_node_config` the node's lock (~29856), which the watchdog never takes; on
  the motherbee `set_node_config` holds that lock across the forward (~29856-29880), so commands
  queued behind a late answer wait too. The sync hint's curls (~5355-5367: a 120 s scan, a 10 s
  status) have the same pattern.
- **Effect:** while its Syncthing hangs, that hive's orchestrator answers late or not at all, about
  10 s of every 15: not all of Fluxbee, and a dead Syncthing refuses the connection at once and
  does not stall it; the nodes and the router run on. This goes beyond A-313, whose wait
  (`wait_for_syncthing_health`, ~16345) is async and holds only the watchdog, and it answers the
  operator's question on A-313 (2026-11-12: *"si al no funcionar syncthing todo fluxbee muere ...
  si es así charlemos"*).
- **Fix (local, no new watchdog, the operator's principle):** wrap the folder checks
  (~20264-20267) in `tokio::task::block_in_place`, as the tick already does for the egress, host
  rules and listener checks (~4106-4112) and the blob GC (~4057), and the sync hint's curls alike;
  update the source-text assertion at ~64263. To ask the operator; recommended with the next
  orchestrator release.

### A-342 [WF] 🔵 low (found 2026-11-13 by 0.1.139's gate; seen on 8.x, read in the code; there before it, since A-254 in 0.1.131) — A WF's resends of its timers land up to about 2 s late

- **Seen on 8.x (0.1.139's gate, `wf-missing`, 2026-11-13 01:45Z and 05:33Z):** 300 of
  `WF.gate0239@worker1`'s timers cancelled in SY.timer while it was stopped; at its start it found
  them missing and sent them again from its loop, and SY.timer held them later than the store's
  time: at most 2,114 ms in the first pass (a FAIL against the gate's 2 s), 4,497 ms on 0.1.138's
  baseline; the rerun's 300: median 341 ms, p95 763, max 1,073, none over 2,000. In the first pass
  the same per-row commits paced the drain (`DEGRADED` about 17.5 s for the 300 resends), and the
  SDK dropped 2 of SY.timer's answers, the node's 64-deep channel for them full
  (r139-after.log:317, 322). The WF's next restarts moved 8 of them to their time (*"later than its
  time ...; moved to its time"*).
- **Why:** `sendTimerAgain` sends `fire_in` = the timer's `FireAtMS` less the tick's now, read once
  a tick (`go/nodes/wf/wf-generic/node/timer_requests.go` ~424, ~494-498), up to 32 resends a tick
  (`maxTimerRequestsPerTick`, ~63), each after its own synchronous `TimerSentAgain` commit; SY.timer
  sets `fire_at` = its own now + `fire_in` (`oneShotFireAt`, `go/sy-timer/main.go` ~1262-1274, from
  the schedule's handler at ~542), so a tick's later resends are held late.
- **Effect:** within the engine's 2 s `timerFireTolerance` (`correlate.go` ~168-171), but a new
  schedule's answer is taken with no `heldLate` check (`timer_requests.go` ~409-410), so such a
  timer fires late unless a restart's reconcile moves it (`recover.go` ~539-546, `moveHeldTimer`; 8
  moved after the first pass).
- **Fix, later (the WF engine):** read the clock at each send, or send `fire_at_utc_ms`; apply
  `heldLate` to a new schedule's answer; commit a tick's `TimerSentAgain` writes in one transaction.

### A-341 [General/orchestrator] 🔵 low (found 2026-11-13 by 0.1.139's gate; seen on 8.x, read in the code; there before it) — `start_node` within about 10 s of a node's exit answers `already_running` and launches nothing

- **Seen on 8.x (0.1.139's gate, `wf-missing`):** in the first pass `WF.gate0239@worker1` was
  killed at 01:44:14Z, the gate's client under its name closed at about 01:44:50Z, and `start_node`
  at 01:44:53Z answered ok, `already_running`, with no `relaunch_on_boot`; the unit Started at
  01:45:30.935Z, from the 60 s managed reconcile (*"persisted custom node relaunched during
  reconcile"*, 01:45:30.951Z), `RUNNING` at +43.6 s; 0.1.138's baseline the same (`RUNNING` at
  +65.8 s). The rerun (05:32Z), waiting 12 s after the client's end, got `started`.
- **The clean (2026-11-13 06:02Z to 06:04Z)** read 4 nodes as still there right after their delete
  or purge (`WF.gate0239`, `WF.gate0239w`, `AI.gate0239rc`, `AI.gate0239rm`). Each read
  `NODE_NOT_FOUND` seconds later, at the clean's final check: the status route lags the same way.
- **Why:** `start_node` takes the router's view as running ahead of systemctl's `Inactive`
  (`start_node_finds_running`, `src/bin/sy_orchestrator.rs` ~30960-30972; `local_inventory_has_node`
  ~29043 reads the LSA region). The router writes that region on its 10 s hello tick
  (`lsa_broadcast_loop`, `src/router/mod.rs` ~3215-3224; `DEFAULT_HELLO_INTERVAL_MS` 10_000,
  `src/config/mod.rs:9`), on a stale change (~3262) or on a peer's LSA (~3913), not when a local
  node leaves. So `start_node` sets `relaunch_on_boot` true and leaves the node to the 60 s managed
  reconcile; its answer omits `relaunch_on_boot` (~31076-31088), and the help
  (`src/bin/sy_admin.rs` ~19112) does not name the lag.
- **Effect:** a start right after a stop or a crash waits up to a minute for the reconcile, and
  answers as if the node ran.
- **Fix, later (candidates):** write the LSA region when a local node joins or leaves; or trust
  systemctl's `Inactive` over an LSA entry of the node's own hive; and put `relaunch_on_boot` in
  the `already_running` answer.

### A-340 [General/orchestrator] 🔵 low (found 2026-11-13 by 0.1.139's gate; seen on 8.x, read in the code; there before it) — A join refused at its key probe says the motherbee's key is still on the box, when the key does not open it

- **Seen on 8.x (0.1.139's gate, its first pass's VM steps, 2026-11-13 02:22–02:26Z):** 7 joins
  failed `SSH_AUTH_FAILED` at their key probe (r139-after.log:842, 862, 897, 907, 918, 930, 940),
  the box answering *"Permission denied (publickey,password)"*: the gate's tool had not put the
  motherbee's key back on the box between joins, which the orchestrator takes off after each by
  design. Each join recorded `ssh_bootstrap_open` true and logged *"failed join: could NOT remove
  the motherbee bootstrap key — it is still on the box"*.
- **Why:** an `SSH_AUTH_FAILED` is not transient (`is_transient_ssh_error`,
  `src/bin/sy_orchestrator.rs` ~39502-39515, read by `add_hive_result_is_transient` ~39545), so B-2
  (~35128-35165) runs `best_effort_revoke_bootstrap` (~39228-39259), which cannot log in either:
  the join records `ssh_bootstrap_open` true and logs that line, although the probe's refusal shows
  that the key does not open the box.
- **Effect:** misleading only; nothing is left open by it.
- **Fix, later:** a revoke that fails to authenticate with that same key reports the key as not
  opening the box.

### A-339 [General/AI SDK] ✅ VALIDATED ON 8.x (0.1.142, 2026-11-13; AI.pruebas and SY.frontdesk.gov leave a `NOT_TAKEN` and a `<msg>.error` unanswered, a WARN naming who refused and why, where 0.1.141 answered each `unknown_system_msg`) (found 2026-11-12 by the review of `coa/rustsdk-143`; read in the code, also on `coa/aisdk-142`; there since the WF engine's `NOT_TAKEN` (0.1.94), more often from 0.1.141) — An AI node answers a `NOT_TAKEN` with an `unknown_system_msg` error

- **Why:** the AI SDK's `is_infrastructure_message` (`crates/fluxbee_ai_sdk/src/control_plane.rs`
  ~114, the same on `coa/aisdk-142`, `bf7cd152`) takes for a notice or a reply only a message
  without `src_l2_name`, one without a name, `HELLO`, `ANNOUNCE`, `WITHDRAW`, `UNREACHABLE`,
  `TTL_EXCEEDED` and any `*_RESPONSE`. A `NOT_TAKEN` (the WF engine's since 0.1.94, the Go SDK's
  `node_busy` since 0.1.141, the Rust SDK's since 0.1.143, planned) or a `<msg>.error` reply reaches
  the control plane as a system request it does not know. ai.generic
  (`nodes/ai/ai-generic/src/bin/ai_node_runner.rs` ~1361) and SY.frontdesk.gov
  (`nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs` ~2300) answer it with a
  `CONFIG_RESPONSE` error `unknown_system_msg`.
- **Effect:** one wasted message for each `NOT_TAKEN` an AI node gets, and the node's log does not
  say that one of its messages was not taken. No loop: the answer is a reply, which no SDK answers.
  The Go SDK (`IsNoticeOrReply`) and the Rust SDK (`may_answer_not_taken`) leave both unanswered.
- **Fix, later (the AI SDK; best on `coa/aisdk-142`, before 0.1.142 ships; A-325's family):**
  `is_infrastructure_message` takes `NOT_TAKEN` and `<msg>.error` for replies, and
  `note_infrastructure_message` logs a `NOT_TAKEN` as it logs an `UNREACHABLE`, with its reason.
- **Built on `coa/aisdk-142` (planned 0.1.142, the AI SDK alone, with A-294, A-290, A-325 and
  A-327's `tool_choice`), the fix above as it reads:**
  - **No answer.** `is_infrastructure_message` takes `NOT_TAKEN` and any name ending in `.error`
    for replies, the Go SDK's `IsReplyName` name by name, in either kind the control plane takes
    (`system`, `admin`). ai.generic and SY.frontdesk.gov answer neither, through their own entry
    (`on_message`) too. Any other `system` message from a node that is no control verb is still
    answered `unknown_system_msg`.
  - **One line, on the node that failed.** `note_infrastructure_message` logs a `NOT_TAKEN` as it
    logs an `UNREACHABLE` (`note_not_taken`): a WARN, *"NOT_TAKEN: a message this node sent was not
    taken, and nothing of it ran there; not answered, and not sent again"*, which names the node
    that refused it (`refused_by`), the trace and the thread, the `reason`, `retryable` (false when
    the `NOT_TAKEN` has none, as from a node before 0.1.141) and the `detail` (quoted, its first 240
    characters). The first of each reason at once, then at most one every 10 s with how many came
    since, the ones between at debug; its counts are the router notices', bounded at 32 kinds. A
    `<msg>.error` gets the info line of any other reply.
  - **The operator's principle, applied** (A-313's call of 2026-11-12: on a failure, retry and log
    on the one that fails, never act globally): the agent whose answer was not taken logs it, and
    nothing else acts on it, the WF included (no answer goes back). The retry is the resend below,
    proposed and not built: this entry's fix decides the log alone.
  - **Tests,** each checked failing without its part: the SDK's
    `a_not_taken_and_an_error_answer_are_replies`; ai.generic's and the frontdesk's
    `a_not_taken_or_an_error_answer_is_never_answered` (no answer to a `NOT_TAKEN` `node_busy`, nor
    to a `system` or `admin` `ask.error`, and the warning with `refused_by`, `reason`, `retryable`
    and the detail). Without the change all three fail, the SDK's test on
    `is_infrastructure_message`'s result, the runners' on the `unknown_system_msg` answer; without
    the `.error` arm, on the error answers; without the warning, on the runners' log checks.
  - **Docs:** the AI SDK's README, ai.generic's and the frontdesk's READMEs, the frontdesk's page,
    the WF spec's §14.5 (what an agent does with a `NOT_TAKEN`) and Archi's handbook §4.1 (*An
    answer the WF does not take*). The handbook, the AI SDK's README and `note_not_taken` say which
    `NOT_TAKEN` reaches an agent: a busy WF's `node_busy` for any answer, and a WF without its store
    `unavailable` only for an answer with a name (`answerNotTaken`, `neverAnswered`), so an agent's
    usual answer, which has none, gets nothing back from it; and that a given turn may have no
    warning of its own (the first of each reason, then at most one every 10 s).
  - **At integration, after `coa/gosdk-141`:** five of its texts take this release's words. Two
    say what an AI node does with a `NOT_TAKEN`: `docs/reference/protocol.md` §6.10's row *"An AI
    node (the AI SDK)"* (it answers `unknown_system_msg`), and the WF spec's §14.5 paragraph on
    `NOT_TAKEN`, whose older sentence this branch changed, so the rebase stops there. Three say the
    AI SDK's rule leaves `NOT_TAKEN` and `<msg>.error` out, each to read *"wf-generic's rule and the
    AI nodes', name by name (0.1.142)"*: `docs/sy-timer.md` §6.1's *"Only requests are answered"*
    (*"wf-generic's rule, name by name, and the AI nodes' but for `NOT_TAKEN` and `<msg>.error`"*),
    `docs/nodes/sy-timer.md`'s **Owner** rule (*"wf-generic's rule, name by name, and the AI nodes'
    but for the last two"*), and SY.timer's `isInfrastructureMessage` comment
    (`go/sy-timer/main.go`, *"name by name but for NOT_TAKEN and <msg>.error"*), the last with
    SY.timer's next change.
  - **With the WF engine's next change:** two of its comments, the same on `coa/gosdk-141`, still
    say *"ai.generic ignores a system message it does not know"*, the sentence this branch corrected
    in the WF spec's §14.5: false before (ai.generic answered `unknown_system_msg`), and false after
    for any other `system` message it does not know. They are `answerNotTaken`'s
    (`go/nodes/wf/wf-generic/node/correlate.go`) and `TestAMessageWithNoThreadStartsNothing`'s
    (`go/nodes/wf/wf-generic/node/delivery_test.go`), each to read *"an AI node takes a
    `NOT_TAKEN` for a reply and logs it (0.1.142)"*. No Go file changes in this release, the AI SDK
    alone, as with SY.timer's comment.
  - **At 0.1.143's rebase onto 0.1.142:** `coa/rustsdk-143` carries this entry's heading and first
    three points (*Why*, *Effect*, *Fix, later*), unbuilt, and a board line of its own, so FINDINGS
    conflicts there. Keep this branch's built entry and board line, and add `coa/rustsdk-143`'s
    A-336.
- **The resend of an answer not taken (proposed, to decide; not built):** what the runtime could do
  with a retryable `NOT_TAKEN` for its own answer.
  - **Today** the answer is lost: the WF's instance goes on as if the agent had not answered (in
    `wf.sales.invoice` its 120 s for the answer run out, and the person gets no answer to that
    message), and the model call it cost is wasted.
  - **Safe, for a retryable `NOT_TAKEN` alone** (`node_busy`; `unavailable` from 0.1.141): nothing
    of the answer ran at the receiver, so sending it again cannot run it twice, and the receiver
    says whether it may pass. The other reasons are never sent again.
  - **Cheap:** no model call runs again. The runtime would keep each answer it sent for a short
    while, bounded (the last 256 answers or 8 MiB, say, each for 60 s), and on a retryable
    `NOT_TAKEN` from the node the answer went to (its uuid the answer's destination, its trace the
    answer's) send the kept answer again after 5, 10 and 20 s (SY.timer's first steps), a line
    each, then drop it. In the runtime, before the node, so both runners get it.
  - **Its edges.** A WF takes an agent's answer by its thread, not its trace: an answer sent again
    after the WF stopped waiting reaches the instance as a late answer, which a definition already
    meets when a model is slow (`wf.sales.invoice` forwards sales' late answer to the person, or
    acts on its option); the last attempt by about 35 s keeps it within a usual wait, and two
    answers on one thread swap only when the WF asked twice within it. The router copies each
    message a node sends as a turn at its entry: cognition takes the second copy as the same turn
    (its key is the trace, the writer and the text), while SY.storage would keep a second turn row
    (the trace entry's time differs) unless the resend is marked. Under a long backlog (1,024
    messages at 30 a second, about 34 s) the attempts may be refused again, each logged.
  - It fits the operator's principle (the node that failed retries and logs) and A-329's call for
    SY.timer (a fire not taken is sent again). Best with the IO family's resend of a retryable
    `NOT_TAKEN` (A-329's leftovers): one rule for both.
- **Validated on 8.x (0.1.142's gate, gate0242, 2026-11-13; `scratchpad/gate-tools/r142_gate.py`, git-ignored; 97 PASS, 1 SEE, 0 FAIL, no model call):** on 0.1.141 AI.pruebas@worker1 and SY.frontdesk.gov@motherbee
  answered a `NOT_TAKEN`, a `gate0242_probe.error` and an unknown verb each with `unknown_system_msg`
  (0.003 to 0.023 s). On 0.1.142 the `NOT_TAKEN` and the `.error` got no answer within 5 s on either
  node; the `NOT_TAKEN`'s WARN came 0.002 to 0.003 s after it, naming the refuser, the reason and
  `retryable`; the error answer's info line *"a notice or a reply, not a request; not answered"*;
  the unknown verb still answered. The resend of an answer not taken stays a proposal.

### A-338 [General/blob] 🔵 low (found 2026-11-12 by the review of A-264's build on `coa/gosdk-141`; read in the code, not seen; there since the Rust SDK's text offload, wider from 0.1.141) — A text sent as a blob can fail at a reader on another hive: the message comes before Syncthing copies the blob

- **Why:** a long text goes as a blob of the sender's hive, its `content_ref` in the message: the
  Rust SDK's over about 62 KiB (an agent's long answer; the IO nodes the same), and from 0.1.141
  the Go SDK's past the frame (a WF's text, A-264). The blob reaches another hive through
  Syncthing, whose watcher scans a change 10 s after it (`fsWatcherDelayS`; renames held up to
  60 s were seen in A-19), so the message comes first. Today ai.generic and SY.frontdesk.gov read a
  `content_ref` at once (`BlobNotFound`), io.slack and io.wapp wait 5 s (`read_blob_with_retry`),
  and a WF, io.api, io.web and io.linkedhelper read none (A-264's review names the code).
- **Effect:** such a text fails at a reader on another hive (io.slack does not post it); a reader
  on the sender's hive reads it at once. On 8.x the test agent and the gates' WFs run on worker1,
  and the IO.slack nodes on the motherbee. From 0.1.141 it also takes a WF's text over the frame,
  which no reader got before (refused), never one that went inline.
- **Options (A-264's (c)):** the producer asks a sync and waits before it sends
  (blob-annex-spec §6.6; the sync hint is SY.admin's alone today), or each reader waits longer
  (the AI SDK's model input, the IO common code: their own releases).
- **The operator (2026-11-12, A-264's call):** a topic of its own.
- **Seen on 8.x (0.1.141's gate, 2026-11-13):** a WF's 140 KiB text sent as a blob to a reader on
  the motherbee: the message arrived at once and the blob 11.2 s after it (112 polls, its SHA-256
  right), past io.slack's and io.wapp's 5 s wait and ai.generic's none: such a reader on another
  hive fails it today. Recorded, not judged (the operator's call keeps it a topic of its own).

### A-337 [General/admin] 🔵 low (found 2026-11-12 by A-124's review; read in the code, not run; there before it) — SY.admin's plan review gives its model each step as written, a secret the operator wrote into its args included

- **Why:** `executor_validate_plan` (`src/bin/sy_admin.rs` ~10460) runs
  `semantic_review_executor_plan` (~3127) whenever SY.admin's executor has an AI: one model call
  per step (stage `executor_review`, charged to the root tenant), whose prompt holds the step as
  written (`serde_json::to_string_pretty(step)`), its args included. Archi sends every plan it runs
  there first (`execute_executor_plan_with_context`), and its plan compiler's prevalidation does
  too.
- **Effect:** a secret the operator writes into a plan's args (a `vault_put`'s or a
  `vault_rotate`'s `value`, an `add_hive`'s `ssh_password` or `ssh_key`) reaches the hive's AI
  provider before the plan runs, though Archi keeps it masked in its history and its log since
  0.1.139 (A-124). A secret an answer brings does not: the review runs before any step, on
  `$steps.<id>...` references as written. SY.admin's HTTP routes take no plan and no model.
  Archi's own model sees what the operator types into the chat, a secret included, as before.
- **Fix, later (SY.admin):** the review's prompt carries the step's args with their secrets masked
  (`redact_executor_log_value`: names that read as secrets, and a vault-shaped `value`), since it
  checks that a field is there and its shape, not its value; or a step whose args hold a secret is
  not given to the model.
- **Today:** a secret is stored over SY.admin's HTTP API (`POST /hives/<hive>/vault/secrets`), not
  in a plan. Archi's handbook (§4.5.1) tells Archi to say so before it stages one.

### A-336 [General/orchestrator] 🔵 low (found 2026-11-12 by the review of `coa/rustsdk-143`; read in the code; from 0.1.143, planned) — The orchestrator reads SY.admin's `NOT_TAKEN` as an unknown outcome, and a spoke orchestrator's as unreachable

- **Why:** since 0.1.143 (planned; A-295) a Rust node whose command channel has no room answers
  `NOT_TAKEN` `node_busy`, retryable, and the requester's call ends with `RpcError::NotTaken`:
  nothing of the request ran. The orchestrator's readings of a failed call do not know it:
  - `teardown_node_call_failure` (`src/bin/sy_orchestrator.rs` ~31129) takes every `RpcError` but
    `InvalidRequest`, `Json`, `Unreachable` and `TtlExceeded` for `TeardownNodeCall::NoAnswer`
    (*"SY.admin may still have run it"*), where `NotDelivered` means nothing ran;
  - the runtime-usage fan-out (`group_runtime_usage`, ~21944) names any failure but a timeout
    `UNREACHABLE` (*"the request never reached it"*) in `skipped_hives`.
- **Effect:** a teardown whose `teardown_node` a busy SY.admin refused reports an unknown outcome,
  where nothing ran and sending it again may pass; a spoke whose orchestrator was busy is skipped as
  unreachable. Nothing wrong is done: an unknown outcome is the careful reading. It needs SY.admin or
  a spoke's orchestrator with 1,024 messages (or 8 MiB) waiting in a lane.
- **Fix, later (the orchestrator's next change, after 0.1.143):** `RpcError::NotTaken { .. }` reads
  as `TeardownNodeCall::NotDelivered`, and the fan-out names a reason of its own (`BUSY`,
  retryable) in `skipped_hives`. `RpcError::NotTaken` exists from 0.1.143, so not in 0.1.139.

### A-335 [General/orchestrator] 🔵 low (found 2026-11-12 by A-301's builder on `coa/orch-139`, numbered by its review; read in the code; there before it) — A box `remove_hive` did not reach is taken again over the socket by a join under its hive's name, whatever address it names

- **Why:** a box the removal's cleanup did not reach (`remote_cleanup` other than `socket_ok`)
  keeps its install and its mesh certificate. The router admits that certificate again once the
  registry lists a hive of that id (A-48: leaf pinning parked). `add_hive_flow`'s socket-first
  path (`src/bin/sy_orchestrator.rs` ~35350 on `coa/orch-139`) takes any `SY.orchestrator@<hive>`
  it sees online within 3 s (`wait_for_remote_orchestrator_node`), and
  `probe_remote_orchestrator_socket_ready` forwards `GET_VERSIONS` by the hive's name, so
  `address` is never checked. The socket-only finalize provisions no identity key and no TLS leaf
  (`distribute_hive_identity_key_inner` is the SSH bootstrap's), and no join restarts a running
  sy-identity, which loads its HMAC key only at its start (`src/bin/sy_identity.rs` ~4570).
- **Effect:** a fresh `add_hive` under that name, even one whose `address` names a new box, can
  adopt the old box: connected, with its old certificate, and its identity replica refused at
  :9100 for good: the motherbee has no key for it, or a new one the box never got, and a restart
  of its sy-identity loads only the old key the box kept. Before 0.1.139 a removal whose key deletion failed (`delete_failed`) let
  that box back with its old key (A-277's case, A-301); since A-301 every such removal behaves as
  one that deleted the key.
- **Today:** the texts say it. SY.admin's `remove_hive` help (`REMOVE_HIVE_BOX_NOT_REACHED_NOTE`),
  Archi's handbook, sy-orchestrator.md and operations.md: when `remote_cleanup` is not
  `socket_ok`, stop that box's install, or make sure the box is gone, before adding a hive under
  its name (built on `coa/orch-139` with A-301's review, for 0.1.139).
- **Fixes considered (the orchestrator), not built:** a fresh join (`resumed == false`) never takes
  the socket-only path; and A-331's probe refuses a fresh join onto a box that still runs that same
  hive id, as it refuses another hive's install. Neither stops an old box from dialing in with its
  certificate: that is A-48's leaf pinning, parked.
- **The operator's call (2026-11-12):** left as documented, the texts warning of it (above); the
  real guard comes with A-48's leaf pinning, which is parked security.
- **Its number:** `coa/orch-139`'s review numbered it A-335 at 17:14, after `coa/rustsdk-143` had
  given A-335 at 16:56 to the AI SDK's item that `coa/aisdk-142` builds (*an AI node answers a
  `NOT_TAKEN` with an `unknown_system_msg` error*), its code comments included. 0.1.139's
  integration review renumbered this one A-339; the operator's call of 2026-11-12 gave it A-335
  back, in its places (this entry, its open line and A-301's entry, SY.admin's
  `REMOVE_HIVE_BOX_NOT_REACHED_NOTE` and its test, Archi's handbook, sy-orchestrator.md and
  operations.md), and the AI SDK's item A-339, renamed at the integration of `coa/aisdk-142` and
  `coa/rustsdk-143` (board item 30).

### A-334 [General/orchestrator] ✅ CLOSED (2026-11-12, read on 8.x: transient, nothing left; no fault) (found 2026-11-12 in 0.1.138's deploy timeline; seen on 8.x) — worker1's Syncthing fails to delete some runtime directories of its receive-only dist mirror

- **Seen on 8.x (0.1.138's deploy and gate, 2026-11-12, `ops timeline`):** worker1's Syncthing
  logged *"Failed to delete directory"* (INF) in the folder `fluxbee-dist-runtimes`, receive-only:
  at 13:26:44Z, while the motherbee's package boot restarted its core services, for five runtime
  version directories (io.api's, io.cloud's and io.blob's 0.1.137; wf.engine's and
  io.linkedhelper's, their names cut off); at 13:32:09Z, after the forced delete of the workflow
  gate0238, for `wf.gate0238` (two lines) and a version directory of it. The timeline cuts each
  line before its reason.
- **Read on 8.x (2026-11-12, about 14:25Z):** Syncthing's whole lines give the reason, *"directory
  has been deleted on a remote device but is not empty; the contents are probably ignored on that
  remote device, but not locally"*: content the folder ignores held each deletion up. A later pass
  finished them: worker1's `dist/runtimes` holds what the motherbee's holds (io.api, io.cloud,
  io.blob, wf.engine and io.linkedhelper on 0.1.138 only; no `wf.gate0238`), and the folder is
  `idle` with `receiveOnlyTotalItems` 0, no errors and nothing needed. Nothing to fix; which
  ignored content held them was not read.

### A-333 [WF] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; a restart with 3,000 pending timers reconciled in 0.042 s, its receive loop at 1.209 s and `HEALTHY` at 1.000 s, where 0.1.138 took 4.214 s, 6.653 s and 5.621 s; 300 timers SY.timer lacked reconciled in 0.263 s, against 7.810 s; the one FAIL the tool's 10 s bound on `RUNNING`, 11.3 s through SY.admin's polls) (found 2026-11-12 by 0.1.138's gate; seen on 8.x; the receive loop after Recover read in the code, there before it; the reconcile's cost not read in the code) — A WF's reconcile of 3,000 pending timers takes 3.6 s on worker1, and its receive loop starts after it

- **Seen on 8.x (0.1.138, gate0238, 2026-11-12 13:29–13:31Z):** after A-278, the reconcile of
  `WF.gate0238@worker1`'s 3,000 pending timers (300 instances, 10 keys each) took 3.558 s from its
  *"listed"* line to its *"complete"* line in worker1's journal, at a restart with the hive at
  rest, and 4.372 s at the deploy's relaunch; the listing itself took 0.931 s (13 pages). On the
  builder's Mac SY.timer listed 3,000 in 0.13 s and the WF's reconcile took 0.23 s more (A-278);
  the gate's 1 s bound came from those numbers, and this was its one FAIL.
- **Effect:** the receive loop starts after the reconcile (at T0w+4.967 s at that restart, T0w the
  node's start in worker1's journal), so a restarted WF takes no message into it for about 5 s at
  3,000 timers, likely 12 to 15 s at 10,000. That order is the engine's, before A-278 too:
  `Recover`, the timers' reconcile its fourth step, runs before the receive loop
  (`go/nodes/wf/wf-generic/node/recover.go` ~26, `Run` in `node.go`). On 0.1.137 the same restart
  reached its receive loop at T0w+18.205 s and ran `DEGRADED` from T0+17.4 s to T0+93.5 s (A-278).
- **Not read:** whether the cost is a store commit per instance (300 × about 12 ms) or per timer.
- **Fix, later (the WF engine):** read the code; batch the store writes, or start the receive loop
  before the reconcile ends.
- **Built on `coa/wf-139` (for 0.1.139).** **Where the time went, measured on the Mac** (a store
  file of 300 instances and 3,000 timers, as gate0238's, SY.timer a fake): neither commits per
  instance nor writes, at rest. The reconcile read each row of the index twice, again before it
  acted on it and again to settle a timer SY.timer holds (`timerHeld`), each read a statement
  outside a transaction, so a read transaction of its own: 6,001 transactions for 3,000 timers
  held, 0.286 s, two thirds of its CPU in the `fcntl` locks of the WAL's shared memory
  (`walTryBeginRead`); worker1's 3.558 s is the same work 12.5 times slower. Each write was a commit
  of its own, which waits for the disk (`synchronous` FULL): with every timer refused in the index
  and held, 9,001 transactions, 1.244 s; with SY.timer holding none (each marked to go again),
  6,001, 1.104 s; on worker1 the 0.1.137 baseline's 2,000 such marks took most of its 16.727 s.
  - **The fix (`node/recover.go`, `node/store.go`, `node/timer_requests.go`):** the reconcile reads
    the index once, and reads a row again only when it gave that row's instance a fire before it,
    whose transition may have cancelled, moved or scheduled the timer again: nothing else writes
    the index meanwhile (the reconcile runs on the receive loop's goroutine, `Recover` before the
    loop; the GC drops only the rows of instances that ended days before, read as not running). A
    timer SY.timer holds is settled on the row as read (`heldLate`, `moveHeldTimer`, split from
    `timerHeld`). What it writes of the rows it passed (a refusal cleared, a timer SY.timer lacks
    marked, a row of an instance that no longer runs dropped) is kept with its log line and made in
    one transaction (`indexWrites`, `Store.WriteTimerIndex`) before each fire it gives an
    instance, whose transition may read or change the instance's rows, and at its end. A commit
    that fails leaves its rows as they were, is logged (*"N write(s) to the timer index not made,
    those timers stay as they were"*), and the reconcile says the timers are not reconciled: the
    node runs `DEGRADED` (`timers_not_reconciled`) and the loop reconciles again every 30 s, where
    a mark that failed was only logged and its timer went again only at the node's next start.
    The same store after it: 1 transaction, 9.4 ms (held); 2, 74.6 ms (held and refused); 2,
    86.7 ms (none held). A start with 3,000 held timers, its own work: the instances loaded in
    16 ms, drained in 13 ms, the reconcile 13 ms; the rest is SY.timer's listing.
  - **The receive loop stays after the reconcile:** a fire the reconcile gives reaches its
    instance before any message the loop takes, in the order the timers fire, and only the loop's
    goroutine touches the registry and the timer requests. With the reconcile's cost gone, a
    restart's time to its loop is SY.timer's listing (A-326: 0.931 s for 13 pages on worker1 at
    rest).
  - **Tests** (`node/reconcile_cost_test.go`; `node/store_count_test.go` opens the store through a
    driver that counts its transactions): `TestAReconcileReadsTheIndexOnceAndWritesItInOneCommit`
    (1, 2 and 2 transactions for the three cases, where the reconcile before made 6,001, 9,001 and
    6,001), `TestAReconcileWhoseWritesFailIsNotDone` (the reconcile before was complete) and
    `TestAReconcileMakesItsWritesBeforeAFire` (marks made only at the reconcile's end land on a
    deadline the fire scheduled again: it fails without the commit before a fire);
    `TestRecoveryFiresLostTimersInTheOrderTheyFire` fails without the read again after a fire.
    Docs: the WF's README, [spec-systemnode-wf-v1_1](../../docs/spec-systemnode-wf-v1_1.md)
    §11.4 and [sy-wf-rules.md](../../docs/nodes/sy-wf-rules.md). **The review:** the README and
    the comments of `recover.go` and `timer_requests.go` named `timerHeld` for the reconcile's move
    of a timer SY.timer holds late; they name `heldLate` and `moveHeldTimer`, `timerHeld` being
    SY.timer's `already_existed` answer's alone.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `6bc7d2e8` (its review
  `60485aeb`). `wf-restart`, a measured restart (T0 the restart's answer, T0w the node's start in
  worker1's journal), on 0.1.138 and then on 0.1.139: `WF.gate0239@worker1`'s 3,000 pending timers
  listed in 13 pages in 2.019 s against 0.489 s, the reconcile, *"listed"* to *"complete"*,
  4.214 s against 0.042 s, the receive loop at T0w+6.653 s against 1.209 s, `HEALTHY` at
  T0+5.621 s against 1.000 s; `WF.gate0239o1@worker1`'s 4,430 in 19 pages, 2.258 s against
  1.207 s, the reconcile 4.421 s against 0.114 s, the loop at 7.062 s against 2.161 s. Never
  `DEGRADED`, nothing missing or sent again, the store and SY.timer's digests unchanged. The
  deploy's relaunches reconciled in 0.221 s (3,000) and 0.130 s (4,430). The write path
  (`wf-missing`: 300 of the WF's timers cancelled in SY.timer while it was stopped): on 0.1.138 the
  reconcile took 7.810 s; on 0.1.139 0.455 s, and 0.263 s in the rerun (05:31Z), with no *"write(s)
  to the timer index not made"*, the 300 found missing and sent again, and SY.timer holding them
  again. The rerun's one FAIL: `RUNNING` 11.327 s after `start_node`, against the tool's 10 s bound,
  its polls going through SY.admin at 1 to 5 s each (`HEALTHY` at +14.9 s). The first pass's three
  FAILs came from `start_node`'s lag (A-341) and the WF's resends held late (A-342), neither this
  item's.

### A-332 [General/cognition] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; a turn the agent refused busy cost no model call, the refusal counted at 0.311 s, where 0.1.138's tagger call was under way 0.213 s after the send; the agent's answer ended the wait, the call at 0.416 s, and an answer at 8 s found the 5 s grace over at 5.027 s; no SY.cognition record on a refused turn's trace) (found 2026-11-12 by 0.1.137's gate; seen on 8.x; left by A-292's fix) — Cognition's model call on a person's turn that the agent then refuses completes

- **Why:** cognition starts analyzing a person's turn as soon as its copy arrives. When the agent
  refuses the turn `ai_runtime_busy` milliseconds later (A-257's immediate answer), cognition drops
  the turn (A-292, 0.1.137), but the call already in flight completes.
- **Seen on 8.x (0.1.137, gate0237, 2026-11-12 10:03–10:05Z):** with AI.pruebas' real runtime
  (`cog-busy`), one call (1,223 tokens in, 78 out, Sonnet 5.5) on the refused trace, the refused
  turn being its communication's first: `semantic_tagger_calls_total` went from 6 to 7, and the
  cost step counted one SY.cognition record on a refused turn's trace. None with the agent played
  by a mesh client (`cog-agent`), where the refused turn waited behind the previous turn's
  analysis. The turn itself was dropped and never analyzed, as A-292 says.
- **Effect:** under load, one wasted model call per refused first turn. Before 0.1.137 cognition
  kept and analyzed every refused turn, so this is no regression.
- **Fix, later (cognition):** start an In turn's analysis after the agent's answer or a short
  grace, or drop the call's result and cancel the call on the refusal.
- **Built on `coa/cognition-139` (for 0.1.139): a short grace, which the agent's answer ends.**
  The worker analyzes a person's message the agent got (its `request_of`) once the agent answered
  or refused it, or 2 s after the intake took it (`COGNITION_REFUSAL_GRACE`), whichever comes
  first (`wait_for_the_agent`, `src/bin/sy_cognition.rs`).
  - The intake notes each answer and each refusal of an agent by its communication and the message
    it answers, and wakes the worker (`Answers`, `agents_answered`). The message is found as the
    refusal's is: the answer's trace before the agent's entry, which must be the trace's last
    (`answered_request`).
  - A refused turn is then left out as A-292 does, with no model call. One the agent took is
    analyzed as soon as its answer comes, or at the grace's end.
  - A turn older than the grace when the worker takes it, one that waited in the queue, does not
    wait. The agent's own answers, the relays and the turns with no person at their other end
    never wait.
  - New counters, `agent_waits_total` and `agent_waits_grace_over_total`, in `STATUS`
    (`turns_consumer`) and in `CONFIG_GET`'s `runtime`, with A-292's three
    (`refusals_total`, `refused_turns_dropped_total`, `refused_turns_not_analyzed_total`), which
    only `STATUS` had: SY.admin, and so Archi, reaches `CONFIG_GET` (`node_control_config_get`),
    not `STATUS`.
  - **Why the grace, of the three fixes:**
    - **Cancelling the call on the refusal does not save it.** The request has reached the
      provider, which bills its input at least, whatever the client does. The AI SDK records a
      dropped call `abandoned`, without tokens (`CallGuard`), so the cost would leave Fluxbee's
      accounts. It would also add a cancel path to the model call.
    - **Waiting for the agent's whole answer holds the worker.** The agent answers in seconds
      (4.3 s in A-292's gate), a minute with tools, or never (an agent that answers nothing, or a
      refusal its runtime could not send, `busy_answers_dropped`). An answer lost to `LINK_BUSY`
      still reaches cognition: the router copies the agent's send at entry, before routing it
      (A-292). The worker is serial, so every other communication's turns would wait behind it;
      setting a waiting turn aside would analyze a communication's turns out of order.
    - **The grace is bounded and covers the refusal.** A turn is analyzed at most 2 s late, and
      waits only when the worker comes to it within 2 s of its intake: a worker that far behind
      under load waits on no turn, so its throughput does not change. The wait does hold the
      serial worker, and the turns queued behind it (the review of the calls' build, below). It
      never loses a turn's analysis. The runtime refuses in 7 to 12 ms (A-257's gate), and on 8.x
      cognition took the refusal 0.04 to 0.26 s after it (A-292's gate), some eight times under
      the grace. The agent's answer ends it too, so a turn the agent took is not held for the
      whole grace.
  - **The choice is this branch's, not an operator's call.** The entry's *Fix, later* named two
    fixes, and no call of the operator's chose between them (board item 30 lists the day's
    calls, none on A-332). The branch took the first, for the reasons above: the operator may
    confirm it, or reverse it for the cancel. The operator kept the grace, at 5 s (below).
  - **What it leaves:**
    - A refusal later than 2 s (a batch the stream sends again, a slow intake) finds the call
      made, and A-292 drops its result, as before. When that call fails (no answer from the
      provider within its 8 s), the turn counts as a provider error
      (`semantic_tagger_provider_errors_total`) and not in `refused_turns_not_analyzed_total`: the
      failure skips the turn before A-292's checks, as on 0.1.138.
    - With an LLM agent, which answers in seconds, and a worker with nothing else to do, a
      person's turn is analyzed 2 s later than on 0.1.138; what it derives reaches the next
      message's package, which comes seconds later.
    - A cognition that cannot call a model (its `ai` settings unusable, or no key) waits too, and
      its wait counters count those turns. Kept on purpose: the wait costs no call, and a refused
      turn is then counted as refused, not as a tagger failure.
    - Whether `agent_waits_total` rises depends on the stream's batches: a turn and an answer that
      come in one batch (an answer within milliseconds) find the answer at the wait's first look,
      with no wait to count.
  - **Tests:** `a_refusal_after_the_worker_took_its_turn_costs_no_model_call` (the bug: the
    worker on the turn when the refusal comes 40 ms later; without the wait, one tagger call),
    `a_turn_the_agent_took_is_analyzed_as_soon_as_it_answers` (an answer 300 ms later; without the
    intake's note of the answers, analyzed at 2 s) and
    `a_turn_with_no_answer_is_analyzed_once_its_grace_is_over`, the three on paused time; the unit
    tests `an_answer_names_the_message_it_answers_by_the_agents_entry` and
    `the_answers_leave_once_no_wait_looks_for_them`. A-292's tests pass as they
    were, their busy answer built by the shared `reply_to`. Docs:
    [sy-cognition.md](../../docs/nodes/sy-cognition.md),
    [cognition.md](../../docs/reference/cognition.md) §3, §3.4, §8 and §10,
    [cognition-hands-v1](../../docs/cognition-hands-v1.md) §6.
  - **From its review (2026-11-12):** no change to the code's behaviour. This record and the docs
    now say what the review found: an answer lost to `LINK_BUSY` reaches cognition; the five
    counters are in `CONFIG_GET`; a refused turn whose late call fails is a provider error; a
    cognition that cannot call a model waits too; and the fix was the branch's choice. Gate0239's
    plan reads the 0.1.138 baseline from `CONFIG_GET` and the journal (no refused-not-analyzed
    count there), accepts `agent_waits_total` +0 when a turn and its answer share a batch, and
    expects two usage records on a trace whose answer is analyzed too.
- **The operator's call (2026-11-12), a change:** the grace goes from 2 s to 5 s:
  *"timeout para esto es peligroso, yo ahora podría 5sec y dejaría anotado para revisar en mejoras
  posteriores"*: a timeout here is dangerous; 5 s for now, recorded to review in a later
  improvement.
- **Built at 0.1.139's integration (`coa/sweep-139`):** `COGNITION_REFUSAL_GRACE` is 5 s. What
  the build says above of 2 s holds of 5 s now: a refusal later than 5 s finds the call made (one
  in a batch the stream re-sends after its 5 s ack timeout, for one), and a person's turn the
  worker comes to within 5 s of its intake is analyzed once the agent answers it, or 5 s after its
  intake at most, where 0.1.138 analyzed it at once; the turns queued behind it wait with it.
  `COGNITION_ANSWERS_KEPT` stays 4,096 answers: some 820 a second over the grace, where it was
  2,048 a second over 2 s. Tests: `a_refusal_seconds_late_still_costs_no_model_call` (a refusal the
  intake takes 3 s after the turn, as after a slow write of the batch before it, or in a batch the
  stream re-sent after a quick failure: no model call; with 2 s, one)
  and `a_turn_with_no_answer_is_analyzed_once_its_grace_is_over` (analyzed 5 s after the intake,
  the value written out); both fail with 2 s. Docs:
  [sy-cognition.md](../../docs/nodes/sy-cognition.md),
  [cognition.md](../../docs/reference/cognition.md) §3 and §3.4,
  [cognition-hands-v1](../../docs/cognition-hands-v1.md) §6. **To review later:** the timeout
  itself, the operator's note; the cancel (the entry's second fix) stays the other way. With the
  timeout, two things the review of this build found (below): the wait holds the serial worker,
  the turns queued behind a waiting turn with it; and the grace equals the stream's ack timeout,
  so a refusal in a batch re-sent after it always comes too late.
- **From the review of the calls' build (2026-11-12, on `coa/sweep-139`):** two findings on this
  entry's texts, fixed; the code is unchanged.
  - **The wait holds the serial worker:** `run_turn_worker` awaits each turn, and `handle_turn`
    awaits `wait_for_the_agent`. While a person's turn waits, every turn queued behind it waits
    too, up to 5 s from that turn's intake: other communications' turns, and those that never
    wait (the agent's answers, the relays, the turns with no person). The comment on
    `COGNITION_REFUSAL_GRACE` and this entry said the wait held a turn back only when the worker
    had nothing else to do, and that the worker's throughput did not change. It is bounded: a
    turn that spent 5 s in the queue does not wait again. So each time the worker catches up it
    adds at most 5 s (2 s before) to the turn at its head and the turns behind it, and may sit
    idle up to 5 s; its throughput is unchanged only while the queue stays 5 s or more behind.
    With an LLM agent that answers in more than 5 s, nearly every person's turn on an idle worker
    waits the whole 5 s. Code options, for later: wait only when the queue is empty, or let other
    communications' turns pass a waiting one, which keeps each communication's turns in order.
  - **5 s does not cover a batch the stream re-sends after its ack timeout:** the router's stream
    re-sends a batch with no ack only after `STREAM_ACK_TIMEOUT_SECS`, 5 s, plus at least 0.5 s
    of backoff (`src/router/stream.rs`), and sends no next batch before one is acked. A refusal
    whose own batch was lost, or that waited behind a batch whose ack was lost, reaches cognition
    5.5 s or more after its first send, past the grace: the tagger's call is made. Only a batch
    re-sent after a quick failure (backoff 0.5, then 1 and 2 s) fits within 5 s, and a refusal in
    its turn's own batch is found at the first look. The comment, the test
    `a_refusal_seconds_late_still_costs_no_model_call` and this entry said 5 s left room for a
    batch the stream sends again; they name the quick failure now, and say that one re-sent after
    the ack timeout comes too late.
  - Docs: [sy-cognition.md](../../docs/nodes/sy-cognition.md),
    [cognition.md](../../docs/reference/cognition.md) §3.4 and
    [cognition-hands-v1](../../docs/cognition-hands-v1.md) §6 say the wait holds the worker and
    the turns behind it; the first two, that a refusal in a batch re-sent after the ack timeout
    comes too late.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `8a44c199` (its review
  `8c3d877e`; the 5 s grace at the integration, `37ccd739`). worker1's cognition cut from its
  provider by nft, so each tagger call ends `abandoned` with no tokens; the agent played by a mesh
  client; `STATUS` read ten times a second. On 0.1.138 (01:16–01:18Z): a turn the agent refused
  busy had the tagger's call under way 0.213 s after the send (B1); an answer at 8 s, the call at
  0.104 s (B3). On 0.1.139 (02:09–02:15Z): G1, refused busy: `refusals_total` and
  `refused_turns_not_analyzed_total` +1 at 0.311 s, `semantic_tagger_calls_total` +0; G2, an answer
  at 0.3 s: the call at 0.416 s, the answer having ended the wait, `agent_waits_grace_over_total`
  +0; G3, an answer at 8 s: the call at 5.027 s, the grace over (+1); G4, a real ai.generic with no
  config (`AI.gate0239cu@worker1`): its `node_not_configured` at 0.104 s ended the wait, the call at
  0.109 s (its status read `RUNNING` `DEGRADED`, not `UNCONFIGURED`: A-345); G5, AI.pruebas' own
  busy refusal (its HTTPS cut; answered in 0.082 s): refused, not analyzed, no call. The usage: no
  SY.cognition record on a refused turn's trace (G1, G5); B1 1, B3 2, G2 2, G3 2, G4 1, all
  `abandoned`, none with tokens. At the deploy both hives' sy-cognition loaded their communications
  in 0.590 s and 0.965 s, `CONFIG_GET` carrying the five counters at 0. The grace's cost on an idle
  worker: G3's first call at 5.027 s, where 0.1.138 called at 0.104 s (the operator's timeout, to
  review later).

### A-331 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; a join onto the box of a connected hive refused `BOX_RUNS_ANOTHER_HIVE` in 29 s, nothing installed, where 0.1.137's failed `WORKER_ORCHESTRATOR_TIMEOUT` after 191 s; a box of no hive of this motherbee, one with a managed node's unit running and a motherbee's box refused with their remedies; (c) failed through A-347, the removal's cleanup race, not this fix) (found 2026-11-12 by 0.1.137's gate; seen on 8.x) — `add_hive` onto a box that already ran another hive id fails `WORKER_ORCHESTRATOR_TIMEOUT`

- **Seen on 8.x (0.1.137, gate0237, 2026-11-12 10:39–10:42Z):** VM 104 joined as `gate0237i` and
  was removed with its box cut off (`remote_cleanup_via` `local_only`), so the box kept its
  install. An `add_hive` of `gate0237j` onto the same box failed after 191 s with
  `WORKER_ORCHESTRATOR_TIMEOUT` (*"node 'SY.orchestrator@gate0237j' not observed ... (visible:
  RT.gateway.stream@gate0237j)"*): the box's router came up under the new name, its orchestrator
  did not. A fresh VM 104 joined as `gate0237j` in 829 s, and A-277's checks went on there. Whether
  an `add_hive` onto such a box failed so before 0.1.137 was not read.
- **Effect:** a box reused for another hive id needs a factory reset first, and nothing in the
  join's answer says so.
- **Fix, later (the orchestrator):** `add_hive` sees another hive's install on the box and refuses
  it with a code that says so.
- **Built on `coa/orch-139` (for 0.1.139; `9aec2c5d`).** The cause, read in the code: a worker
  join `restart`s the box's rt-gateway but only `start`s its sy-orchestrator, a no-op on one that
  runs, so the box's orchestrator stayed `gate0237i`; and no join restarts the other services it
  finds running. Each join (worker, egress, ingress) now reads the box once its sudo works and
  before it copies anything (`refuse_box_running_another_hive`): `/etc/fluxbee/hive.yaml`'s
  `hive_id` and `systemctl is-active` of the units `remove_hive`'s cleanup stops
  (`BOX_INSTALL_UNITS`). A hive.yaml that names another hive id while one of them runs refuses the
  join `BOX_RUNS_ANOTHER_HIVE`, not retryable, nothing installed, naming `box_hive_id`,
  `running_units` and what to do: `remove_hive` of that hive first, while the box can be reached,
  when the registry still lists it; otherwise `systemctl disable --now <units>` on the box, or a
  fresh box. A box joined again under its own name, one whose units a removal's cleanup stopped
  (its hive.yaml stays) and a fresh box join as before; a probe that fails is logged and the join
  goes on. Test `a_join_refuses_a_box_that_runs_another_hives_install` (fails without the refusal),
  with a source check that every flow probes after its sudo check and before its first copy.
  Docs: sy-orchestrator.md, operations.md, Archi's handbook. One SSH round trip more per join.
- **The review (2026-11-12), built on `coa/orch-139`:** (1) the remedies assumed the box was the
  one meant. A mistyped address that lands on a connected worker was told to remove that worker,
  and the motherbee's own address (its hive.yaml names the motherbee, which has no registry entry)
  to `systemctl disable --now` its whole core. Every message now starts with *"check the
  address"*: a box whose hive.yaml names this motherbee's hive id, or the role `motherbee`, is a
  motherbee and is offered no step that stops anything; a hive of the registry is named with its
  status and recorded address (`box_hive_status`, `box_hive_address`), and `remove_hive` is
  offered only to reuse that box (the probe passes this motherbee's hive id; `box_role` in the
  answer). Archi's handbook: confirm the address with the operator before staging any removal.
  Test `a_join_refusal_starts_from_the_address_and_never_stops_a_motherbee`. (2) The remedy for
  any other box stopped only the core units, and left the old hive's managed nodes: their units
  run on (`Restart=always`), and the next hive's reconcile relaunched each node directory under its
  old `@<hive>` name. The probe now also reads the managed nodes' units (`systemctl list-units
  'fluxbee-node-*'`, `activating` counted) and their directories, and refuses while one runs or a
  managed node's directory named for another hive is left (`running_node_units`, `left_node_dirs`;
  a core node's files under `nodes/SY/`, which no reconcile relaunches, do not count); the remedy
  names the cleanup's steps (`systemctl disable --now <units>; systemctl stop 'fluxbee-node-*';
  rm -rf /var/lib/fluxbee/nodes/* /var/lib/fluxbee/state/nodes/*`) or a fresh box. Test
  `a_join_refuses_a_box_whose_managed_nodes_another_hive_left`. Both fail without the review's
  fix.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-13, VM 104 from the template 9000):**
  shipped as `04f4aaa0` (its review `8bc3c5f3`). The first pass's VM steps failed at the key probe,
  the tool's (A-340). The rerun's (05:42–05:58Z), the motherbee's key put back before every join:
  (a) `add_hive gate0239j` onto the box of the connected `gate0239i` refused
  `BOX_RUNS_ANOTHER_HIVE` in 29 s, nothing installed (0.1.137: `WORKER_ORCHESTRATOR_TIMEOUT` after
  191 s), naming `box_hive_id` gate0239i, `box_role` worker, `box_hive_status` connected,
  `box_hive_address` 10.10.10.60 and its `running_units`, the message starting *"check the
  address: the box at 10.10.10.60 is hive 'gate0239i' of this motherbee (status connected; the
  address its entry records)"*; gate0239i untouched (its units' MainPIDs, the binaries' mtimes,
  the box's sudoers.d), and the refusal took the motherbee's key and the bootstrap sudoers off the
  box (B-2's revoke). (b) A box of no hive of this motherbee: *"runs hive 'gate0239i' (its
  /etc/fluxbee/hive.yaml), no hive of this motherbee"*, with the steps (41 s). Its managed nodes:
  refused again with `running_node_units` [`fluxbee-node-AI.gate0239n-gate0239i`] and
  `left_node_dirs` [`AI.gate0239n@gate0239i`] (*"the unit of 1 managed node runs"*), and the remedy
  run on the box left no node unit. A motherbee's box: *"is a motherbee, no box to join"* (35 s).
  (c), a box after a `socket_ok` removal joined under another id, failed: six of the removed hive's
  services still ran 35 s after the removal's answer, and the join was refused
  `BOX_RUNS_ANOTHER_HIVE` in 36 s, the guard right: the removal's cleanup race, A-347, older than
  this item.

### A-330 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; `GET /hives` left a planted `gate0239.bak` out in 0.105 s, where 0.1.137's gate saw such a directory listed; `GET /hives/gate0239.bak` 400 `INVALID_HIVE_ID`, warned of once) (found 2026-11-12 by 0.1.137's gate; seen on 8.x, read in the code; left by the integration's fix) — `list_hives` lists a registry directory whose name is not a hive id

- **Why:** `list_hives` (`src/bin/sy_orchestrator.rs` ~23774) lists every directory of the hive
  registry, by its name alone when it cannot read its `info.yaml` (it pushes `{"hive_id": name}`).
  The 0.1.137 integration (`a857cbb7`, `7423878d`) made the other readers skip such a name
  (`registry_directory_is_hive`, ~21832), not this one.
- **Seen on 8.x (0.1.137, gate0237, 2026-11-12 09:55Z, `a311-dir`):** a directory `gate0237.bak`
  planted in the motherbee's registry showed in `GET /hives`; `get_hive` refuses it
  `INVALID_HIVE_ID`; the runtime-use read, `/versions` and the Cloud's `list_nodes` left it out.
- **Effect:** the hive list shows an entry that is no hive, which every action on it refuses.
- **Fix, later (the orchestrator):** `list_hives` takes `registry_directory_is_hive`.
- **Built on `coa/orch-139` (for 0.1.139; `c7a733ac`).** `list_hives` (`list_hives_in`) leaves out
  a directory whose name is not a hive id (`registry_directory_is_hive`, warned of once); an entry
  being accepted, with no `info.yaml` yet, is still listed by its name, and this hive when the
  registry has no entry for it. Test `list_hives_lists_no_registry_directory_that_is_not_a_hive`
  (fails without it: `gate0237.bak`, `lost+found` and `worker 3` listed). Docs: sy-orchestrator.md.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-13 01:54Z):** shipped as `80919553`. A
  directory `gate0239.bak` planted in the motherbee's registry: `GET /hives` left it out (0.105 s),
  where 0.1.137's gate saw `gate0237.bak` listed; `GET /hives/gate0239.bak` 400 `INVALID_HIVE_ID`;
  the orchestrator warned of it once. The plant removed and checked gone.

### A-329 [General/Go SDK] ✅ VALIDATED ON 8.x (0.1.141, 2026-11-13; 2,000 messages at 100/s to a WF: 1,207 created and 793 answered `NOT_TAKEN node_busy`, retryable, none unaccounted for, where 0.1.140 created 283 and dropped 1,717 silently; SY.timer sent again 1,943 fires a WF answered `NOT_TAKEN`, up to 4 tries, all 3,000 conversations done within 98 s of their time, none lost, where 0.1.140 lost 867 of 1,500) (found 2026-11-12 by 0.1.137's gate, on 0.1.136; seen on 8.x, read in the code; there before it) — The Go SDK drops a user message when its command channel is full, and nothing goes back to the sender

- **Why:** the Go SDK's dispatcher hands a node's inbound messages to its `incoming` command
  channel, 64 deep (`go/fluxbee-go-sdk/dispatcher.go` ~466). When it is full, `noteCommandDrop`
  (~1062) counts the message (`CommandChannelDrops`) and logs ONE warning per channel for the
  node's life; nothing goes back to the sender.
- **Seen on 8.x (0.1.136, gate0237's `wf-base`, 2026-11-12 09:39–09:40Z):** 1000 user messages at
  50 a second from `IO.gate0237w@worker1` to `WF.gate0237w@worker1`, one new conversation each: the
  router took all 1000; the engine created about 30 a second (656 *"create accepted"* over 21.6 s);
  the SDK dropped 344 (the store held 656); the sender saw no `LINK_BUSY` and no `NOT_TAKEN`. Sent
  again at 10 a second, all 344 were created.
- **Effect:** any Go node (a WF, SY.timer, SY.wf-rules...) loses inbound messages the same way in a
  burst over its pace for about 2 s.
- **Fix, to decide (the Go SDK, alone):** backpressure instead of a drop, so the router's per-link
  queue answers `LINK_BUSY` (A-222), or `NOT_TAKEN` to the sender; system messages must not wait
  behind user ones.
- **The operator's call (2026-11-12):** *"Avisar al que mandó"*: the Go SDK answers `NOT_TAKEN`
  (retryable) to the sender for each message that does not fit, its buffer grows, and SY.timer sends
  again a fire it gets `NOT_TAKEN` for; system messages never wait behind user ones. The Go SDK's
  own release (SY.timer's resend with it), in the bug push of item 30.
- **Built on `coa/gosdk-141` (the Go SDK alone, planned 0.1.141; every Go binary takes it: SY.timer,
  SY.wf-rules, SY.opa.rules and the WF engine), the call as it reads:**
  - **`NOT_TAKEN` to the sender.** A message that does not fit its command channel is answered at
    once, never waited for: a `system` message to the sender's uuid, on its `trace_id` and
    `thread_id`, `{"ok": false, "error": "not_taken", "reason": "node_busy", "detail", "retryable":
    true}` (`BuildNotTaken`, `go/fluxbee-go-sdk/not_taken.go`; `refuseCommand`,
    `command_refusals.go`). A notice or a reply is never answered (`IsNoticeOrReply`: SY.timer's
    rule, and the WF's but for SY.vault's `VAULT_SECRET_CHANGED`, which a WF drops unanswered with
    every system message that is none of its verbs; the vault's notice since A-325's review), so
    two Go nodes never bounce one; a person's text, a user message without a name, is. Each refusal
    is counted (`CommandChannelDrops`, `NotTakenSent`: answered, unanswered, or whose `NOT_TAKEN`
    could not go) and logged, the first at once, then a line at most every 10 s per channel that
    counts what came since. The engine's own `NOT_TAKEN` takes the SDK's builder,
    and with it `retryable`, true only for `unavailable` (its store opens again). Today's
    `NOT_TAKEN` had no `retryable`; one without it is not retryable.
  - **The buffer grows from 64 messages to 1,024 messages or 8 MiB a lane, whichever comes first**
    (`command_queue.go`): the router's own bound for a node's link (`LINK_QUEUE_MAX_FRAMES`,
    `LINK_QUEUE_MAX_BYTES`, A-222), so a Go node holds what its link would. It holds this entry's
    burst (about 400 waiting at its end: 50 a second for 20 s against the engine's 30) two and a half
    times over, about 34 s of that WF's work; past that the sender hears so, rather than waiting
    longer without knowing. The bytes bound a lane's memory: 8 MiB where 1,024 frames of 128 KiB
    would be 128 MiB.
  - **System messages never wait behind user ones:** a command channel is a queue of two lanes,
    `system` and the rest, and its consumer gets the oldest system message first. A system message
    waits only for the message the consumer works on and, at most, the one handed over as it came,
    never for the backlog; each lane has its own bound, so a user burst never refuses a
    `NODE_STATUS_GET`, a `CONFIG_SET` or a `TIMER_FIRED`. The consumers' API is the same
    (`TakeCommandReceiver`).
  - **The other side of it, a consequence of the call (the review):** under a backlog, a WF's
    `TIMER_FIRED` or `WF_CANCEL_INSTANCE` (SY.admin's, a system request) goes before user messages
    that came before it, where the channel of 64 kept arrival order. A person's reply received in
    time but still queued can lose to the timeout's transition, and a reply queued behind a cancel
    finds no instance (from a `start_from` node it starts a new one). With 1,024 user messages at
    the measured 30 a second, a fire can overtake a reply that came up to about 34 s before it. Said
    in the handbook, the WF spec (§14.5), SY.wf-rules' page, the quickstart and wf-generic's README.
  - **SY.timer sends again a fire its target answers `NOT_TAKEN` retryable** (`notTaken`,
    `go/sy-timer/scheduler.go`), as one whose target is down: after 5, 10, 20 and 40 s, then each
    minute, while it is on time (an hour, a `fire_if_within`'s window, a recurring timer's next
    instance; a `drop` one-shot gets none), logged at each attempt (`timer_fire_undelivered`, and a
    plain line *"fire of <uuid> not taken by <target> (node_busy): sent again at <time>"* only once
    the attempt is queued; past its time `timer_fire_lost` and *"fire of <uuid> lost: ..."*, never a
    line that says it goes again: the review). Only the fire's target decides
    it, by the name its router stamps; one not retryable is not sent again (`timer_fire_refused`).
    SY.timer answers no `NOT_TAKEN` and no `<msg>.error` now (`isInfrastructureMessage`; before, a
    `NOT_TAKEN` got `TIMER_UNKNOWN_VERB`).
  - **Found on the way, built with it:** the connection's reader dropped a frame when the
    receiver's queue (256) was full and pushed an error, on which the dispatcher failed every
    pending request as disconnected and slept 50 ms: the reader waits for room now (backpressure,
    as the Rust SDK), the frames waiting in the router's queue for the link, which answers
    `LINK_BUSY` past its bound; the dispatcher never waits, so the wait is short. The Go request
    helpers (`SendSystemRPC`, `SendAdminRPC`, the timer client) end at once on the target's
    `NOT_TAKEN`, `RpcErrNotTaken` with its `Reason` and `Retryable`, where a system request ended as
    an invalid response and an admin request at its timeout. A WF takes SY.timer's `NOT_TAKEN` for
    a timer request as the request's refusal (`takeNotTaken`): its timer goes again 5 s later, where
    it waited for its 30 s without an answer; any other `NOT_TAKEN` it gets is logged.
  - **By its tests, each checked failing without its fix** (the SDK on the Mac, SY.timer's in a
    local Linux container, its tests being Linux-only): `TestAMessageThatDoesNotFitIsAnsweredNotTaken`,
    `TestANoticeOrAReplyThatDoesNotFitIsNeverAnswered`, `TestTheQueueHandsOverItsSystemMessagesFirst`,
    `TestASystemMessageThatComesDuringABacklogGoesNext`, `TestALaneHoldsAtMostItsMessagesAndBytes`,
    `TestTheLogSaysWhatTheQueueRefusedWithoutFlooding`, `TestARequestAnsweredNotTakenFailsWithItsReason`,
    `TestTheReaderWaitsForRoomInsteadOfDroppingAFrame`, and from the review
    `TestANotTakenThatCannotBeSentIsCountedAndLogged` (a `NOT_TAKEN` the node's full send queue
    could not take; the tests read every frame under a deadline, so a fix taken away fails in 2 s,
    not at go test's 10 minutes) (the SDK);
    `TestAFireItsTargetAnsweredNotTakenIsSentAgain` (with its log lines since the review),
    `TestANotTakenOfAnotherNodeLeavesTheFireWaiting`,
    and `TestRouterNoticesAndRepliesGetNoAnswer`'s two new cases (SY.timer); the WF's
    `TestAScheduleSYTimerDoesNotTakeGoesAgainUntilItDoes` (SY.timer's queue full), and its
    `NOT_TAKEN` checks hold `retryable`.
  - **Mixed versions (the deploy only):** no Go node stays on the old SDK past the deploy. SY.timer,
    SY.wf-rules and SY.opa.rules come from the package, and every WF node runs its package's base,
    wf.engine's `current`, which it moves to within a minute of its files reaching its hive, pinned
    or not (`docs/nodes/sy-orchestrator.md`, "A running node and a new runtime version"); the
    orchestrator runs no WF node outside a workflow package (`WF_RUNTIME_PACKAGE_REQUIRED`). The
    review's correction: an earlier text here named "a WF pinned to an older wf-generic", which
    cannot be. In the deploy's window (the hives one after the other; on a hive, its WF nodes'
    relaunch before or after its core's, A-302) a node on the old Go SDK never sends `NOT_TAKEN`,
    holds 64 and drops the rest silently, and drops a `NOT_TAKEN` it gets as a system message; an
    old SY.timer answers a new WF's `NOT_TAKEN` for a fire `TIMER_UNKNOWN_VERB` and does not send it
    again (the fire lost, as before), which the new WF logs and drops (a reply, the request it would
    settle unknown). Rust senders: `send_system_rpc` ends as `InvalidResponse` at once, where it
    timed out; `send_admin_rpc` still waits for its timeout; io.slack shows the WF's `node_busy` as
    *"Not taken."* (a reason it does not know, with a `warn` line); ai.generic answers it with a
    `CONFIG_RESPONSE` `unknown_system_msg`, which the WF drops as a reply. No loop on any side: a
    `NOT_TAKEN` and every `*_RESPONSE` are replies, never answered.
  - **Left for the other releases of item 30, recorded here:** the AI SDK (0.1.142, with A-325): its
    `is_infrastructure_message` takes neither `NOT_TAKEN` nor `<msg>.error` as a reply, so ai.generic
    answers each `NOT_TAKEN` with an `unknown_system_msg` error; the Rust SDK (0.1.143): its request
    helpers take no `NOT_TAKEN` as a terminal error (`send_admin_rpc` times out, `send_system_rpc`
    loses its reason); the IO track: io.slack's text for `node_busy`, and an IO node's resend of a
    retryable `NOT_TAKEN`. Not changed, seen in the code: the Go dispatcher still fails every pending
    request when a frame does not parse as a message (a malformed frame from the router; never
    seen).
- **Validated on 8.x (0.1.141's gate, gate0240-0243, 2026-11-13; `scratchpad/gate-tools/r141_gate.py`, git-ignored; 152 PASS, 19 SEE, 1 FAIL (a status read's 1.5 s bound), no model call):**
  - **The baseline on 0.1.140 (16:18 to 16:38Z):** 2,000 messages at 100/s to `WF.gate0240@worker1`:
    283 created, 1,717 dropped with nothing back to the sender (no `NOT_TAKEN`, no `LINK_BUSY`), the
    old SDK's *"is FULL"* line once; a probe's timer fire behind them never taken; status reads
    p50 2.619 s, 6 of 7 inferred by the orchestrator. 1,500 conversations with one `fire_at`: SY.timer
    fired all 1,500, and 300 s later 867 instances were still waiting (their fires lost unseen).
  - **The deploy (mark 16:39:03Z, 245 s):** worker1 ran an old and a new Go SDK side by side for
    27.6 s (the WF relaunched first); nothing under load then, no line of the mixed window.
  - **After (16:44 to 17:11Z):** the same burst: 1,207 created and 793 answered `NOT_TAKEN`
    (`node_busy`, retryable, the detail naming 1,024 waiting), 2,000 of 2,000 accounted for, the
    first `NOT_TAKEN` once about 1,024 waited (11.13 s into the burst), the SDK's line at most every
    10 s, no `LINK_BUSY`; the refused sent again at 10/s, the store at 2,000. Status reads while
    900 or more waited: p50 0.528 s, none inferred (one at 2.029 s, over the gate's 1.5 s bound:
    its one FAIL); the probe's fire taken 0.143 s after SY.timer fired it. 3,000 conversations with
    one `fire_at`: SY.timer sent again the 1,943 fires the WF answered `NOT_TAKEN` (up to 4 tries;
    *"not taken by"* lines and `timer_fire_undelivered` 1,943 each), every conversation done within
    98.2 s of its time, each fire taken once, 0 lost and 0 refused. (A first run at 1,500 did not
    fill the lane: SY.timer fired 50.9 a second, the WF took 25.5.)
  - **By its tests:** the reader's backpressure, a notice or a reply never answered, the WF's
    take of SY.timer's `NOT_TAKEN` for a timer request, a `NOT_TAKEN` that cannot be sent.

### A-328 [General/admin] ✅ BUILT (0.1.139, 2026-11-12; not provoked on 8.x, it takes an executor answer past the mesh's frame: by SY.admin's and SY.architect's tests) (found 2026-11-12 by A-322's review; read in the code) — SY.admin's cut of an executor answer drops a refused step's `retryable`, `unanswered_hives` and `next`

- **Why:** when SY.admin cuts an executor answer to fit the mesh frame (A-198 step c),
  `executor_result_essentials` (`src/bin/sy_admin.rs` ~4235-4288) keeps of each step its status,
  its error code, its ids and a list of small fields (`wf_node`, `warning`, `pending`, `held`,
  `nothing_to_do`, `pages_left_up`, `system_ilks_retired`, `packages_left`, the update's view), not
  a refused step's `retryable`, `unanswered_hives` or `next`.
- **Effect:** for a run whose answer was cut, A-322's explanation turn (0.1.137) cannot say whether
  asking again may pass.
- **Fix, later:** the essentials keep those three fields, bounded; with SY.admin's next change.
- **Built on `coa/admin-139` for 0.1.139:** `executor_result_essentials` keeps a step's
  `retryable`, its `unanswered_hives` (each hive's `hive`, `reason` and `next`,
  `unanswered_hive_essentials`, which `packages_left_essentials` uses too, the hives that fit in
  1 KiB, the rest counted) and its `next` (256 bytes), which Archi's report reads
  (`payload_view` in `src/bin/sy_architect/execution_report.rs`). Its test,
  `an_executor_answer_without_its_outputs_keeps_a_refusals_retryable_and_hives`, fails without it.
  sy-admin.md's executor section says it.
- **Its review (2026-11-12), on `coa/admin-139`:** Archi's report dropped the count of the hives
  SY.admin cut (`{"truncated_items": n}`): `unanswered_hive_view` kept only an item with a `hive`.
  It now keeps the count as `{"more": n}`, which the report's text says as "<n> more", for a
  refusal's hives and for a version left's alike (`src/bin/sy_architect/execution_report.rs`, in
  0.1.139 with SY.admin). Its test, `the_hives_sy_admin_cut_from_a_refusal_are_counted`, fails
  without it.
- **Shipped in 0.1.139** (`e45fb83c`, its review `60ab8e30`, on daily). Not provoked on 8.x
  (gate0239, 2026-11-12/13): it takes an executor answer past the mesh's frame. By its tests,
  `an_executor_answer_without_its_outputs_keeps_a_refusals_retryable_and_hives` (SY.admin) and
  `the_hives_sy_admin_cut_from_a_refusal_are_counted` (SY.architect).

### A-327 [Archi] 🔵 low (found 2026-11-12 by A-322's review; read in the code; since 0.1.137) — Archi's explanation turn after a CONFIRM never reads the prompt cache

- **Why:** the turn sends no tools, and Anthropic caches a prompt as tools, then system, then
  messages, with the AI SDK's breakpoints on the last tool and on the system prompt
  (`crates/fluxbee_ai_sdk/src/llm.rs` ~1705, ~1929): without the tools, the turn's prompt never
  reads the tools-and-system entry the chat's turns wrote.
- **Effect:** each CONFIRM writes about 40,000 tokens to the cache (Claude Opus 5.5: $5 a million
  tokens for a 5-minute write, about $0.20 a run), where a read would cost $0.20 a million (about
  $0.01). Nothing breaks; recorded for later.
- **Fix, later:** the AI SDK gains `tool_choice` (`none`), in a release of its own; then the turn
  sends Archi's same tools with `tool_choice` `none` (a `tool_choice` change keeps the
  tools-and-system cache).
- **Built on `coa/aisdk-142` (planned 0.1.142, the AI SDK alone, with A-294, A-290 and A-325),
  the AI SDK's part:** its `ToolChoice` (`Auto`, the default, and `None`), on
  `FunctionCallingConfig` and each `FunctionModelTurnRequest`: under `None` Anthropic gets
  `tool_choice: {"type": "none"}` and OpenAI `tool_choice: "none"`; `Auto` sends nothing, so every
  other call's request is as before; a turn without tools sends none. The runner runs no tool under
  `None`: a turn that returns a call anyway ends the run with an error. Forced choices (`any`,
  `tool`, `required`) are left out: Claude Opus 5.5 and Sonnet 5.5 refuse them. The cache rule,
  checked against Anthropic's prompt-caching invalidation table: a `tool_choice` change keeps the
  tools and system caches and invalidates only the messages'. Tests:
  `every_turn_goes_with_the_runs_tool_choice`, `a_run_whose_tool_choice_is_none_runs_no_tool`,
  `an_anthropic_turn_under_tool_choice_none_starts_as_the_turns_before_it`,
  `an_openai_turn_under_tool_choice_none_sends_none`. No node sends `None` in 0.1.142, so it is
  checked by its tests alone.
- **SY.architect's part, built on the same branch and taken out of it by its review** (the AI SDK
  goes alone: board item 30 and the fix above), **for SY.architect's first release after
  0.1.142; its code is in the branch's A-327 commit:** the explanation turn shown the chat's tools
  (`architect_chat_tools`, the chat's own registry, by their definitions alone: `ShownTool`, which
  does nothing if called) under `ToolChoice::None`, so its prompt starts with the tools and system
  prompt the chat's turn cached; its request saying the turn *calls no tool*; Archi's tests
  `the_explanation_turn_gets_each_steps_answer_and_no_secret` (the chat's tools, under `None`) and
  `a_turn_that_calls_a_shown_tool_runs_nothing_and_falls_back`; docs/nodes/sy-architect.md and
  the handbook's §6.4. From the review, for that release: the turn's `token_usage` grows by the
  tools' tokens (the AI SDK's `tokens_used` counts cache reads), while its cost falls from a cache
  write of about 40,000 tokens to a cache read; and the cache lives 5 minutes from its last use,
  the chat's last model call, with the executor's run before the explanation's call, so its gate
  confirms within about 3 min of the chat's answer. To check on 8.x then: the
  `execution_report` usage record's `cache_read` about the 40,000 tokens it wrote before.
- **The AI SDK's part shipped in 0.1.142 (2026-11-13):** `tool_choice` (`ToolChoice::Auto`, the
  default, which sends nothing, and `ToolChoice::None`) has no caller in 0.1.142, so by its tests
  only; SY.architect's use of it, the fix itself, stays open for SY.architect's release.

### A-326 [General/SY.timer] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; SY.timer 1.7's first start built the four list indexes and dropped the two old ones, in 876 ms on worker1; the walks' page medians through SY.admin 0.090 to 0.106 s against 0.106 to 0.129 s on 0.1.138, a schedule's round trip 23 ms against 53 ms (medians); the 16 MiB cache: worker1's sy-timer 24,028 kB resident against 19,764 kB; the clean's purge of 4,430 timers 1,194 ms) (found 2026-11-12 by A-278's review; read in the code, its query plan checked with SQLite; there before it, more pages since A-278; seen live by 0.1.138's gate) — Each `TIMER_LIST` page reads every pending timer of the hive

- **Why:** SY.timer's `listTimers` (`go/sy-timer/persistence.go` ~281-323) asks
  `WHERE owner_l2_name=? AND status=?` (and the cursor) `ORDER BY fire_at_utc, uuid LIMIT limit+1`,
  the status bound as a parameter. Its indexes are `idx_timers_owner(owner_l2_name)`,
  `idx_timers_status(status)`, and two partial ones on `status='pending'`, which a bound status
  cannot use (~122-135), and SY.timer runs no `ANALYZE`. SQLite (3.50.4 and 3.51.0 on the Mac)
  plans `SEARCH timers USING INDEX idx_timers_status (status=?)` and then
  `USE TEMP B-TREE FOR ORDER BY`: it reads every pending timer of the hive, every owner's, and
  sorts the owner's past the cursor. With an index on `(owner_l2_name, status, fire_at_utc, uuid)`
  it searches that index, with no sort.
- **Effect:** a page costs in proportion to the hive's pending timers, not to its limit, and since
  A-278 a WF's reconcile asks a page per frame, about 210 to 240 of its timers: 13 or 14 pages
  for 3,000, 42 to 48 for 10,000. Measured on the Mac with python3's SQLite, a walk of one owner's
  10,000 timers: 5.6 ms a page with 15,000 pending on the hive, 14.8 ms with 100,000 (the hive's
  cap), and 3.5 ms with that index, whatever the hive holds (SY.timer's SQLite, modernc's, measured
  10 to 16 ms a page at 15,000 for A-278). SY.timer serves one request at a time (`run`,
  `go/sy-timer/main.go` ~267), so each page holds up the other owners' requests. Small on 8.x
  today, where no WF holds more than a handful of pending timers (A-278).
- **Fix, later (SY.timer's own release):** that index, created at the start as the others are;
  then, a smaller gain, a page that reads only what its frame can hold (it reads `limit+1` rows,
  1001, where about 238 of a WF's timers fit).
- **Seen live (0.1.138's gate, 2026-11-12, its clean-up at 13:31–13:32Z):** the walk of
  `WF.gate0238@worker1`'s timers of any status (`status_filter=all`), 3,000, all pending: 13
  pages in 8.79 s, its slowest page 7.204 s, where the pending pages of the same 3,000 timers took
  0.14 to 0.21 s: a page of another status filter reads far more. SY.timer serves one request at
  a time.
- **Built on `coa/timer-139` (for 0.1.139, SY.timer 1.7):**
  - **An index for each filter, in the list's order** (`ensureTimerSchema`):
    `idx_timers_list_owner_status` on (owner_l2_name, status, fire_at_utc, uuid), the entry's;
    `idx_timers_list_owner` on (owner_l2_name, fire_at_utc, uuid), for `status_filter` `all`, the
    walk 0.1.138's gate timed; and `idx_timers_list_status` and `idx_timers_list`, for SY.admin's
    pages of the whole hive in one status or in any, which read and sorted every timer of the status,
    or the whole table, the same way. Created at every start, as the others are; the first start of
    1.7 builds them over the table it finds (on the Mac 0.18 s for 15,000 timers, 2.7 s for 200,000:
    the hive's cap of 100,000 pending and as many fired). They take room (the file 5.8 to 9.8 MB at
    15,000 timers, 67 to 120 MB at 200,000) and slow a timer's writes on the Mac (an insert 289 to
    376 µs, a fire's read and save 358 to 462 µs). `idx_timers_owner` and `idx_timers_status`, which
    lead two of them, stay: an older SY.timer's start would create them again. An older SY.timer
    started on the table leaves the new ones as they are, and its lists read them too.
  - **The cursor a row value** (`timerListQuery`): `(fire_at_utc, uuid) > (?, ?)`, which SQLite
    searches the index for, so a page starts at its place. With the old
    `(fire_at_utc > ? OR (fire_at_utc = ? AND uuid > ?))` a page read its index from the start of
    the owner's timers and filtered out those before its cursor.
  - **A page that reads no further than its frame** (`takeTimerPage`; the frame's cut moves from
    `respondTimerList` into the read): it reads the timers one at a time and stops at the first it
    cannot take, past its limit or its frame, which says more follow: what it lists and one more,
    about 229 of a WF's timers, where it read 1001. The pages, their cursors and their errors are
    the same: a timer that cannot be answered is still `TIMER_INTERNAL`, the database's error
    `TIMER_STORAGE_ERROR`.
  - **The plans** (EXPLAIN QUERY PLAN with the parameters bound, modernc's SQLite): before, every
    filter's page used a temp B-tree for its order (an owner's in a status on `idx_timers_status`,
    an owner's in any on `idx_timers_owner`, the hive's on `idx_timers_status` or a scan of the
    table); after, each searches its index, the cursor in the search, with nothing to sort. Every
    other statement's plan is the same or better: the start's replay reads `idx_timers_list_status`
    in order, with no sort; and the owner's pending count, which each new timer's cap check runs
    (`checkPendingLimits`), and `TIMER_PURGE_OWNER`'s first delete read the owner's pending entries
    in `idx_timers_list_owner_status` alone, where both searched `idx_timers_status` and read every
    pending timer of the hive: on the Mac the count took 11.0 ms with 18,000 pending timers in the
    hive and 58.3 ms with 100,000, and takes 0.48 ms and 1.75 ms. What stays: the hive's count in
    the same check reads `idx_timers_status`'s pending entries, about 2 ms at 18,000 and 10 ms at
    100,000 there, before and after.
  - **Tests:** `TestATimerListPageReadsAnIndexInTheListsOrder` (each filter's plan, with and
    without a cursor, on a new table and on SY.timer 1.4's after the start: without the indexes it
    fails on `idx_timers_status` and a temp B-tree, and with the old cursor on a search with no
    cursor in it) and `TestATimerListPageReadsNoTimerPastTheFirstItCannotTake` (with 1.6's read it
    fails: *"the page of 85 timers read 1001, want 86"*). SY.timer's page tests pass unchanged
    (`TestTimerListAnswersInPagesThatFitAFrame`, and every filter, scope and limit of
    `TestATimerListPageItsLimitCutsSaysWhereTheNextOneStarts`), on Linux with the race detector.
  - **Measured on the Mac** (the same walk on 1.6 and on 1.7: SQLite and the encoding, no router;
    the median page of the best of 3 to 5 walks, limit 1000, about 250 of these timers a page):

    | A page | 1.6 | 1.7 |
    |---|---|---|
    | An owner's 3,000 pending, 15,000 pending of other owners | 14.7 ms | 1.5 ms |
    | The same owner's, `all` | 8.6 ms | 1.8 ms |
    | An owner's 10,000 pending, 15,000 of other owners | 21.8 ms | 1.9 ms |
    | An owner's 10,000 pending, 90,000 of other owners (the hive's cap) | 47.1 ms | 2.1 ms |
    | An owner's 3,000 pending and 20,000 fired, `all` | 31.4 ms | 2.7 ms |
    | The hive's pending (SY.admin), 100,000 | 64.5 ms | 2.5 ms |
    | The hive's `all` (SY.admin), 100,000 | 48.4 ms | 2.5 ms |

    The index alone, a page reading its 1001 timers and then cutting at the frame, took 5.1 ms in
    the first row's case, where 1.7 takes 1.5 ms: the read that stops at the frame is most of the
    rest. On a busy Mac (other builds running) the first row's case took 27 ms on 1.6 and 3 ms on
    1.7, about the same ratio.
  - **0.1.138's 7.204 s page is not this plan's cost:** the same walk on the Mac (`all`, an owner's
    3,000) took 8.6 ms a page on 1.6, and the 12 other pages of that walk on 8.x took about 0.13 s
    each through SY.admin. What held that one page up was not found: SY.timer logs no request's
    time, and the router's and SY.admin's part of it was not measured. 0.1.139's gate times the
    walk page by page on 0.1.138 and again on 0.1.139.
- **The review (2026-11-12), applied on `coa/timer-139`:**
  - **The purge's time (medium).** `TIMER_PURGE_OWNER` deletes each of the owner's timers from the
    table and from every index, in one transaction, and the orchestrator waits 3 s for its answer: a
    purge that misses them stops the node's removal (`TIMER_PURGE_FAILED`; running it again finishes
    it). The four indexes lengthen it. Measured with SY.timer's SQLite (modernc) on the Mac, the
    medians of 5 runs taken in turn, the timers scheduled ten at a time and interleaved over time:

    | The owner's timers, among the hive's pending | 1.6 | 1.7 as first built | 1.7 after the review |
    |---|---|---|---|
    | 7,500 pending, among 18,000 (the gate's) | 130 ms | 211 ms | 206 ms |
    | 7,500 pending and 20,000 fired, among 18,000 | 305 ms | 901 ms | 668 ms |
    | 10,000 pending, among 100,000 | 438 ms | 819 ms | 757 ms |
    | 10,000 pending and 20,000 fired, among 100,000 | 911 ms | 1,981 ms | 1,504 ms |

    The slowest run of the last case took 2.06 s. With the Mac busier (a load of 12 on 16 cores),
    single runs at 100,000 pending took 3.1 to 5.1 s on both 1.7s, none over 1.6 s on 1.6. The
    owner's history counts most, and SY.timer deletes history only at a start, so it can grow for
    weeks.
  - **Two indexes dropped.** EXPLAIN over every statement (modernc, with the three old indexes and
    the four new): `idx_timers_fire_at` is read by none; `idx_timers_owner` only by the purge's
    delete of the owner's timers, which `idx_timers_list_owner` serves as well; `idx_timers_status`
    by the hive's count of pending timers, which each new timer runs, and by the start's GC. So the
    start drops `idx_timers_owner` and `idx_timers_fire_at` once it has the four (`retiredTimerIndexes`,
    `ensureTimerIndexes`), and keeps `idx_timers_status`: its entries are narrower, and the hive's
    count read it in 8 to 11 ms at 100,000 pending, where `idx_timers_list_status` took 13 to 16.
    The purge is 2 to 26% faster than 1.7 as first built (the table above), still 1.6 to 2.2 times
    1.6's. A rollback's start builds the two again, and the next start of 1.7 drops them again.
  - **The builds best-effort (low).** An index the start cannot build (a disk short of room for the
    first start's build over the whole table) goes in `indexes_failed` with its error, and the start
    goes on without it; it keeps the two old indexes while one of the four is missing, and the next
    start tries again. Before, the failure stopped the start (`log.Fatalf`), and systemd started
    SY.timer again every 5 s, failing each time.
  - **The start says what it did** (the gate's measurement of the build): `timer_startup` and the
    ready line give `indexes_built`, `indexes_dropped`, `indexes_ms` (the index step's own time) and
    `indexes_failed`, when the start built or dropped an index or could not (`startupIndexes`); the
    first start of 1.7 says `indexes_built=4 indexes_dropped=2`. The interval from systemd's
    `Started` line to the ready line also holds the identity wait and the router's connection,
    which come first. Measured on the Mac on 1.6's table: 0.14 s at 18,000 pending timers, 1.8 to
    1.9 s at 200,000 (100,000 pending and 100,000 fired); the pages in use from 8.1 to 12.0 MB and
    from 81 to 126 MB, the dropped indexes' pages left free in the file.
  - **The purge's line gives its time:** `timer_owner_purged` has `took_ms`, the delete's own time.
  - **Tests:** `TestTheStartBuildsTheListIndexesAndDropsTheTwoTheyReplace` (a new table, SY.timer
    1.4's, a second start; the purge's two deletes and the hive's count read their indexes),
    `TestAStartThatCannotBuildAnIndexGoesOnWithoutIt` (the build fails on a name a table holds),
    `TestSYTimer14KeepsWorkingOnAVersion4Table` (1.4's start builds the two again, 1.7's drops them),
    `TestAStartSaysWhatItDidToTheIndexes`, `TestATimerListOfATimerThatCannotBeAnsweredIsInternal`
    (a stored payload that is not JSON: `TIMER_INTERNAL`, and the refusal's line gives no `error`;
    no test covered the code's move into the read), `TestAPurgedOwnersTimersLeaveTheHeap`
    (`took_ms`), and the plan test's case of the hive's pending timers, the default list. Each
    checked to fail without its part: the drop, the best-effort build, the wait for the four, the
    code the read's error carries, the report, `took_ms`.
  - **Wording:** before 1.7 a page read every timer of the hive in its status and sorted the
    owner's past its cursor, not all of them (the code's comments, `docs/sy-timer.md` §7.6, the
    node page).
  - **A lever measured, not taken:** SY.timer runs with SQLite's default page cache, 2 MB per
    connection, and a large purge spills to the WAL as it goes. With `cache_size` at 16 MiB the
    purge of 10,000 pending and 20,000 fired among 100,000 took 1.12 s on 1.7 as first built, where
    it took 2.2 s with the default, and 0.54 s on 1.6. It costs memory on every hive, up to that
    much for each connection of the pool, so it is the operator's call if the gate's purge times
    ask for it.
- **The operator's call (2026-11-12), a change:** SY.timer's SQLite page cache goes to 16 MiB per
  connection now (*"Subirla ahora"*).
- **Built at 0.1.139's integration (`coa/sweep-139`), SY.timer 1.7:** `PRAGMA cache_size` at
  -16384 (16 MiB, whatever the page size) in `timerDSN`, so modernc's SQLite sets it on every
  connection the pool opens, as `busy_timeout` and WAL (`timerPageCacheKiB`,
  `go/sy-timer/persistence.go`). Test: `TestEveryConnectionGetsThePragmas` reads it on three
  connections at once; without it each says -2000.
  - **Measured as this entry's review did** (on the Mac, SY.timer's SQLite, the purge's delete
    alone on a copy of the table, its timers and the others' scheduled ten at a time and
    interleaved over time; the medians of 10 runs, two series of 5 taken in turn, with a load of
    about 2.7 on 16 cores, quieter than the review's):

    | The owner's timers, among the hive's pending | 1.6 | 1.7 as first built | 1.7, default cache | 1.7, 16 MiB |
    |---|---|---|---|---|
    | 7,500 pending, among 18,000 | 116 ms | 202 ms | 176 ms | 166 ms |
    | 7,500 pending and 20,000 fired, among 18,000 | 288 ms | 836 ms | 630 ms | 424 ms |
    | 10,000 pending, among 100,000 | 384 ms | 697 ms | 635 ms | 554 ms |
    | 10,000 pending and 20,000 fired, among 100,000 | 652 ms | 1,368 ms | 1,128 ms | 892 ms |

    The call's case, 10,000 pending and 20,000 fired among 100,000: 1.13 s with the default cache,
    0.89 s with 16 MiB, a fifth less, its slowest run 1.20 s, within the orchestrator's 3 s. The
    history case among 18,000 gains a third (630 to 424 ms), the pending alone among 18,000 a
    twentieth (176 to 166 ms).
  - **Its memory** (measured on the same Mac, a process of its own for each): a connection takes a
    page of the cache only when a statement reads or writes it, and keeps it until the connection
    closes. In modernc's SQLite a cached page of 4 KiB holds about 8 KiB: after a read of every
    page of an 82 MB `timers.db` (the call's case), a connection's cache held 3.8 MiB with the
    default cache, 15.5 MiB at 8 MiB, 31.0 MiB at 16 MiB and 62.1 MiB at 32 MiB (the process's
    resident memory, the connection idle less the same once it closed), all of it freed once the
    connection closed. After the call's purge the process held 43 MiB with the connection idle,
    16 MiB with the default, 12 MiB once it closed, either way. The pool keeps two connections
    (`database/sql`'s default idle ones; SY.timer serves one request at a time beside its
    scheduler): up to about 62 MiB on a hive whose `timers.db` outgrows the cache (some 25,000
    timers with 1.7's indexes), where the default kept up to about 8 MiB; on a smaller hive, what
    its pages take. `docs/sy-timer.md` §5.2 and §7.7 and the node page say it.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `5499bb17` (its review
  `a5aa0d86`; the 16 MiB cache at the integration, `37ccd739`). worker1 held 7,430 pending timers of
  the gate's WFs (3,000 and 4,430): the setup stopped there on 0.1.138, where SY.timer left 1,220
  schedules unanswered in 30 s under the load of its full-table walks. At the deploy SY.timer 1.7's
  first start said `indexes_built=4 indexes_dropped=2`: worker1's index step 876 ms (Started to
  ready 4.834 s; a plain restart after it 2.505 s, its ready line naming no index), the
  motherbee's 82 ms (0.653 s); `timer_startup` named the four `idx_timers_list*` built and
  `idx_timers_owner` and `idx_timers_fire_at` dropped; `user_version` 4. The walks through
  SY.admin, three rounds each, their page medians on 0.1.138 and on 0.1.139: an owner's pending
  0.106 and 0.102 s, its `all` 0.124 and 0.106 s, the hive's pending 0.127 and 0.090 s, its `all`
  0.129 and 0.092 s (SY.admin's share about 0.1 s a page), the same pages and digests; one page of
  the first round took 5.8 s, not read. A schedule's round trip: median 53 against 23 ms, p95 81
  against 46 ms. On worker1 the four new indexes take 2.2 MB, the two dropped took 0.4 MB, the
  table 2.6 MB. The 16 MiB cache: worker1's sy-timer resident at 24,028 kB (its peak 32,776 kB)
  against 19,764 kB on 0.1.138, the motherbee's 10,156 against 10,272 kB. The clean's purges
  (`timer_owner_purged`'s `took_ms`): 4,430 timers in 1,194 ms and 3,900 (3,000 pending and the
  cancelled rows) in 434 ms, within the orchestrator's 3 s. An index that fails to build was not
  provoked: by its test.

### A-325 [General/AI runners] ✅ VALIDATED ON 8.x (0.1.141 and 0.1.142, 2026-11-13; SY.timer (0.1.141), ai.generic and SY.frontdesk.gov (0.1.142) leave `VAULT_SECRET_CHANGED` unanswered, where they answered an error; an unknown verb still answered) (found 2026-11-12 by the review of the AI SDK's 0.1.136; read in the code; there before it) — ai.generic answers each `VAULT_SECRET_CHANGED` broadcast with an `unknown_system_msg` error, which SY.vault drops

- **Why:** SY.vault broadcasts `VAULT_SECRET_CHANGED`, a `system` message, at each change of a
  secret, and one for each secret it announces at its start (`emit_bootstrap_secret_broadcasts`,
  `src/bin/sy_vault.rs` ~714). The AI SDK hands it to the node, since it comes from a named sender
  and is no notice or reply (`is_infrastructure_message`,
  `crates/fluxbee_ai_sdk/src/control_plane.rs` ~114). ai.generic's control plane answers any such
  message that is none of its verbs with a `CONFIG_RESPONSE` error `unknown_system_msg`
  (`nodes/ai/ai-generic/src/bin/ai_node_runner.rs` ~1361-1373; its test pins it for this
  broadcast, ~12455-12460), and SY.vault's loop skips every message that is not a vault request
  (`src/bin/sy_vault.rs` ~331-343). The frontdesk takes the broadcast and answers nothing.
- **Effect:** one wasted message per AI node per vault change; nothing is refused or lost.
- **Fix, later:** ai.generic leaves the broadcast unanswered, as the frontdesk does, or the AI SDK's
  notice list takes it (ai.generic's test changes with it); with ai.generic's or the AI SDK's next
  change.
- **Its SY.timer sibling, built on `coa/gosdk-141` (the Go SDK's release, planned 0.1.141):** SY.timer
  answered each broadcast `TIMER_UNKNOWN_VERB` the same way (`handleMessage`'s default arm,
  `go/sy-timer/main.go`), and SY.vault dropped it. It now takes the broadcast as a notice
  (`isInfrastructureMessage`, by the Go SDK's new `MSGVaultSecretChanged`): no answer, one
  `timer_message_ignored` line bounded per sender, never a refusal's line; by
  `TestTheVaultsSecretChangedBroadcastGetsNoAnswer` (a change's broadcast and a start's, in a
  Linux container; it fails without the fix). The WF engine, SY.wf-rules and SY.opa.rules already
  leave it unanswered (a system message that is none of their verbs; the WF's by
  `TestTheNoticesAreTheAINodesAndSYTimers` since the review). **The review of that build (read in
  the code, and a probe):** the Go SDK's `NOT_TAKEN` `node_busy` (A-329, new in the same release)
  still answered it to SY.vault from a Go node whose system lane was full, since `IsNoticeOrReply`
  did not name it, though the SDK's comment and the protocol reference said the notice is never
  answered. Now `IsNoticeOrReply` names it (`MSGVaultSecretChanged`), so no Go node answers it
  either way; by `TestANoticeOrAReplyThatDoesNotFitIsNeverAnswered`'s two vault cases (a change's
  broadcast and a start's), which fail without the name (a `NOT_TAKEN` `node_busy` to
  `vault-uuid`).
- **Its Go sibling validated on 8.x (0.1.141's gate, gate0240-0243, 2026-11-13; `scratchpad/gate-tools/r141_gate.py`, git-ignored; 152 PASS, 19 SEE, 1 FAIL (a status read's 1.5 s bound), no model call):** on 0.1.140 SY.timer@worker1 answered a
  `VAULT_SECRET_CHANGED` from a mesh client `TIMER_UNKNOWN_VERB` in 0.01 s and logged a refusal; on
  0.1.141 it gave no answer within 5 s and logged one `timer_message_ignored`, no refusal, while an
  unknown verb from the same client was still answered `TIMER_UNKNOWN_VERB` within 1 s.
- **Built on `coa/aisdk-142` (planned 0.1.142, with A-294, A-290 and A-327's `tool_choice`):** the
  first: the AI SDK's `is_vault_secret_changed` (the frontdesk's own check, moved there), and
  ai.generic's control plane leaves the broadcast unanswered, a debug line, as the frontdesk does
  (its key is read from SY.vault at each model call, so it has nothing to refresh); any other
  `system` message from a node that is no control verb is still answered `unknown_system_msg`.
  The notice list (`is_infrastructure_message`) is unchanged: SY.timer's and wf-generic's are the
  same rule, name by name (A-218). Test: ai.generic's
  `the_control_plane_refuses_every_other_caller` (no answer, through `on_message` too), failing on
  the old answer; the SDK's `the_vault_broadcast_is_told_apart`.
- **Its sibling, found by this build (read in the code, not fixed here):** SY.timer answers each
  `VAULT_SECRET_CHANGED` broadcast with a `TIMER_RESPONSE` error `TIMER_UNKNOWN_VERB`
  (`handleMessage`, `go/sy-timer/main.go` ~286-340: a `system` message from a named sender that is
  none of its verbs goes to the `default` arm; `isInfrastructureMessage` does not name it), one per
  hive's SY.timer at each change and per secret at SY.vault's start, which SY.vault drops. Low;
  with SY.timer's next change (it leaves the broadcast unanswered, as the AI nodes and wf-generic
  do). Not on board item 30: SY.timer's 0.1.139 (`coa/timer-139`) was built before it was found.
  It takes an A-number of its own when this branch is integrated, unless that release takes it
  first.
- **Validated on 8.x for ai.generic (0.1.142's gate, gate0242, 2026-11-13; `scratchpad/gate-tools/r142_gate.py`, git-ignored; 97 PASS, 1 SEE, 0 FAIL, no model call):** on 0.1.141 AI.pruebas@worker1 answered a
  `VAULT_SECRET_CHANGED` `CONFIG_RESPONSE` `unknown_system_msg` in 0.015 s; on 0.1.142 no answer
  within 5 s (its metrics: processed +1, responses +0), while an unknown verb from the same client
  was still answered `unknown_system_msg` in 0.005 s.

### A-324 [General/admin] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; SY.admin's help of `node_help`, read on 8.x, carries the new note: an AI node on ai.generic 0.1.136 or later answers at once however busy; the orchestrator's three are comments) (found 2026-11-12 by the review of the AI SDK's 0.1.136; read in the code; since 0.1.136) — SY.admin's `node_help` note and three orchestrator comments still say a busy AI node may miss the deadline

- **Why:** since 0.1.136 (A-257) an AI node on ai.generic 0.1.136 or later answers `NODE_HELP` and
  `NODE_STATUS_GET` at once however busy, and only one pinned to an older ai.generic may still miss
  the 3 s. The review of `coa/aisdk-136` (`ba2addb2`) changed Archi's handbook and SY.admin's and
  SY.orchestrator's pages, not the binaries' own texts: SY.admin's `node_help` note
  (`src/bin/sy_admin.rs` ~17528, *"did not answer within 3 s (a busy node may not)"*), which Archi
  reads, and three comments in the orchestrator (`src/bin/sy_orchestrator.rs` ~161-162,
  ~28061-28062, ~55873-55874).
- **Effect:** Archi reads that a busy node may not answer, where on ai.generic 0.1.136 or later a
  silence of 3 s means the node is stuck or restarting, or its 64 places for control requests were
  all taken. Nothing is refused or done by it.
- **Fix, later:** each text says what the pages say (the release, and the older case); with
  SY.admin's and the orchestrator's next text changes.
- **SY.admin's part built on `coa/admin-139` for 0.1.139:** `node_help`'s note says what the pages
  say: an AI node on ai.generic 0.1.136 or later answers at once however busy, so a silence of 3 s
  means it is stuck or restarting or its 64 places for control requests were taken (its journal
  says which), and only one pinned to an older ai.generic may still miss the 3 s while its workers
  are busy. Its test, `node_help_says_a_busy_ai_node_answers_at_once_since_0_1_136`, fails without
  it. The orchestrator's three comments are the orchestrator branch's.
- **The orchestrator's three comments built on `coa/orch-139` (for 0.1.139; `09081da6`):**
  `NODE_HELP_PROBE_TIMEOUT_SECS`, `node_help_from_answer`'s timeout arm and its test's comment say
  what the pages say: on ai.generic 0.1.136 or later a silence of 3 s means the node is stuck or
  restarting or its 64 control places were all taken, and only a node pinned to an older ai.generic
  may miss it for being busy. Comments only, no test. SY.admin's `node_help` note, the other half,
  goes with SY.admin's branch.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-13 01:37Z):** shipped as `7c1fed3a`
  (SY.admin's note) and `098e6c22` (the orchestrator's comments). The gate's `help` step read
  `node_help`'s help on 8.x after the deploy (1.42 s): it carries the note. The comments are
  comments.

### A-323 [Archi] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; asked to stop `SY.orchestrator@worker1`, Archi staged nothing, said a core node is not stopped through it and asked what the operator wants, in 7.3 s) (found 2026-11-12 by the natural-language gate of Archi; seen on 8.x on 0.1.135) — Archi plans `kill_node` for a core node and says nothing brings it back

- **Seen:** asked to stop SY.timer on the motherbee, Archi staged one `kill_node` of
  `SY.timer@motherbee` (no force, no purge) and said neither the boot nor the periodic checks would
  bring it back. Since 0.1.134 the orchestrator refuses an `SY.*` name with `INVALID_REQUEST` (A-314's
  review), and the watchdog restarts a stopped core service within seconds. Archi did warn that the
  motherbee might refuse it. The plan was canceled, not run.
- **Fix:** SY.admin's help names the refusal from 0.1.137 (`coa/admin-136`, `b0bfce41`); Archi's
  handbook should also say a core node is not stopped with `kill_node` and that the watchdog keeps the
  core services running.
- **Built (0.1.137):** SY.admin's help (`3507f09a` on daily) and Archi's handbook (`688d2d5a`, with
  A-322's review `55fbdd85`): a core node is never stopped, started, restarted or removed through
  Archi; `kill_node` and `remove_node_instance` refuse an `SY.*` or `RT.*` name `INVALID_REQUEST`
  and do nothing (since 0.1.134), as `teardown_node`, `start_node` and `restart_node` do; the
  hive's orchestrator checks the router and the other core services every 5 s and starts any that
  is not running, SY.orchestrator itself the exception; asked to stop one, Archi stages nothing,
  says so and asks what the operator wants.
- **Checked on 8.x (0.1.137, gate0237, 2026-11-12 10:17Z):** asked in the chat to stop
  `SY.orchestrator@worker1`, Archi staged nothing, said a core node is not stopped through it and
  asked what the operator wants (7.3 s), with no page link, door path, bearer or token in its
  words. SY.admin's help for `kill_node`, `remove_node_instance` and `teardown_node` names
  `AI.sales@motherbee` and `INVALID_REQUEST`.

### A-322 [Archi] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; after each of three CONFIRMs one model call told what each step answered: a refusal's code, the hive not read and that asking again may pass; a delete's versions left and why; 9.4 to 14.2 s and 60,514 to 61,938 tokens a run, no link or token in its words) (found 2026-11-12 by the natural-language gate of Archi; seen on 8.x on 0.1.135; there before it) — After CONFIRM the chat shows only "Execution completed successfully" or "Execution stopped at step 's1'"

- **Seen:** with worker1's orchestrator stopped, a plan Archi staged to remove an unused version ran
  on CONFIRM, and the chat said only *"Execution stopped at step 's1'"*: not
  `RUNTIME_USAGE_UNKNOWN`, nor which hive was not read, nor that it may be asked again. A workflow's
  delete then answered ok with `packages_left` (0.0.1 `RUNTIME_USAGE_UNKNOWN`, 0.0.2
  `PURGE_STOPPED`) and the chat said only *"Execution completed successfully"*, so the operator reads a
  full success while two versions stayed. Asked *"¿Cómo quedó?"*, Archi explained both exactly as its
  handbook says (from reads: the command log keeps only the code, so it found worker1 in the LSA).
- **Why:** the executor's answer goes to the chat as one fixed line; the model gets no turn with the
  step answers its handbook tells it to report (`packages_left` and its reasons, `warning`,
  `pages_left_up`, a refusal's code, detail, `unanswered_hives` and `retryable`).
- **Effect:** what A-281, A-242 and A-285 added for the operator is reachable through Archi only if
  the operator asks; the fixed line can mislead. A canceled plan reads *"Programmer plan discarded"*.
- **Fix, later (SY.architect):** after an execution, give the model one turn with each step's answer
  (the executor's essentials already carry them), or render each step's code, detail and
  `packages_left`/`warning`/`pages_left_up` in the line.
- **The operator's call (2026-11-12):** yes, in 0.1.137 with SY.architect: after an execution the
  model gets one turn with each step's answer and tells the operator what happened, the fixed line
  kept only when that turn fails. No secret ever reaches that turn (A-124): only the fields Archi's
  handbook says to report. A-323's texts go with it.
- **Built (0.1.137, `coa/architect-137`: `6827b1e1`, its review `55fbdd85`, on daily):** once a
  plan the plan compiler prepared ran on `CONFIRM`, Archi's model gets one call, with no tool and
  one turn, 90 s at most, recorded with the usage stage `execution_report` and counted in the
  answer's `token_usage`. It is given an allowlist of each step's fields (`step_views`,
  `run_view`), never a step's answer copied, so no token, link, key, vault value or password
  reaches it, with a scrub of the texts it keeps as a backstop; its words become the run's
  `message`. When the turn fails or Archi has no AI runtime, the chat keeps the fixed line with
  each step's code and detail under it. A canceled plan answers *"Plan discarded."*. Left out, as
  sy-architect.md says: the pipeline's `CONFIRM 2` and a plan pasted or sent to `POST
  /api/executor/plan`. Its review (two lenses) found A-327 and A-328, and A-124 unchanged.
- **Checked on 8.x (0.1.137, gate0237, 2026-11-12 10:15–10:18Z):** three CONFIRMs on the gate's
  workflow `gate0237a`, each one model call (`source` model) whose words the session keeps as the
  chat showed them, with no page link, door path, bearer or token in them: removing a version
  refused `RUNTIME_USAGE_UNKNOWN`, and Archi named worker1 as the hive not read, said that asking
  again may pass and what to do (980 characters, `execution_report` 60,514 tokens, 10.8 s);
  deleting the workflow answered ok with 0.0.1 `RUNTIME_USAGE_UNKNOWN` and 0.0.2 `PURGE_STOPPED`
  left, and Archi said they are retryable, that nothing is blocked and how to remove them (1,388
  characters, 61,673 tokens, 14.2 s), where 0.1.135 said *"Execution completed successfully"*;
  removing the leftovers, which versions went (665 characters, 61,938 tokens, 9.4 s). Not run on
  8.x: the fallback to the fixed line (when the turn fails or Archi has no AI runtime) and a
  canceled plan's *"Plan discarded."*: by their tests
  (`a_failed_turn_keeps_the_fixed_line_with_each_steps_code`,
  `a_run_without_an_ai_runtime_keeps_the_fixed_line_with_each_step`,
  `a_discarded_plan_answers_as_archis_own_words`).

### A-321 [Archi] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; `SCMD: help`'s `add_hive` example carries `ssh_user` (2.7 s), where 0.1.138's had none; a manifest's hive ids checked when planned, and a closed record's words, by their tests) (found 2026-11-11 by A-311's Archi builder and review; read in the code; there before it) — Archi's plans and texts around `add_hive`

- **Manifests:** Archi never checks a manifest's hive ids: a hive `a.b` is planned, and fails only
  at execution, after the operator confirmed (`add_hive` refuses it `INVALID_HIVE_ID`; a node, a
  route, a VPN rule or a workflow on it fails its own way).
- **The SCMD help:** its `add_hive` example (`handle_meta_scmd`, `src/bin/sy_architect.rs` ~12099) has
  no `ssh_user`, so a copied example is refused `INVALID_REQUEST`; SY.admin's example has it.
- **A closed record:** one closed on `NOT_FOUND` says *"hive 'X' no longer exists"*, where a
  timed-out `add_hive`'s hive usually never existed.
- **Fix, later (SY.architect):** check a manifest's hive ids when it is planned; the example's
  `ssh_user`; *"the registry does not have hive 'X' (removed, or never created)"*.
- **Built on `coa/architect-139` for 0.1.139:** the three, by their tests.
  - `validate_manifest_v2` refuses a hive id SY.admin would refuse (`refuse_manifest_hive_id`,
    `is_valid_hive_id`: 1 to 64 letters, digits, `_` or `-`, as written) in each place a manifest
    names one: a hive its topology adds, a VPN rule's, a node's, a route's and its next hop's, a
    workflow's. The error names the field and the id, so the designer's next round fixes it
    (`a_manifest_hive_id_that_is_not_one_is_refused_when_planned`: `a.b`, ` worker1`,
    `worker/1` and `../x` in each place, and `worker-1` passes). It says no hive has such an id,
    and names no code (the review: it said SY.admin refuses it `INVALID_HIVE_ID`, which only
    `add_hive`, `remove_hive`, `get_hive` and the joins answer).
  - `SCMD: help`'s `add_hive` example carries `ssh_user`
    (`the_scmd_help_examples_translate_and_add_hive_names_its_required_fields`: every SCMD example
    of the help translates, and the `add_hive` one names `hive_id`, `address` and `ssh_user`).
  - An open `add_hive` record closed on `NOT_FOUND` says *"the registry does not have hive 'X'
    (removed, or never created); the operation cannot be resolved"*
    (`an_open_add_hive_closes_on_a_hive_id_that_get_hive_refuses`).
  - Each test fails without its fix. sy-architect.md says all three.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `f6bc13f8` (its review
  `d488b25e`). `SCMD: help`'s `add_hive` example: on 0.1.138 (00:46Z) `{"hive_id":"worker-220",
  "address":"192.168.8.220"}`, no `ssh_user`; on 0.1.139 it carries `"ssh_user":"administrator"`
  (3.0 s at 02:02Z, 2.7 s in the rerun). A manifest's hive ids checked when it is planned, and an
  open record's words on `NOT_FOUND`, were not provoked: by their tests.

### A-320 [Archi] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; with SY.admin frozen, an `add_route` that timed out in Archi at 30 s closed on its entry in SY.admin's command log, the identical request getting its own answer in 0.584 s; a `sync_hint`, which the log does not keep, answered *"cannot be settled in this chat"*, and its own answer in a new chat; the 10-minute close by its tests) (found 2026-11-11 by A-311's Archi builder; read in the code; there before it) — Only `add_hive`'s timed-out records ever close

- **Why:** Archi's reconcile asks the registry only for an open `add_hive` record. Any other tracked
  mutation that times out in Archi stays `timeout_unknown` in its chat, and an identical request
  there answers `OPERATION_ALREADY_TRACKED` (`remove_hive` waits Archi's 30 s while the
  orchestrator's removal can take longer).
- **Effect:** the operator's retry in the same chat is refused; a new chat, or other parameters,
  gets past it.
- **Fix, later (SY.architect):** a reconcile for each tracked mutation that has a read to settle it.
- **Built on `coa/architect-139` for 0.1.139:** the read is SY.admin's command log, which keeps
  each command's outcome once SY.admin ends it, whether Archi still waited or not.
  - An identical request in the chat of a `timeout_unknown` record of any action the log keeps
    (`action_in_command_log`: every tracked mutation SY.admin runs as a command, `remove_hive`,
    the node actions, a runtime's publish and version removal, routes, VPNs, ILKs, tenants, a node's
    config and messages, the storage, a WF instance's cancel) reads `list_recent_commands` for its
    action on its hive since 5 s before its dispatch (200 a page, up to 5 pages). The entry this
    Archi's command made (its origin `l2:SY.architect@<hive>`, its hive, started no earlier than
    5 s before the dispatch, the record's plain params, redacted values and cut params not
    compared; the nearest start of several) closes the record: `ok` as `succeeded_after_timeout`,
    `error` as `failed` with SY.admin's code and its duration (the log keeps no detail), and the
    request goes on.
  - No entry yet: still open, refused `OPERATION_ALREADY_TRACKED` with *"timed out locally and has
    no outcome yet"*. None 10 minutes after its dispatch with the whole log read: `failed`, its
    outcome unknown: SY.admin never ended it (a restart, or the request lost), or its entry rotated
    out (the review: the log keeps about the last 400 commands, reads included, 100 a file in the
    active one and 3 rotations, so on a busy installation, or for a record left open before the
    upgrade, the entry may be gone; the summary said SY.admin never ended it). A log that cannot
    be read leaves it open. `add_hive` keeps its registry read.
  - Not in the log, so not read, as before: `update`, `sync_hint`, the OPA and the wf-rules writes
    (Archi waits for each as long as SY.admin does, or longer). An identical request in the chat of
    such an open record is refused saying it cannot be settled there: read the state, and repeat
    it in a new chat (`open_operation_is_read`; the review: it said to ask again once it ends,
    which that chat refuses for good).
  - Tests: `an_open_mutation_closes_on_its_outcome_in_the_command_log` (through the mesh harness:
    a `remove_hive` closed by its entry among others of another origin, another hive id and an
    earlier start, the read's target and params; a node's purge refused on the log's second page,
    its cursor; no entry yet; none past 10 minutes; a log refused) fails without the fix: nothing
    is asked and the record stays open.
    `a_command_log_entry_is_a_records_by_its_action_origin_hive_time_and_params` judges the
    entries. A-311's test asks nothing of a record that is not open, an accepted `remove_hive` and
    an `update`. The review's `an_open_records_texts_say_what_is_known_and_the_way_out` checks the
    two texts above, and fails without them.
  - Not changed: Archi's own wait (30 s for `remove_hive`, `kill_node` and
    `remove_node_instance`, where SY.admin waits up to 180 s for a purge), so such a request still
    answers `TIMEOUT_UNKNOWN` first; its outcome is read on the next identical request.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-13 02:04–02:05Z):** shipped as `5c5719de`
  (its review `d488b25e`). SY.admin frozen 55 s; in one chat an SCMD `add_route` of `gate0239.`, in
  another a `sync_hint`: they timed out in Archi at about 30 s and 45 s (`TIMEOUT_UNKNOWN`).
  SY.admin's command log held the `add_route`'s outcome 1.6 s after the continue; the identical
  `add_route` in its chat then got its own answer (`DUPLICATE_PREFIX`) in 0.584 s, and a third one
  too; the identical `sync_hint`, which the log does not keep, answered `OPERATION_ALREADY_TRACKED`,
  *"An equivalent operation timed out locally and cannot be settled in this chat"*, and in a new
  chat its own answer. SY.admin's MainPID and NRestarts unchanged, Archi's journal with no error. No
  baseline on 0.1.138 (the optional `track` was not run before the deploy). The 10-minute close, by
  its tests.

### A-319 [General/admin] ✅ BUILT (0.1.139, 2026-11-12; live on 8.x: the deploy's installation update closed 36.470 s after it opened, every hive on the target; the cap of 8 reads, which four hives never reach, by its tests) (found 2026-11-11 by A-263's review; read in the code) — The installation update's background reads are bounded in time, not in number (0.1.137)

- **Why:** since 0.1.137 every due hive is read at once. Past about 16 hives in flight the
  motherbee's orchestrator queues the rest (16 forwarding places, 4 per target) while SY.admin's
  per-read limit runs, and a queued read that times out outside a restart holds its hive 50 s.
- **Effect:** none at 8.x's four hives; on a large installation the marks come late again.
- **Fix, later:** cap the background reads below 16 (for example 8).
- **Built on `coa/admin-139` for 0.1.139:** the background task reads at most 8 hives at once
  (`BACKGROUND_READS_AT_ONCE`, a semaphore of `Updates`). Each look (the follow, the automatic
  step, the re-read) takes its place before it claims its hive and lets it go once its read ends,
  before anything else it does (`background_place`, which the review below made
  `background_claim`): a hive waiting for a place is free for an
  explicit run, whose pass still waits only for that hive's read under way (A-263), and no look
  waits for a second place while it holds one. The explicit run's reads and the motherbee's own
  (`target_is_current`, which its orchestrator answers itself) take none. Its test,
  `the_background_reads_at_most_eight_hives_at_once`, has twelve workers restart onto the target
  with each read taking 300 ms: 8 at once, the update closed; without it, 12. sy-admin.md and
  hive-config-update-v1 say it.
- **Its review (2026-11-12), on `coa/admin-139`:** a look that waited for its place held no hive,
  so the tick, which passed over busy hives only, spawned another look at that hive at each tick
  past its cadence (3 s for a hive in flight), and each read the hive again as soon as the one
  before let it go: on an installation with more than 8 hives due, more reads, off the cadence.
  Now the tick marks each look it spawns as waiting (`Updates::waiting`, `WaitingLook`) until the
  look tried to claim its hive (`background_claim`, which takes the place and then the hive), or
  ended, and passes over a waiting hive as over a busy one. Its test,
  `a_hive_whose_look_waits_for_a_place_gets_no_second_look` (every place taken, five ticks past
  the hive's cadence, then the places freed: one read), fails without it (five reads).
- **Shipped in 0.1.139** (`3ac55428`, its review `60ab8e30`, on daily). **On 8.x (gate0239,
  2026-11-13):** the deploy's update `upd-1794533637082` opened at 01:33:57.082Z and closed done at
  01:34:33.552Z, 36.470 s, every hive on the target (0.1.138's, read from its timeline, about
  43 s). With four hives the cap never binds: by its tests,
  `the_background_reads_at_most_eight_hives_at_once` and
  `a_hive_whose_look_waits_for_a_place_gets_no_second_look`.

### A-318 [General/admin] ✅ BUILT (0.1.139, 2026-11-12; live on 8.x, no failed publish's clean-up line since the deploy, none being due; a failed publish's row not provoked, since failing one at the edge would stop the Cloud's door and every public page: by its tests) (found 2026-11-11 by A-280's review; read in the code) — A failed publish's clean-up runs after the answer, so a stop of SY.admin during IO.blob's release orphans the copy

- **Why:** since 0.1.137 the row is removed under the lock, the answer goes out, and IO.blob's
  release runs in a task of its own. If SY.admin stops during that release (up to 30 s, a deploy
  say), IO.blob keeps a copy no ledger row names, and IO.blob has no collection of such orphans.
  Before 0.1.137 the row stayed `curated` and the expiry sweep retired it. A release that fails
  orphaned the copy before too.
- **Fix, later:** the clean-up task removes the row (under the lock, only while still `curated`)
  once IO.blob answered the release.
- **Built on `coa/admin-139` for 0.1.139:** from its `curated` row on, a failed publish lets the
  lock go with the row as it is, and its clean-up removes it once its steps went through: the
  edge's unpublish when it was asked, and IO.blob's ok to the release (`drop_failed_publish_row`,
  under the publication lock, only while the row is still `curated`). A step that fails keeps the
  row for a take-down or the expiry sweep, as before 0.1.137; a row a take-down moved on is the
  take-down's. The clean-up's line says what became of the row (`ledger_row`: `removed`, `kept`,
  `none`, `failed`). Its test, `a_failed_publish_s_row_goes_once_io_blob_released_its_copy`
  (paused time: the row `curated` after the answer and while the release is unanswered, gone once
  IO.blob released, kept when the release failed, a taken-down row left alone), fails without it;
  `a_failed_publish_answers_within_its_budget_and_cleans_up_after` now finds the row `curated`.
  sy-admin.md says it. Left as it was: a curate that failed writes no row, so a stop during its
  release still leaves an orphan, as a release that fails always did.
- **Shipped in 0.1.139** (`4f59d130`, on daily). Not provoked on 8.x (gate0239, 2026-11-12/13): a
  publish that fails at the edge would mean freezing SY.edge, which stops the Cloud's door and
  every public page. The deploy's reads found no *"a failed publish's clean-up ended"* line since
  the deploy, none being expected. By its test,
  `a_failed_publish_s_row_goes_once_io_blob_released_its_copy`.

### A-317 [General/edge] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; the record before the token, its `set_node_config` ending 16 ms before SY.vault's `VAULT_PUT`; a refused `edge_node` and a failed record changed nothing (0 ms in SY.admin's log, `CONFIG_RECORD_FAILED` in 0.042 s, nothing rotated); a door that did not open rolled the vault back to the Cloud's token, which still opened the door, HTTP 200; the move to another edge and the old door's close by its tests, S4's fake record having gone to a stray key: A-346) (found 2026-11-11 by A-273's builder; read in the code; there before it) — SY.edge keeps one previous channel secret, in memory only

- **Why:** `apply_open_url` (`src/bin/sy_edge.rs` ~696-711) keeps only the replaced entry's current
  secret as the previous one, for 10 minutes, in memory.
- **Effect:** two rotations within 10 minutes drop the older token at once; a run that answers
  `CONFIG_RECORD_FAILED` puts a token nobody holds in that slot (0.1.137's texts say so); moving the
  Cloud's door to another edge leaves the old edge's door open with the old token in its memory
  until that edge restarts, when its vault fetch fails and the old door refuses every token.
- **Fix, later (to decide):** keep the older grace token when the current one was never used; close
  the old edge's door when the door moves; or record `io.edge_node` before the externalize mints,
  so a failed record rotates nothing.
- **The operator's call (2026-11-12):** the rotation mechanism complete, in the right order:
  `io.edge_node` recorded before the externalize mints, so a failed record rotates nothing; and the
  old edge's door closed when the door moves to another edge. No grace-token extension: *"no pongas
  cosas raras para resolver el problema de que el cloud pierda el token que ya tenía aprobado,
  porque eso es mecanismo distinto"*. A topic of its own for the Cloud track, to review: how Fluxbee
  Cloud gets a new token when it was already registered. In the bug push (item 30).
- **Built on `coa/edge-139` (0.1.139), in SY.admin alone.** SY.edge is unchanged: its one previous
  token, 10 minutes in its memory, stays as it is by the operator's call, and its `EDGE_CLOSE_URL`
  closes the old door. `publish_cloud_endpoint` (`src/bin/sy_admin.rs`) now runs in this order:
  - **The record first** (`record_cloud_edge`): IO.cloud's config read (`get_node_config`), then
    the edge written in it (`set_node_config` of its `io` with `edge_node`, the rest of `io` as
    read, where the record wrote `io` with the edge alone), each tried once more at once, before
    the externalize mints the token. A read or a write that fails twice answers 502
    `CONFIG_RECORD_FAILED` with nothing else done: no token minted, no door opened or closed,
    IO.cloud not asked, so a token the Cloud holds works as before. Its detail says so, and that a
    write that got no answer may have been written all the same.
  - **A door that does not open** (the externalize's error) puts the record back as it was read
    (`put_back_cloud_edge_record`, tried once more at once; nothing on a rotation on the same
    edge), and the error's detail says so, or that IO.cloud's config still names the edge.
  - **The apply** to the running IO.cloud, as before (A-255).
  - **The old door closed on a move** (`close_moved_cloud_door`): when the config named another
    `SY.edge@<hive>`, SY.admin sends that edge `EDGE_CLOSE_URL` of IO.cloud's channel, tried once
    more at once. The answer adds `previous_edge_node` and `previous_edge_closed`; one that did not
    close is in its `warning`, with the `unexternalize` that closes it once that edge answers.
  - `edge_node` must be `SY.edge@<hive>`, refused `INVALID_REQUEST` before anything is read or
    recorded, since the record comes first; one run at a time (a lock: two runs at once would read
    the same old edge, and the door the first opened would stay open behind the second).
  - Texts: SY.admin's help for `publish_cloud_endpoint` (its order, a move, the record's failure, a
    door that does not open, `edge_node`); docs/io-cloud-api.md §2, §7 and §9 0.1.139;
    docs/nodes/sy-admin.md's route row; docs/nodes/sy-edge.md's grace note.
  - Tests: the publish end to end in its new order; a record that fails twice rotates nothing (the
    read, two writes and nothing else, its log lines and command log); the record's read or write
    failing twice; a move that closes the old door; an old door that does not close (asked twice,
    named in the warning; closed on a second ask); a door that does not open (put back on a move and
    on a first publish, nothing on a rotation, a put-back that fails named); an `edge_node` that is
    no edge; one run at a time; what the record reads and writes. Each fails on a mutant without its
    part of the fix (the old order, no close, no put-back, no edge check, no lock, the edge alone in
    `io`, no second close).
  - **Left as they were, not in the operator's call (to decide):** a door whose open fails after
    the vault took the new token (the edge refused `EDGE_OPEN_URL` or did not answer it): the
    vault's `edge_channel_secret:<ich>` holds the new token, for the new edge, and no one received
    it; the old edge keeps the token it holds in memory until it restarts, then reads the new token
    (a rotation on the same edge) or none (a move): 401 for the Cloud. And a record write that got
    no answer but was written: a run again then reads the new edge as the door's, and does not close
    the old door, which IO.cloud no longer trusts. Since the review below, both are told to the
    operator in the answer; whether to roll the vault's key back (`vault_rollback`) on a door that
    did not open stays the operator's call. Also not told: a vault put that got no answer but
    landed (the detail then starts `failed to store channel token in vault`), the same residual.
    The first is decided since: the operator's call of 2026-11-12, below.
- **Its review (`coa/edge-139`, for 0.1.139):**
  - **The old door closes only once IO.cloud trusts the new edge** (`settle_moved_cloud_door`):
    the apply `Applied` or `AlreadyTrusted`. When IO.cloud did not take it (`NotApplied`,
    `Superseded`) or SY.admin could not tell (`Unchecked`), the running IO.cloud may still trust
    the old edge, whose door is then the Cloud's only way in, so the old door is left open, not
    asked to close: `previous_edge_closed: false`, and a `warning` that says so and to close it
    with `unexternalize` once IO.cloud trusts the new edge and the Cloud has the new URL and token.
    The build closed it whatever the apply did, which cut the Cloud off until IO.cloud took the new
    edge, against its own help ("once the new door is open and applied"). The branch's reading of
    the operator's call ("in the right order") and of the operator's principle of 2026-11-12
    (A-313: act only on what fails), **for the operator to confirm**; the other way is to keep the
    close whatever the apply did and say so in the help. Confirmed: the operator's call of
    2026-11-12, below.
  - **A door that does not open says what it left in the vault** (`unreceived_cloud_token`): when
    the edge refused the open or did not answer it, the error's detail adds that the vault already
    holds the token this run minted, for that edge, which no one received; that the token the Cloud
    holds works at most until the edge the door was on restarts, as a deploy does; and to publish
    again on an edge that answers. The build said only that the config was put back, which read as
    a full undo.
  - **`CONFIG_RECORD_FAILED` says a write may have landed only when a try got no answer**
    (`cloud_edge_write_unanswered`: HTTP 504, or no answer at all), the first try's or the last's,
    where the build said it on every failed write, two refusals included; on a move it adds to put
    `io.edge_node` back to the old edge first, since IO.cloud would trust, from its next start, an
    edge with no door, and a run again would close no old door (`cloud_edge_maybe_written`).
  - The one-run lock is SY.admin's context's (`cloud_door_lock`), like `publication_lock` and
    `runtime_manifest_lock`, where it was a static that every test's admin shared.
  - Tests: a move IO.cloud did not take (refused, then not answered) leaves the old door open and
    sends no close; a door that does not open, after the vault took the token (the edge refused,
    or not reached) or not (the vault refused); a write that got no answer on the first or the last
    try, or both, and two refusals that say nothing of it. Each fails on a mutant without its part
    (the close whatever the apply did; no vault sentence; the sentence on every failure; the last
    try alone; no way back on a move; no lock).
  - Left for the release's records: board item 30 lists `SY.edge: A-317` among 0.1.139's binaries,
    where the fix is in SY.admin alone (moved under SY.admin at 0.1.139's integration review).
- **At 0.1.139's integration review (`coa/sweep-139`):** the integration sent a run again "through
  Archi" too (A-124), and that path could lose the token. Archi waited 120 s for a whole plan, while
  a run, which SY.admin runs with no step timeout, takes up to 225 s by its own bounds (the
  record's read and write, each 30 s tried twice; the vault's store, 30 s; the edge's open, 15 s;
  the apply, 30 s; the old door's close, 15 s tried twice), and waited for the one-run lock with no
  bound: Archi could stop waiting while the run minted the token, stored it and opened the door,
  its only clear copy then in an answer no one read. Now:
  - **Another run is refused at once** (`cloud_door_busy`): 409 `BUSY`, `payload.retryable: true`,
    nothing read, recorded or minted, where it waited for the lock. The help, sy-admin.md's row,
    io-cloud-api.md §7 and the handbook (§4.5.1) say it. Test `one_publish_runs_at_a_time` (the
    second refused, nothing sent; a run after the first goes), failing without it.
  - **Archi waits for each publish of the door** in a plan, 240 s more
    (`EXECUTOR_CLOUD_ENDPOINT_WAIT_SECS`, as for a workflow's deploy). Test
    `an_executor_run_waits_for_each_publish_of_the_cloud_door`.
  - **A cut answer keeps the token** (A-124's entry).
- **The operator's call (2026-11-12) on the old door (i):** the old door closes only once IO.cloud
  trusts the new edge; otherwise it is left open, with a warning. The code as built (A-317's
  review).
- **The operator's call (2026-11-12) on a door that did not open (ii), a change:** when the door
  does not open after the vault took the new token (the edge refused `EDGE_OPEN_URL` or did not
  answer it), SY.admin rolls the vault's `edge_channel_secret:<ich>` back to the token before it
  (`vault_rollback`), automatically, so the vault holds the token the door serves, and the answer
  says it rolled back. A rollback that fails is told in the answer and logged, never retried in a
  loop: the operator's principle is retry and log on the one that fails (A-313).
- **Built at 0.1.139's integration (`coa/sweep-139`), in SY.admin:**
  - **The run knows the vault took its token** from the externalize itself: `externalize_channel`,
    which `handle_externalize` now wraps, gives `publish_cloud_endpoint` the key and the version
    the vault's put answered (`StoredChannelToken`) once the vault took the token. The review's
    `unreceived_cloud_token` read it from the start of the error's wording.
  - **The rollback** (`roll_back_cloud_token`): `VAULT_ROLLBACK` of the key in the root tenant,
    straight to SY.vault with a bound of its own, 5 s (`CLOUD_TOKEN_ROLLBACK_TIMEOUT`), before the
    record is put back (the run's steps undone in reverse). A refusal SY.vault answers, or an ask
    the router did not deliver (`UNREACHABLE`, `TTL_EXCEEDED`), changed nothing and is asked once
    more at once. `NO_PREVIOUS_VERSION` is no failure: the vault held no token for the channel
    before the run (a first publish), and none is rolled back to. An ask SY.vault did not answer in
    time is not asked again: a rollback swaps the token and the one before it, so a second one after
    a first that landed unanswered would put the run's token back. Each outcome is logged, and
    nothing asks again later.
  - **The answer** (`cloud_token_said`, after what became of the record): rolled back, and on a
    rotation on the same edge, when the edge refused the open or the router did not reach it, that
    edge serves the token the Cloud holds and reads it again when it restarts, so nothing changed
    for the Cloud (an edge that did not answer the open: the review, below); on a move the old edge
    serves it, but reads none once it restarts (below), so publish again on an edge that answers;
    with no door before, the vault holds what it held. Or why not: the vault then holds (after an
    ask not answered, may hold: `vault_get_metadata` shows the key's version, the one the run's put
    answered while it still holds that token) the token no one received, which the edge the door
    was on reads when it restarts on a rotation (after a move it reads none, below); publish again.
  - **Its time:** a run whose door did not open takes up to 235 s by its bounds (the record 120 s,
    the store 30 s, the open 15 s, the rollback 10 s, the put-back 60 s), within the 240 s Archi
    waits for each publish of the door (`EXECUTOR_CLOUD_ENDPOINT_WAIT_SECS`, whose comment and
    test's bound name both paths now).
  - **What it leaves:**
    - **On a move the key stays the new edge's.** The put names its owner (`owner_node`, the edge
      the run opens the door on), and SY.vault's rollback restores the token and its version, not
      the key's metadata, which it keeps one of per key. So the old edge, whose door the Cloud
      uses, serves the Cloud's token from its memory and reads none once it restarts, as without
      the rollback; the answer says to publish again on an edge that answers. Ways to close it, for
      the operator: SY.vault's rollback brings back the owner of the version it restores (SY.vault);
      SY.admin assigns the key to the old edge after the rollback (`vault_assign`, which SY.admin's
      API refuses for an edge's key since A-316); or leave it, the answer telling to publish again.
    - **An edge that took the open and whose answer was lost** read the run's token from the vault
      right after the open (`resolve_secrets`), before the rollback, and serves it (on a rotation
      with the old one beside it for 10 minutes) until it restarts: a `VAULT_SECRET_CHANGED`
      re-reads only an entry that holds no token. Once it restarts it reads the Cloud's token from
      the vault; without the rollback it read the run's there too. The answer said nothing of it,
      and on a rotation that the edge served the Cloud's token; it says so since the review below.
    - **A writer of the key meanwhile:** the rollback comes within seconds of the put, under the
      one-run lock, so only an operator's own `externalize` of IO.cloud's channel in those seconds
      could write it, and the rollback would undo that write. Unlike the ACME's rollback
      (`rollback_if_current`), which may come hours after its write, it does not read the key's
      version first.
  - **Tests:** `a_door_that_does_not_open_puts_the_record_back` (the rollback asked before the
    put-back after an edge's refusal and after an edge not reached, none when the vault refused the
    token; the detail on a rotation, a move and a first publish) and
    `a_token_rollback_is_asked_again_only_when_it_changed_nothing` (ok; `NO_PREVIOUS_VERSION`; a
    refusal, then ok; an ask not delivered, then ok; two refusals; an ask not answered, not asked
    again; the log lines; each detail), with the help's
    (`the_cloud_endpoint_help_says_what_a_run_does_to_the_token_before_it`). Each fails on a mutant
    without its part: no rollback; an unanswered ask asked again; no second ask after a refusal;
    `NO_PREVIOUS_VERSION` read as a failure.
  - **Texts:** SY.admin's help for `publish_cloud_endpoint`, sy-admin.md's row, io-cloud-api.md §7,
    its 401 row and §9 (0.1.139), sy-edge.md's grace note and sy-architect.md's wait.
- **From the review of the calls' build (2026-11-12, on `coa/sweep-139`):** two of its findings
  were texts of A-317's rollback, both fixed. The rollback itself was checked and loses no token
  the Cloud holds: it runs only after the vault answered the put, SY.vault swaps the two versions
  in one transaction, and only an ask that changed nothing is asked again.
  - **An edge that did not answer the open** (medium): the externalize answers
    `edge unreachable / open_url failed` for any error of `EDGE_OPEN_URL`, its 15 s timeout
    included, and SY.edge answers an open before it reads the new token from the vault, so an edge
    whose answer was lost or late may serve the run's token, the Cloud's beside it for its
    10 minutes only (*What it leaves*, above). The answer said on a rotation that the edge served
    the Cloud's token, the help that this was all, and io-cloud-api.md §9 that the Cloud's token
    kept working. Now the run knows how the open failed, not from the error's words:
    `StoredChannelToken`'s `open_unanswered`, set for an error that may have reached the edge,
    any but the router's `UNREACHABLE` or `TTL_EXCEEDED` (`never_reached`, which the rollback's
    outcome uses too). The answer (`cloud_token_said`) then says the edge may have taken the open
    and serve the run's token, on a rotation the Cloud's only for up to 10 minutes after the open,
    and to run again on an edge that answers; an edge that refused the open, or that the router
    did not reach, says as before. The help, sy-admin.md's row, io-cloud-api.md §7, its 401 row
    and §9, and sy-edge.md's grace note say so.
  - **After a move the old edge reads no token** once it restarts: the help and sy-admin.md said
    that, when the rollback failed or there was none, the edge the door was on read the token no
    one received; they say now that after a move it reads none, the key being the new edge's. The
    answer's own detail said it right.
  - **Tests:** `an_edge_that_did_not_answer_the_open_may_serve_the_run_token` (new, on a paused
    clock: the edge never answers the open, SY.admin gives it up after 15 s, rolls the token back
    and puts the record back; the detail on a rotation, a move and a first publish);
    `a_door_that_does_not_open_puts_the_record_back` (a rotation the router did not reach says as
    a refusal does); the rollback test's details with an open not answered (a rotation whose
    rollback was refused, a move with no token before); the help's test (the new texts, and the
    two old ones refused). Mutants: an open never marked unanswered fails the new test; every
    error read as one that may have reached its target fails the door test and the rollback test.
  - **To decide, the operator:** after a rollback whose open got no answer, SY.admin could send
    the edge `EDGE_OPEN_URL` once more: the row comes back with no secret, so the edge's
    `resolve_secrets` reads the rolled-back token, and the Cloud's works again at once, the run's
    kept beside it for the grace. Not built: it is a change of behaviour, not of texts, and an
    edge that did not answer may not answer again.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `ee1fa6f3` (its review
  `80531483`; the integration's `c695490c` and `68a292ff`; the vault's rollback `37ccd739`, its
  review `204ee161`), in SY.admin alone.
  - **S2 and S3 (`edge-refusals`, 02:00Z, and the rerun at 05:36Z):** `edge_node`
    `SY.edges@ingress1` and `IO.gate0239@ingress1` each answered 400 `INVALID_REQUEST`, the exact
    detail, no token, 0 ms in SY.admin's log (the first pass's FAIL timed the client's call,
    0.168 s, the opener built inside it); nothing changed (`config_version`, the vault's version,
    no *"opened URL"* on ingress1). With IO.cloud's config.json immutable the publish answered 502
    `CONFIG_RECORD_FAILED` in 0.042 s (0.212 s in the first pass), no token, its detail saying the
    record failed twice, *"Nothing else was done"* and *"no token was minted"*; the command log:
    `get_node_config`, `set_node_config` `CONFIG_WRITE_FAILED` twice, the publish in 24 ms; nothing
    rotated (no `VAULT_PUT` or `VAULT_ROLLBACK`, IO.cloud `HEALTHY` on the same pid, its door row
    there).
  - **Step 44, the operator's call (ii) (`edge-rollback`, 01:51Z):** with `SY.edge@ingress1`
    frozen, the door did not open: 400 `INVALID_REQUEST` after 17.7 s (the edge's 15 s), no token.
    The answer said SY.admin rolled `edge_channel_secret:<ich>` back to the token it held before
    the run, and that the edge did not answer the open and may have taken it all the same (open
    point 4, a SEE). The vault: the same version 4 and the same owner (ingress1's ILK), `updated_at`
    moved; its audit `VAULT_PUT`, then `VAULT_ROLLBACK`, both success. Once continued, ingress1's
    sy-edge logged *"opened URL"* (01:51:03.220Z) and *"resolved channel secret"* 0.057 s later: its
    late open read the rolled-back secret. The Cloud's token, the one minted in 0.1.133's gate,
    still opened the door: HTTP 200. IO.cloud `HEALTHY` on the same pid, one door row.
  - **Step 44.1 (`edge-rotate`, 03:42Z, the operator's OK; one rotation of the Cloud's token):**
    S5, a door on an edge that does not exist (`SY.edge@gate0239y`): `INVALID_REQUEST` in 1.4 s, no
    token; IO.cloud's record put back (`config_version` 5 to 7: the record and the put-back), the
    live config too, IO.cloud's pid unchanged; the vault rolled back to version 4; the answer says
    *"The key stays SY.edge@gate0239y's"* (the owner a rollback keeps, as built; a SEE). S4, a
    publish on `SY.edge@ingress1` after a fake record of `SY.edge@gate0239x`: ok in 1.3 s; the
    record's `set_node_config` ended at 03:42:27.454Z and SY.vault's `VAULT_PUT` at 03:42:27.470Z,
    the record first; on ingress1 *"opened URL"* at 03:42:27.522Z, then *"resolved channel
    secret"*; after it IO.cloud's config named `SY.edge@ingress1` (`config_version` 9), IO.cloud
    `HEALTHY`, one door row, the vault at version 5 and owned by `SY.edge@ingress1` again. S4's two
    FAILs (the answer named no previous edge and gave no warning; the journal held no close) are the
    tool's setup, not this fix: its fake went to a stray top-level `config` key, which no record
    reads (A-346), so the publish saw a rotation on the same edge. That key, a hazard for IO.cloud's
    next start, was removed on 8.x at about 05:44Z (A-346). The Cloud's token rotated at
    03:42:27Z; the previous one worked its 10 minutes.
  - **By its tests, not provoked on 8.x:** the move to another edge and the old door's close on a
    real second edge (S4 never moved the door; one more rotation of the Cloud's token for it not
    worth it now), a move IO.cloud did not take (the old door left open), a record write with no
    answer, two runs at once (`BUSY`), and a rollback that fails.

### A-316 [General/admin] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; `vault_get` and `vault_assign` of an edge's keys answered 403 `RESERVED_KEY` in 0.004 to 0.253 s without reaching SY.vault (no audit row), where 0.1.138's SY.vault answered 404 `KEY_NOT_FOUND` in 0.259 s and 0.04 s; an edge key's metadata still readable, with no value field) (found 2026-11-11 by A-273's review; read in the code, not run; there before it) — An edge channel's bearer can be read back through `vault_assign` then `vault_get`

- **Why:** `handle_externalize` stores `edge_channel_secret:<ich>` owned by the edge, in the root
  tenant; `vault_assign` (`check_assigned_ilks`, `src/bin/sy_admin.rs` ~23229) admits any ILK that is
  not a person's and is charged to the secret's tenant, SY.admin's own system ILK included;
  `refuse_acme_account_access` (~22510) guards only the ACME account key; SY.vault's
  `handle_assign` (`src/bin/sy_vault.rs` ~1092) asks only for a known admin caller; a `vault_get`
  through SY.admin goes with SY.admin's ILK (~22387), and `authorize_read` admits an assigned ILK
  (~1941).
- **Effect:** the operator, or Archi with the operator's confirmation of a `vault_assign` step (the
  A-62 table admits Archi to both), can read the Cloud's bearer or any edge channel's; through Archi
  it reaches the AI provider (A-124). SY.admin's texts say it cannot be read back.
- **Fix, later:** a guard like the ACME one on `edge_channel_secret:*` and `edge_fanout_verify:*`
  for `vault_get` and `vault_assign`, or SY.vault refusing to assign an infrastructure secret
  dedicated to one node.
- **Built on `coa/admin-139` for 0.1.139:** the guard, beside the ACME accounts' in
  `handle_vault_command` (`refuse_edge_secret_read_back`): `vault_get` and `vault_assign` of a key
  under `edge_channel_secret:`, `edge_tls` or `edge_fanout_verify:`, whatever its case, answer
  `403 RESERVED_KEY` before anything reaches SY.vault, whoever asks (the operator, the executor,
  Archi, Cloud). `edge_tls*` with them, as the state review of 2026-11-12 found: the edge's TLS key
  was read back the same way. Their metadata stays readable, and the operator's puts, rotations,
  rollbacks and deletes are not refused (SY.admin's own externalize stores the bearers and verify
  tokens). SY.vault is unchanged. Its test, `the_admin_api_never_reads_back_an_edge_secret`, fails
  without it. SY.admin's help (`vault_get`, `vault_assign`), sy-admin.md, sy-vault.md and Archi's
  handbook (§4.5) say it.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `6c6498c2`. Keys that
  do not exist, so nothing could leak: on 0.1.138 (00:46Z) `vault_get` of
  `edge_channel_secret:ich:gate0239-none` and `vault_assign` of `edge_tls_gate0239_none` reached
  SY.vault, 404 `KEY_NOT_FOUND` in 0.259 s and 0.04 s; on 0.1.139 (01:50Z) both, and `vault_get`
  of `edge_fanout_verify:ich:gate0239-none`, answered 403 `RESERVED_KEY` in 0.253 s, 0.005 s and
  0.004 s, the command log `RESERVED_KEY` 403, and SY.vault's audit held no row of the three
  since. The metadata of `edge_tls_fluxbee_ai` stays readable, with no value field.

### A-315 [Archi] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; `get_runtime ai.generic` answered `hives_without_runtimes` egress1 and ingress1 with their roles, the help of `get_runtime` and `remove_runtime_version` names the field, and Archi's handbook holds it once, where 0.1.138's held it 0 times) (found 2026-11-11 by 0.1.133's gate; read in SY.admin's help, Archi's handbook and the code; since 0.1.133) — SY.admin's help (`get_runtime`, `remove_runtime_version`) and Archi's handbook do not name `hives_without_runtimes`

- **Seen:** 0.1.133's gate read SY.admin's per-action HELP for the answers 0.1.133 added (A-285):
  each it looked for is named, but `hives_without_runtimes` is named nowhere in SY.admin's help
  nor in Archi's handbook (`docs/archi/handbook_fluxbee.md`). In `src/bin/sy_admin.rs` only a test
  names it; `docs/nodes/sy-admin.md` and `docs/reference/operations.md` do.
- **Why:** A-281 (0.1.133) lists every ingress and egress of a runtime's use across the hives
  (`get_runtime`, and the read `remove_runtime_version` decides on) in `hives_without_runtimes`, by
  the role the hive registry records, where they were in `skipped_hives`; A-285's texts named the
  new codes, not this field.
- **Effect:** Archi reading a runtime's use sees a field its texts do not explain: the ingress and
  the egress listed apart, neither counted nor skipped. Nothing is refused or removed by it.
- **Fix, later:** SY.admin's help for `get_runtime` and `remove_runtime_version`, and Archi's
  handbook, name the field and what it means.
- **SY.admin's part built on `coa/admin-139` for 0.1.139:** `get_runtime` has notes of its own
  (its `usage_global_visible`: `running_nodes`, `skipped_hives`, `hives_without_runtimes` and
  `in_use`, null never read as not in use), and both its help and `remove_runtime_version`'s name
  `hives_without_runtimes` (`HIVES_WITHOUT_RUNTIMES_NOTE`): each ingress and egress, `{hive_id,
  role}`, by the role the registry records, neither asked, counted nor skipped, and never holding
  a removal up. Its test, `the_runtime_help_names_the_hives_without_runtimes`, fails without it.
  Archi's handbook is the architect branch's.
- **Archi's handbook part, built on `coa/architect-139` for 0.1.139 (docs only):** §7.7 says where a
  runtime's use across the hives is (`get_runtime`'s `payload.usage_global_visible`, the read
  `remove_runtime_version` decides on) and names each field: `running_nodes` (and
  `running_count`), `skipped_hives` (`{hive_id, reason}` and a `message` when there is one, a use
  unknown, never none), `hives_without_runtimes` (`{hive_id, role}`: each ingress and egress by
  the role the registry records, whatever its join; not asked, neither counted nor skipped, never
  a hive not read, and one that is down holds no removal up) and `in_use` (true, false, or null
  while a hive was skipped), and the shape `{scope, status: "error", message}` when the motherbee
  cannot read its own nodes or its hive registry, a use unknown too (the review: it named a
  skipped hive's `message` as always there, and not that shape). SY.admin's help for
  `get_runtime` and `remove_runtime_version` is the other part, in SY.admin's binary, not on this
  branch. Archi reads the handbook from `/etc/fluxbee/handbook_fluxbee.md`, which the package
  installs.
- **At 0.1.139's integration (`coa/sweep-139`):** the two parts, and A-300 on `coa/orch-139`, were
  built apart. `get_runtime`'s own note in SY.admin's help said a skipped hive's `message` is always
  there and named neither the error shape nor `RELAUNCH_DUE`, which A-300 adds to `running_nodes`;
  the handbook's `running_nodes` named only a unit that may run. Both now say what the orchestrator
  answers: `RELAUNCH_DUE` (a node pinned to a version whose unit does not run and that its
  orchestrator relaunches within a minute), a skipped hive's `{hive_id, reason}` and its `message`
  when its answer had one, and `{scope, status: "error", message}` when the motherbee cannot read
  its own nodes or its registry. `the_runtime_help_names_the_hives_without_runtimes` checks the
  note, and fails without it.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `bea59036` (SY.admin's
  help) and `be09a8bf` (the handbook), the review `d488b25e` and the integration's `c695490c`. On
  0.1.138 (00:46Z) the handbook held `hives_without_runtimes` 0 times. On 0.1.139 `get_runtime
  ai.generic` answered `usage_global_visible.hives_without_runtimes` egress1 (egress) and ingress1
  (ingress) in 4.2 s; the help of `get_runtime` and `remove_runtime_version` read after the
  deploy; the handbook (P1, 02:02Z and again 05:37Z) holds `hives_without_runtimes` once, beside
  A-124's §4.5.1, the error shape and A-337, each once.

### A-314 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.134, 2026-11-11; a new node launched and killed in 9.8 s, the stopped nodes with no unit read `STOPPED`, a core node's kill refused `INVALID_REQUEST`) (found 2026-11-11 by 0.1.133's gate; seen on 8.x, read in the code; since 0.1.133) — A node unit systemd does not have reads as no answer: no node can be launched

- **Seen on 8.x (0.1.133, 2026-11-11 22:41Z):** a `run_node` of `AI.gate0233@worker1` answered
  `SERVICE_FAILED` *"systemctl did not answer whether the node's unit runs, asked again; the node
  was not launched, and its config and identity were undone"*. The stopped `AI.facturacion@worker1`
  and `AI.ventas@worker1` and the three `AI.gate0192*@worker1`, none with a unit, read
  `lifecycle_state` `UNKNOWN`, and the three gate0192 nodes, pinned to ai.generic 0.1.95, counted in
  ai.generic's use.
- **Why:** `service_activity` (`src/bin/sy_orchestrator.rs` ~36727; A-267, A-271, 0.1.133) reads
  `systemctl is-active` by its exit: 0 active, 3 not running, anything else no answer. On the hives'
  systemd 255 (Ubuntu 24.04) it exits 4 for a unit whose LoadState is not-found: a new node's unit
  before its first launch, and a stopped node's transient unit once it is gone. The code's comment
  took 3 for that case, and its test pinned 4 as no answer.
- **Effect:** every `run_node` of a new node, every start of a node whose unit is gone, and every
  `kill_node` (purge included) and `remove_node_instance` of such a node are refused
  `SERVICE_FAILED`; the reconcile never launches a node whose unit is gone, so after a reboot the
  nodes that should run stay down; Archi and the Cloud read such nodes `UNKNOWN`, and a managed node
  with nothing left reads `UNKNOWN` where it was `NODE_NOT_FOUND`. Read in the code, not seen live:
  a spoke's core update that drops a core node keeps its binary with an error (its unit file is
  gone, so its check reads as no answer), so that settle never closes; a hive without the
  `fluxbee-syncthing` unit never gets it repaired and its sync probe answers `SYNC_STATUS_FAILED`;
  a node directory whose `config.json` cannot be read and whose unit is gone fails the hive's whole
  runtime-use read (A-281). Nodes already running are not touched.
- **Why the tests passed:** the test doubles answer not running for a missing unit directly, past
  `service_activity`; the exit-code test pinned 4 as no answer. Since systemd 253 `is-active` exits
  4 when every named unit's LoadState is not-found and none is active; bus errors exit 1 (the
  review read systemd's source at v252, v253, v255 and v258).
- **Fix (0.1.134, the orchestrator alone, a hotfix; `coa/orch-134`, `7964c265`):** exit 4 reads as
  not running; exit 1 and a killed check stay no answer. **Its review (`8539bc64`):** reading a
  missing unit as not running reopens what 0.1.132 had and 0.1.133 refused only by accident: a
  `kill_node` with purge, or a `remove_node_instance`, of an `SY.*` or `RT.*` name (a core node,
  whose `fluxbee-node-*` unit never exists) removed the running core node's state (SY.timer's
  `timers.db`, SY.identity's config and secrets) and answered ok. Both now refuse such a name
  (`INVALID_REQUEST`), as `run_node` does. SY.admin's catalog example for both names
  `SY.admin@motherbee`: a managed node's name instead, with SY.admin's next change. Left, low: a
  core service on the role's list whose unit is not installed is started every 5 s with two
  warnings, as in 0.1.132 (only a broken install; none on 8.x). The releases planned after it move
  by one: the router 0.1.135, the AI SDK 0.1.136, the sweep's binaries 0.1.137 (their texts that
  say 0.1.136 change at their integration).
- **Checked on 8.x (0.1.134, 23:33Z):** `run_node` of `AI.gate0233@worker1` ok and its kill (ILK
  kept) ok, 9.8 s in all; the three gate0192 nodes and `AI.gate0233` read `STOPPED`; ai.generic's
  use lists only `AI.pruebas@worker1`, skipped none, `hives_without_runtimes` egress1 and ingress1
  (1.4 s); `kill_node` of `SY.gate0233@motherbee` answered `INVALID_REQUEST` (*"SY.* nodes are core
  nodes: they run under their own unit and are never stopped, purged or removed as a managed
  node"*). Before the deploy no node on the motherbee or worker1 had `relaunch_on_boot` true with
  its unit gone, so 0.1.134's reconcile launched nothing unexpected.
- **Its leftover, SY.admin's help (0.1.137, `3507f09a` on daily):** `kill_node`,
  `remove_node_instance` and `teardown_node` give `AI.sales@motherbee` as their example and say
  that a core node's name is refused `INVALID_REQUEST`; `get_node_config` and `set_node_config`
  give `AI.sales@motherbee` too. Checked on 8.x by 0.1.137's gate (2026-11-12 09:55Z); its smoke
  launched and killed a new node in 10.8 s and had `kill_node SY.gate0237@motherbee` refused
  `INVALID_REQUEST` in 1.061 s.

### A-313 [General/orchestrator] 🔵 low (found 2026-11-11 by A-283's second review; read in the code; there before it; to decide) — A Syncthing that cannot come up holds the whole watchdog about 35 s a tick

- **Why:** while Syncthing crash-loops (a corrupt database, a full disk: `Restart=always` with
  `RestartSec=5` never reaches the start limit), every tick that sees it inactive takes the
  unhealthy path: `systemctl start` succeeds (`Type=simple`), then `wait_for_syncthing_health`
  waits its full 30 s, and the tick is the whole watchdog's.
- **Effect:** a stopped core service waits up to about 35 s for the watchdog instead of 5 s, for as
  long as Syncthing cannot come up.
- **Fix, later (to decide):** the Syncthing watch and the peer reconcile run in a task or loop of
  their own, as `run_syncthing_options_switch_loop` already does; a structural change to the
  watchdog.
- **The operator's call (2026-11-12):** left as it is. *"este es un tema general, podría pasar
  con cualquier nodo del sistema, acá lo que hay que hacer es reintentar y log, lo que sí no puede
  pasar es tomar acciones globales sobre lo que ocurra, solo acciones sobre el nodo o herramienta que
  tenga problemas. no sirve watchdog de watchdog"*; if Syncthing failing stopped Fluxbee, a talk. It
  does not: the running services go on, and only the restart of another core service that fails
  meanwhile waits up to about 35 s instead of 5 s, while Syncthing cannot come up. The principle, for
  every node and tool: on a failure, retry and log, act only on the one that fails, never globally.

### A-312 [General/runtimes] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; a publish of 0.1.0+b1 refused 400 `INVALID_REQUEST`, *"carries build metadata ('+')"*, in 1.67 s, nothing installed) (found 2026-11-11 by A-287's review; read in the code; there before it) — A version with build metadata (`1.2.0+b1`) is published, then breaks every node built on it and cannot be removed

- **Why:** `validate_semver` (`src/runtime_package.rs` ~98) accepts build metadata, while SY.admin's
  `valid_runtime_token` (`src/bin/sy_admin.rs` ~18358, `remove_runtime_version`) and the
  orchestrator's `valid_token` (`src/bin/sy_orchestrator.rs` ~23923: `resolve_runtime_spawn_entrypoint`,
  `stamped_runtime_version`, `get_runtime_flow`) refuse `+`.
- **Effect:** once `ai.generic 0.2.0+b1` is current, every launch or restart of a config-only or
  workflow node on it fails `BASE_RUNTIME_NOT_AVAILABLE`, and `remove_runtime_version` answers
  `INVALID_REQUEST`. A version segment is also the one place where Archi (keeps `+`) and SY.admin's
  route (reads `+` as a space) disagree; both readings are refused today.
- **Fix, later:** refuse build metadata in `validate_semver`, or take `+` in both tokens.
- **Built on `coa/admin-139` for 0.1.139:** the package check SY.admin's publish and
  `fluxbee-publish` share (`validate_package`, its `validate_semver` in `src/runtime_package.rs`)
  refuses build metadata, in the package.json's `version` and in `fluxbee-publish --version`
  alike, before anything is installed: SY.admin answers `400 INVALID_REQUEST` with *"... carries
  build metadata ('+'), which a runtime version may not: use MAJOR.MINOR.PATCH, with an optional
  -prerelease such as 1.2.0-b1"*. A pre-release stays a version. SY.admin's publish maps its
  install's errors in one place (`publish_runtime_package_error_code`). Its tests:
  `a_version_with_build_metadata_is_refused_and_a_prerelease_is_not` (the library) and
  `a_package_version_with_build_metadata_is_refused_before_it_is_installed` (SY.admin), each
  failing without it. SY.admin's help (`publish_runtime_package`), sy-admin.md (A runtime
  package's publish), operations.md and Archi's handbook (§7.2) say it. Not covered:
  `scripts/publish-runtime.sh` (the .deb's seed and the `publish-*-runtime.sh` scripts) checks no
  version's form; it is given the release's own versions.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-13 01:50Z):** shipped as `86dc11ea`. A
  publish of `ai.gate0239m` 0.1.0+b1 answered 400 `INVALID_REQUEST`, *"package.json invalid:
  version='0.1.0+b1' carries build metadata ('+'), which a runtime version may not"*, in 1.67 s, and
  `get_runtime ai.gate0239m` then `RUNTIME_NOT_FOUND`: nothing installed. Not run on 0.1.138, where
  it would publish a version no API removes.

### A-311 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; `GET /hives/a.b` 400 `INVALID_HIVE_ID` with the rule in 0.895 s and the registry unchanged; through Archi `..%2Fx` refused the same, and a timed-out `add_hive` closed on it, never `OPERATION_ALREADY_TRACKED`; a planted `gate0237.bak` no hive to the runtime-use read, `/versions` and the Cloud's `list_nodes`, though `list_hives` lists it, A-330) (found 2026-11-11 by A-287's review; read in the code; there before it, widened by 0.1.137) — `get_hive` reads any file named `info.yaml` its hive id leads to

- **Why:** `get_hive` (`src/bin/sy_orchestrator.rs` ~4142, ~22522, `read_hive_info` ~22959) joins the
  hive id into `hives_root()/<hive_id>/info.yaml` with no `valid_hive_id` check; `remove_hive`
  checks it. SY.admin passes the id on as it comes (`handle_hive_paths`, the action's payload).
- **Effect:** `get_hive {hive_id: "/srv/x"}` answers `/srv/x/info.yaml` as JSON, and `..` climbs out
  of the hive registry; `hive_is_registered_for_inventory` and `hive_present_for_deployments`
  tell whether such a path exists. Only files named `info.yaml`. A mesh caller allowed `get_hive`
  could send any id before; an HTTP path segment cannot hold `/`. A-287's decoding (0.1.137) lets
  Archi's read tool name one: `GET /hives/%2Fsrv%2Fx`.
- **Fix (0.1.137, the orchestrator):** `get_hive` refuses a hive id that fails `valid_hive_id`
  (`INVALID_HIVE_ID`), as `remove_hive` does, and so do `read_hive_info` and the existence probes,
  as a defense. 0.1.137's own change widens it, so it ships with it.
- **Built on `coa/orch-136` (`111633b3`, its two-lens review's fixes `c23a485f`):** `get_hive` refuses
  such an id and reads nothing; `read_hive_info` and `write_hive_info` refuse it before any path is
  built; the inventory's, the deployments' and the drift alerts' probes and the add flows' stale
  directory probe answer it as no hive, with no path touched; every refusal of a hive id (add_hive's
  accept and each role's join flow, `remove_hive`, `get_hive`) is one answer whose message names the
  rule in SY.admin's and SY.identity's words (*"hive_id must be a hive id: 1 to 64 letters, digits,
  '_' or '-'"*), where the accept named a lowercase-only rule. The audit found no other path or
  shell a request's hive id reaches unchecked; no legitimate id is refused. **With it in 0.1.137:**
  SY.admin's help for `get_hive`, `add_hive` and `remove_hive` names `INVALID_HIVE_ID` (Archi reads
  it); Archi's reconcile of a timed-out `add_hive` closes its record on `INVALID_HIVE_ID` as on
  `NOT_FOUND` (it stayed open, and a retry was `OPERATION_ALREADY_TRACKED`); at the rebase on
  0.1.133, A-281's registry reader (`registry_hives_with_roles`) skips a directory whose name is not
  a hive id, as `read_registry_snapshot` does.
- **The readers, at the 0.1.137 integration (`a857cbb7` and `7423878d` on `coa/sweep-137`,
  `1e2491d2` and `6fbe98ab` on daily):** a registry directory whose name is not a hive id is no
  hive to A-281's reader (`registry_hives_with_roles`), to the orchestrator's
  `list_managed_hive_ids` (Fluxbee Cloud's `list_nodes` and `/versions`) and to every SY.admin read
  of the registry, which counts a connected entry as a hive only under its own name; two checks,
  each warned of once per name: the orchestrator's `registry_directory_is_hive` and SY.admin's
  `registry_hive_entries`. `list_hives` still lists one: A-330.
- **Checked on 8.x (0.1.137, gate0237, 2026-11-12 09:55–09:56Z and 11:08Z):** SY.admin's routes:
  `GET /hives/a.b` 400 `INVALID_HIVE_ID` with the rule in 0.895 s, an unknown hive 404 `NOT_FOUND`
  in 0.009 s, `DELETE` and `POST` of `a.b` 400 in 8 ms, the registry unchanged; the help of
  `get_hive`, `add_hive` and `remove_hive` names `INVALID_HIVE_ID` and the rule. Through Archi's
  SCMD: `GET /hives/a.b` and `GET /hives/..%2Fx` `INVALID_HIVE_ID` with the rule's message (2.8 s,
  1.2 s); `POST /hives` of `a.b`, twice, `INVALID_HIVE_ID` each time, never
  `OPERATION_ALREADY_TRACKED` (2.6 s, 2.8 s); with SY.admin frozen 184 s, an `add_hive` of `a.b`
  timed out in Archi (`TIMEOUT_UNKNOWN`), and once SY.admin went on the identical SCMD answered
  `INVALID_HIVE_ID` in 2.8 s, the open record closed (SY.admin's MainPID unchanged). The readers: a
  directory `gate0237.bak` planted in the motherbee's registry (a timer removed it) was left out of
  the runtime-use read, `/versions` and the Cloud's `list_nodes` (no unreachable hive), and the
  orchestrator warned of it once; `list_hives` listed it (A-330).

### A-310 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; with a running node's config.json broken, a runtime update removed no version and named the node *"while its unit runs"*, and in its crash loop *"while systemd has its unit on its way up or down"*; `get_runtime` skipped its hive `USAGE_UNREADABLE`) (found 2026-11-11 by A-281's orchestrator review; read in the code; there before it) — Runtime retention skips a node config it cannot read, and can delete a version only that node pins

- **Why:** `persisted_runtime_keep_versions_with_root` (`src/bin/sy_orchestrator.rs` ~23523) logs
  *"runtime retention skipping unreadable persisted node config"* and skips the node, so the
  version it pins is not kept.
- **Effect:** the next retention pass (the watchdog's verify, or a runtime `SYSTEM_UPDATE`) deletes
  from `dist/runtimes` a version directory that only that node pins and that the manifest no
  longer lists, under the running node. Since 0.1.133 a version leaves the manifest only through
  `remove_runtime_version`, which refuses while such a node may run (A-281's `USAGE_UNREADABLE`),
  so the window is a version removed by hand or before 0.1.133.
- **Fix, later:** keep every version of the runtime while such a config exists and its unit runs
  or gives no answer, and log it.
- **Built on `coa/orch-139` (for 0.1.139; `d081d505`).** The retention pass
  (`apply_runtime_retention_with_roots_and_protection`: the watchdog's verify and a runtime
  `SYSTEM_UPDATE`) asks systemd, through the state's unit checks, for the unit of each node whose
  `config.json` cannot be read or parsed (`persisted_runtime_keep_versions_with_root`). While that
  unit runs or gives no answer, the runtime and version the node runs on are unknown, so "every
  version of the runtime" is every version: the pass deletes nothing, names each such node
  (`held_by_unreadable_configs`) and logs *"runtime retention removed no version"*, and the next
  pass reads the configs again. One whose unit does not run is left out, as before (nothing
  relaunches a node whose config cannot be read). Test
  `runtime_retention_deletes_nothing_while_a_node_whose_config_cannot_be_read_may_run` (fails
  without it: the version only that node pins is deleted). Docs: sy-orchestrator.md.
- **The review (2026-11-12), built on `coa/orch-139`:** something does relaunch such a node:
  systemd. A node's unit is transient with `Restart=always`, so systemd restarts it on its own on
  the version its `ExecStart` names, and a crash loop's unit between two restarts (`activating`,
  `auto-restart`) reads not running (`is-active` exits 3), most of the time, for a node whose config
  cannot be read, which may well crash-loop. The pass now asks systemd for such a unit's `ActiveState`
  (`UnitChecks::at_rest`, `systemd_unit_at_rest`) and leaves the node out only when it is
  `inactive` or `failed` (`unreadable_config_unit_state`); the runtime's use (A-281's
  `USAGE_UNREADABLE`) reads it the same way, so the two still agree. Tests: the retention test's
  crash-loop cases (fail without it: the pinned version is deleted), the use's
  `a_node_whose_config_cannot_be_read_while_it_may_run_leaves_the_use_unknown`, and
  `a_unit_is_at_rest_only_when_systemd_has_it_inactive_or_failed`. Docs: sy-orchestrator.md,
  operations.md, sy-admin.md and SY.admin's `RUNTIME_USAGE_UNKNOWN` note.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-13 01:56–01:58Z):** shipped as `151ff693`
  (its review `8bc3c5f3`). `AI.gate0239rm@worker1` running, pinned to `ai.gate0239r` 1.0.1, its
  config.json cut short (`{"_system":`): (a) a runtime update of worker1 (ok in 2.1 s) logged
  *"runtime retention removed no version"* naming the node *"while its unit runs"*, and no
  *"retention applied"*; `get_runtime ai.gate0239r` skipped worker1 `USAGE_UNREADABLE`, naming the
  node; (b) in a crash loop (the unit SIGKILLed, the update at once; NRestarts 2), the line named it
  *"while systemd has its unit on its way up or down"*, nothing removed (kill to update 19.355 s,
  RestartSec 5 s); worker1's `dist/runtimes/ai.gate0239r` kept 1.0.0 and 1.0.1. Once the config was
  restored the line no longer named it. Not run on 0.1.138 (the optional baseline); a version that
  left the manifest by hand while the node runs on it, by its tests.

### A-309 [General/SDK] 🔵 low (found 2026-11-11 by A-281's orchestrator review; read in the code; latent) — The Rust SDK's `send_with_matcher` keeps its pending entry when its caller drops the call

- **Why:** `crates/fluxbee_sdk/src/rpc.rs` ~1103 inserts the `PendingEntry` before the send
  (~1111); only an answer, the inner timeout or a failed send removes it. A caller that wraps the
  call in an outer timeout or a `select!` and drops it leaves the entry for the life of the
  process, and a send dropped before it went is never sent.
- **Today:** nothing drops one since A-281's orchestrator review removed the usage fan-out's outer
  cut (`84653ab5`, 0.1.133); the review checked `sy_orchestrator`, `sy_admin`, `sy_architect`,
  `sy_cognition`, `sy_identity` and `sy_storage`. The next caller that wraps an RPC in a timeout
  would leak again.
- **Fix, later:** a drop guard that removes the entry and marks the trace stale, in a release of
  the Rust SDK of its own (METHOD §9), with the SDK's lows.
- **Built on `coa/rustsdk-143` (for 0.1.143, planned; the Rust SDK alone, board item 30).**
  - **The guard.** `send_with_matcher` holds its entry through a `PendingGuard` from its insert to
    the call's end. A caller that drops the call drops the guard, and the guard removes the entry
    and notes the trace stale under one hold of the pending table. This covers an outer `timeout`
    and the losing branch of a `select!`, while the call waits for its answer or for room to send.
    - So the `trace_id` is free again, a late answer is dropped as stale and counted, never handed
      to a command channel, and a message that had not gone out is never sent.
    - `metric_abandoned_requests` counts such calls.
    - Each entry carries its call's id, so a guard never removes a later call's entry under the same
      `trace_id`.
  - **The tables.** The pending and stale tables became `std::sync::Mutex`es, which no holder keeps
    across a wait, so the guard's `Drop` can take them. When a holder takes both, it takes pending
    first, and the timeout's removal and its stale note now run under that one hold too.
  - **Its tests** (`rpc.rs`): a call its caller drops frees its trace_id; a late answer to a dropped
    call is stale; a call dropped before its message went out never sends it. Each fails on the
    code before it. A guard never removes a later call's entry.
  - **Docs:** the SDK's README and `docs/reference/protocol.md` §10.3 and §11 (Go's `ctx` did the
    same already).
  - **Not reachable on 8.x today:** nothing drops a call since `84653ab5`, so no live gate provokes
    it; it is checked by its tests.

### A-308 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; with worker1's orchestrator frozen and 4 `/versions` held, the Cloud's `list_nodes` answered 200 in 12.5 s naming worker1 `NODE_LIST_FAILED`, where 0.1.138 answered `TIMEOUT` at 30.0 s; egress1's and ingress1's own reads in 0.54 to 0.77 s) (found 2026-11-11 by A-281's orchestrator review; read in the code; there before it; seen live by 0.1.133's gate) — Fluxbee Cloud's `list_nodes` can wait behind `/versions` while a worker does not answer

- **Why:** `read_target` maps `list_versions` and `list_tenant_nodes` to the same
  `ReadTarget::EveryHive` (`src/bin/sy_orchestrator.rs` ~1495), 4 places; `list_versions_flow`
  (~21212) asks the hives one after another, each through the 45 s forward.
- **Effect:** while a worker's orchestrator does not answer, 4 `/versions` reads (Archi, the
  operator, `ops.py versions`) hold the places for 45 s or more, and a Cloud `list_nodes` waits
  past the Cloud's 20 s.
- **Fix, later:** a target of its own for `list_tenant_nodes`, as 0.1.133 gives the runtime reads
  (`ReadTarget::RuntimeUse`), or `list_versions` asks every hive at once with a 10 s bound.
- **Seen live (0.1.133's gate, run on 0.1.134, 2026-11-11):** with worker1's orchestrator
  SIGSTOPped, 4 `GET /versions` held 30.8 to 31.3 s each; the Cloud's `list_nodes` for `pruebas`,
  started while they held, answered `TIMEOUT` at 31.8 s. With only 4 runtime reads in flight it
  answered in 13.1 s, worker1 in `unreachable_hives` (`NODE_LIST_FAILED`, 10 s).
- **Built on `coa/orch-139` (for 0.1.139; `0b07426b`), the first of the two fixes:**
  `list_tenant_nodes` waits in a read target of its own (`ReadTarget::TenantNodes`, 4 places),
  never behind `list_versions` (`ReadTarget::EveryHive`), as the runtime reads do since 0.1.133. A
  `/versions` read keeps its own cost (the hives asked one after another, 45 s for each that does
  not answer), now in places that only `/versions` reads share. Test
  `fluxbee_clouds_node_list_never_waits_behind_versions` (fails without it: the tenant read waits
  behind the 4 held `/versions`), and the lane-target tests name the new target. Docs:
  sy-orchestrator.md.
- **The review (2026-11-12), built on `coa/orch-139`:** with a target of its own, one hive that
  does not answer delays 4 targets (its own, `/versions`, the runtime use, the tenant's nodes), 4
  reads each: all 16 forwarding places of the admin worker, where A-199 had left 4 to the hives
  that answer (a read of a healthy hive would wait up to 45 s). The fan-outs
  (`ReadTarget::is_fan_out`: `EveryHive`, `RuntimeUse`, `TenantNodes`) now hold only their
  target's 4 places and no forwarding place, so a silent hive holds 4 forwarding places and 12 stay
  for the others; a worker runs at most 16 local reads, 16 forwarded reads and 12 fan-outs at once.
  Test `a_silent_hive_and_the_fan_outs_it_delays_leave_room_for_other_hives` (fails without it:
  the other hives' reads wait). Docs: sy-orchestrator.md (the lanes and the runtime use's time).
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `c9375192` (its review
  `8bc3c5f3`). worker1's orchestrator SIGSTOPped. On 0.1.138 (01:10Z) 4 `GET /versions` held, and
  the tenant's node list (the Cloud's `list_nodes` for `pruebas`) 2 s later waited behind them: 504
  `TIMEOUT` in 30.005 s. On 0.1.139 (01:55Z), at once, 4 each of worker1's versions, `/versions`,
  `get_runtime ai.generic` and the tenant's nodes: the 4 tenant reads 200 in 12.50 to 12.58 s,
  worker1 in `unreachable_hives` (`NODE_LIST_FAILED`); the runtime reads 11.28 to 11.31 s; worker1's
  own reads and `/versions` 504 `TIMEOUT` at 30.7 to 30.9 s. Two seconds later egress1's own
  versions answered in 0.771 s, ingress1's in 0.54 s and the motherbee's in 0.741 s: the fan-outs
  hold no forwarding place. worker1 answered again after the thaw (0.87 s).

### A-307 [WF/wf-rules] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; with 14 changes queued behind a frozen Syncthing, 6 answered 409 `BUSY`, `retryable`, within SY.admin's 75 s after waits of 56.0 to 72.6 s, and 11 were refused in all, each after more than 55 s; no read refused, the refused changes changing nothing; on 0.1.138 a queued change timed out at 75 s while it ran) (found 2026-11-11 by A-281's SY.wf-rules review; read in the code; there before it, longer since 0.1.133) — SY.wf-rules handles one request at a time, and a request queued behind another starts its 60 s late

- **Why:** SY.wf-rules serves its requests in turn, and each request's 60 s start when its turn
  comes. Since 0.1.133 a purge waits up to 35 s for each removal while a hive cannot be read.
- **Effect:** two callers on one hive's SY.wf-rules while some hive is not read: the second can end
  just past SY.admin's 75 s, so SY.admin answers `TIMEOUT` for a change that may still complete.
  Bounding a request by its arrival alone is worse: the Go SDK's `SendWithMatcher` sends even with
  an expired context (`go/fluxbee-go-sdk/dispatcher.go` ~627-687), so a deploy would answer
  `PACKAGE_PUBLISH_FAILED` while SY.admin installs its package.
- **Fix, later (to decide):** (a) a change whose turn comes with less than about 20 s left is
  refused at once, nothing changed, with a retryable `BUSY` (SY.admin maps it to 409; its help and
  Archi's handbook would name it); or (b) the requests run at the same time, with a lock per
  workflow.
- **The operator's call (2026-11-12):** (a): a change whose turn comes with less than about 20 s
  left is refused at once, nothing changed, with a retryable `BUSY` (SY.admin's 409; its help and
  Archi's handbook name it). In the bug push (item 30).
- **Built on `coa/wf-139` (for 0.1.139), SY.wf-rules and SY.admin's texts:**
  - **SY.wf-rules (`node/turn.go`, `node/service.go`, `node/dispatch.go`):** a goroutine takes each
    message as it arrives, with when it came (`time.Now`, monotonic), into a queue of 64 where the
    requests wait for their turn in order (`takeRequests`); the loop still serves one at a time. A
    change, a compile (a `CONFIG_SET` with no operation is one), an apply, a rollback, a
    `compile_apply` or a delete, as an operation or a command, whose turn comes with less than
    20 s left of SY.admin's 75 s wait (`wf_rules_request_timeout`'s default; SY.wf-rules cannot
    read `JSR_ADMIN_WF_RULES_TIMEOUT_SECS`), counted from its arrival, is refused at once, before
    its first step: `BUSY`, `retryable: true` beside the error (`WfRulesError.Retryable`), the
    detail saying how long it waited and how much was left; the log says `<operation> refused
    BUSY (trace_id=...): its turn came <waited> after it arrived`. A check, the queries,
    `CONFIG_GET` and `NODE_STATUS_GET` are never refused so. "Left" is read as SY.admin's wait,
    whose 75 s the entry's effect names: a change refused waited more than 55 s. A change that
    runs keeps its 60 s from its turn, as the entry's reasoning asks: bounded by its arrival, one
    that ran out mid-flight would still send its next step. Its residue: a change whose turn
    comes with 20 to 75 s left and then takes most of its 60 s can still end past SY.admin's
    wait, as before; and a wait is counted from when SY.wf-rules took the message, so time spent
    before it (the process stopped, or 64 requests already waiting) is not counted.
  - **SY.admin (`src/bin/sy_admin.rs`):** `BUSY` already mapped to 409 (a runtime lifecycle code of
    before); its comment names SY.wf-rules' refusal, and `WF_RULES_BUSY_NOTE` is in the help of
    `wf_rules_compile_apply`, `wf_rules_compile`, `wf_rules_apply`, `wf_rules_rollback` and
    `wf_rules_delete` (not the check's nor the reads'). SY.admin passes SY.wf-rules' payload, its
    `retryable` included.
  - **Tests:** `TestAChangeWhoseTurnComesLateIsRefusedBusy` (each of the nine changes at 55.001 s,
    nothing changed in the store, the dist or the publishes) and
    `TestTheLoopCountsATurnFromTheRequestsArrival` (the loop itself, the rule shortened to 2 s and
    1 s: a deploy that came while another ran for 1.2 s is refused, and runs when asked again), each
    failing without the fix (the second also without the arrival's stamp: counted from the turn,
    the deploy ran); `TestAChangeWithItsTurnInTimeRunsAndAReadAlways` (55 s runs; six reads at 80 s
    answered); SY.admin's `a_wf_rules_change_refused_busy_answers_409_and_its_help_names_it` (the
    help of each change names it, which fails without the note; the 409 with the payload's
    `retryable` kept, from a `CONFIG_SET` and from the delete's command, held before too).
    Docs: [sy-wf-rules.md](../../docs/nodes/sy-wf-rules.md),
    [sy-wf-rules-spec](../../docs/sy-wf-rules-spec.md) §5.2 and §12,
    [sy-admin.md](../../docs/nodes/sy-admin.md) and Archi's handbook §4.2.7.
  - **The review (on `coa/wf-139`):** the refusal's detail said a change waited *"less than the
    20s a change is given"*, while a change that runs is given its 60 s from its turn: it now says
    *"less than the 20s of it a change needs left at its turn"*. The zero rule (no wait) refuses
    nothing, where a Service that `NewService` did not build would have refused every change that
    waited at all (none is built so today). The residue is named where Archi and the operator read
    it: `WF_RULES_TIMEOUT_NOTE`, in the help of the five changes, says a `504 TIMEOUT` from a
    change is no refusal and the change may still complete, and to read `wf_rules_get_status`
    before staging it again (SY.wf-rules answers it once the change has ended, its requests in
    order; `WORKFLOW_NOT_FOUND` after a delete that went through); the handbook §4.2.7,
    sy-admin.md, sy-wf-rules.md and the spec §5.2 say it too. A read is never refused, but it
    waits its turn as any request, so one sent while changes hold SY.wf-rules can still get
    SY.admin's `TIMEOUT` (the gate's reads are timed for it). Tests:
    `TestTheZeroTurnRuleRefusesNothing` and the detail's words in
    `TestAChangeWhoseTurnComesLateIsRefusedBusy`, each failing without its fix; SY.admin's
    `a_wf_rules_change_that_times_out_is_no_refusal_and_its_help_says_to_read_the_status` (fails
    without the note).
  - **Left for Archi's branch (`coa/architect-139`), found by the review:** Archi's pipeline does
    not know `BUSY`. A plan step refused so matches no rule of `classify_failure_deterministic`
    (`src/bin/sy_architect/failure_classifier.rs`: at Execute only `COMPILE_ERROR` and
    `WF_NODE_NOT_DEPLOYED`), falls to `ExecutionActionFailed`, and the run is blocked as
    *"Pipeline execution failed"* with retry, restart_from_design and discard; Archi's prompt
    facts (`src/bin/sy_architect.rs`, the General section and the executor's Lifecycle selection)
    name only `SERVICE_FAILED` with `retryable: true` as a refusal that did nothing, and its text on
    `wf_rules_compile_apply` names no `BUSY`. The handbook says it, so Archi's model knows; the
    pipeline's own words do not. The fix there: a deterministic rule for `BUSY` (its payload
    `retryable: true`) that says nothing changed and offers to stage the step again, and `BUSY`
    beside `SERVICE_FAILED` in those facts. Since 0.1.139's integration review it has its own line
    in the board's Archi list (*A-307's Archi part*), so it stays open once this item closes.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `a7d6bb23` (its review
  `60485aeb`). worker1's Syncthing frozen (SIGSTOP, 240 s at most), so each change's node step
  waits for its package. On 0.1.138 (01:06–01:08Z): A answered 502 `WF_NODE_NOT_DEPLOYED` at
  56.5 s, B `TIMEOUT` at T0+76.0 s and C `TIMEOUT` at T0+110.0 s while it ran: the effect. In the
  first pass on 0.1.139, C's turn came at once with about 75 s of SY.admin's wait left, so no
  `BUSY` was due: its three FAILs were the tool's expectation. The rerun (05:34–05:36Z): 14
  changes queued (A at T0, B at +1 s, C1 to C12 every 5 s), three checks and a status read. A ran
  (502 in 55.2 s, Syncthing frozen); 6 changes answered 409 `BUSY` within SY.admin's 75 s, each
  `retryable`, after waits of 56.0 to 72.6 s, *"less than the 20s of it a change needs left at its
  turn"*; SY.wf-rules' journal refused 11 `BUSY` in all, each after more than 55 s (56.0 to
  97.5 s; the five past SY.admin's 75 s answered `TIMEOUT` by it first); no read was refused (two
  checks answered 200, 63.6 and 65.1 s after they were sent; one ran past SY.admin's 75 s); C10 ran
  after waiting 52.6 s; B ran past SY.admin's 75 s (504 `TIMEOUT` at 76.0 s: its turn came with
  20 s or more left, the residue the call accepts; a SEE). The refused changes changed nothing:
  the store went from version 9 to 12 by A, B and C10, the versions 0.0.10 to 0.0.12;
  `compile_apply` after the thaw ok in 6.5 s, the node `RUNNING` `HEALTHY`. A change that ran and
  could not rebind its node answers with no `retryable`: A-344.

### A-306 [General/admin] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; a publish behind a removal whose use read waited on a frozen worker1 answered 200 in 0.011 s, where 0.1.138's waited 10.074 s for it; with the lock held 80 s a publish answered `RUNTIME_MANIFEST_BUSY` at 60.044 s, `retryable`, and a SY.wf-rules deploy 422 in 4.126 s, nothing published) (found 2026-11-11 by A-281's SY.wf-rules review; read in the code; there before it) — A version's removal holds the manifest lock through its read of every hive's use, and a publish waits on it with no bound

- **Why:** `remove_runtime_version` takes `runtime_manifest_lock` and the file lock, then reads the
  version's use (up to SY.admin's 30 s); `handle_publish_runtime_package` waits on
  `runtime_manifest_lock` with no bound.
- **Effect:** while a hive is not read, each removal (SY.wf-rules' purges included) holds every
  runtime publish for up to 30 s, and a publish queued behind several waits longer with no answer.
  The hold is the same as 0.1.132's.
- **Fix, later:** read the use before taking the locks, then check the manifest again under them
  and read again only if it changed; or a bounded wait for the publish that answers a retryable
  code.
- **Built on `coa/admin-139` for 0.1.139, both halves:** a removal checks the manifest and reads the
  version's use with no lock held, then takes SY.admin's lock and the file lock, checks the
  manifest again (`runtime_removal_view`) and reads the use again, under the locks, only when the
  runtime's entry changed meanwhile; and SY.admin's own lock is waited for at most 60 s
  (`take_runtime_manifest_lock`, as the file lock is): a publish past it answers
  `409 RUNTIME_MANIFEST_BUSY` with `payload.retryable: true` (a new code; the file lock held past
  its 60 s answers it too, where it was `SERVICE_FAILED`), a removal `RUNTIME_REMOVE_FAILED` and a
  reset `RUNTIME_RESET_FAILED`, both retryable (A-305). The manifest's writers read and write the
  dist SY.admin's context names (`runtimes_root`, `DIST_RUNTIME_ROOT_DIR` on the motherbee), so the
  tests run a removal and a publish on a dist of their own. `test_hold_ms` holds the lock as
  before, now after the read of the use. Its tests: `a_removal_reads_the_versions_use_with_no_lock_held`,
  `a_removal_reads_the_use_again_when_its_runtime_changed_meanwhile` and
  `a_publish_waits_for_the_manifest_lock_a_bounded_time`, each failing without it; A-171's
  structural test reads the locked read the write is built on. SY.admin's help
  (`publish_runtime_package`, `remove_runtime_version`), sy-admin.md, sy-wf-rules.md,
  operations.md and Archi's handbook (§7.2) say it. A publish whose lock's file cannot be opened
  or locked still answers `SERVICE_FAILED` with no payload, as sy-admin.md now says (its review
  of 2026-11-12: it said false). Left: the comment over `runtimeRemovalTimeout`
  in `go/sy-wf-rules/node/admin.go` still says a removal holds the lock across its read (SY.wf-rules'
  branch, a comment only).
- **At 0.1.139's integration (`coa/sweep-139`):** that comment says what SY.admin does since this
  item: the use read with no lock held, the lock waited for up to 60 s, held by a publish across its
  install and by another removal across its write, and the use read again under it when the runtime
  changed meanwhile.
- **At 0.1.139's integration review:** this item's bound came after what its caller waits.
  SY.wf-rules waited 5 s for a publish, while A-304's admission reads every other hive's
  `WF.<x>` for up to 3 s first and SY.admin then waited up to 60 s for its lock: a lock held more
  than about 2 s made the deploy a timeout (`PACKAGE_PUBLISH_FAILED`, the slots moved before
  A-304's review fix), and SY.admin, given no deadline, published the version as current once the
  lock came. Now a `SY.wf-rules@<hive>` publish waits for SY.admin's lock 4 s at most
  (`WF_RULES_PUBLISH_LOCK_WAIT`, `publish_lock_wait`), and SY.wf-rules waits 10 s for the answer
  (`packagePublishTimeout`), so `RUNTIME_MANIFEST_BUSY` reaches it, retryable, with nothing
  published; a deploy's node step keeps 35 s at least (40 s before). The file lock, held long only
  by a writer outside SY.admin, is still waited 60 s. The comment over `runtimeRemovalTimeout`
  counts the admission's 3 s before the use read's 30 s (2 s of its 35 s left for the rest), and
  `WF_PACKAGE_HOLDER_READ_TIMEOUT`'s no longer calls its 3 s well within the publish's 5 s. Tests:
  `a_publish_waits_for_the_manifest_lock_a_bounded_time` (each caller's bound; fails without
  `publish_lock_wait`) and the publish's wait in `TestARemovalWaitsForSYAdminsReadOfEveryHive`.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `b6b14129` (its review
  `60ab8e30`; the lock's wait for SY.wf-rules at the integration's review, `68a292ff`).
  `admin-a306`, worker1's orchestrator frozen so the use read waits: on 0.1.138 (01:09Z) the
  removal answered `RUNTIME_USAGE_UNKNOWN` at 11.023 s, and the publish sent 1 s after it answered
  200 only after it ended, 10.074 s after its start; on 0.1.139 (01:51Z) the removal the same at
  10.766 s (`retryable`, worker1 `TIMEOUT` in `unanswered_hives`), and the publish 200 in 0.011 s.
  `admin-hold` (01:52–01:53Z), a removal holding SY.admin's lock 80 s (`test_hold_ms`): a publish
  3 s later answered 409 `RUNTIME_MANIFEST_BUSY` at 60.044 s and a second removal
  `RUNTIME_REMOVE_FAILED` at 60.095 s, each `retryable`; a SY.wf-rules deploy of `gate0239w` 422
  `PACKAGE_PUBLISH_FAILED` (*"RUNTIME_MANIFEST_BUSY: ... for 4s"*, `retryable`) in 4.126 s, nothing
  of `wf.gate0239w` published; the held removal 200 at 80.188 s, and `ai.gate0239b` then listing
  0.2.0 and 0.2.1.

### A-305 [General/admin] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; a removal that waited 60 s for the manifest's lock answered 409 `RUNTIME_REMOVE_FAILED` with `payload.retryable` true; `INVENTORY_UNAVAILABLE` not provoked, by its tests) (found 2026-11-11 by A-281's SY.wf-rules review; read in the code) — `INVENTORY_UNAVAILABLE` and a manifest lock not taken in time do not say in their payload that a retry may pass

- **Why:** `internal_inventory_unavailable` (`src/bin/sy_admin.rs` ~9828) answers 503 with
  `payload: null`, although its comment and the help call it worth a retry; `RUNTIME_REMOVE_FAILED`
  is the same when the manifest's lock is not taken within 60 s. Since 0.1.133
  `RUNTIME_USAGE_UNKNOWN` carries `retryable: true`.
- **Effect:** a caller that reads the payload cannot tell them from a final refusal. SY.wf-rules
  takes `INVENTORY_UNAVAILABLE` as retryable on its own (0.1.133), and Archi reads the help's
  "retry".
- **Fix, later:** `payload.retryable: true` on both.
- **Built on `coa/admin-139` for 0.1.139:** `INVENTORY_UNAVAILABLE` answers `payload.retryable:
  true`, from the A-62 table's admission and from `teardown_node` alike; a runtimes manifest's
  lock not taken answers its code (`RUNTIME_REMOVE_FAILED`, `RUNTIME_RESET_FAILED`) with
  `payload.retryable`: true when another writer held the lock past the 60 s, false when the lock's
  file cannot be opened or locked (`manifest_lock_refusal`, over the library's
  `runtime_manifest_lock_held_too_long`). SY.wf-rules' purge already reads `payload.retryable`
  (`removalFailure`), so no change there. Its test,
  `an_inventory_or_a_lock_not_read_in_time_says_a_retry_may_pass`, holds the manifest's lock with
  a second writer, and `the_http_teardown_waits_for_the_node_to_be_gone` checks the teardown's;
  both fail without it. SY.admin's help (`remove_runtime_version`, `reset_base_runtime`, the
  teardown note), sy-admin.md, sy-wf-rules.md and Archi's handbook (§7.7) say it. A publish's lock
  not taken in time is A-306's.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-13 01:51–01:53Z):** shipped as `ddfe64f6`.
  With SY.admin's lock held 80 s (A-306's `admin-hold`), a removal that waited 60 s for it answered
  409 `RUNTIME_REMOVE_FAILED` with `payload.retryable` true (60.095 s), and a SY.wf-rules deploy
  passed it on (`PACKAGE_PUBLISH_FAILED`, `retryable`); a removal whose use read timed out answered
  `RUNTIME_USAGE_UNKNOWN`, `retryable`, as since 0.1.133. `INVENTORY_UNAVAILABLE` was not provoked
  (the optional `admin-cut` was not run): by its tests.

### A-304 [General/admin] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; the motherbee's deploy of a workflow worker1 holds refused `UNAUTHORIZED`, naming `WF.gate0239w@worker1`, in 1.20 s while it ran and 1.22 s once stopped, its versions unchanged, and worker1's own redeploy ok in 6.5 s; a hive cut off and a removed hive by its tests) (found 2026-11-11 by A-281's SY.admin review fixes; read in the code; there before it) — A-62's workflow-package admission misses a WF node on a hive cut off from the LSA

- **Why:** `workflow_nodes_elsewhere` reads the connected nodes, which leave out the nodes of a hive
  the LSA holds stale, so while worker1 is cut off another hive's SY.wf-rules may publish
  `wf.<x>` although `WF.<x>@worker1` exists.
- **Effect:** bounded: a publish moves no pinned WF node, and a removal goes through A-281's read
  of every hive since 0.1.133. Refusing whenever any hive is stale would block every workflow's
  deploy on every hive.
- **Fix, to decide later:** refuse only when a cut-off hive may hold a `WF.<x>`, or leave it.
- **Widened by 0.1.133 (SY.wf-rules' review of A-281):** `wf.<x>` is one runtime for every hive.
  Since 0.1.133 a deploy numbers its versions above every version the dist holds, so a deploy of a
  name another hive also runs (its `WF.<x>` stopped, or its hive cut off) no longer stops at
  `VERSION_CONFLICT` on 0.0.1: it publishes above that hive's versions, and its purge, like a
  delete's, asks for their removal. A-281's read refuses while the other node runs or its hive is
  not read, but a stopped node is not counted (A-300), and the runtime's current moves to this
  hive's version. Before, only a third deploy reached the other hive's versions.
  `docs/nodes/sy-wf-rules.md` describes it.
- **Options:** (a) SY.admin refuses a publish or a removal of `wf.<x>` while another hive holds a
  `WF.<x>` config, connected or not; (b) SY.wf-rules numbers and purges only the versions it
  published; the shared current would still move between hives; (c) SY.wf-rules refuses a
  compile while the dist holds another hive's versions of `wf.<x>`, naming them; (d) a docs rule
  only. The review recommends (a), with this item.
- **The operator's call (2026-11-12):** (a): SY.admin refuses a publish or a removal of `wf.<x>`
  while another hive holds a `WF.<x>` config, connected or not. In the bug push (item 30).
- **Built on `coa/admin-139` for 0.1.139, (a):** SY.wf-rules@H's publish or removal of `wf.<x>` is
  refused `403 UNAUTHORIZED`, naming the node, while another hive holds a `WF.<x>`
  (`admit_workflow_package`, now async in the A-62 dispatch): first the nodes connected now and
  what the LSA last saw of each hive it holds stale (`ConnectedNow::last_seen`,
  `last_seen_node_names`), read from shared memory; then `WF.<x>`'s config on every other hive
  that may run one (the motherbee and each worker whose join is done), asked at once through the
  motherbee's orchestrator (`get_node_config`, 3 s each, within SY.wf-rules' 5 s for a publish;
  `NODE_CONFIG_READ_FAILED` counts as held). A hive that does not answer in time is judged by what
  the LSA last saw of it and named in SY.admin's log (a warn line, in no answer), never refused
  for: refusing whenever a hive
  cannot be read would stop every workflow's deploy on every hive while one is down (this entry's
  own concern, and the operator's principle of 2026-11-12 in A-313). What stays open: a stopped
  `WF.<x>` on a hive that cannot be read at that moment is not seen, nor one, stopped or running,
  on a hive cut off since before the motherbee's router last started, since the LSA's last view
  lives in the router's memory and a deploy restarts it; their versions still stay, since a
  removal reads every hive's use first (A-281). Its tests:
  `a_wf_rules_package_is_refused_while_another_hive_holds_its_wf_node` (a config held, one that
  cannot be read, a motherbee that does not answer, a hive cut off) and the LSA's last view in
  `connected_nodes_come_from_the_routers_and_the_lsa`, each failing without it. sy-admin.md,
  sy-wf-rules.md, control-path-hardening-v1 and Archi's handbook (§7.5) say it.
- **Its review (2026-11-12), on `coa/admin-139`:** (1) what the LSA last saw counts only for a hive
  the registry still lists (`nodes_of_listed_hives`): the router keeps a removed hive's last LSA
  until it restarts, so once a hive that ran `WF.<x>` was removed with `remove_hive`, every hive's
  deploy and delete of the name was refused, naming a hive that no longer exists, where 0.1.138
  admitted them; a hive gone for good holds nothing up, as in a teardown. The A-304 test now has
  that removed hive admitted and a listed one refused, and fails without it. (2) The docs say what
  the LSA's last view misses (above). (3) A workflow delete's purge meets the same refusal at the
  first version it asks to remove and stops there, so every version it had to remove stays in
  `packages_left`, the first `UNAUTHORIZED` (retryable false, SY.wf-rules' `removalFailure`) and
  each after it `PURGE_STOPPED` (said here, as first written, to be each `UNAUTHORIZED`; and
  retryable until 0.1.139's integration review, below), until that hive's workflow is deleted,
  and a deploy of the name meanwhile is refused too; SY.admin's help of `wf_rules_delete`
  (`WF_DELETE_VERSIONS_LEFT_NOTE`), sy-wf-rules.md and Archi's handbook say so, and that such
  versions are never offered for removal. Left: SY.wf-rules' own `warning` on a delete still says
  a later deploy of the name purges the versions left, after an `UNAUTHORIZED` too (its branch).
- **At 0.1.139's integration (`coa/sweep-139`):** SY.wf-rules' branch was built beside this one, so
  the integration fixes that warning (`packagesLeftWarning`, `go/sy-wf-rules/node/delete.go`): a
  purge that stopped at `UNAUTHORIZED` says that another hive holds a `WF.<workflow>` (its detail
  names it), whose package the versions are too, that they stay until that hive's workflow is
  deleted, and that a deploy of the name on this hive is refused meanwhile, as SY.admin's help says;
  no `remove_runtime_version` and no later deploy offered. Its test,
  `TestADeleteWhosePurgeIsRefusedUnauthorizedSaysItsVersionsStay`, fails without it.
  sy-wf-rules.md, the SY.wf-rules spec's delete and the WF quickstart say so (sy-wf-rules.md said
  SY.admin refuses both "whatever the warning says"; the other two named no such exception).
- **At 0.1.139's integration review (`coa/sweep-139`):**
  - **Every version a purge leaves after an `UNAUTHORIZED` is not retryable**
    (`purgeStoppedAt`, `go/sy-wf-rules/node/package_publish.go`): the admission refuses by the
    workflow, never by the version, so each `PURGE_STOPPED` after it may be the other hive's
    current or backup. They said `retryable` true, so Archi's report printed them
    *"PURGE_STOPPED, retryable"* and SY.admin's help offered a `remove_runtime_version` for each,
    which the operator's own call, not admitted by A-304, would have run: the other hive's
    versions removed, its backup slot's included, which no use read protects. SY.admin's help
    (`WF_DELETE_VERSIONS_LEFT_NOTE`: none of them is offered, and "the others" are only those
    after any other refusal), Archi's handbook (§4.2.7 and §7.5), sy-admin.md, sy-wf-rules.md,
    the spec, the WF quickstart and operations.md say the shape as the code gives it: the first
    `UNAUTHORIZED`, each after it `PURGE_STOPPED`, none retryable. Tests:
    `TestADeleteWhosePurgeIsRefusedUnauthorizedSaysItsVersionsStay` (now its `PURGE_STOPPED`'s
    `retryable`) and `TestEachVersionLeftSaysWhetherAskingAgainMayRemoveIt`, each failing
    without it.
  - **A deploy refused at its publish moves no slot** (`publishSlot`, `applyWorkflow`,
    `rollbackWorkflow`): SY.wf-rules publishes the staged definition (a rollback's backup) first
    and rotates the slots only once SY.admin took it. It rotated first, so an apply refused (this
    refusal, routine since A-304) left `current` naming a version never published and dropped
    the backup while the WF node still ran the version before it, and a rollback refused the same
    way rotated them again. A rotation that fails after the publish (this hive's disk) says the
    package was published and moving the slots failed, to ask again. `PACKAGE_PUBLISH_FAILED` carries
    SY.admin's `retryable` (a `RUNTIME_MANIFEST_BUSY`). Test
    `TestADeployWhosePublishIsRefusedMovesNoSlot` (an apply and a rollback, refused
    `UNAUTHORIZED` and busy, then the apply taken), failing without it.
  - **The deploys' help names the refusal** (`WF_RULES_PACKAGE_REFUSED_NOTE` on
    `wf_rules_compile_apply`, `wf_rules_apply` and `wf_rules_rollback`): 422
    `PACKAGE_PUBLISH_FAILED`, its detail starting with SY.admin's `UNAUTHORIZED` and naming the
    other hive's `WF.<x>`, and that nothing changed on the hive; the handbook's §7.5 too (it named
    only `UNAUTHORIZED`, which the deploy never answers). Test
    `a_deploys_help_says_what_a_refused_package_answers`.
  - Its admission's 3 s and the publish's wait: A-306's entry.
- **The operator's call (2026-11-12), on what the build left to it:** a hive that does not answer in
  3 s is judged by the LSA's last view and named in a warning (SY.admin's warn line), never refused
  for it. The code as built: refusing whenever a hive cannot be read would stop every workflow's
  deploy on every hive while one is down.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-13 01:53–01:54Z):** shipped as `89cd0394`
  (its review `60ab8e30`; SY.wf-rules' side at the integration, `c695490c`, and its review,
  `68a292ff`). `gate0239w` deployed on worker1, its node `RUNNING`: the motherbee's deploy of the
  same body answered 422 `PACKAGE_PUBLISH_FAILED`, its detail *"UNAUTHORIZED: SY.wf-rules@motherbee
  may not change wf.gate0239w: wf.gate0239w is also the package of WF.gate0239w@worker1 ..."*, in
  1.20 s; the node killed without purge (its config kept), the same deploy refused again in 1.22 s,
  *"whose config that hive holds, connected or not"*; `wf.gate0239w` still listed 0.0.2 alone, the
  node `STOPPED`; worker1's own redeploy ok in 6.5 s, the node `RUNNING` on 0.0.3, the previous
  version purged; no *"workflow package admitted without reading every hive"* line while every hive
  answered. No baseline (the deploy hold). Not run: the optional `admin-cut` (a hive cut off from
  the LSA); a hive removed with `remove_hive` whose last LSA showed a `WF.<x>`, by its test.

### A-303 [General/frontdesk] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; with the identity region unreadable the frontdesk answered `identity_unavailable` in 0.76 to 0.81 s after reading it again, where 0.1.138 answered a final `tenant_unresolved`; under identity rewrites 590 of 590 probes `invalid_request`, where 0.1.138 had 28 of 596 `tenant_unresolved`; the two rarer paths left, the operator's call) (found 2026-11-11 by A-293's review fixes; read in the code; there before it) — The frontdesk answers a final `tenant_unresolved` when the identity SHM is momentarily unreadable

- **Why:** `identity_shm_ilk_tenant` gives nothing when the SHM cannot be read for a moment, so
  `resolve_case_tenant` gives `CASE_TENANT_MISSING`, answered `tenant_unresolved` with `retryable`
  false, although sending it again would pass.
- **Effect:** the Cloud reads a final refusal for a passing failure (`docs/io-cloud-api.md` says it
  is final); the operator sees the frontdesk's warning *identity SHM unreadable*.
- **Fix, later (the frontdesk):** an unreadable SHM answers a retryable code, as SY.identity's own
  passing failures do since 0.1.125 (`identity_unavailable`).
- **Built on `coa/frontdesk-139` (for 0.1.139; the frontdesk and `gov-common`, no SY.identity or
  SDK change):**
  - **A read of the identity SHM gives one of three answers** (`IlkTenantRead`): the ILK's tenant;
    not held (the region holds SY.identity's own ILK but not the case's: final, `tenant_unresolved`
    as before; mostly an ILK SY.identity does not hold, and two rarer paths read the same, below);
    or unreadable (the SDK's read failed: a full rewrite of the region holds a reader past its 5 ms
    seqlock wait, at SY.identity's start and on every delete, restore or purge; or the region is
    being created again, or gone).
  - **A registration reads it again** (`read_ilk_tenant`, `SHM_READ_RETRY`), the handoff and
    `ilk_register` alike: after 50 ms, 200 ms and 500 ms, each failed read logged with its reason,
    the first answer the region gives taken. Still unreadable, nothing is sent and the answer may
    pass: `identity_shm_unreadable` (`CASE_TENANT_UNREADABLE` in `gov-common`, which
    `identity_error_is_transient` counts), `retryable: true`, `IDENTITY_UNAVAILABLE` /
    `identity_unavailable`, the case left open; in a conversation it is the turn's attempt. About
    0.75 s and the reads before the registration's 8 s, under io.api's 10 s: a handoff reads it
    once, and its registration takes the tenant the handoff read (`IlkRegisterTool::case_tenant`;
    the branch's review found a second read with its own 0.75 s, up to 9.5 s in all). The SHM
    stays shared memory: the frontdesk asks SY.identity nothing in its place. The root-tenant check
    and the usage account read it once, as before (unreadable: the turn goes on, usage
    `fallback_root`).
  - **The rarer paths** (the state review's two, and a third from the branch's review):
    - *A region created again empty after a limits change* (SY.identity unlinks and recreates it
      at a start with other limits, and writes it a moment later; the region stays empty if that
      first write fails): told apart. A region SY.identity wrote always holds SY.identity's own
      system ILK, seeded into its store before its first write; a read that lacks both the case's
      ILK and SY.identity's (`ilk_tenant_in_region`) is unreadable, read again, never
      `tenant_unresolved`.
    - *A failed delta apply* (SY.identity answers the provision although its write to the region
      failed, `identity shm delta apply FAILED`: a full region is the case that happens, the
      provision's fast path refusing `SlotFull` without falling back to a rebuild): not told apart.
      The region looks the same as for an ILK SY.identity does not hold, and the SDK's reader keeps
      the header's counts and limits to itself; a read again would not pass either, since nothing
      writes the ILK until a whole rewrite of the region holds it (a full region needs higher limits
      first). It stays a final `tenant_unresolved`.
    - *A primary SY.identity running without its database* (the branch's review, 2026-11-12, read
      in the code; there before it): its store starts as the bootstrap one (the root tenant), it
      seeds the system ILKs, and rewrites the region from it, the region reused when the limits are
      the same; the ILKs its replicas push come in too. So the region holds SY.identity's own ILK
      and no person (`sy.identity started without active DB backend`, or `continuing with
      in-memory bootstrap` when the load fails; SY.vault not answering within the 60 s SY.identity
      waits for it at its start does it, until SY.vault's secret broadcast restarts SY.identity).
      Not told apart: every person reads as not held, a final `tenant_unresolved`, while
      SY.identity, with no database at all, answers a registration `DB_NOT_READY`, which may pass.
      It answers a provision so too, so `register_human` stops at its provision; what meets it is a
      case provisioned before that start: an io.api handoff for a subject io.api keeps in its
      cache (it reuses the ILK when the SHM misses it), or a conversation's turn, which closes its
      case as `completed_error` for good (`missing_tenant_id` is not transient).
  - **To decide (the operator), for the two paths not told apart:**
    - *The failed delta apply:* (a) the Rust SDK exposes the header's counts and limits (its own
      release), so the frontdesk tells a full region apart; (b) SY.identity fails the provision, or
      rebuilds the region, when its write fails; (c) leave it as it is.
    - *The primary without its database:* (a) the frontdesk takes a region with no human ILK for
      one not written in full: read again, then `identity_unavailable` (the frontdesk alone; on a
      hive with no person yet, a case whose ILK is gone then answers one that may pass; a region of
      system ILKs only, or of the root tenant only, would not catch it, since the replicas' node
      ILKs come in); (b) SY.identity: a primary without its database keeps the content of a region
      it reuses instead of rewriting it from the bootstrap store; (c) leave it as it is.
  - **The operator's call (2026-11-12):** (c) for both: the two rare paths (a failed delta apply, a
    primary without its database) are left as they are, recorded here and in the frontdesk's docs
    (`docs/nodes/sy-frontdesk-gov.md`, `docs/ai-frontdesk-gov-spec.md` §4.3).
  - **Tests** (each fails without its part of the fix):
    `a_handoff_whose_identity_shm_cannot_be_read_answers_identity_unavailable` and
    `a_turn_whose_case_tenant_cannot_be_read_awaits_the_confirmation` (they read
    `tenant_unresolved`, or a final answer, with the unreadable read taken as no tenant);
    `the_identity_shm_is_read_again_while_it_cannot_be_read` and
    `a_handoff_reads_the_identity_shm_again_and_registers` (without the reads again; the latter
    also with the handoff's registration reading the region again: 4 reads, not 3);
    `a_handoff_reads_the_tenant_of_its_case_once` (with that second read, a region that turns
    unreadable after the handoff's read failed the handoff with `identity_unavailable`);
    `an_identity_shm_not_written_yet_says_nothing_of_an_ilk` (an empty region read as not holding
    the ILK); `gov-common`'s `an_identity_shm_that_could_not_be_read_is_a_failure_that_may_pass`.
  - **Docs:** `docs/nodes/sy-frontdesk-gov.md` (Case tenant rules, ILK_REGISTER, Failure behavior,
    Health), `docs/ai-frontdesk-gov-spec.md` §4.3, §7.2, §9 and §12, `docs/reference/identity.md`,
    `docs/reference/shared-memory.md` §7, `docs/io-cloud-api.md` §5.2 and §9 (0.1.139, not deployed
    yet; `tenant_unresolved` as the frontdesk's copy of the registry not holding the identity, which
    may still exist), the node's README. Archi's handbook and SY.admin's help name none of it.
  - **At the integration:** `coa/edge-139` adds its own `### 0.1.139` to `docs/io-cloud-api.md` §9
    at the same place; the two become one entry, under this branch's heading, which says
    *not deployed yet* until the release is deployed.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `77a5fce8` (its review
  `9b7659b3`). Probes from a mesh client, `IO.gate0239f@motherbee` (a `frontdesk_handoff` that
  stops at the tenant check: no registration, no model call), for two temporary people in pruebas.
  On 0.1.138 (01:27–01:29Z, run again after the tool's fix): under 60 s of one person's deletes and
  restores, 28 of 596 probes answered `tenant_unresolved`; with the region unreadable (an
  `InaccessiblePaths` drop-in) and with an empty region bound over it, 3 of 3 `tenant_unresolved`
  each, in 5 to 40 ms, the second with no SHM line. On 0.1.139 (02:05–02:08Z): every probe of the
  churn `invalid_request` (590), with 18 *"reading it again"* and 16 *"readable again"* lines; the
  unreadable region `identity_unavailable` in 764 to 769 ms, the empty one in 768 to 806 ms with the
  reason *"the region does not hold SY.identity's own ILK (0 ILKs)"*, each after its three reads
  again (50, 200 and 500 ms), no `ILK_REGISTER`; an ILK the region does not hold
  `tenant_unresolved` in 11 ms, read once. The two rarer paths (a failed delta apply, a primary
  without its database) are left as they are by the operator's call: not provoked.

### A-302 [WF/wf-generic] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; each WF node's status names its engine, `extensions.engine` wf.engine 0.1.139 and its executable, as its unit's ExecStart does, where 0.1.138's named none) (found 2026-11-11 by A-284's review fixes; read in the code; seen live by 0.1.138's gate) — Which wf.engine a WF node runs shows only in its unit's ExecStart

- **Why:** wf-generic's `NodeRuntime.VersionStr` is a fixed `"1.0"` that nothing reads
  (`go/nodes/wf/wf-generic/node/node.go:102`); `get_node_status`'s `runtime_version` and the
  config's `_system.runtime_version` hold the workflow package's version.
- **Effect:** while a deploy is under way (Syncthing can bring `wf.engine` to a worker before its
  core update, and its orchestrator relaunches the WF node at its next pass), nothing the operator
  reads says which engine each WF node runs; only the unit's ExecStart or the orchestrator's
  relaunch lines do.
- **Fix, later:** the WF node reports the engine version it runs, and `get_node_status` shows it
  next to the package's.
- **Seen live (0.1.138's deploy, 2026-11-12):** the deploy relaunched `WF.gate0238@worker1` on
  wf.engine 0.1.138: the gate's `pre` step read it in the WF unit's ExecStart at 13:29:35Z, after
  1 s of waiting for the relaunch.
- **Built on `coa/wf-139` (for 0.1.139), the WF node alone:** the node reads the path of its own
  binary at its start (`os.Executable`) and takes the runtime version whose directory holds it,
  `<dist>/runtimes/<runtime>/<version>/bin/<binary>`, the version its unit's ExecStart names
  (`EngineOf`, `node/engine.go`); its journal says it (`wf: engine wf.engine 0.1.139 (<path>)`).
  `NODE_STATUS_GET` answers it as `extensions.engine` `{runtime, version, executable}` in every
  state (`HEALTHY`, `DEGRADED`, a config not in use), `runtime` and `version` null for a binary that
  is no runtime version's; the orchestrator's `get_node_status` already passes a node's
  `extensions` on, so `GET /hives/{hive}/nodes/{node}/status` shows `node_status.extensions.engine`
  next to `runtime.resolved_version`, the package's, with no orchestrator change. The dead
  `VersionStr` (`"1.0"`) is gone; the router's HELLO still says `"1.0"`, never read for a version.
  Not in `WF_HELP`, which no caller reads for it. Tests: `TestTheStatusNamesTheEngineTheNodeRuns`
  (fails before: no `extensions`) and `TestEngineOfReadsTheRuntimeVersionItsBinaryIsIn`. Docs: the
  WF's README, [spec-systemnode-wf-v1_1](../../docs/spec-systemnode-wf-v1_1.md)'s handlers,
  [sy-wf-rules.md](../../docs/nodes/sy-wf-rules.md), [sy-orchestrator.md](../../docs/nodes/sy-orchestrator.md),
  [operations.md](../../docs/reference/operations.md) and Archi's handbook §4.2.7.
  **The review:** SY.admin's help of `get_node_status`, which Archi reads with the action catalog,
  says it too (`WF_NODE_ENGINE_NOTE`: the engine in `node_status.extensions.engine`, and
  `node_status.runtime.resolved_version` the package's own version), where only the handbook and
  the node docs did; test `get_node_status_help_names_the_engine_a_wf_node_runs` (fails without
  the note).
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `39cc85c0` (its review
  `60485aeb`). On 0.1.138 (00:58Z) the three WF nodes' status had no
  `node_status.extensions.engine`, their units' ExecStart naming wf.engine 0.1.138. On 0.1.139
  (01:40Z) each status answers `extensions.engine` `{"runtime": "wf.engine", "version": "0.1.139",
  "executable": "/var/lib/fluxbee/dist/runtimes/wf.engine/0.1.139/bin/wf-generic"}` within 1 to
  2 s of polling, the same version as its ExecStart, `runtime.resolved_version` the package's;
  each journal says *"wf: engine wf.engine 0.1.139 (...)"* once. `get_node_status`'s help, read
  after the deploy, names it.

### A-301 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; a fresh join under a removed hive's name refused `IO_ERROR` in 2.62 s naming the identity key it could not delete, nothing written; with the key deletable, the fresh join replaced it (its sha256 changed), connected in 139 s, the box's identity synced) (found 2026-11-11 by A-277's review fixes; read in the code; there before it) — A fresh join under a removed hive's name reuses a surviving identity key

- **Why:** `ensure_local_identity_key` (`src/bin/sy_orchestrator.rs` ~1875) reuses any `<hive>.key`
  it finds. When a removal's key deletion failed (`identity_key_cleanup: delete_failed`), a new join
  under the same name lists the hive again at its accept (~32862) with the old key, so the old box,
  if it still runs, is let back in past A-277's refusal: its publishes count again, and its stale
  snapshot competes with the new box's every 30 s. A narrow window stays even with that fixed: a
  re-add within one 10 s recheck keeps the old box's session open (the recheck finds the hive
  listed again, and no restart follows a `delete_failed`).
- **Fix, later (the orchestrator):** a fresh join (`resumed == false`) deletes a surviving key before
  `create_dir_all`, and refuses with `IO_ERROR` when it cannot; the recheck could also compare the
  session's key with the key file (`src/mesh_hmac.rs` is shared code).
- **Gates:** never join a gate hive again under a name whose box may still run with a surviving key.
- **Built on `coa/orch-139` (for 0.1.139; `ee7eece4`).** `accept_add_hive` deletes a surviving
  `<hive>.key` for a fresh join (`resumed == false`, and never the motherbee's own name) before it
  creates the entry's directory (`drop_surviving_identity_key`), and refuses `IO_ERROR` naming the
  file, nothing written, when it cannot; the bootstrap then generates a new key. Left, not built
  (SY.identity's and the shared `src/mesh_hmac.rs`): the recheck comparing a session's key with the
  key file, so a session the old box opened before the accept stays open until sy-identity
  restarts; the texts say to wait one recheck (10 s) after a removal whose key could not be
  deleted. Test `a_fresh_join_reuses_no_surviving_identity_key` (fails without it), with a source
  check of the accept's order. Docs: sy-orchestrator.md, sy-identity.md, identity.md, Archi's
  handbook and SY.admin's `remove_hive` help, which no longer call it a known limit. **Found on the
  way (read in the code; there before it):** a fresh join under a removed hive's name onto a box
  that still runs that hive can take the socket-only path once the accept lists the hive (the
  box's router comes back with its old leaf, A-48's leaf pinning parked), which provisions no
  identity key or TLS leaf, and no join restarts a running sy-identity, which loads its HMAC key
  only at start (`src/bin/sy_identity.rs` ~4570): the box's replica is refused until its
  sy-identity restarts. That holds for every removal that deleted the key; this fix makes a
  `delete_failed` removal behave the same, where the key it left let the box back in. Numbered
  A-335 by the review (A-339 for a while at 0.1.139's integration review; A-335's entry says why).
- **The review (2026-11-12), built on `coa/orch-139`:** the first texts said that box was "never
  let back in" and dropped the caution not to add a hive under its name. A-301 closes :9100 to
  it, not the mesh (A-335). SY.admin's `remove_hive` help (`REMOVE_HIVE_IDENTITY_KEY_NOTE`, and
  `REMOVE_HIVE_BOX_NOT_REACHED_NOTE` for the caution), Archi's handbook, sy-orchestrator.md and
  operations.md say it again: when `remote_cleanup` is not `socket_ok`, stop that box's install,
  or make sure the box is gone, before adding a hive under its name. Test
  `remove_hives_help_keeps_the_caution_about_a_box_it_did_not_reach` (fails with the first texts).
  The code guard (a fresh join never takes the socket path) is A-335's: not built, by the
  operator's call of 2026-11-12, the real guard coming with A-48's leaf pinning.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-13 05:50–05:55Z, VM 104):** shipped as
  `9fcf5d82` (its review `8bc3c5f3`). The first pass's steps failed at the key probe (the tool's,
  A-340). The rerun: `remove_hive gate0239i` with its key pinned (`chattr +i`) and the box cut off
  answered `local_only`, `identity_key_cleanup` `delete_failed`, `identity_restart` `not_needed`
  (10.4 s); (a) a fresh `add_hive gate0239i` with that key still there refused `IO_ERROR` at once
  (2.62 s) naming the key, and `GET /hives/gate0239i` `NOT_FOUND`: nothing written; (b) the key
  unpinned, the fresh join connected in 139 s, *"a fresh join deleted the identity key ..."* logged
  once, the key's sha256 prefix changed (`e70c908eeede` to `64b499c1bc70`), and the box's
  sy-identity applied a full sync, 2 sessions to :9100.

### A-300 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; the removal of a version a stopped pinned node is to be relaunched on refused `RUNTIME_IN_USE` in 1.29 s, naming it alone, `RELAUNCH_DUE`, a node on current left out; the node relaunched on its version, which went once the node was killed) (found 2026-11-11 by A-281's SY.admin review; read in the code; there before it) — A node pinned to a version is not counted as its use between crash-loop restarts, and the version can be removed under it

- **Why:** the global usage lists a node whose unit runs or gives no answer. A unit between
  restarts after a crash, or failed, reads inactive (`systemctl`'s exit 3 also means "on its way
  up or down"), yet `relaunch_on_boot` makes the 60 s reconcile relaunch it
  (`src/bin/sy_orchestrator.rs` ~20580 on `coa/admin-usage-133`'s base).
- **Effect:** AI.x@worker1, pinned to ai.generic 0.3.1, crashes; during its backoff the operator
  removes 0.3.1 and gets 200; a minute later the reconcile relaunches AI.x, which fails with
  *"unable to resolve runtime version"*. SY.admin's note says a node pinned to the version may run
  on it, which this case contradicts.
- **Fix, later:** list a pinned node whose config has `relaunch_on_boot` and whose unit does not
  run (for example as lifecycle `RELAUNCH_DUE`), so it must be stopped before its version goes; or
  narrow the note to units that run or give no answer.
- **Built on `coa/orch-139` (for 0.1.139; `8cb15b27`), the first fix:** the runtime's use
  (`local_runtime_usage_summary`) lists `RELAUNCH_DUE` a node whose unit does not run that the 60 s
  reconcile relaunches on the version its config pins (`pinned_node_relaunch_due`: a kind the
  reconcile takes, no SY or RT name, `relaunch_on_boot`, a pinned `requested_runtime_version`), so
  `remove_runtime_version` refuses that version (`RUNTIME_IN_USE`) until the node is stopped
  (`kill_node` clears the flag) or moved. A node that follows `current` is not listed: its relaunch
  resolves the manifest's current, which SY.admin never removes. SY.admin's texts, built here in
  `sy_admin.rs` (the removal's help note and `RUNTIME_IN_USE`'s detail on `RELAUNCH_DUE`), with
  sy-admin.md, operations.md, Archi's handbook and sy-orchestrator.md, say it. Tests
  `a_pinned_node_the_reconcile_relaunches_is_listed_as_its_versions_use` and
  `no_answer_for_a_nodes_unit_starts_stops_or_removes_nothing` (fail without it); SY.admin's
  `runtime_in_use_names_each_node_with_its_hive_and_state` and
  `the_help_says_what_a_runtime_versions_removal_answers` (fail with the old texts).
- **At 0.1.139's integration review:** the node's own hive computes `RELAUNCH_DUE`
  (`pinned_node_relaunch_due` in its usage read), and the motherbee passes a worker's
  `running_nodes` on as given, so a worker still on 0.1.138 lists none: while the motherbee runs
  0.1.139 and that worker does not, a removal (the operator's, or a workflow deploy's or delete's
  purge) can still take the version a crash-looping pinned node there is about to be relaunched
  on. `RUNTIME_REMOVAL_IN_USE_NOTE`, `get_runtime`'s help, the handbook, sy-admin.md and
  operations.md say it needs the node's hive on 0.1.139; 0.1.139's deploy holds removals,
  workflow deploys and deletes until every hive runs it (board item 30).
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-13 01:58–02:00Z):** shipped as `e72b5efc`
  (its review `8bc3c5f3`). `AI.gate0239r@worker1` pinned to `ai.gate0239r` 1.0.2 and
  `AI.gate0239rc@worker1` on current, both `RUNNING` on 1.0.2; right after a reconcile both units
  stopped, 1.0.3 published as current, and the removal of 1.0.2 answered 409 `RUNTIME_IN_USE` in
  1.29 s, `running_nodes` only `AI.gate0239r@worker1 (worker1, RELAUNCH_DUE)`, rc left out (it
  follows current); once worker1's copy listed 1.0.3, within 58 s, r ran again on 1.0.2 and rc on
  1.0.3; with r killed (`relaunch_on_boot` false), 1.0.2 went, 200. No baseline held on 0.1.138:
  its first run's node never started (`RUNTIME_NOT_PRESENT` before the package reached worker1),
  and its second's version was the current one (`RUNTIME_CURRENT_CONFLICT`).

### A-299 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; a join's Syncthing device recorded in its entry and in the motherbee's 7 Fluxbee folders, and `remove_hive` unlinked it, `local_unlinked`; a failed join's own device and `kept_for_other_hive` by their tests, step 3 not run: A-347) (found 2026-11-11 by A-281's orchestrator review fixes; read in the code; there before it) — A worker join that fails after linking the box into Syncthing records no device id, so `remove_hive` cannot unlink it

- **Why:** `add_hive_flow` writes `syncthing_device_id` into the registry entry only on success, on
  both of its paths. A join that fails after the link (`DIST_SYNC_TIMEOUT`, `SSH_HARDEN_FAILED`,
  `SSH_KEY_FAILED`, or `IO_ERROR` on its final write) leaves the device a member of the
  motherbee's folders, `fluxbee-dist-runtimes` included, and `remove_hive` skips the unlink
  (`skipped_no_peer_device_id`). Only the peer reconcile's orphan pass removes those memberships,
  and it waits until every hive is connected with a recorded device.
- **Effect:** a box whose join failed keeps the motherbee's folders until every hive is connected;
  `remove_hive`'s cleanup still stops its managed nodes when it reaches the box.
- **Fix, later:** record the device id as soon as the link is made, or unlink by the device id the
  failure path reads, so `remove_hive` unlinks a failed join too.
- **Built on `coa/orch-139` (for 0.1.139; `22252d9a`).** Every join flow records the device it
  links (`record_join_syncthing_device`) just before `ensure_syncthing_peer_link_runtime`: the
  worker's two paths and the ingress, and, de-diverged, the egress, which recorded it only after a
  link that succeeded. `write_join_entry` keeps a recorded device id through a flow's write that
  names none, so a retry's writes before its own link keep the device an earlier attempt linked.
  `remove_hive` then unlinks a failed join's device by it; the peer reconcile leaves alone the
  device of a hive not at rest, as before. Test
  `a_join_that_fails_after_its_link_keeps_the_device_it_linked` (fails without it), with a source
  check that every link has its record just before it. Docs: sy-orchestrator.md. Not covered: a
  box reinstalled between two failed attempts of one join (a new device id) leaves the first
  device unrecorded, for the orphan pass.
- **The review (2026-11-12), built on `coa/orch-139`:** a failed attempt's entry now keeps its
  device, so two entries can record one device (the same box, its units stopped by hand, joined
  under another name with its Syncthing home kept), and `remove_hive` of the failed entry would
  unlink the device the connected hive syncs with, which the peer reconcile never adds back.
  `remove_hive` now keeps a device another hive's entry records (`removed_hive_peer_device`):
  `syncthing_peer_cleanup: kept_for_other_hive`, those hives in `syncthing_peer_kept_for`; and
  `kept_registry_unread` when the registry cannot be read whole, for the orphan pass. Test
  `remove_hive_keeps_a_device_another_hives_entry_records` (fails without it). Docs:
  sy-orchestrator.md, operations.md, Archi's handbook.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-13 05:43–05:50Z, VM 104):** shipped as
  `2b2eed6a` (its review `8bc3c5f3`). The first pass's steps failed at the key probe (the tool's,
  A-340). The rerun's step 2: `add_hive gate0239i` with `require_dist_sync` and a 5 s probe ended
  `connected` in 227 s, not `DIST_SYNC_TIMEOUT`; its entry records `syncthing_device_id`, which the
  motherbee's Syncthing listed in its 7 Fluxbee folders; `remove_hive` (`socket_ok`, 12.3 s)
  answered `syncthing_peer_cleanup` `local_unlinked`, the device in no Fluxbee folder after. A
  failed join's own device (the probe's timeout) and step 3's `kept_for_other_hive` were not
  provoked: a join of the same box 10 s after the removal failed `WORKER_SOCKET_UNREACHABLE` while
  some of the removed hive's services still ran there (A-347), so the second hive's join was not
  tried. By their tests. Step 4, a join with the default options, connected in 144 s, the device
  recorded, the box's hive.yaml naming gate0239i.

### A-298 [Archi] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; through Archi's panel API a publish that made 1.0.1 current moved the two running nodes on `current` in place, ILK, `launched_by` and config kept, and left a pinned and a stopped node alone; Archi sent only `publish_runtime_package`) (found 2026-11-11 by the io-cloud-api docs verification; read in the code; there before it) — Archi's runtime publish that makes a version current recreates every motherbee node of that runtime: new ILK, owned secrets purged, `launched_by: "archi"`

- **Why:** `redeploy_runtime_nodes_with_current` (`src/bin/sy_architect.rs` ~11046-11210; A-235
  fixed its tenant) runs after Archi's software and package publish when `set_current` is true:
  the API's default (`unwrap_or(true)`, ~10809); the UI's promote box starts unchecked. It takes
  every node of that runtime on Archi's own hive (the motherbee), by runtime name only, running or
  stopped, pinned or on `current`. For each it runs `kill_node` with `force` and `purge_instance`
  (its timers, the secrets it owns and its ILK go), then `run_node` on `current` in the node's
  tenant.
- **What the node comes back as:** a new ILK (a credential assigned to the old one no longer
  reaches it), running even if it was stopped, with the runtime's template config unless
  `preserve_node_config` (the API's default is false; the UI's box starts checked), and with
  `launched_by: "archi"` (`launched_by_for_caller`, `src/bin/sy_admin.rs` ~9887). A node the Cloud
  launched then answers `NODE_NOT_FOUND` to the Cloud's start, stop and delete, which need
  `launched_by: "cloud"`.
- **Against the operator's rule of 2026-11-11** (A-275; the deletion chapter, A-159): nothing
  deletes ILKs or secrets on its own, and an ILK with history is never deleted.
- **What it adds over the reconcile:** since 0.1.88 (A-109) the orchestrator moves a running node on
  `current` to a new current version within a minute, in place, and leaves a pinned node as it is.
  The redeploy adds only the template config and the move of pinned and stopped nodes.
- **Proposed:** drop the redeploy and leave the move to the reconcile (a config reset, when wanted,
  is a config change of its own); or, at least, relaunch in place without the purge and skip
  pinned, stopped and Cloud-launched nodes.
- **Docs:** `docs/io-cloud-api.md` §5.4 and §9 0.1.88, and the Cloud team's page, tell the Cloud
  of it as today's behavior, pending this decision.
- **The operator's call (2026-11-11):** the redeploy goes. The orchestrator's reconcile moves the
  nodes on `current` in place, and a config reset is a config change of its own. In 0.1.137
  (SY.architect, with A-286 and A-287).
- **Built on `coa/architect-136` (0.1.137), with its review's fixes:** the redeploy and
  `preserve_node_config` are gone. A publish answers `next`, which says what follows: the running
  nodes on the runtime's `current`, and the running nodes of the packages built on it
  (`config_only`, `workflow`), pinned to their package's version or not, move to it in place,
  keeping their ILK, config and `launched_by`; a node of the runtime pinned to a version, and a
  stopped node, stay; a package's new template is its `.zip`'s `config/default-config.json`, and
  no read shows a published version's template. `next` comes whenever the version is installed,
  a failed follow-up included; without `set_current` it says that nothing moves. Archi's handbook
  (§7.2), the panel's Promote To Current label and `sy-architect.md` say the same.
- **Shipped in 0.1.137** (`9cc20ca0`, `70600804`, `cbef4df1` on daily). **Checked on 8.x (gate0237,
  2026-11-12 10:08–10:13Z, through Archi's panel API):** the runtime `ai.gate0237p` 1.0.0, current,
  with `AI.gate0237pa@motherbee` and `AI.gate0237pc@worker1` running on `current`,
  `AI.gate0237pb@motherbee` pinned to 1.0.0 and `AI.gate0237pd@motherbee` stopped. 1.0.1 published
  with Promote To Current (10:09:14Z): `next` said no node is recreated and named the packages'
  nodes and `config/default-config.json`, with no `redeploy` key; pa moved to 1.0.1 in place at
  10:09:29Z and pc at 10:10:35Z, each with its ILK, its `launched_by` (operator) and its config
  (`multimodal` true, 1.0.0's); pb stayed on 1.0.0, not restarted, and pd stopped; started, pd ran
  1.0.1 with its own config in 10.3 s. Archi sent `publish_runtime_package` and no `kill_node` or
  `run_node`. 1.0.2 without promote: `next` said no node moves, no relaunch over two reconcile
  passes, `current` still 1.0.1. The panel's label: *"Promote To Current: running nodes on current,
  and those of packages built on it, move to it in place, keeping their config"*.
- **Docs at 0.1.137:** `docs/io-cloud-api.md` §5.4 and §9's 0.1.137 entry say the redeploy is gone,
  with these records. The Cloud team's page, outside the repo, says so since its Version 4
  (2026-11-12), with A-243's.

### A-297 [General/admin] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; `GET /deployments?hive=` answered the global read's 5 rows and `/versions?hive=` the four hives, where 0.1.138 answered the motherbee's 3 rows and the motherbee alone; `?hive=motherbee` still the motherbee's) (found 2026-11-11 by A-288's review; read in the code) — SY.admin's global deployments and drift-alerts routes read an empty `?hive=` as the motherbee

- **Why:** `GET /deployments?hive=` and `GET /drift-alerts?hive=` take the value untrimmed, so an
  empty one is `Some("")`; `normalize_admin_payload` drops it and the orchestrator's
  `target_hive_from_payload` falls back to the local hive. Archi (A-288, 0.1.133) reads an empty
  filter as none, every hive's history; SY.admin's `get_deployments`/`get_drift_alerts` help and
  `example_scmd` name no query either.
- **Fix, later (SY.admin):** trim it and drop it when empty, as its other filters; the help names
  the filters.
- **Built on `coa/admin-139` for 0.1.139:** the global reads take their `?hive=` trimmed, and an
  empty or blank one is no filter, every hive's (`query_hive_filter`): `GET /deployments`,
  `GET /drift-alerts`, and `GET /versions`, which read it the same way. The help of
  `list_deployments`, `get_deployments`, `list_drift_alerts` and `get_drift_alerts` names the
  query filters (`category` and `limit`; `severity` and `kind` for the drift alerts; `hive` for
  the global two) and their examples carry a query. Its test,
  `an_empty_hive_filter_reads_every_hive`, drives the HTTP routes and fails without it (the
  motherbee's orchestrator was asked `get_deployments` for `/deployments?hive=`).
- **Its review (2026-11-12), on `coa/admin-139`:** Archi's test that every quoted GET example of
  SY.admin's catalog translates (`every_quoted_get_example_in_the_admin_catalog_translates`)
  counted eight and failed with the four new examples, each of which translates: it counts twelve.
  The gate judges the fix by the bodies: the command log never shows a read of `/versions`, and a
  hive's log never showed 0.1.138's entries of hive `""`.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `a27745b4` (its review
  `60ab8e30`). On 0.1.138 (00:46Z) `GET /deployments?hive=` answered the motherbee's 3 rows where
  the global read had 5 (the motherbee's and `gate0237j`'s), and `/versions?hive=` the motherbee
  alone. On 0.1.139 (01:50Z) `?hive=` answered the global rows (deployments 5, drift alerts 5) and
  `/versions?hive=` the four hives; `?hive=motherbee` stayed the motherbee's alone, and
  `/deployments?hive=worker1` answered worker1's 3 rows; the command log names the `?hive=` reads
  `list_deployments` and `list_drift_alerts`, the `?hive=motherbee` ones `get_*`. The drift alerts
  are all the motherbee's on 8.x, so their read could not tell. `get_deployments`' help carries a
  query in its example (`?category=core&limit=20`).

### A-296 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; `GET /deployments?category=gate0239none` answered no row, where 0.1.138 answered the 3 snapshot rows, and a category's read only that category's rows) (found 2026-11-11 by A-288's review; read in the code; there before it) — A deployments read filtered by category also answers the local snapshot rows of every category

- **Why:** `get_deployments_flow` and `list_deployments` pass no category to
  `enrich_deployment_history_entries` (`src/bin/sy_orchestrator.rs` ~21433), which appends the
  local-core-, local-runtime- and local-vendor-<hive> rows (`local_deployment_history_snapshot_entries`,
  ~21338) when no filtered row names the local hive, then sorts and cuts to the limit.
- **Effect:** `GET /deployments?category=vendor&limit=5` on a motherbee whose vendor rows do not
  name it answers core and runtime rows; through SY.admin's route, and since A-288 (0.1.133) through
  Archi's filtered reads, which report them as vendor deployments.
- **Fix, later (the orchestrator):** the category reaches the enrichment, which appends only that
  category's snapshot row, or none (the drift alerts' synthetic rows went for the same class, U-4b).
- **Built on `coa/orch-139` (for 0.1.139; `c815d641`).** The category reaches the enrichment
  (`enrich_deployment_history_entries`, from `list_deployments` and `get_deployments`), which adds
  only that category's snapshot row, none for a category that has no snapshot row; a read with no
  category keeps the three. Test
  `a_deployments_read_by_category_gets_only_that_categorys_snapshot_row` (fails without it). Docs:
  sy-orchestrator.md describes the two reads, which no page did.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-12/13):** shipped as `ac37fbf9`. On 0.1.138
  (00:46Z) `GET /deployments?category=gate0239none` answered the 3 snapshot rows (core, runtime and
  vendor, `local_current_state`); on 0.1.139 (01:55Z) no row; `?category=vendor` answered vendor
  rows only, and the motherbee's `?category=runtime` its runtime row only. Each read took 1.07 to
  1.81 s through SY.admin (0.99 to 1.78 s on 0.1.138), over the step's 1 s: a SEE.

### A-295 [General/Rust SDK] 🔵 low (found 2026-11-11 by A-257's builder; read in the code; there before it) — The Rust SDK's command channels are unbounded

- **Why:** `RouterDispatcher`'s command channels are `mpsc::unbounded_channel`
  (`crates/fluxbee_sdk/src/rpc.rs` ~593), with only a WARN at a depth of 1,000 (~76, ~614). The AI
  runtime's old *queue of 128* was no real bound: what waited behind it piled up there. Since A-257
  (0.1.136) the AI runtime drains it continuously; any other node that stops draining a command
  channel grows without limit.
- **Fix, later (the Rust SDK alone):** a bounded channel with a named refusal past its bound.
- **The operator (2026-11-11):** after the sweep (the Rust SDK alone, with the next change there).
- **Built on `coa/rustsdk-143` (for 0.1.143, planned; the Rust SDK alone, board item 30), as
  A-329's call for the Go SDK says:** NOT_TAKEN, retryable, to the sender, and system messages
  never wait behind user ones. The contract is the Go SDK's of `coa/gosdk-141` (`620f07e7`, the
  `detail`'s words as `6d35cf2a` changed them): reason `node_busy`, its `detail`'s words, lanes of
  1,024 messages or 8 MiB, a log line at most every 10 s, the error's text.
  - **The bound.** Each command channel has two lanes of 1,024 messages
    (`RPC_COMMAND_CHANNEL_CAPACITY`, which replaces `RPC_COMMAND_DEPTH_WARN_THRESHOLD`) or 8 MiB of
    their frames (`RPC_COMMAND_CHANNEL_MAX_BYTES`, counted as they came off the socket: the SDK's
    reader hands each message on with its frame's length). One lane holds the `system` messages,
    which `RpcCommandReceiver::recv` and `try_recv` hand over first; the other holds every other
    kind. The recv loop hands a message to its lane with `try_send`, never waiting.
  - **A refused message.** A message that finds its lane full never reaches the node. It is
    counted (`command_refused(name)`). The node logs a warning, *rpc command channel full: messages
    refused, never handed to the node*, at the first refusal. After that, at most one comes every
    10 s per channel: at the next refusal once the 10 s have passed, or at their end, so a burst
    gets its count even when nothing follows it (the Go SDK's rule). Each warning carries
    `refused`, `told_not_taken`, `unsent`, `unanswered` and `refused_total`, and names the last
    message refused, its lane and what the lane held.
  - **The answer.** Its sender gets `NOT_TAKEN`, the WF engine's message: a `system` message to its
    `routing.src`, on its `trace_id` and thread, with
    `{"ok": false, "error": "not_taken", "reason": "node_busy", "retryable": true, "detail"}`, the
    `detail` the Go SDK's words (*"IO.x@worker1 holds 1024 non-system messages, all it can take
    now; send it again later"*). A lane full by its bytes names fewer messages (the Go SDK's words
    said *"as many as it takes"* until `6d35cf2a`, which this follows).
    - It goes for any message a node sent, named or not.
    - It never goes for a notice: no `src_l2_name`, `HELLO`, `ANNOUNCE`, `WITHDRAW`, `UNREACHABLE`,
      `TTL_EXCEEDED`, or a `system` message without a name.
    - It never goes for a reply: `*_RESPONSE`, `NOT_TAKEN`, `<msg>.error` (the Go SDK's
      `IsNoticeOrReply`). So no two nodes bounce one.
    - Beyond the Go SDK's rule, it never goes for a kind ending in `_response`, a broadcast or a
      tap copy. The Go SDK answers a broadcast it refuses; it has no tap copies.
    - The answer is queued with `try_send` (`NodeSender::try_send_now`). When the link's outgoing
      queue of 256 is full, it is dropped and counted (`unsent`). While the link is down it is not
      queued at all and counted the same: the reconnect would drop it (`drain_tx_queue`, protocol
      §2.5).
  - **A channel whose consumer dropped its receiver** refuses nothing and tells no one: its
    messages are dropped unread, as before the bound (*"no consumer attached"*), since a sender told
    `NOT_TAKEN` retryable could never pass. The bytes the dropped receiver held stay counted, and
    nothing reads them once the lane is closed.
  - **A request that gets `NOT_TAKEN`** on its `trace_id` ends at once with
    `RpcError::NotTaken {trace_id, target, request_msg, reason, detail, retryable}`, whatever its
    matcher. It names its request as `Timeout` does, and its text is the Go SDK's `RpcErrNotTaken`
    text, which calls `request_msg` the verb (*"rpc not taken: trace_id=... target=... verb=...:
    NOT_TAKEN reason=node_busy retryable=true: ..."*, the last `": ..."` only with a detail). One
    that comes after its request ended is stale.
    - Before, a system request got `InvalidResponse`, and an admin request waited for its timeout.
    - A `send_with_matcher` whose success matches any message got the `NOT_TAKEN` as its answer:
      SY.edge's handler calls, `edge_reply_matcher`.
  - **Its tests**, each failing on the code before it (`rpc.rs`):
    - a full lane refuses the next message and tells its sender;
    - system messages never wait behind other kinds;
    - only a sender that may be answered is told;
    - a NOT_TAKEN the link cannot take is dropped without waiting;
    - a burst refused after a warning is warned when the interval ends (paused time);
    - a lane holds at most its bytes;
    - a request its destination did not take ends at once, its error naming the request;
    - a NOT_TAKEN while the link is down is counted unsent;
    - a channel whose consumer is gone refuses nothing.
  - **Docs:** the SDK's README, `docs/reference/protocol.md` §10.2, §10.4 and §11,
    `docs/nodes/sy-edge.md` (the reply step), `docs/io-cloud-api.md` (§7 and §9), and Archi's
    handbook §4.4.
  - **Its review (2026-11-12):** the `detail`'s words brought to the Go SDK's last ones; the error
    names its request; a `NOT_TAKEN` is not queued while the link is down; a channel whose consumer
    is gone refuses nothing. It found A-339 (the AI SDK) and A-336 (the orchestrator).
  - **Mixed versions** (during 0.1.143's deploy, and while a runtime stays pinned below it):
    - A node on an older Rust SDK never refuses (its channels stay unbounded) and never sends
      `node_busy`.
    - A requester on an older Rust SDK that gets `node_busy` ends a system request with
      `InvalidResponse`; an admin request waits for its timeout (the `NOT_TAKEN` goes where its
      profile sends any system message); a `send_with_matcher` whose success matches any message
      takes the `NOT_TAKEN` as its answer.
    - An SY.edge before 0.1.143 is such a caller: in front of a handler on 0.1.143 it answers `200`
      with the `NOT_TAKEN`'s payload as its body, the handler's name in its `detail`, which A-88
      keeps off the internet otherwise. Accepted: it needs 1,024 waiting requests in that window.
    - ai.generic and SY.frontdesk.gov answer each `NOT_TAKEN` they get with an `unknown_system_msg`
      `CONFIG_RESPONSE` until A-339: one wasted message, no loop.
    - The Go nodes take it since 0.1.141 (A-329): their requests end with `RpcErrNotTaken`,
      SY.timer sends again a fire refused retryable, a WF logs any other.
    - A requester on 0.1.143 that gets the WF engine's own `NOT_TAKEN` (`no_thread`,
      `invalid_input`, `unavailable`...) gets `RpcError::NotTaken` where it got `InvalidResponse`:
      only the error's type changes.
  - **Found on the way, not changed (each its own binary):**
    - SY.edge answers a handler's NOT_TAKEN `502 HANDLER_ERROR`, a code other failures share
      (`rpc_error_code`). A code of its own, retryable (`503 HANDLER_BUSY`), is SY.edge's next
      change.
    - ai.generic answers a `NOT_TAKEN` it gets with an `unknown_system_msg` `CONFIG_RESPONSE`, as
      it answers any system message it does not know: A-339, the AI SDK's.
    - The orchestrator reads SY.admin's `NOT_TAKEN` as an unknown outcome, and a spoke
      orchestrator's as unreachable: A-336, the orchestrator's.
    - The WF engine logs a `NOT_TAKEN` for a message it sent (`takeNotTaken`, since 0.1.141,
      A-329; before, it dropped it as any system message but its timers) and does not send that
      message again. The person a WF writes to through an IO node with no room loses that message,
      as before, but the IO node names it in its warning and the WF in its log.
    - io-sim (`nodes/io/io-sim`, test-grade, not in the package) declares an `incoming` channel
      for its `system` messages and never reads it. Before 0.1.143 every system message sent to a
      running io-sim waited there for good, a case of this item. Since then the 1,025th and later
      are refused. Run by hand with its stdin held open, it is the live gate's node that never
      drains its channel.

### A-294 [General/AI SDK] ✅ VALIDATED ON 8.x (0.1.142, 2026-11-13; a reply over the frame limit failed alone, the runtime went on: the same process, no restart, `replies_too_large` 1, the next message answered at once, where 0.1.141's process exited and its unit restarted it, back 7.1 s later) (found 2026-11-11 by A-257's builder and review; read in the code; there before it) — One answer over the frame limit stops the whole AI runtime

- **Why:** `NodeError::MessageTooLarge` (over 128 KiB, refused before sending in `send_traced`) is
  not recoverable (`crates/fluxbee_ai_sdk/src/errors.rs` ~185): `run_with_config` returns it, both
  runners propagate it with `?`, and the node process exits, cutting every other turn in flight.
  ai.generic offloads long text to `content_ref`, so the likely source is a large request context
  copied into a reply, or a large control answer.
- **Since 0.1.136 (A-257's review):** no waiting turn starts after such an end.
- **Fix, later (the AI SDK alone):** a write refused as too large fails that message alone, counted
  and logged, as an exhausted recoverable write does.
- **The operator (2026-11-11):** after the sweep (the AI SDK alone, with the next change there).
- **Built on `coa/aisdk-142` (planned 0.1.142, the AI SDK alone, with A-290, A-325 and A-327's
  `tool_choice`):** `send_reply` settles a reply the sender refuses as too large within its
  message (`send_reply_too_large`), as the WF engine, SY.wf-rules, SY.timer and SY.admin treat an
  answer too large to send: the runtime goes on. The requester of a conversation's or a WF's
  message gets the same reply with an error in place of its payload, `ai_runtime_reply_too_large`
  (`retryable: false`: the message ran), named `<msg>.error`, without the evaluation of the answer
  that did not go, its trace entry adding `reply_too_large`; a reply to a `system` or `admin`
  request gets nothing, as for a handler that fails; an error that does not fit either (the
  request's context is what is too large) sends nothing. Each counts in `replies_too_large` (the
  metrics line and A-290's load) and a bounded WARN names it (*"ai runtime could not send a reply:
  it is larger than a message may carry"*). A handler that panics still stops the runtime. Tests:
  `an_answer_too_large_fails_its_message_alone`,
  `a_control_answer_too_large_fails_alone_with_nothing_in_its_place`,
  `a_context_too_large_for_any_answer_fails_its_message_alone` (runtime.rs) and
  `an_answer_over_the_frame_limit_fails_that_message_alone` (tests/runtime_lanes.rs, through the
  SDK's own sender), each failing with the old settle (the runtime stopped).
- **Validated on 8.x (0.1.142's gate, gate0242, 2026-11-13; `scratchpad/gate-tools/r142_gate.py`, git-ignored; 97 PASS, 1 SEE, 0 FAIL, no model call):** on 0.1.141 a reply of 131,223 bytes from AI.pruebas@worker1 logged
  `FRAME_TOO_LARGE` and the process exited (MainPID changed, NRestarts 0 to 1), the node back 7.1 s
  later. On 0.1.142 the same: the SDK's `FRAME_TOO_LARGE` for the reply and for the error that would
  replace it (its context as large), the runtime's WARN *"could not send a reply: it is larger than
  a message may carry"* 0.032 s after the send, no *"ai runtime stopped"*, MainPID and NRestarts
  unchanged over 20 reads, `replies_too_large` 1, and the next message answered in 0.008 s.

### A-293 [IO/io.cloud] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; through IO.cloud's public door a `phone` that is a number and a missing `display_name` refused `INVALID_REQUEST` naming the field, nothing created; a registration ok in 1.42 s and again `existing` in 0.50 s; with the frontdesk down `FRONTDESK_UNAVAILABLE` in 0.79 s, as before 0.1.137; a retryable frontdesk error, its own case, not provoked (no frontdesk sends one): by its tests; the `request_id` on 9 of 9 request lines) (found 2026-11-11 by A-257's review fixes; read in the code) — io.cloud answers `FRONTDESK_REJECTED` to a retryable frontdesk error

- **Why:** `handle_register_human` (`nodes/io/io-cloud/src/main.rs` ~2390-2400) maps any error reply
  from the frontdesk to `FRONTDESK_REJECTED`, whatever its `retryable`; `docs/io-cloud-api.md` does
  not list that code.
- **Effect:** a retryable error (the frontdesk busy) reads to Cloud as a final refusal; this is why
  A-257 drops a handoff past the frontdesk's places instead of answering it busy (a drop gives the
  documented `FRONTDESK_UNAVAILABLE` after IO.cloud's 20 s).
- **Fix, with IO.cloud's release:** a retryable error maps to `FRONTDESK_UNAVAILABLE` (or a code of
  its own), and `FRONTDESK_REJECTED` is documented.
- **Built and shipped in 0.1.137 (`coa/iocloud-136`; `f2c7db86`, `3472538c`, its review `288cb888`
  on daily):** a frontdesk error marked retryable answers `FRONTDESK_UNAVAILABLE`, and
  `FRONTDESK_REJECTED` stays for one that is not; a `subject` without a `display_name`, or with a
  field of another type, is refused `INVALID_REQUEST` naming the field before anything is created;
  IO.cloud logs each Cloud request's `request_id`; `docs/io-cloud-api.md` lists the codes and what
  Cloud does.
- **Checked on 8.x (0.1.137, gate0237):** step 0, on 0.1.136 (09:37–09:39Z): in 30 days IO.cloud
  took 24 `register_human`, none answered `FRONTDESK_REJECTED`; the 2 refused
  `missing_required_fields` with no `display_name` were our own live checks of 2026-10-15 (0.1.67's
  and 0.1.68's), none Cloud's; the frontdesk logged no *"not a handoff"* line for a
  `frontdesk_handoff` message in those 30 days, though 0.1.136's gate had sent it a malformed one
  (2026-11-12, about 05:44Z). Through the public door with the Cloud's bearer (read from its 0600
  file, never printed; 10:07–10:08Z): `subject.phone` a number refused `INVALID_REQUEST`
  *"params.subject.phone must be a string"* and one with no `display_name` `INVALID_REQUEST` naming
  it, nothing created for either (`exists` false); a well-formed registration ok, `complete`, in
  1.42 s (one person left in pruebas), and the same again `existing` true in 0.50 s; with
  sy-frontdesk-gov stopped, `FRONTDESK_UNAVAILABLE` in 0.79 s, its detail the router's *"transport
  unreachable"* (`NODE_NOT_FOUND`), no `ilk_id`, as 0.1.136 answered a frontdesk it could not reach
  (`handle_register_human` on `ba2addb2`), and the retry with the frontdesk back ok, 5.7 s from its
  start to the first handoff served (the orchestrator's watchdog started it again within its 5 s,
  so a first try right after the stop found it back and answered ok); IO.cloud's *"received a Cloud
  request"* lines carry the `request_id`, 9 of 9. A frontdesk error marked retryable, this item's
  own case, was not seen: today's frontdesk sends none for a registration; its tests cover it
  (`a_frontdesk_error_reaches_cloud_by_its_retryable`). The gate's first run (10:05Z) reached
  nothing: its tool's door had no host (fixed).

### A-292 [General/cognition] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; a person's turn the agent refused busy counted 0.037 to 0.255 s after the busy answer, dropped and never analyzed, its text once in the next package; with AI.pruebas' real runtime the same text sent again answered in 4.3 s; a call already under way on a refused first turn completes, A-332) (found 2026-11-11 by A-257's review; read in the code) — A turn the AI runtime refuses stays in cognition, unanswered

- **Why:** the router copies a user message to cognition when the agent's link takes the frame
  (`src/router/mod.rs` ~7774-7777), before the runtime refuses it; the `ai_runtime_busy` answer is
  not text/v1, so cognition keeps no Out turn (`src/bin/sy_cognition.rs` ~1157-1167).
- **Effect:** the person's message stays unanswered in the communication, and appears twice in the
  next package if the person sends it again. Since A-257 (0.1.136) a busy node refuses at once, so
  this becomes the usual case under load (the old runtime ran such a turn, late).
- **Fix, later (cognition):** an In turn whose `trace_id` the agent answered `ai_runtime_busy` is
  dropped or marked unanswered; the busy answer's trace entry says `busy` since 0.1.136.
- **Built on `coa/cognition-136` (0.1.137).** From its review: cognition drops the refused turn even
  when the busy answer never reached the sender (the router copies the agent's send for cognition
  at entry, before routing, so a busy answer lost to `LINK_BUSY` still drops it); under io.slack
  the person reads *Error (invalid_text_payload)* and is never told to send again, so A-291's fix
  (post the answer's `message`, with a send-again hint when `retryable`) goes with this one. The
  4,096 refusals it remembers are a count: under refusals above about 3 a second for a whole
  20-minute queue drain, a refused turn still waiting can be analyzed again (wasted calls; its line
  stays out).
- **Shipped in 0.1.137** (`18a1f650`, its review `2a3e8bf5`, on daily). **Checked on 8.x (gate0237,
  2026-11-12):** after the deploy (09:52Z) SY.cognition's STATUS on worker1 and the motherbee has
  the new counters at 0 and `load_failures_total` 0, worker1's 23 communications loaded at its
  start. From the unit's first line to its load line the step measured 0.73 s on the motherbee and
  59.54 s on worker1 (it expects under 3 s): the step's start was worker1's old process's first
  `received router message` line after the mark (pid 1642167, 09:48:21.8Z, SY.vault's
  `VAULT_SECRET_CHANGED`); the new process started at 09:49:19Z and wrote the load line at
  09:49:21.377Z, 2.4 s. With the
  agent `AI.gate0237c@worker1` played by a mesh client that refuses a person's turn in the exact
  busy shape (10:02Z): `refusals_total` rose 0.255 s after the busy answer, with
  `refused_turns_dropped_total` +1, and `refused_turns_not_analyzed_total` +1 7.367 s after; M3
  (M2's text again, a new trace) got a package with M1 and its answer only, no M2 line; M4's
  package showed M1, its answer, M2's text once and M3's answer (4 turns); the journal's line named
  the refusal (`turn` dropped). With AI.pruebas' real runtime, its HTTPS dropped by nft and its
  queue full (10:03–10:05Z): the person's turn answered `ai_runtime_busy`, `retryable` true, act
  `busy`, in 0.011 s; `refusals_total` rose 0.037 s after, dropped and not analyzed +1 each, the
  journal naming the refused trace; the same text sent again once the queue drained was answered in
  4.286 s. One tagger call on that refused trace (1,223 tokens in, 78 out) was already under way
  when the refusal came: A-332.

### A-291 [IO/io.slack] 🔵 low (found 2026-11-11 by A-257's review; read in the code; there before it) — io.slack posts an agent's error answer as *invalid_text_payload*

- **Why:** io.slack posts any agent answer that is not text/v1 (`nodes/io/io-slack/src/main.rs`
  ~3099-3112; `TextV1Payload::from_value`, `nodes/io/common/src/text_v1_blob.rs` ~383) as *"Error
  (invalid_text_payload) while processing the content or attachments."*, error payloads included
  (`ai_runtime_busy`, `ai_runtime_error`, `ai_runtime_timeout`).
- **Effect:** the person reads that their content was invalid; since A-257 (0.1.136) a busy node
  answers `ai_runtime_busy` at once, so under load this is the usual answer.
- **Fix, with io.slack's revamp:** post the payload's `message`, with a send-again hint when
  `retryable` is true.

### A-290 [General/AI runners] ✅ VALIDATED ON 8.x (0.1.142, 2026-11-13; with its 4 workers held and its queue of 128 full, AI.pruebas and SY.frontdesk.gov answered one more turn `ai_runtime_busy` at once and read `DEGRADED`, `ai_runtime_saturated` naming the workers and the queue, until 30 s past the last refusal, then `HEALTHY`; status reads under 1 s throughout) (found 2026-11-11 by A-257's reviews; read in the code) — An AI node answers `HEALTHY` while its runtime refuses every turn

- **Why:** `build_node_status_response_payload` (`crates/fluxbee_sdk/src/managed_control_plane.rs`
  ~342-367) looks only at the config state; since A-257 (0.1.136) `NODE_STATUS_GET` is always
  answered at once, while the lanes' load (`turns_waiting`, `turns_refused`, `direct_dropped`,
  `busy_answers_dropped`) shows only in the journal's *"ai runtime metrics"* line.
- **Effect:** neither `get_node_status` nor Archi, the operator's interface, sees a saturated node;
  before, its status timed out, which at least showed something was wrong.
- **Fix, later:** the AI runtime gives the node a read-only load handle (the AI SDK, alone); both
  runners put it in `NODE_STATUS_GET`'s `extensions`, which the orchestrator passes on, and report
  `DEGRADED` while turns were refused in the last interval.
- **The operator (2026-11-11):** after the sweep (the AI SDK, then the runners alone, with the next change there).
- **Built on `coa/aisdk-142` (planned 0.1.142, the AI SDK with both runners, A-294, A-325 and
  A-327's `tool_choice`):** the AI SDK's `RuntimeLoad`, a handle a node keeps and hands its runtime
  (`NodeRuntime::with_load`), and `RuntimeLoad::add_to_status` on the managed answer: its
  `extensions.ai_runtime` (`state` `serving` or `saturated`, `window_s`,
  `last_turned_away_ms_ago`, `model_workers` and `model_workers_busy`, `turns_waiting` and
  `queue_capacity`, `direct_capacity` and `direct_running`, and since the start `turns_refused`,
  `direct_dropped`, `busy_answers_dropped`, `replies_too_large`). `saturated` while the runtime
  turned work away in its last window, the metrics interval (30 s): a turn refused, a message
  dropped from the direct lane, a busy answer not sent; then a `HEALTHY` node reads `DEGRADED`
  with `last_error` `ai_runtime_saturated` (its message says when, the bounds and the counts), as
  io.web reports its port, and `HEALTHY` again 30 s after the last one; a node `DEGRADED` by its
  config keeps its config's `last_error`. The metrics line gains `turns_running` and
  `direct_running`. ai.generic and SY.frontdesk.gov answer `NODE_STATUS_GET` with it
  (`node_status_payload`). Tests: `a_saturated_runtime_reads_degraded_until_its_window_passes`,
  `a_message_dropped_from_the_direct_lane_saturates_the_load`,
  `a_load_whose_runtime_does_not_run_adds_nothing` (runtime.rs),
  `the_status_reads_the_saturated_runtime_it_was_given` (tests/runtime_lanes.rs, through
  `with_load`), and each runner's `node_status_reports_the_ai_runtimes_load`; each failing when
  the status leaves the load out. From its review: each runner puts the node on its runtime in one
  place, `node_runtime`, which hands the runtime the node's own load, and each runner's
  `the_status_reads_the_load_of_the_runtime_the_node_runs_on` runs that runtime on the dispatcher's
  test harness and asks its status through it (the load there, with the bounds the runtime was
  started with), failing when the runtime is given no load or another one.
- **Validated on 8.x (0.1.142's gate, gate0242, 2026-11-13; `scratchpad/gate-tools/r142_gate.py`, git-ignored; 97 PASS, 1 SEE, 0 FAIL, no model call):** the status of AI.pruebas@worker1, SY.frontdesk.gov@motherbee and
  the smoke node carries `extensions.ai_runtime` (serving, busy 0/4, waiting 0/128, direct 1/64,
  refused, dropped, too large, the window 30 s), each read in about 0.12 s. With the provider's
  HTTPS held (an nft drop, its safety timer armed) and 4 turns holding the workers, 128 fillers
  queued, one more turn answered `ai_runtime_busy`, retryable, in 0.006 to 0.008 s; the first read
  after it `DEGRADED`, `last_error` `ai_runtime_saturated` (*"turned work away 4 s ago ... its 4
  model workers busy and its queue of 128 full"*), every read `DEGRADED` while the last refusal was
  under 30 s old, `HEALTHY` again 34 s after it with the queue still full (the state follows
  refusals, not the queue); 19 status reads under the load, median 0.17 s, max 0.30 s; the holders
  answered the provider's failure text after about 92 s, the fillers drained. A frontdesk handoff
  during the hold answered at once.

### A-289 [General/router] 🔵 low (found 2026-11-11 by the router's 0.1.134 builder and review; read in the code; there before it; several routers a hive, not supported) — A broadcast from a router that is not the gateway needs ttl 4 to cross the WAN

- **Why:** the IRP hand-over takes a hop, and the gateway's `handle_peer_message` sends a copy toward
  the WAN only at ttl >= 3: a broadcast that enters a router that is not the gateway with ttl 3
  stops at the gateway, unanswered (a gateway sibling exists, so no `GATEWAY_UNAVAILABLE`). The
  entering router also hands the gateway two copies, and each other sibling one more.
- **Impact:** none on 8.x (one router a hive; the SDKs send ttl 16). Documented in
  `docs/reference/routing-and-policy.md` §4 and §10 with 0.1.134. A known limit (the operator,
  2026-11-10: the several-routers mode's defects past A-208, A-246 and A-247 are not scheduled).
- **Fix, if the mode is ever supported:** a router that is not the gateway hands no IRP copy toward
  the WAN.

### A-288 [Archi] ✅ VALIDATED ON 8.x (0.1.133, 2026-11-11; through Archi, the drift alerts' severity filter and the deployments' hive filter kept: 2 and 3 entries, 50 and 48 on 0.1.132) (found 2026-11-11 by A-279's review; read in the code; there before it) — Archi's SCMD reads of deployments and drift alerts drop their query

- **Why:** the SCMD arms of `GET /deployments`, `/drift-alerts`, `/hives/{hive}/deployments` and
  `/hives/{hive}/drift-alerts` (`src/bin/sy_architect.rs` ~13578, ~13584, ~14093, ~14099 on A-279's
  branch) send `params: {}`, while SY.admin's own routes read `?limit=&category=`, `?severity=&kind=`
  for drift alerts and `?hive=` on the global ones (`deployments_payload_from_query`,
  `drift_alerts_payload_from_query`), which the orchestrator filters by.
- **Effect:** Archi asking for one hive's or one kind's alerts, or a short page, gets everything.
- **Fix (0.1.133, A-279's review):** those arms read their query through `scmd_query_params`, as
  SY.admin's routes do; a test each.
- **Built for 0.1.133 (with A-279's review, 2026-11-11):** the four arms read their query as
  SY.admin's routes do, and the read tool and the chat name the filters. **Its sibling, fixed with
  it:** the SCMD of `DELETE /hives/{hive}/taps` read the tap's key from the body only, while SY.admin's
  route and its `delete_tap` example name it in the query, so the example reached SY.config.routes
  empty and was refused `NOT_FOUND`; it now reads the query when the body names none.
- **Checked on 8.x (0.1.133, against 0.1.132):** through Archi, the drift alerts' severity filter
  and the deployments' hive filter are kept: 2 and 3 entries, where 0.1.132 answered 50 and 48.

### A-287 [Archi] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; through Archi's SCMD `AI.pruebas%40worker1` answered as the `@` form, a vault key's `%3A` read the same metadata, Archi's own `config%2Dget` answered as `config-get`, and an encoded `/` in a segment was refused *"unsupported SCMD path"*) (found 2026-11-11 by A-279's builder; read in the code; there before it) — Archi's SCMD paths go to SY.admin undecoded: `AI.sales%40worker1` matches no node

- **Why:** `translate_scmd` (`src/bin/sy_architect.rs`) copies each path segment as it comes (node
  names, ILK and tenant ids, route prefixes, VPN patterns, WF instance ids, timer uuids, action
  names); only the vault key segment is decoded. SY.admin's HTTP routes decode every segment
  (`decode_percent`).
- **Effect:** a path the model writes encoded (`/hives/worker1/nodes/AI.sales%40worker1/status`)
  works over SY.admin's HTTP route and finds nothing through Archi.
- **Fix, later (SY.architect):** `translate_scmd` decodes each segment once, as SY.admin's routes do.
- **Built on `coa/architect-136` (0.1.137):** each segment is decoded once after the path is split
  and the query set apart; its review's fix: Archi's own two paths (`/architect/control/...`) are
  read the same way. Found by the review: A-311 (with the decoding, Archi's read tool can name a
  `/` in `GET /hives/{hive_id}`) and A-312.
- **Shipped in 0.1.137** (`4302757b`, its review `4b8841cf`, on daily). **Checked on 8.x (gate0237,
  2026-11-12 09:56Z, through Archi's SCMD):** `AI.pruebas%40worker1` answered as the `@` form
  (`RUNNING` both, 2.8 s and 1.2 s); a segment with an encoded `/` answered *"unsupported SCMD
  path"* (`SCMD_EXECUTION_FAILED`); a vault key of 23 characters with a `:` written `%3A` read the
  same key, metadata and version as the plain form, no value read; Archi's own `config%2Dget`
  answered as `config-get`; `GET /hives/..%2Fx` reached the orchestrator and was refused
  `INVALID_HIVE_ID` (A-311).

### A-286 [Archi] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; through Archi's SCMD a usage read with the offset `+01:00` reached SY.storage as with `%2B01:00`: ok, the same 5 records) (found 2026-11-11 by A-279's builder; read in the code; there before it) — Archi's SCMD reads take `+` for a space where SY.admin keeps it

- **Why:** `percent_decode_simple` (`src/bin/sy_architect.rs`), used by every SCMD read's query since
  A-279, reads `+` as a space; SY.admin's `parse_query` and `decode_query_component` keep it (their
  comment: a raw `+` offset is commoner).
- **Effect:** through Archi, `/hives/motherbee/usage?from=2026-11-01T00:00:00+01:00&to=...` reaches
  SY.storage as `2026-11-01T00:00:00 01:00`, refused *"from is not an RFC 3339 time"*
  (`src/bin/sy_storage.rs` ~1231); the same query over SY.admin's HTTP route works. `Z` and `-03:00`
  offsets are not affected.
- **Fix, later (SY.architect):** the SCMD reads decode with `+` kept, as SY.admin does; Archi's own
  HTTP `query_param` keeps its reading.
- **Built on `coa/architect-136` (0.1.137):** the SCMD reads keep `+`, as SY.admin does.
- **Shipped in 0.1.137** (`4302757b` on daily). **Checked on 8.x (gate0237, 2026-11-12 09:56Z):**
  through Archi's SCMD, a usage read whose `from` has the offset `+01:00` reached SY.storage and
  answered ok, 5 records, the same as with `%2B01:00`.

### A-285 [Archi] ✅ VALIDATED ON 8.x (0.1.133, 2026-11-11; SY.admin's help names each new answer the gate looked for, all missing on 0.1.132, but not `hives_without_runtimes`, A-315) · the Cloud catalog's text (`cloud.rs`) built on `coa/rustsdk-143`, for 0.1.143 (planned) (found 2026-11-11 by the reviews of A-270 and A-271) — The texts Archi and Cloud read do not describe 0.1.133's new answers

- **`delete_workflow` (A-270):** a WF node that is not RUNNING, or whose instances cannot be
  counted, now needs `force` or a start first, and a config read with no answer is
  `ORCHESTRATOR_ERROR` with `force` too. SY.admin's `force` field (`src/bin/sy_admin.rs` ~16544,
  *"Force delete even if active instances exist."*), Archi's catalog line for `wf_rules_delete`
  (`src/bin/sy_architect.rs` ~2493) and the reconciler's comment on a null count
  (`src/bin/sy_architect/reconciler.rs` ~580) say only *running instances*.
- **`run_node`, `start_node`, `kill_node`, `remove_node_instance` (A-271):** `SERVICE_FAILED` with
  `retryable: true` means systemctl did not answer and nothing was done (ask again), and a node's
  `lifecycle_state` can be `UNKNOWN`; SY.admin's per-action HELP names neither, and the Cloud
  catalog (`crates/fluxbee_sdk/src/cloud.rs` ~143, `list_nodes`) lists four states.
- **`RUNTIME_IN_USE`'s detail** says *"still running"* for the `STARTING` and `UNKNOWN` entries the
  orchestrator lists since A-271.
- **Fix:** SY.admin's and SY.architect's texts in 0.1.133 with A-281; `cloud.rs` with the SDK's
  next release.
- **Checked on 8.x (0.1.133):** SY.admin's HELP names `retryable` (`kill_node`, `run_node`,
  `start_node`, `remove_node_instance`), `UNKNOWN` (`get_node_status`), `RUNNING`,
  `wf_rules_get_status` and `packages_left` (`wf_rules_delete`), 1000 (`timer_list`),
  `INVENTORY_UNAVAILABLE` (`teardown_node`), and `RUNTIME_USAGE_UNKNOWN`, `NO_USAGE` and
  `USAGE_UNREADABLE` (`remove_runtime_version`), each missing on 0.1.132. Named neither there nor in
  Archi's handbook: `hives_without_runtimes` (A-315).
- **`cloud.rs` built on `coa/rustsdk-143` (for 0.1.143, planned; the Rust SDK alone, board item
  30):** the catalog that `list_cloud_actions` returns says what §5.4 of `docs/io-cloud-api.md`
  says since 0.1.133.
  - `list_nodes` lists the fifth state, `UNKNOWN` while the hive cannot tell (read it again), never
    a `STOPPED` it did not see. `get_node_status` gives the same view.
  - `provision_node`, `start_node`, `stop_node` and `delete_node` name `SERVICE_FAILED`: the hive's
    systemd did not answer, nothing was done, so send it again. The answer Cloud gets carries no
    retry flag (IO.cloud passes the code and the text), so the summaries say what to do.
  - Its test (`the_node_ops_name_unknown_and_service_failed`) fails on the text before it.
  - `docs/io-cloud-api.md` §9 has its line for 0.1.143.
  - It reaches Cloud with IO.cloud's runtime of 0.1.143, which carries this SDK.
  - **Found on the way, not changed:** the catalog names no paging for `list_nodes` and
    `list_tenants` (`params.limit`, `params.cursor`, `next_cursor`), which the contract has
    (§4.4, §5.1, §5.4). It is a text of the same kind, for the SDK's next change.

### A-284 [WF/wf-generic] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; a WF node with 1000 running instances counted 1000 in 0.50 to 1.04 s, where 0.1.136's count failed `INTERNAL_ERROR` past the frame; a delete without force `INSTANCES_ACTIVE`; Archi named 1000; across the deploy every answer one of the five kinds allowed) (found 2026-11-11 by A-270's review; read in the code) — The running-instance count fails for a busy WF: past ~350 instances its answer passes the frame

- **Why:** SY.wf-rules counts a WF node's running instances with `WF_LIST_INSTANCES {status:
  running, limit: 0}` (`go/sy-wf-rules/node/wf_client.go` ~63-69); wf-generic
  (`handleWFListInstances`, `go/nodes/wf/wf-generic/node/dispatch.go` ~399) takes limit 0 as no
  limit, answers every instance's summary (about 366 B each) and counts the rows. Past about 350
  running instances (spec §21 allows 1000 a node) the answer passes the SDK's 128 KiB frame and the
  node answers `INTERNAL_ERROR` (*"WF_LIST_INSTANCES_RESPONSE not sent: too large"*).
- **Effect:** `get_status` and `list_workflows` show `active_instances` null with
  `instances_error`; Archi's removal plan says *unknown* instead of the count the operator decided
  it shows (2026-11-03); a delete without force answers `INSTANCES_UNKNOWN` instead of
  `INSTANCES_ACTIVE` (safe: nothing is purged). The count fails exactly for busy workflows.
- **Fix, later (the WF alone):** a count-only `WF_LIST_INSTANCES` (`count_only: true`: a `COUNT(*)`
  with the same filters, no rows) that SY.wf-rules asks for.
- **Built on `coa/wf-136` (0.1.137). Found on the way (the builder):** SY.wf-rules read the count
  through a float64 into an int: a count of -1, or 1e300 (MinInt64 on amd64), read as a node with
  none, and a delete without force would have purged its instances; 2.5 read as 2. Since 0.1.137 a
  count that is not a whole number from 0 is a failed count (`INSTANCES_UNKNOWN`). And the engine
  answered a `WF_LIST_INSTANCES` it could not decode, or one with a status none of the four, from
  the fields it could read (a count of `Running` was 0); since 0.1.137 both are refused
  `INVALID_REQUEST`. Neither is reachable on PROD: a correct engine sends no such count, SY.admin
  checks the status, and SY.wf-rules sends `running`.
- **Shipped in 0.1.137** (`e279c396`, its review `c7ad15b0`, on daily). **Checked on 8.x (gate0237,
  2026-11-12):** `WF.gate0237w@worker1` (tenant pruebas) with 1000 running conversations from
  `IO.gate0237w@worker1` (the Go SDK dropped 344 of the burst: A-329; topped up). On 0.1.136
  (09:40–09:44Z): the count `active_instances` null, `INTERNAL_ERROR` *"WF_LIST_INSTANCES_RESPONSE
  not sent: message too large"* (222,036 bytes at 656 instances, 338,309 at 1000), a delete without
  force refused `INSTANCES_UNKNOWN`, nothing killed, and Archi said at least 200 and that it cannot
  tell (40.5 s). Across the deploy, `get_status` every 3 s (09:44:34–09:49:49Z): `internal_error`
  28, `transport` 43, `relaunch` 3, `ok_1000` 3, every answer one of the five kinds allowed; the
  engine moved first (09:48:38.355Z) and SY.wf-rules 41.6 s later (09:49:20Z); the relaunched
  engine logged *"recover: complete, 1000 running instances"* 1.76 s after its relaunch; a delete
  without force in that window refused `INSTANCES_UNKNOWN` (*"the wf node is STARTING, not
  RUNNING"*). On 0.1.137 (09:53–09:54Z): `active_instances` 1000, no `instances_error`, in 1.039 s,
  then 0.50 to 0.80 s in six more reads; `list_workflows` 1000; a page of 200, newest first;
  `status=completed` 0; no *"not sent"* and no `INVALID_REQUEST` line; a delete without force
  refused `INSTANCES_ACTIVE` (*"wf node still has 1000 running instances"*), nothing purged; Archi
  named 1000 running conversations (12.9 s); the forced delete took 3.0 s.

### A-283 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; worker1's Syncthing frozen: the warning at the 12th tick, +179 s, the restart at +329 s, the API answering 2.2 s after, and no second restart within the hour; a refused API port warned at its 12th check, and lasting on egress1 restarted it at 304 s unanswered, once; `TimeoutStopUSec=30s` on every hive; its drift read at +24 h, 2026-11-13: no warning opened unprovoked) (found 2026-11-11 by A-271's review; read in the code; there before it; to decide) — The Syncthing watchdog waits on an API that never answers, and never repairs it

- **Why:** `watchdog_blob_sync` (`src/bin/sy_orchestrator.rs` ~18627) asks each folder with a 10 s
  timeout: about 120 s a tick on the motherbee (10 folders and 2 folder-error reads), about 80 s on
  worker1. Its *"syncthing unhealthy; restarting"* path runs `ensure_blob_sync_runtime`, which on a
  running Syncthing only calls `systemd_start` (a no-op) unless its binary or unit is stale.
- **Effect:** while Syncthing's REST accepts connections and never answers, a crashed rt-gateway or
  SY service waits up to about 2 min for the watchdog instead of 5 s, and nothing repairs
  Syncthing. Since A-271 (0.1.133) the only trace is one journal line a folder after 12 such ticks
  (about 24 min), with no drift alert.
- **Fix (the review's proposal):** stop asking after the first no answer in a tick, skip
  `report_blob_folder_errors` that tick, and add a `syncthing` drift alert when the count is
  reached. **To decide (the operator):** whether that run also restarts Syncthing.
- **The operator's call (2026-11-11):** the safe part, and a bounded restart: after 5 min with no
  answer, `systemctl restart` of Syncthing, at most once an hour, with a drift alert and a log
  line. In 0.1.137 (the orchestrator).
- **Built on `coa/orch-136` (0.1.137):** a tick counts toward the restart only when Syncthing
  answered for no folder, and a slow folder is asked last; the unit gains `TimeoutStopSec=30`, so
  each hive's first 0.1.137 start restarts Syncthing once (*"syncthing runs a stale binary or unit;
  restarting"*). **To decide (the operator), from its review (there before it):** when Syncthing is
  active but its API port refuses connections or times out the 2 s connect, every tick tries a
  start that does nothing and waits 30 s for health, a stopped core service waits about 35 s, and
  nothing repairs Syncthing. Proposed: count those ticks too (reason *API port refused*), so the
  same 5 min, once-an-hour restart repairs it.
- **The operator's call (2026-11-11):** yes: a tick whose API port refuses the connection or times
  out its connect counts toward the same bounded restart. With 0.1.137's orchestrator.
- **Built on `coa/orch-136` (`754b81c6`), with its second review's fixes:** a refused or timed-out
  connect counts (*API port refused*); any other connect error is the host's own (file
  descriptors run out, `EPERM`) and counts nothing, a check with no answer warned after a minute;
  a setup the boot or a config change could not finish is owed, and runs at the first tick whose
  port takes the connection; a setup that restarts Syncthing, or starts one that did not run,
  starts the run of no answers again; every tick counts, a hive with no folder included; an
  egress no longer reads its own start's config as a change. Left, and documented: a restart no
  tick sees (systemd's own `Restart=always`, or a manual `systemctl restart fluxbee-syncthing`)
  can count the new process's first seconds in the old run, at most one restart more within the
  hour's bound; an owed setup that keeps failing for a lasting reason runs every 5 s with a
  warning, as the unhealthy path does. Found on the way: A-313, and an unreachable branch in
  `bootstrap_local`'s Syncthing step (cleanup only).
- **Shipped in 0.1.137** (`f60acf55`, `80170089`, `230a9481`, `9330642a` on daily). **Checked on
  8.x (gate0237, 2026-11-12):**
  - **At the deploy:** `TimeoutStopUSec=30s` on every hive's Syncthing, and one restart for the
    changed unit on each (the motherbee 09:48:11Z, egress1 09:49:18Z, ingress1 09:49:26Z, worker1
    09:49:29Z).
  - **An egress's own start (G5, egress1, 10:20Z):** after its orchestrator's restart, no
    *"blob/dist sync config changed in hive.yaml"* line in the minute after its `hive ready`.
  - **Frozen (worker1, SIGSTOP, 10:20:48–10:26:44Z):** `sy-opa-rules`, stopped at +66 s and at
    +131 s, ran again in 14.0 s and 9.1 s; the warning at +179 s, after 12 checks and 164 s with no
    answer (a tick about 14.9 s); the restart at +329 s, 315 s with no answer; the API answering
    2.2 s after the restart; Syncthing's start changed once; no peer-reconcile line while frozen;
    worker1's own rows `api_unanswered` open, `api_unanswered_restart`, `api_unanswered` closed.
  - **Within the hour (10:26:44–10:35:00Z):** worker1's orchestrator restarted, then Syncthing
    frozen 7 min: the warning again at the 12th tick (+177 s), no second restart, one
    `api_unanswered_restart` that hour; it answered again at once after the continue (417 s with no
    answer).
  - **The port refused at a boot (G1, worker1, 10:35:57–10:38:09Z):** *"blob sync runtime bootstrap
    failed; continuing startup"* at +39 s; a stopped `sy-timer` ran again in 4.6 s meanwhile; the
    warning at 12 checks (55 s) named *"API port refused: 127.0.0.1:8384: Connection refused (os
    error 111)"*; the owed setup ran 4.6 s after the rule went, then healthy; no restart.
  - **A connect that times out (G2, worker1, 10:38:09–10:44:39Z):** an nft drop on loopback made
    the connect time out, never fail `EPERM`; the check counted it as the port's refusal (*"took no
    connection within 2 s"*, a warning at 12 checks), no restart (worker1 had its one that hour),
    and it answered again 1.2 s after the rule went. The `EPERM` path stays covered by its unit
    tests only.
  - **Lasting (G3, egress1, 10:44:39–10:52:22Z):** the warning at 12 checks (54 s), the restart at
    +312.8 s from the rule with 304 s unanswered, one restart while the rule stayed, and *"blob
    sync watchdog failed"* once, the restart's own wait for its port.
- **The drift read at +24 h (2026-11-13, 14:35Z; `r137_gate.py drift --hours 29`, from 0.1.137's
  deploy at 09:44Z the day before):** no `api_unanswered` opened unprovoked on any hive. The
  motherbee and ingress1 had no row; egress1 3, all in the gate's A-283 steps; worker1 11, 9 in
  the gate's steps and one warning, opened at 01:08:36Z and closed 5.7 s later, during 0.1.139's
  gate's own freeze of worker1's Syncthing (01:05:38 to 01:08:39Z, 177.6 s: the warning at its
  12th tick, as built); no restart outside the gates.

### A-282 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; the motherbee's package boot restarted the 10 services the package replaced, none unanswered, and no core process ran a deleted binary on any hive; an orchestrator restart on worker1 restarted no core service, and a vendor update with nothing new restarted nothing; a `systemctl show` with no answer not provoked on 8.x: by its tests) (found 2026-11-11 by A-271's builder and review; read in the code; there before it) — A replaced binary is taken for current when `systemctl show` gives no answer

- **Why:** `service_runs_replaced_binary` (`src/bin/sy_orchestrator.rs` ~2212) reads a failed or
  killed `systemctl show -p MainPID` as *not replaced* (`systemd_unit_show` returns an empty map).
  Its callers: the boot after a package upgrade (~2207), where `start_core_service` then starts (a
  no-op) instead of restarting, so the service keeps the old inode while the package and
  `/versions` say new (U-1's shape); and Syncthing's staleness check in `ensure_blob_sync_runtime`
  (~18315), A-57's catch-up for a restart an earlier call never reached.
- **Fix, later (the orchestrator):** ask again; at boot, a check with no answer restarts (a restart
  also starts a stopped service), recorded in `BootUpgradeRestart` apart from a real replacement;
  Syncthing's caller never restarts on every ensure while `systemctl show` fails.
- **Built on `coa/orch-136` (0.1.137).** **To decide (the operator), from its review:** any start
  of the orchestrator restarts a service of the role whose `systemctl show -p MainPID` gives no
  answer 4 times in 1 s, so a `systemctl restart sy-orchestrator`, or a spoke's start after its
  core update with systemd's bus busy, restarts rt-gateway and every node reconnects once, with
  only a journal warning. Proposed: restart on no answer only when the same boot found a replaced
  binary (the boot's own sign of an upgrade), each such restart recorded in `core.last_update`;
  one line in `BootBinaryChecks::restarts`. Lost: a package upgrade where every replaced service's
  check gives no answer keeps its old binary, as before A-282.
- **The operator's call (2026-11-11):** yes: at boot, a check with no answer restarts only when the
  same boot found a replaced binary, each such restart recorded in `core.last_update`. With
  0.1.137's orchestrator.
- **Built on `coa/orch-136` (`754b81c6`);** its second review changed only the SIGTERM comment: the
  orchestrator's exit never stops a service, and its boot restarts only the services a package
  upgrade replaced and, beside them, the listed ones with no answer; any other start, `systemctl
  restart sy-orchestrator` included, restarts none.
- **Shipped in 0.1.137** (`46f51bc7`, `0fd06638`, `5e739962`, `07fae956` on daily). **Checked on
  8.x (gate0237, 2026-11-12):** the motherbee's `core.last_update` after the deploy: via `package`,
  phase `done`, status `ok`, 24.707 s (0.1.136's 17.168 s: this boot also restarted Syncthing for
  its changed unit, once, A-283's `TimeoutStopSec=30`); `updated` the 10 services (rt-gateway,
  sy-config-routes, sy-identity, sy-opa-rules, sy-architect, sy-storage, sy-cognition, sy-timer,
  sy-frontdesk-gov, sy-vault), `unanswered` [], `restarted` equal to `updated`; the boot's line
  once; no *"systemctl did not answer ..."* line. No core process on a deleted binary on any hive
  (the motherbee 14, worker1 9, ingress1 6, egress1 5); each spoke's `core.last_update` lists
  sy-orchestrator in `updated` and carries `settled`. On worker1 (10:18–10:19Z): an orchestrator
  restart restarted none of the 7 core services checked, `core.last_update.started_at_ms`
  unchanged, `hive ready` 7.4 s after; an update of category vendor with nothing new answered ok,
  `restarted` [], Syncthing's start unchanged, no *"did not answer"* warning. A boot with
  Syncthing's API port refused (A-283's G1) left rt-gateway's MainPID unchanged and wrote no
  `core.last_update` record. A `systemctl show` with no answer, this item's own case, was not
  provoked on 8.x (every read found `unanswered` [] and no *"did not answer"* line): its tests
  cover it (`a_replaced_binary_check_with_no_answer_is_asked_again`,
  `a_boot_with_a_replaced_binary_restarts_the_unanswered_ones_too_and_records_every_restart`).

### A-281 [General/admin] ✅ VALIDATED ON 8.x (0.1.133, run on 0.1.134 past its first read, 2026-11-11; with worker1 not read, a version's removal refused `RUNTIME_USAGE_UNKNOWN` naming it, nothing removed; the ingress and the egress in `hives_without_runtimes`; a workflow's delete ok naming its `packages_left`) (found 2026-11-11 by A-271's builder and review; read in the code; there before it) — `remove_runtime_version` removes a version whose usage it could not read

- **Why:** `handle_remove_runtime_version` (`src/bin/sy_admin.rs` ~19422-19470) refuses only when
  `usage_global_visible.running_nodes` is not empty, and `query_runtime_usage_global_visible`
  (~18820) fills a missing `usage_global_visible` with `in_use: false`. The orchestrator's global
  usage (`global_visible_runtime_usage_summary`, `src/bin/sy_orchestrator.rs` ~20128) answers
  `{status: error}` with no list when the motherbee's own read fails, and counts only the hives that
  answered: a spoke that is down appears only in `skipped_hives`, and one whose own node scan fails
  answers ok with a failed usage, which counted as no node (`USAGE_UNREADABLE` since 0.1.133).
- **Effect:** worker1's orchestrator is restarting, so its `GET_RUNTIME` times out, while AI.x@worker1
  runs pinned to ai.generic 0.3.1; the operator removes 0.3.1; SY.admin sees no running node and
  deletes it from dist; worker1's receive-only mirror deletes it when it reconnects; AI.x runs on
  an unlinked binary until its next restart, where the reconcile fails it (*"unable to resolve
  runtime version"*). SY.wf-rules' package purge goes through the same removal.
- **Fix (0.1.133, SY.admin):** a usage that is missing, has `status: error` or a non-empty
  `skipped_hives` refuses the removal (409, naming the hives that did not answer), the
  `in_use: false` default goes, and `RUNTIME_IN_USE`'s detail names each node's state (A-285).
- **Seen on 8.x (2026-11-11, 0.1.132):** every global usage lists egress1 and ingress1 in
  `skipped_hives` with `RUNTIME_MANIFEST_MISSING`: those roles receive no runtimes. Refusing on any
  skipped hive would refuse every removal for good, so A-281 has an orchestrator part too
  (`coa/orch-usage-133`): a hive whose role runs no managed nodes is never reported as skipped; a
  worker or the motherbee with no manifest still is.
- **The two halves (0.1.133):** `coa/admin-usage-133`, SY.admin's refusal (`RUNTIME_USAGE_UNKNOWN`
  naming the hives not read, retryable; `RUNTIME_IN_USE` naming each node's hive and state); and
  `coa/orch-usage-133`, the motherbee asks no ingress or egress, by the role its registry records,
  and lists them in `hives_without_runtimes`; an ok answer whose own usage failed is skipped as
  `USAGE_UNREADABLE`; a registry that cannot be listed makes the whole usage an error; `in_use` is
  null while a hive is skipped; a registry entry keeps its role until `remove_hive` (`add_hive`
  with another role answers `HIVE_EXISTS`).
- **Found by SY.admin's review (high), fixed with it in 0.1.133:** with the refusal, a workflow's
  delete answers 422 `PACKAGE_PUBLISH_FAILED` after the workflow, its node and its state are gone
  whenever a hive cannot be read (worker1's copy lagging a `wf.<x>` deployed seconds before
  included), and the versions left behind block a new workflow of the same name: its versions
  start again at 0.0.1, and the publish answers `VERSION_CONFLICT`. Built: SY.wf-rules' delete
  answers ok and names each version left in `packages_left` (`{runtime, version, error_code,
  detail, retryable, unanswered_hives}`) with a warning, also removes the versions its slots
  named (a worker whose copy lags answered `[]` with a version left), and a name created again
  numbers its versions above every version the dist holds (`coa/wfrules-usage-133`, `b04ecc2c`);
  the motherbee asks every worker at once for its nodes alone (`GET_RUNTIME`'s `usage_only`,
  answered from the node configs whatever its copy lists), each within 10 s, so a worker whose
  copy lags is counted and a silent one is named `TIMEOUT` in about 10 s (`coa/orch-usage-133`).
- **The orchestrator's review round (`84653ab5`):** the motherbee's runtime reads wait in places
  of their own (`ReadTarget::RuntimeUse`, 4 at a time), never behind `list_versions`'s 45 s asks,
  and Fluxbee Cloud's `list_nodes` never behind them; a hive's ask ends by the forward's own 10 s,
  with no outer cut, which left its request in the router client for good (A-309); a node whose
  `config.json` cannot be read while its unit runs or gives systemd no answer makes its hive
  `USAGE_UNREADABLE` (on the motherbee, the whole use an error). Found on the way: A-308, A-310.
- **SY.wf-rules' review round (`f11b6c58` to `b04ecc2c`):** each removal is waited for up to 35 s
  (SY.admin reads every hive's use first) and a deploy's or a rollback's whole purge 15 s; each
  version left says whether asking again may remove it and which hives were not read; a dist that
  cannot be listed has its own warning; `fluxbee-factory-reset` counts the versions left
  (`packages_left=` beside `problems=` in its log line); the lifecycle e2e's default name is
  `testwfrules` (`test-wf-rules` is refused since 0.1.98). Found on the way: A-305, A-306, A-307,
  and a name on two hives (A-304's addition), all for later.
- **The integration (0.1.133, `coa/a281-133`):** SY.admin's texts were written before the
  orchestrator's last two rounds: they said the motherbee asks the workers one at a time and a
  delete fails with `PACKAGE_PUBLISH_FAILED`. One commit (`defd2b60`) aligns SY.admin's notes and
  tests, Archi's handbook, `sy-admin.md` and `operations.md` with the built behaviour, checked
  against the code (a delete's removal waits for what is left of its request's 60 s, at most 35 s;
  `[]` means every version its hive knew of; `TRANSPORT_ERROR` is no timeout), and puts back three
  meanings `operations.md` lost in a conflict: a node whose unit does not run is not listed, a join
  not done is named with its status and phase, and the motherbee alone means its read failed.
- **The integration's review (five lows, fixed in `8879ff2e`):** `RUNTIME_USAGE_UNKNOWN` says
  `retryable: false` while a hive not read is a join that ended (failed or interrupted: only
  `add_hive` again or `remove_hive` changes it), and for the motherbee's `USAGE_READ_FAILED` (a node
  whose `config.json` cannot be read while it may run, or its registry), whose detail now says what
  to fix instead of *"Ask again"*; SY.wf-rules' `PURGE_STOPPED` after such a refusal says false too;
  a plan's answer cut to fit the frame keeps a workflow delete's `packages_left` in short; the texts
  say a `DIST_UNREADABLE` entry blocks a compile of the name until that hive's
  `dist/runtimes/wf.<name>` is fixed, its versions then read from the motherbee's `get_runtime`; and
  `wf-standard-path-v1.md` gives a removal 35 s, a deploy's purge 15 s, where it said 5 s.
- **By design:** a failed join's registry entry is a hive that may run nodes and is not read, so it
  refuses every removal until the join is finished or the hive removed; the 409 names it.
- **SY.admin's review fixes (`coa/admin-usage-133`, `a75c2d4f`):** an ingress or an egress is exempt
  by the role its registry entry records, in any join state; a hive whose join is not done is
  named with its status, its join phase and what to do (finish the join or `remove_hive`); the
  dependents check comes before the use's read; a node pinned to a version whose unit does not run
  is not counted (A-300), and the texts say to stop it first. Same class, fixed with it:
  `teardown_node` refuses 503 `INVENTORY_UNAVAILABLE`, nothing removed, for a node whose hive the
  LSA holds stale (or does not list) while the registry still lists it; before, it removed the
  node's secrets and purged its ILK.
- **Checked on 8.x (0.1.133, run on 0.1.134 past its first read, 23:37 to 23:47Z):**
  `get_runtime`'s use skipped egress1 and ingress1 `RUNTIME_MANIFEST_MISSING` on 0.1.132 (2.8 to
  3.2 s); from 0.1.133 it skipped none and listed them in `hives_without_runtimes` (1.1 to 1.5 s,
  on 0.1.133 and on 0.1.134). With worker1's orchestrator stopped, the removal of
  io.slack 0.1.14 (unused) was refused `RUNTIME_USAGE_UNKNOWN` naming worker1 `UNREACHABLE`,
  retryable, in 1.5 s, nothing removed; SIGSTOPped, it named worker1 `TIMEOUT` (*"no answer within
  10 s"*) in 12.8 s. In the lanes, worker1's orchestrator SIGSTOPped: 4 `GET /versions` held 30.8 to
  31.3 s, a removal started 3 s after them answered in 11.2 s, `/hive/status` in 1.4 s, 4 runtime
  reads in 11.2 to 11.9 s (the Cloud's `list_nodes` behind the `/versions`: A-308). Workflows
  deleted right after their deploy answered ok with `packages_left` [] (2.7 and 3.2 s; the lagging
  copy not reproduced live, its test covers it); with worker1's orchestrator stopped, two deletes
  answered ok in about 3 s naming `packages_left` (0.0.1 `RUNTIME_USAGE_UNKNOWN`, 0.0.2
  `PURGE_STOPPED`, both retryable) with the warning, one deployed again over its leftovers ran
  0.0.3, and once worker1 answered the leftovers went (a current version first refused
  `RUNTIME_CURRENT_CONFLICT`). `teardown_node` of `AI.gate0233@worker1` with worker1's orchestrator
  and router stopped 43 s: 503 `INVENTORY_UNAVAILABLE`, nothing removed, its ILK kept. io.slack
  0.1.14 removed with every hive up (3.1 s).

### A-280 [General/admin] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; with `SY.edge@ingress1` frozen 88 s, a publish answered `EDGE_PUBLISH_FAILED` in 76.6 s, within its 110 s, with no ledger row; a list and a take-down right after answered in 0.079 s and 0.003 s; its clean-up ended 0.028 s after the answer) (found 2026-11-11 by A-274's review; read in the code; there before it) — A publish holds the publication lock across its curate and the edge's publish, and on its failure paths across a clean-up its budget does not bound

- **Why:** `handle_publish_artifact` (`src/bin/sy_admin.rs` ~6637-6773) takes the publication lock
  before IO.blob's curate (30 s at most) and holds it through the edge's publish (75 s at most),
  inside its 110 s budget. On a failure path it also holds the lock across `release_curated_blob`
  (30 s; also inside `curate_publication_blob`, ~7655), and when its last ledger write fails,
  across `unpublish_edge_blob` and the release (60 s). The budget bounds none of these clean-ups,
  so the docs' *"one budget of 110 s covers a whole publish_artifact"* is not exact on a failure
  path.
- **Effect:** with SY.edge@ingress1 listed but down, each publish holds the lock about 75 s, then
  up to 30 s more, and every take-down's and every sweep step's read and write waits behind it
  (A-260 and A-274, 0.1.133, took out only their own holds). The publish itself can answer at
  about 140 s, after ai.generic gave up at 120 s.
- **Fix, later:** split the publish the way A-260 and A-274 did (the curate and the edge's publish
  outside the lock, the row written under it); the take-down must then handle the `curated` row of
  a publish still under way, which `handle_unpublish_artifact` today rules out by the lock (comment
  at ~6833-6836). The smaller fix keeps the failure path's clean-up inside the budget, or outside
  the lock.
- **The operator's call (2026-11-11):** the small fix: the failure path's clean-up inside the
  budget or outside the lock; a publish that succeeds is unchanged. In 0.1.137 (SY.admin).
- **Built and shipped in 0.1.137 (`coa/admin-136`; `34c85e03`, its review `e29a03e0`, on daily):**
  a publish that fails once it asked IO.blob to curate answers at once and lets the publication
  lock go; its clean-up (IO.blob's release, after the edge's unpublish when the edge took the page
  and the last ledger write failed) runs after the answer in a task of its own, each call within
  its 30 s, and ends with one info line, *"a failed publish's clean-up ended"*, with what each step
  did and its `elapsed_ms`. One budget of 110 s covers a whole publish; a publish that succeeds is
  unchanged. Found by its review: A-318.
- **Checked on 8.x (0.1.137, gate0237, 2026-11-12 10:00–10:02Z):** with `SY.edge@ingress1` frozen
  88 s (SIGSTOP, a timer to continue it), a publish answered `EDGE_PUBLISH_FAILED` in 76.591 s,
  within its 110 s, and the ledger kept no row for it; `list_artifacts` right after answered in
  0.079 s and a take-down of an unknown page in 0.003 s, no 30 s more; the line *"a failed
  publish's clean-up ended"* came 0.028 s after the answer, both on the motherbee's clock (the
  command log's `EDGE_PUBLISH_FAILED` at 10:01:38.223Z, 76,569 ms; `edge_unpublish` `not_asked`,
  the clean-up's `elapsed_ms` 23).

### A-279 [Archi] ✅ VALIDATED ON 8.x (0.1.133, 2026-11-11; through Archi, a timer cursor written `%3A` and an owner `%40` page, where 0.1.132 refused them) (found 2026-11-11 by A-266's review; read in the code; there before it) — Archi's timer list passes its query undecoded, so a cursor or an owner it encoded is refused

- **Why:** SY.architect's SCMD translation of `GET /hives/{hive}/timer/timers`
  (`src/bin/sy_architect.rs` ~14104-14130) copies `cursor`, `owner_l2_name` and `status_filter`
  as they come; Archi's other paged reads decode through `scmd_page_params` (~12914), and SY.admin's
  own route decodes too (`parse_query`). SY.timer's cursor is `<fire_at>:<uuid>`: one Archi
  writes as `%3A`, or an owner with `%40`, reaches SY.timer encoded and is refused
  `TIMER_INVALID_PAYLOAD` (an owner with no `@` is no canonical L2 name; A-279's review).
- **Effect:** since A-266 (0.1.133) a timer list cut by its limit (100 by default) answers
  `next_cursor`, so Archi pages it far more often. Archi's `fluxbee_get` tool text names the paged
  lists without the timer list (~1039), and SY.admin's catalog text for `timer_list`'s `cursor`
  names only the frame's cut.
- **Fix:** the translation decodes as `scmd_page_params` does; the tool text names the timer list.
- **Found and fixed with it (A-279's builder):** `percent_decode_simple` sliced the text at byte
  offsets, so a `%` before a character of more than one byte (`%aé`) panicked the request's task
  (no answer; the process stays up), and `%+A` decoded to a newline.
- **Checked on 8.x (0.1.133, against 0.1.132):** through Archi's SCMD reads, a timer cursor written
  `%3A` or `%3a` pages (0.1.132: `TIMER_INVALID_PAYLOAD`), `%253A` is still refused, an owner
  written `%40` works; a broken escape in `list_nodes`' cursor answers `INVALID_CURSOR` as JSON
  (0.1.132: no JSON); a path in quotes works (0.1.132: `SCMD_EXECUTION_FAILED`).

### A-278 [WF/Go SDK] ✅ VALIDATED ON 8.x (0.1.138, 2026-11-12; a WF restarted with 3,000 pending timers listed them all in 13 pages in 0.931 s, took none as missing and sent none again, never `DEGRADED`, `HEALTHY` at 3.670 s and its receive loop at 4.967 s, where on 0.1.137 it listed 1000, took 2,000 as missing, sent 2,030 again, ran `DEGRADED` 72 polls and was `HEALTHY` at 94.453 s, its receive loop at 18.205 s; its reconcile took 3.558 s, over the 1 s the builder's numbers gave: A-333; a list that stops before its end and the SDKs' `List`, `list` and `list_page` not provoked on 8.x: by their tests) (found 2026-11-11 by A-266's builder and review; read in the code; there before it) — A WF with more than 1000 pending timers cannot reconcile them

- **Why:** the WF's reconcile (`go/nodes/wf/wf-generic/node/recover.go` ~286-299) lists its pending
  timers with `Limit: sdk.MaxTimerListLimit` (1000); the Go SDK's `List` (and the Rust SDK's) stops
  at its limit and drops SY.timer's `next_cursor`, so even after A-266 the WF cannot tell exactly
  1000 timers from more. It only logs *"reconciliation may be truncated"* at exactly 1000.
- **Effect:** every index timer past the first 1000 by fire time looks missing in SY.timer: it is
  marked refused (*"TIMER_LIST: SY.timer@<hive> does not have it"*) and sent again at 32 a tick;
  SY.timer answers each `already_existed`, and the node stays `DEGRADED` meanwhile (about 1 min for
  3,000 timers, about 5 min for 10,000, at every reconcile). An owner may hold 10,000 pending
  timers. None on 8.x holds more than a handful today.
- **Fix, its own release (the SDKs, then the WF):** `List` says whether more follow (or returns the
  cursor) and the WF pages through all its pending timers; a cheaper stopgap is that the WF takes
  index timers past the last one listed as unknown, not missing.
- **The operator's call (2026-11-11):** built in the sweep, its own release (the SDKs and the WF),
  after 0.1.137.
- **Built on `coa/a278` (for 0.1.138; no SY.timer change):**
  - **The SDKs say where the timers past a list start.** The Go SDK's `ListPage` is one
    `TIMER_LIST`, one page as SY.timer answers it, with its `NextCursor`; `ListFilter.Cursor` is
    where a list starts; `List` keeps its signature and what it returns. The Rust SDK's `list`
    hands back SY.timer's `next_cursor` when it stops at its limit with timers following (before,
    it dropped it), and `list_page` is one page. Go's `List` keeps its signature, the WF moves to
    `ListPage` (its `TimerSender` and its test fakes), and the Rust examples' `list_mine` compile
    unchanged.
  - **The WF reads every page** (`listTimers`, `node/recover.go`): 1000 a page at the most, about
    210 to 240 of a WF's timers to a page's 96 KiB (by the lengths of its name and its timer keys),
    each page from the cursor the page before gave, until one gives none. It stops before the end,
    with a log line that says why, at 200 pages (about 42,000 to 48,000 timers; SY.timer keeps
    10,000 an owner, 42 to 48 pages, about 90 at the longest client_ref), at a page of no timer that
    gives a cursor, at a cursor a page before gave, or at a page of 1000 that gives none, as SY.timer
    before 1.6 answers a page its limit cut (A-266; a frame holds about 400 timers at the most, so
    the limit 1000 never ends a page, and an older SY.timer is read whole too). When the list stops
    before its end, an index timer from the last one listed on (within 2 s: SY.timer normally holds
    a timer at the index's time or up to 2 s later) is never taken as missing: one ahead of its time
    is left as it is, and one past it is fired (the review, below); one before it that SY.timer
    lacks still goes again. One SY.timer holds later still (a move it did not take, or a schedule
    it took more than 2 s late) can read as missing when its index time is before the last one
    listed: it goes again once, and SY.timer's `already_existed` settles it. A timer that fires, or
    that an instance schedules or moves, while the pages are read is taken as before (a fire given
    once; one scheduled or moved after the list was asked left alone).
  - **Tests** (`node/reconcile_pages_test.go`): 2,500 pending timers in pages of 1000, and of 228,
    none taken as missing nor sent again; the bound (200 pages of 10); a page of no timer with a
    cursor, and a cursor given again; an SY.timer before 1.6, whose frame ends the pages, and whose
    limit ends one. Each fails on the reconcile before it, which took 1,500 or 2,000 of the timers
    as missing. Beside them, the timers that fire, are scheduled and moved while the pages are
    read. The SDKs: `TestListPage*` (Go),
    `timer_client_list_hands_back_the_cursor_its_limit_stopped_at`,
    `timer_client_list_stops_at_a_page_of_no_timers`, `timer_client_list_page_is_one_timer_list`
    (Rust).
  - **Its time, measured on the Mac (SQLite and the encoding, no router):** SY.timer lists 3,000 of a
    WF's pending timers in 14 pages in 0.13 s, and 10,000 in 47 pages in 0.6 s, 10 to 16 ms a page
    with 8,000 to 15,000 pending timers on the hive (5,000 of other owners); the WF's reconcile of
    them takes 0.23 s and 0.73 s more. Before, the node ran `DEGRADED` about a minute (3,000) or
    five (10,000) at each reconcile, sending the timers past the 1000th again 32 a second. A page's
    cost grows with the hive's pending timers, every owner's, not with the limit: SY.timer's query
    reads every pending timer of the hive through its status index, then sorts the owner's past the
    cursor (A-326), and the walk does that at each page. At the hive's cap, 100,000 pending, a page
    takes about three times as long, and SY.timer serves one request at a time.
- **The review (2026-11-12; three reviews: the paging, the SDKs' compatibility, operations):**
  - **A timer past its time and past the last one listed is fired.** A list that stops before its
    end is no error: the node is not `DEGRADED` for it and the loop asks no list again, so no
    reconcile comes before the node's next start, where the texts said a later one lists the timers
    left. Such a timer, SY.timer having lost it, would never have fired; it is now taken as a
    pending timer past its time: the instance takes a fire now, and the one SY.timer may hold is
    cancelled unless the fire scheduled the key again (`firePastTimer`; SY.timer answers a cancel
    of a client_ref it holds nothing pending for `found: false`, and a late fire of it is dropped,
    the index no longer having it). One ahead of its time waits for the next start, the log line
    that counts them its only sign. Test:
    `TestAReconcileWhoseListStopsEarlyFiresTheTimersPastTheirTime` (at the start and off the loop),
    which fails without the fix.
  - **The logs:** the listing counts each timer once (a timer whose fire time moved between two
    pages, a page given again); a reconcile of a list that stopped before its end says it is done
    for the timers listed, not complete, and off the loop that the node's next start lists the
    rest. `TestAReconcileStopsAtAPageItCannotGoOnFrom` checks them.
  - **The texts:** the 2 s margin is SY.timer's usual lateness, not a bound (a move it did not take
    holds a timer later); a timer the node itself sends again, or moves to its time, while the
    pages of a reconcile off the loop are read can read as missing once, settled by
    `already_existed` (as it could with one list, over more pages now); the page counts depend on
    the names' and keys' lengths; the Go SDK's `List` returns the timers only (the `TIMER_LIST` row
    of `docs/nodes/sy-timer.md`);
    the README says scheduled or moved. `wholeList` moved to the tests; the older paging tests of
    both SDKs use the new helpers.
- **Shipped in 0.1.138** (`5acf764b`, its review `38ecd878`, on daily). **Checked on 8.x (gate0238,
  2026-11-12):** `WF.gate0238@worker1` (tenant pruebas), each conversation scheduling 10 timer
  keys, 150 conversations and then 150 more at 5 a second (T0, a restart's answer on the
  motherbee's clock; T0w, the node's start in worker1's journal).
  - **On 0.1.137 (12:36–12:44Z):** at 1,500 pending timers a restart logged *"reconciliation may
    be truncated"* once and took the 500 past the 1000th as missing (*"missing from SY.timer
    (fire_at still future)"*); it sent them again and SY.timer answered `already_existed`;
    `DEGRADED` 17 polls, `HEALTHY` at T0+25.069 s; the old reconcile 7.348 s. At 3,000: 2,000
    missing, 2,030 sent again (2,000 taken after a resend, 1,970 of them after one); `DEGRADED` 72
    polls, from T0+17.393 s to T0+93.452 s, `HEALTHY` at T0+94.453 s; the old reconcile 16.727 s,
    the receive loop at T0w+18.205 s, the resends' drain 76.316 s. SY.timer's digest of the WF's
    timers unchanged by each restart: nothing duplicated or replaced.
  - **The deploy's relaunch (13:29Z):** the WF unit on wf.engine 0.1.138 (A-302); the relaunch
    listed the 3,000 pending timers in 13 pages in 3.312 s and logged its reconcile complete once,
    with none of the old engine's or a cut list's lines, the reconcile 4.372 s; `HEALTHY`; the
    store (3,000 timers, 0 refused, 300 running) and SY.timer's digest as after the baseline.
  - **A restart on 0.1.138, the hive at rest (13:30Z):** one listing line, 3,000 pending timers in
    13 pages (SY.timer's walk 13), the node's own measure 0.931 s; the reconcile complete once;
    none taken as missing, none sent again, no orphan cancelled; never `DEGRADED`; `HEALTHY` in
    the node's own answer at T0+3.670 s, the receive loop at T0w+4.967 s; the store and SY.timer's
    digest unchanged. The gate's one FAIL: the reconcile, its *"listed"* line to its *"complete"*
    line in worker1's journal, 3.558 s against the 1 s the builder's Mac numbers set (A-333).
  - **Not provoked on 8.x, so by its tests:** a list that stops before its end (the review's fire
    of a timer past its time and its cancel, the 200-page bound, a page of no timer that gives a
    cursor, a cursor given again, an SY.timer before 1.6): neither the relaunch nor the restart
    logged a cut list's line. And the Go SDK's `List` and the Rust SDK's `list` and `list_page`:
    no caller on 8.x (the WF takes `ListPage`; ai.generic's `TimerClient` asks only
    `TIMER_NOW_IN`, and SY.admin's timer list route sends its own `TIMER_LIST`).
  - **The clean-up:** with the workflow's forced delete, SY.timer's `timer_owner_purged` deleted
    the WF's 3,000 timers, all pending; none left. No model call in the gate's window.

### A-277 [General/identity] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; a removed throwaway worker's reconnects refused *"not in the hive registry"*, 132 with its key deleted and 74 with its key pinned in place, none during its joins; with the key pinned sy-identity was not restarted and the 10 s recheck closed its sessions, the last 7.7 s after the answer; at the deploy no refusal, and worker1's replica in sync, its metrics equal and 2 sessions) (found 2026-11-11 by the review fixes of identity's 0.1.133 branch; read in the code) — A removed hive whose identity key could not be deleted keeps reading the identity store

- **Why:** `server_auth_handshake` (`src/bin/sy_identity.rs` ~5721) checks only the hive's key file,
  not the hive registry; `remove_hive_flow` (`src/bin/sy_orchestrator.rs` ~22100) restarts
  sy-identity only when it deleted the key (`identity_key_cleanup: delete_failed` leaves it), so a
  removed hive the cleanup did not reach keeps its delta subscription, and a new `FULL_SYNC` still
  authenticates, until the orchestrator's next start deletes the orphan key
  (`sweep_orphan_identity_keys`, ~1888) and sy-identity restarts.
- **Since 0.1.133:** the primary drops that hive's publishes (A-191's review); its reads stay.
- **Fix, to decide (the auth path):** the handshake answers a hive the registry no longer lists
  with the same uniform rejection, after checking that a join's first handshake comes after its
  registry entry.
- **The operator's call (2026-11-11):** SY.identity refuses a hive the hive registry no longer
  lists. In 0.1.137 (SY.identity).
- **Built on `coa/identity-136` (0.1.137).** Its residual is the orchestrator's: A-301 (a fresh join
  under a removed hive's name reuses a surviving key).
- **Docs at its integration:** `docs/host-posture-and-exposure-spec-v1.md` still expects *"no key
  for hive"* for a removed hive (A-48's infra steps (2) and (8), and the D6-13/A6-9 row); from
  0.1.137 the line is `auth: hive '<hive>' is not in the hive registry (removed or never joined)`.
- **Its gate:** after a removed box's sessions are cut, its refusals come about once a second (the
  subscription's), then about twice a second once its next reconcile push, within 30 s, finds the
  publish stream closed.
- **Shipped in 0.1.137** (`b2e25c48`, its review `607aecd8`, and `db5e82df` on daily). **Checked on
  8.x (gate0237, 2026-11-12):**
  - **At the deploy:** the motherbee's sy-identity listened from 09:48:28.952Z and its subscribers
    registered 0.55 to 53.4 s after; no *"is not in the hive registry"* refusal and no *"identity
    sync session closed"* since the deploy; worker1 applied a full sync; `IDENTITY_METRICS` equal
    on the motherbee and worker1 (`tenant_count` 3, `ilk_count` 84, `ich_count` 6,
    `deleted_ilk_count` 51); worker1's 2 sessions to :9100 the same 15 s later.
  - **A throwaway worker on VM 104, `gate0237i`:** joined in 773 s (about 674 s of it the fresh
    clone's package upgrade: B-18), no refusal of it during the join, the box's full sync, 2
    sessions. Removed with its box cut off (`remote_cleanup_via` `local_only`, 10:37:43Z): the key
    deleted, sy-identity restarted, `system_ilks_retired` 6; no session from the box; its
    reconnects refused *"not in the hive registry"*, 132, about one a second and then about two
    within 30 s; no *"dropped an identity publish"* line; worker1's replica back with 2 sessions;
    the 6 ILKs still retired a minute later.
  - **`gate0237j`:** an `add_hive` onto the same box failed (A-331); on a fresh VM 104 it joined in
    829 s, with no refusal during the join, its full sync and 2 sessions. Removed with the
    motherbee's key for it pinned (`chattr +i`, a timer to undo it; 11:02:11Z):
    `identity_key_cleanup` `delete_failed` (the key stays) and `identity_restart` `not_needed`
    (sy-identity not restarted, as the Why above has it), `local_only`, `system_ilks_retired` 6;
    its sessions (the delta subscription and the publish) closed by this item's 10 s recheck, the
    last 7.7 s after the answer; the key file still there; 74 reconnects refused *"not in the hive
    registry"* (about one a second, then two); one *"dropped an identity publish"* line, none after
    the cut; worker1's sessions unchanged; the 6 ILKs retired a minute later. The key unpinned and
    removed by hand; with VM 104 destroyed, both names' refusals stopped.

### A-276 [General/identity] 🔵 low (found 2026-11-11 by A-191's builder; read in the code) — A replica cut off during a purge can bring back a purged node ILK

- **Why:** when a node ILK of a replica's hive is purged (or torn down) while that replica is cut
  off, its stale self-owned snapshot, or the backlog its publish loop drains after it reconnects,
  still carries the record, and the primary's `reconcile_hive_ilks` (`src/bin/sy_identity.rs`
  ~2823) upserts any acceptable ILK it does not hold. A-191's guard (0.1.133) covers marked system
  ILKs only.
- **Fix, later:** the primary refuses a replica's push of a node ILK it purged (a tombstone kept
  long enough for every replica), next to A-159's chapter on deleting ILKs.

### A-275 [General/admin] 🔵 low (found 2026-11-11 by A-191's builder; read in the code) — A removed hive's managed nodes keep their ILKs active and their vault secrets

- **Why:** `remove_hive`'s spoke cleanup (`remove_hive_cleanup_script`, `src/bin/sy_orchestrator.rs`
  ~21956) stops the `fluxbee-node-*` units and deletes their state, but `teardown_node` is not run
  for those nodes: their agent, IO and WF ILKs stay active on the primary, and their dedicated
  secrets stay in SY.vault. A-191 (0.1.133) retires a removed hive's system ILKs only.
- **To decide:** the operator's rule for ILKs (2026-11-10: one with history is disabled, never
  deleted) suggests `remove_hive` marks them retired as it does the system ones; whether their
  secrets are deleted with them, or kept until the operator says.
- **The operator's rule (2026-11-11):** only Fluxbee Cloud, through IO.cloud, deletes one thing
  at a time (its own secrets, and what it can today); an ILK with data is a reference and is never
  deleted; ILKs, secrets and history go together only when the Cloud deletes a whole tenant, with a
  warning. So `remove_hive` leaves its nodes' ILKs and secrets as they are, and the chapter on
  deleting (A-159) takes it. **A conflict to settle there:** a node's purge (`kill_node` with
  `purge_instance`, then `teardown_node`) deletes the node's secrets and purges its ILK today.

### A-274 [General/admin] ✅ VALIDATED ON 8.x (0.1.133, run on 0.1.134, 2026-11-11; with the edge stopped, take-downs not waiting on it answered in 1.4 s; of two take-downs of one page, one wrote and the other wrote nothing over it) (found 2026-11-11 by A-260's builder; read in the code; there before it) — A take-down holds the publication lock across its edge and IO.blob calls

- **Why:** `take_down_publication` (`src/bin/sy_admin.rs` ~6919) is called with the publication
  ledger held (`&mut AdminPublicationLedger`) and awaits the edge's unpublish and IO.blob's release,
  30 s each at most. `unpublish_artifact`, Archi's take-downs and the factory reset's [0] pass go
  through it.
- **Effect:** with an edge that is listed but down, each take-down holds every publish for up to
  60 s, and the factory reset takes its pages down one after another.
- **Fix:** A-260's split: the calls outside the lock, the write under it only while the row is
  still at the state read.
- **Checked on 8.x (0.1.133, with A-260, run on 0.1.134):** with `SY.edge@ingress1` SIGSTOPped
  48 s, two take-downs waited on it (`EDGE_UNPUBLISH_FAILED` at 31 s) while take-downs of an
  unknown page answered `NOT_FOUND` in 1.4 s, twice; after the edge went on, two take-downs of one
  page sent together: one took it down (8.2 s), the other wrote nothing over it (7.9 s, *"another
  writer took the page down meanwhile"*).

### A-273 [General/admin] ✅ BUILT (0.1.137, 2026-11-12; live on 8.x, its help checked there: `publish_cloud_endpoint`'s names A-273, `CONFIG_RECORD_FAILED`, never through Archi and `POST /hives/motherbee/cloud/endpoint`; the failure itself not provoked, by its tests) (found 2026-11-11 by A-255's builder; read in the code; there before it) — `publish_cloud_endpoint` answers `CONFIG_RECORD_FAILED` with the door already open and its new token lost

- **Why:** the externalize has already minted a new token and the edge holds it when the record
  step (`set_node_config` of IO.cloud's `io.edge_node`, `src/bin/sy_admin.rs` ~7870-7905) fails;
  the error answer carries no token and says to publish again, which rotates the token once more.
- **To decide:** whether the failed record is retried before answering, the door closed again, or
  the token handed over with the error (A-124: a token through Archi reaches the AI provider).
- **The operator's call (2026-11-11):** the record is tried once more before the error; when it
  still fails, the error says the previous token no longer works and running again hands a new one.
  The token goes out once, only with a success. In 0.1.137 (SY.admin).

- **Built on `coa/admin-136` (0.1.137), with its review's fixes:** the record is tried once more
  at once; when it fails again, 502 `CONFIG_RECORD_FAILED` says the door is open with a token nobody
  received, that the previous token works for up to 10 minutes and stops sooner if the edge
  restarts or the action runs again (SY.edge keeps one previous token: A-317), to fix the cause
  first and run it again on the motherbee's HTTP API, never through Archi (A-124), and to give the
  Cloud the new token at once. On a record that times out, the retry waits another 30 s, so the
  call can take about 2 minutes. Found by its review: A-316 and A-317.
- **Shipped in 0.1.137** (`27b94c5a`, its review `9a9334bd`, on daily). **On 8.x (gate0237,
  2026-11-12 09:55Z):** only its help was checked: `publish_cloud_endpoint`'s names A-273,
  `CONFIG_RECORD_FAILED`, never through Archi and `POST /hives/motherbee/cloud/endpoint`. A failed
  record was not provoked on PROD (it would mint a token nobody receives); its tests cover it.

### A-272 [General/router] ✅ VALIDATED ON 8.x (0.1.135, 2026-11-11; 32 broadcasts from each hive reached every receiver exactly once and no router logged a drop, where 0.1.134 logged the echoes, 7 lines on the motherbee and 6 on each spoke) (found 2026-11-11 by 0.1.132's gate; seen on 8.x, read in the code; there before it) — Every router sends a WAN broadcast back to the peer it came from

- **Seen:** the motherbee logged `foreign_source` drops of its own nodes' broadcasts coming back
  from ingress1 and worker1 (`src_l2_name` `IO.gate0232b@motherbee`, 0.1.131 and 0.1.132), and
  worker1 dropped its own node's broadcast coming back from the motherbee (`IO.gate0232e@worker1`);
  the motherbee's counter reached 40 within 8 minutes of its router's restart.
- **Why:** `handle_wan_message`'s broadcast arm (`src/router/mod.rs` ~4187-4230) delivers locally
  and then calls `broadcast_to_wan` toward every WAN peer, the one the frame came from included. A
  spoke sends every broadcast of the hub back to it, and the hub sends a spoke's broadcast back to
  that spoke; the receiver drops it by its source check (the cache would drop it next).
- **Effect:** one frame wasted per spoke and per broadcast, the `foreign_source` counter and its
  line filled with echoes (most of what A-195 counted), which hides a real drop.
- **Fix:** split horizon: a broadcast that came over the WAN goes on to every WAN peer but the one
  it came from (a spoke, with the hub its only peer, sends none).
- **Checked on 8.x (0.1.135, against a baseline on 0.1.134 the same night):** a receiver
  `IO.gate0235r` and a sender `IO.gate0235s` on each of the four hives, each sender sending a
  broadcast (ttl 16, `meta.target` `IO.gate0235*`) every 15 s. On 0.1.134 (16 per hive) every
  receiver got each broadcast once, and the routers dropped the echoes and logged them as *"router
  dropped a WAN frame"* (`foreign_source`): 7 lines on the motherbee (`peer_hive` worker1 and
  egress1, its own sender's broadcasts coming back) and 6 on each spoke (`peer_hive` motherbee, the
  spoke's own sender's coming back from the hub); lines, not frames, at most one every 30 s a
  router. On 0.1.135 (32 per hive, from 2 min after the last router restart) every receiver got
  each broadcast exactly once (none missed, none twice), and no hive logged a drop line. Since then
  `ops.py health` checks `wandrops=0` (`HEALTH_OK`).

### A-271 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.133 with 0.1.134's A-314 fix, run on 0.1.134, 2026-11-11; 20 self-restarts on worker1 and egress1 acted on no check; its reading of a unit systemd does not have as no answer was A-314, fixed in 0.1.134) (found 2026-11-11 by the review of the orchestrator's 0.1.133 branch; read in the code; there before it) — A check that decides an action takes "no answer" for "no"

- **Why:** A-267 fixed this in `systemd_is_active` only (a killed or failed `systemctl is-active`
  read as "not running"). The same two-state reading is in other checks of
  `src/bin/sy_orchestrator.rs` (lines on daily at 36135e52):
  - `systemd_unit_is_active` (~23944) and `systemd_unit_is_failed` (~26373) return
    `status.success()`: a killed check reads as not active or not failed. They feed the 60 s
    `reconcile_persisted_custom_nodes`'s relaunch, `remove_node_instance_flow`'s
    `NODE_INSTANCE_RUNNING` guard and the run, start and kill flows.
  - `systemd_unit_exists` (~35648): a failed or killed `systemctl show` reads as "no such unit", so
    `restart_local_core_services_with_health_gate` (~4966) skips that service and, after a core
    binary swap, it keeps running the old binary while the update reports ok (U-1's shape).
  - `nft_table_loaded` (~10902): a killed `nft list table` reads as "table missing", and the egress
    watchdog deletes and applies a healthy NAT table, for example during the orchestrator's own stop.
  - `check_syncthing_folder` and `syncthing_folder_status` (~15059, ~14994, curl): a killed or
    failed curl reads as an unhealthy folder, and the blob sync watchdog logs *"syncthing unhealthy;
    restarting"* and runs an ensure.
- **When:** a check in flight when systemd stops the orchestrator (its own self-restart, every
  deploy), or a systemctl, nft or curl that fails for another reason.
- **Fix:** a tri-state per check (yes, no, no answer), where only "no" acts, as A-267 did; each
  caller says what "no answer" means to it.
- **Checked on 8.x (0.1.133, with A-267, run on 0.1.134):** 20 self-restarts of `sy-orchestrator` on
  worker1 and egress1, debug on: no unit restarted or started meanwhile, and no *"did not answer"*,
  *"nft table missing"* or *"syncthing unhealthy"* line. Its reading of `systemctl is-active`'s exit
  4 as no answer was A-314 on the hives' systemd 255: no node could be launched on 0.1.133, fixed
  forward by 0.1.134.

### A-270 [WF/SY.wf-rules] ✅ VALIDATED ON 8.x (0.1.133, run on 0.1.134, 2026-11-11; a stopped WF node's delete without force refused `INSTANCES_UNKNOWN`, with force done) (found 2026-11-11 by the review of the orchestrator's 0.1.133 branch; read in the code; there before it) — `delete_workflow` without force takes a WF node that is not RUNNING for one with no instances

- **Why:** `inspectWFNode` (`go/sy-wf-rules/node/status.go` ~137-146) returns no instance count
  when `get_node_status` answers any `lifecycle_state` but `RUNNING`, or fails with anything but a
  timeout; `DeleteWorkflow` (`go/sy-wf-rules/node/delete.go` ~29-53) then skips the
  `INSTANCES_ACTIVE` refusal and kills the node with its purge, which removes its
  `wf_instances.db`.
- **Effect:** a WF node that crashed, was stopped, or is restarting loses its live instances to a
  delete that did not say force. Since A-240 (0.1.133) a status read no longer waits behind a
  `restart_node` or `run_node` of that node, which widens the window: the operator restarts
  `WF.billing@motherbee`, and a delete a second later sees `STARTING` and purges its instances.
- **Fix:** without force, a WF node whose config exists and that is not `RUNNING`, or whose status
  read failed, is answered `INSTANCES_UNKNOWN` (its state named, and "delete with force to remove it
  anyway").
- **Checked on 8.x (0.1.133, run on 0.1.134):** `WF.gate0233@worker1` deployed (5 s) and killed
  without purge (`STOPPED`); `delete_workflow` without force answered `INSTANCES_UNKNOWN` (*"the wf
  node is STOPPED, not RUNNING (delete with force to remove it anyway)"*); with force, ok.

### A-269 [General/router] 🟡 PARTIAL (0.1.140, 2026-11-13: the router's part VALIDATED ON 8.x: a name that is not an L2 name refused at HELLO in 1.1 to 3.0 ms with no line forged, where 0.1.139 announced a name with a newline and its journal took 3 forged lines, and no live node refused; the operator's call (b), the Rust SDK's logging for every Rust binary, ships with 0.1.143) (found 2026-11-11 by the review fixes of the router's A-253; read in the code) — The router takes any node name and logs it unescaped: a local node can write a line of its own into the router's journal

- **Why:** the router checks no length or characters in a HELLO's name (`normalize_name` in
  `src/router/mod.rs` only appends `@<hive>`), and several of its lines print the name with `%`,
  unescaped (*"loading config snapshot"*, *"vpn assigned"* and others). journald takes each line a
  service writes as an entry of its own, so a name with a newline writes a line of the sender's
  choosing into rt-gateway's journal, for example the stream's loss alarm that `ops.py health` sums.
- **Who can:** only a process allowed on the router's socket (0600 with a peer-uid check since
  0.1.83, A-22), so the installation's own nodes.
- **Already done:** the stream's line that names a dropped record quotes, escapes and cuts every
  field it takes from the record (0.1.132, A-253's review), and `ops.py health` counts the router's
  lines only when the pattern opens the line's message (2026-11-11).
- **Fix, later:** the router refuses at HELLO a name that is not an L2 name (letters, digits, `.`,
  `-`, `_` and one `@`) of a bounded length.
- **Another sender (A-293's review, 2026-11-11):** IO.cloud logs the Cloud's `op` unescaped
  (`op = %op`) on its received and completed lines; a trusted sender, left for the router's fix.
- **Built on `coa/router-140` (planned 0.1.140, the router alone):** `handle_node` checks the name
  as soon as `normalize_name` gave it its hive, before any line prints it and before the `RT.`
  check, the link, the VPN and the registration take it (`is_l2_node_name`): a local part and a
  hive on each side of one `@`, each of ASCII letters, digits, `.`, `-` and `_`, at most 256 bytes
  in all (`NODE_NAME_MAX_BYTES`, the name field of the router's node table, which until then
  refused a longer name only after two lines had printed it). Any other name gets no ANNOUNCE and
  its connection closes, with two warnings: *"HELLO rejected: the node name is not an L2 name"*,
  that shows the name as the router judged it, its `@<hive>` added, quoted, escaped and cut at
  128 bytes with its full length (the stream's `shown_field`), the node's UUID and its process
  id; and the accept loop's *"node connection error"*, which names the rule and no part of the
  name, as after any refused HELLO. The HELLO's *"hello received"* line names the UUID once
  `routing.src` parsed as it, no longer the string the node wrote. Every name a node runs under
  today fits: the orchestrator launches a node only under these characters with a hive id
  (`normalize_node_name_for_target`), and the system nodes', the router's, its stream's and the
  diagnostic binaries' names are of them, so no node that connects today is refused. Tests:
  `the_router_registers_only_l2_node_names_of_at_most_256_bytes`,
  `handle_node_checks_the_name_before_anything_takes_it`,
  `a_hello_whose_name_is_not_an_l2_name_is_refused_before_a_line_prints_it` (a name with a newline
  and the stream's loss alarm after it) and
  `a_name_of_256_bytes_registers_and_no_other_than_an_l2_name_does`. Without the check the last
  three fail: the forged name was announced, and its newline opened three lines of the alarm's
  shape, `ops.py health`'s pattern matching each (*loading config snapshot*, *vpn assigned*,
  *node registered*, each `expired: 99999`). Docs: protocol.md §2.2, rt-gateway.md,
  routing-and-policy.md §2, architecture.md §3.1 and Archi's handbook §2.2.
- **Its review (2026-11-12; read in the code), fixed on the same branch:**
  - **The HELLO's own frame was a way in.** The frame is parsed before the name is checked, serde
    names an unknown variant as its sender wrote it (`meta.action_class`, any enum of `Message`),
    and the accept loop printed the error as it came (*"node connection error: {err}"*), so a
    newline there still opened the stream's loss alarm. The sibling routers' socket admits the
    same peers (the router's own user) and printed its error so too (*"peer connection error"*).
    Both accept loops now end a failed connection with `log_connection_error`, its error quoted,
    escaped and cut at 128 bytes (`shown_field`). Tests:
    `a_hello_frame_that_does_not_parse_forges_no_line_on_either_local_socket` (with the old print,
    two forged alarm lines, one per socket; none now) and the pin
    `both_accept_loops_log_a_failed_connection_escaped`.
  - The refusal shows the name as the router judged it, its `@<hive>` added: a name of 250 bytes
    without `@` is refused for the 260 it has with the hive, and its line now says 260 (it said
    250). The refusal's error says *"ASCII letters"*, as the rule is (it said *"letters"*), and
    so do rt-gateway.md and routing-and-policy.md.
  - The tests' journal could lose a line: a capture set as the thread's default, while it was
    the only subscriber registered, let another test's thread that reached a callsite first cache
    it as unwanted for every thread (tracing-core's `has_just_one`), so a parallel run could fail
    the refusal's count or pass a "no such line" check with nothing captured. The router's tests,
    the stream's among them, now capture through one global subscriber that writes to the
    capturing thread's journal (`src/router/test_journal.rs`, the frontdesk's pattern);
    `a_line_whose_callsite_another_thread_reached_first_is_kept` fails with the old capture (3 of
    3 runs alone).
- **Found while building it (2026-11-12; read in the code):** the name was one door of several. The
  router prints other fields a node writes with `%`, as they come: a message's `meta.type` on
  *"message received"* (every message from a local node, at INFO) and *"router received
  broadcast"*, its `routing.trace_id` on about twenty lines, a claimed `routing.src` on the
  `srcdrops` line itself; and journald splits a line longer than its `LineMax` (48K by default),
  so a long field can open an entry of its own too. The WAN link prints its errors as they come
  too (its review, 2026-11-12): *"wan connection error"* a peer's HELLO that does not parse, and
  *"wan uplink error"* a hub's `WAN_REJECT` reason and message; their sender is another hive's
  router, authenticated under `wan.mtls: required` (the value unless a hive sets another), any
  peer that reaches the port under `permissive` or `disabled`. Elsewhere the reach is the name's,
  the local nodes, and every local node runs as root, the router's own user, which can already
  write an entry that `journalctl -u rt-gateway` shows through the journal's own protocol (a root
  sender's `OBJECT_SYSTEMD_UNIT`, by systemd's `add_matches_for_unit`; not tried here): what is
  left is a line's hygiene, not a hole. **To decide (the operator):** (a) rt-gateway's log
  writer keeps each record one line, its control characters escaped and the record cut at 16 KiB,
  in one place for every line, the router alone; (b) the same in the Rust SDK's `logging`, for
  every Rust binary, IO.cloud's `op` among them, with the Rust SDK's release; (c) leave it.
  Proposed: (b).
- **IO.cloud's `op`** stays for IO.cloud's next change, or (b) above: the router's release goes
  alone (METHOD §9).
- **Validated on 8.x (0.1.140's gate, gate0240, 2026-11-13; the probe
  `scratchpad/gate-tools/gate0240_hello_probe.py` and `r140_gate.py`, git-ignored; 50 PASS, no
  model call):**
  - **The baseline on 0.1.139 (14:33 to 14:35Z):** no node name on any hive's router since its
    start, and none in any node list, broke the rule (so the deploy would refuse no live node). On
    worker1 the name `IO.gate0240\n<marker>` was announced and registered, its newline opening 3
    lines of the probe's marker in rt-gateway's journal (*loading config snapshot*, *vpn
    assigned*, *node registered*); a HELLO frame naming an unknown variant opened 1 on each local
    socket (*node connection error*, *peer connection error*); 0 refusals; the 257-byte name
    printed whole twice before the node table refused it (`value_too_long` 1).
  - **The deploy (15:13:32Z mark, 308 s):** rt-gateway started once on every hive; no HELLO
    refused, no connection error; every node list as before it (17, 10, 5 and 4 names); every name
    registered since is an L2 name.
  - **The 9 cases on worker1 and the motherbee (15:20Z):** the valid name and a 256-byte name
    announced; a newline, 257 bytes, an empty name and two `@` refused at HELLO in 1.1 to 3.0 ms
    (no ANNOUNCE, the connection closed), each refusal showing the name escaped and the long one
    cut; the `RT.` rule still applied; the frame variant refused on both sockets with its error
    escaped; 0 forged lines; the two announced probes disconnected; no probe left in any node
    list; health clean.
  - **Still open:** the operator's call (b) (the Rust SDK's logging, every Rust binary,
    IO.cloud's `op` among them) ships with 0.1.143.
- **The operator's call (2026-11-12): (b).** The Rust SDK's `logging` keeps each record one line,
  its control characters escaped and the record cut at 16 KiB, in one place for every Rust binary,
  IO.cloud's `op` among them, with the Rust SDK's release. The router's own part (the HELLO's name,
  its accept loops' errors) ships on `coa/router-140` (0.1.140, planned).
- **Built on `coa/rustsdk-143` (for 0.1.143, planned; the Rust SDK alone, board item 30):**
  - **The writer.** `fluxbee_sdk::logging::fmt()` writes through `OneLine`, a `MakeWriter`. The fmt
    layer asks it for a writer for each record (`RecordWriter`), which takes the whole record,
    however many writes it takes, and writes it once, as one line, when the layer drops it at the
    record's end; no flush writes part of it. `one_line` escapes every control character but the
    final newline (`char::is_control`: C0, DEL and C1) as Rust's `Debug` shows it (`\n`, `\r`,
    `\t`, `\0`, `\u{1b}`), and cuts a record past 16 KiB (`RECORD_MAX_BYTES`, its newline
    included, a third of journald's default `LineMax`) after a whole character or escape, its line
    ending `... (cut: N bytes)`, N the bytes it had. A record with neither is written as the
    formatter made it, byte for byte, so every line seen today keeps its shape. A record's message
    comes to it with its ESC, BEL, BS, FF and DEL already escaped in tracing-subscriber's own form
    (`\x1b`, `\x07`, `\x08`, `\x0c`, `\x7f`), so the journal shows a message's ESC as `\x1b` and a
    field's as `\u{1b}` (the review, 2026-11-12; the docs name both forms). On a terminal,
    where the lines carry colour (A-31), every colour code (`ESC [` ... `m`) stays, a field's too,
    which can recolour or hide the rest of its line there (a binary run by hand; under systemd
    every escape is escaped), and no other escape does.
  - **Every Rust binary.** Each starts its subscriber with `logging::fmt()`: the router, every SY
    node in Rust, the diagnostic binaries, and the IO, AI, gov and test nodes. SY.edge, whose
    filter reloads live and which built its own `fmt::layer()`, takes `logging::layer()`, the same
    output as a layer; `logging::ansi()` is no longer public, so no binary builds a layer of its
    own that skips the writer. The examples (`examples/`) built theirs with
    `tracing_subscriber::fmt()`, with colour even into a pipe, and take `logging::fmt()` now too.
    The only `tracing_subscriber::fmt()` left are four tests' captures (the router's stream,
    SY.admin, ai.generic and SY.frontdesk.gov).
  - **What it covers.** IO.cloud's `op`, with no change of IO.cloud's: it reaches 8.x with
    IO.cloud's runtime of 0.1.143, which carries this SDK. In rt-gateway's journal the other
    fields a node writes that the router's lines print as they come (a message's `meta.type` and
    `trace_id`, a claimed `routing.src`, the WAN link's errors; `coa/router-140`'s text of this
    entry) stay inside their record's line from 0.1.143, and no record is longer than 16 KiB.
  - **Its tests** (`crates/fluxbee_sdk/src/logging.rs`):
    `a_field_with_a_newline_and_a_forged_warn_stays_one_escaped_line` (IO.cloud's received line,
    its `op` a newline and the router stream's loss alarm at WARN, through what `fmt()` builds;
    with the record written as it came it is two lines, the second the forged alarm with
    `expired: 99999`), the message and a span's field, SY.edge's layered setup, a record past
    16 KiB, every control character, the colour codes, the cut after a whole piece, the 16 KiB
    boundary, a record written in pieces, and what the docs say a reader finds
    (`a_message_and_a_field_show_their_escapes_as_the_docs_say`, the review's: a message's escapes
    in tracing-subscriber's form and a field's in `Debug`'s, through `fmt()`, and on a terminal a
    field's colour code kept, its other escapes not). 11 of the 13 fail with the record written as
    it came; the other two pin what stays (a record with no control character, a writer that got
    nothing).
  - **Docs:** the SDK's README, `docs/reference/operations.md` (the journal's row) and
    `docs/nodes/rt-gateway.md` (what `lab/ops.py health` counts).
  - **Not covered:** the Go nodes (SY.timer, SY.wf-rules, SY.opa.rules, the WF engine) print
    through Go's `log`, their fields as they come (`log.Printf("... (trace_id=%s): %v", ...)` in
    SY.wf-rules' `dispatch.go`, a message's `trace_id`), and it escapes nothing. The same there
    would be a change of the Go SDK's of its own; not decided.
- **Found while building (b) (2026-11-12; read in the code, not tried), pending:**
  `lab/ops.py health`'s `usagerej` is the one count whose pattern is not anchored. It takes any
  line of SY.storage's journal that holds *usage records refused, mismatched or unverified*
  anywhere, and sums every `refused=`, `mismatched=` and `unverified=` after it; no newline is
  needed. SY.storage prints the `trace_id` of every message the router delivers to it as its sender
  wrote it (*"sy.storage received router message"*, `trace_id = %`), so a trace_id with those words
  and `refused=99999` fails the check. **Fix, later (`ops.py`'s next change):** anchor it as the
  router's patterns are, the words opening the line's message after its time, `WARN` and target.
  `docs/nodes/rt-gateway.md` says so meanwhile.

### A-268 [General/router] ✅ BUILT (0.1.135, 2026-11-11; its race by its unit test, no live trigger; A-265's check unchanged on 8.x, three runs: a new node on worker1 reached by name from ingress1 0.05 s, 0.15 s and 0.14 s after its connect, 0.33 s on 0.1.132) (found 2026-11-11 by the review of the router's 0.1.132; read in the code; there before it) — Two FIB builds at once can store the older one last

- **Why:** `rebuild_fib` (`src/router/mod.rs`) reads the nodes, the siblings, the static routes, the
  LSA snapshot and the reachability, and stores its FIB at the end under the FIB's lock; nothing
  orders two builds that run at once, so one that read before a change can be stored after the
  build that saw it. It is A-261's case, for the FIB.
- **How often:** on the hub the node loop rebuilds at every message from a local node, so builds
  overlap often; each takes under a millisecond, so a store out of order is rare.
- **Effect:** A-265's rebuild (an LSA applied) and the stale check's (0.1.132) can be undone until
  the next build, 5 s at most: a node just added is missing from the hub's FIB, or a stale hive's
  names stay in it.
- **Fix:** a generation per router, taken before the reads and compared under the FIB's lock, so
  only a newer build is stored; or a mutex taken first that serializes the builds, after checking
  that no caller holds their inputs.
- **Built (0.1.135):** the first option, a generation per build (`625413a4`). Its race has no live
  trigger; its unit test covers it
  (`a_fib_build_that_read_before_an_lsa_never_undoes_the_lsas_own`).
- **Checked on 8.x (0.1.135):** the rebuild path it guards, not the race (A-265's check, three
  runs): a node connecting on worker1 was reached by name from ingress1, the first message
  delivered 0.05 s, 0.15 s and 0.14 s after its connect (0.1.132: 0.33 s); the messages sent before
  the connect answered `NODE_NOT_FOUND` from ingress1's router, as expected.

### A-267 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.133 with 0.1.134's A-314 fix, run on 0.1.134, 2026-11-11; 20 self-restarts of the orchestrator on worker1 and egress1: no service started, no false line) (found 2026-11-11 by 0.1.131's deploy timeline; seen on 8.x, read in the code) — The watchdog's last tick, during the orchestrator's own stop, reads a healthy service as down and tries to start it

- **Seen:** worker1's old orchestrator logged *"service not active; attempting restart"* for
  `sy-opa-rules` at 06:27:42, the moment its own self-restart began, while sy-opa-rules had been
  active since 06:27:39 (`NRestarts=0`; no restart followed).
- **Why:** `systemd_is_active` (`src/bin/sy_orchestrator.rs` ~35638) takes any failure of its
  `systemctl is-active` child as "not active" (`unwrap_or(false)` and `success()`); the stop's
  SIGTERM reaches that child too, so the watchdog's tick in flight sees the service down and calls
  `systemd_start`.
- **Effect:** a false warning at each self-restart, and a start of a running service that can land
  if the stop does not kill that call first.
- **Fix:** active only on exit 0, inactive only on systemctl's "inactive" exit (3); a killed or
  failed check is unknown and starts nothing.
- **Since 0.1.134 (A-314):** exit 4, systemd's answer for a unit it does not have, reads as not
  running too; exit 1 and a killed check stay no answer. 0.1.133 read 4 as no answer, so on the
  hives' systemd 255 no node could be launched.
- **Checked on 8.x (0.1.133, with A-271, run on 0.1.134):** 20 self-restarts of `sy-orchestrator` at
  staggered offsets, debug on, on worker1 (23:47 to 23:48Z) and egress1 (23:48 to 23:50Z): no unit
  restarted or started meanwhile, and no *"service not active; attempting restart"* line. Stopped on
  worker1, `sy-opa-rules` came back in 3.3 s and `fluxbee-syncthing` in 3.5 s, each with the
  orchestrator's line.

### A-266 [General/SY.timer] ✅ VALIDATED ON 8.x (0.1.133, 2026-11-11; `limit` 1 gave `next_cursor`; pages of 7 listed 355 rows, all distinct) (found 2026-11-11 by 0.1.131's gate; seen on 8.x, read in the code) — `TIMER_LIST` says nothing of the timers its `limit` left out

- **Seen:** `GET /hives/motherbee/timer/timers?status_filter=all&limit=1` listed one of the
  motherbee's three timers and gave no `next_cursor`.
- **Why:** `listTimers` (`go/sy-timer/persistence.go` ~279) reads at most `limit` rows, and
  `respondTimerList` (`go/sy-timer/main.go` ~740-805) sets `next_cursor` only when a page passes its
  96 KiB (A-151). When the limit cuts the list, the answer reads as the whole list.
- **Effect:** Archi or the operator listing a hive's timers with a `limit` (100 by default) sees the
  first ones and no sign of the rest when they are small (worker1 holds 350 canceled ones); the Go
  SDK's `List`, the WF's reconcile included, stops at its own limit the same way.
- **Fix:** read one row more than the limit and, when it is there, answer the cursor of the last
  row listed.
- **Checked on 8.x (0.1.133):** `TIMER_LIST` with `limit` 1 answered `next_cursor`; pages of 7
  listed 355 rows in 51 pages, 355 distinct; one page of 1000 was cut by the frame at 80 rows, with
  `next_cursor`.

### A-265 [General/router] ✅ VALIDATED ON 8.x (0.1.132, 2026-11-11; a new node on worker1 reached by name from ingress1 0.33 s after its connect, 5.98 s on 0.1.131) (found 2026-11-11 by the review fixes of the router's A-195; read in the code) — The hub does not rebuild its FIB when it applies a spoke's LSA

- **Why:** `handle_wan_message`'s `Applied` arm (`src/router/mod.rs` ~3850-3875) calls
  `write_lsa_state`, which updates the LSA snapshot, but not `rebuild_fib`. A frame relayed by name
  (`resolve_by_name_inner` over the FIB, `FibSource::LsaNode`) to a node that just connected on a
  spoke gets `NODE_NOT_FOUND` from the hub until the next rebuild: a message of one of the hub's own
  nodes, or the 5 s loops. Frames by UUID read the snapshot and are not affected.
- **Effect:** since A-195 the other spokes learn of a new node at once, so this is the window left
  for traffic by name between spokes (up to 5 s).
- **Fix:** rebuild the FIB after `write_lsa_state` in that arm.
- **Built (0.1.132):** the `Applied` arm rebuilds the FIB after `write_lsa_state`, before the hub
  advertises that LSA's nodes; the stale check rebuilds it too when a hive's stale mark changes (the
  review), so a stale hive's names leave the FIB at once. A-268 is what is left (two builds at once).
- **Checked on 8.x (0.1.132):** a sender on ingress1 sending by name every 0.25 s from before a node
  connected on worker1: the first frame sent 0.33 s after the connect was delivered (0.1.131: 5.98 s);
  no `NODE_NOT_FOUND` came from the hub.

### A-264 [General/Go SDK] ✅ VALIDATED ON 8.x (0.1.141, 2026-11-13; a WF's 140 KiB text went as a blob (`content_ref`) and arrived in 0.134 s, its copy on the motherbee 11.7 s later, where 0.1.140 refused it `FRAME_TOO_LARGE`; 100 KiB and 40 KiB still inline, as the operator's call (b) has it) (found 2026-11-11 by A-253's verifier; read in the code) — The Go SDK has no text blob offload: a WF's text over 128 KiB fails, and one between about 96 and 128 KiB misses the turns

- **Why:** the Rust SDK puts any `text/v1` payload over about 62 KiB in a blob
  (`crates/fluxbee_sdk/src/send_normalization.rs`, `payload.rs`); the Go SDK only refuses a frame
  over 128 KiB (`go/fluxbee-go-sdk/client.go` ~208-229, `MessageTooLargeError`). wf-generic is the
  only Go producer of turns: SY.timer's messages are system ones, capped at 32 KiB.
- **Effect:** a WF's text over 128 KiB is never sent; one between about 96 and 128 KiB reaches its
  node but not `turns` or cognition (A-253's window). Large resolved `vars` in a WF's
  `meta.context` land in the window with a small text too.
- **Fix, later:** the Go SDK gets the Rust SDK's `text/v1` offload (the blob layout and its promote;
  check the WF nodes' access to the blob root). A Go SDK release, alone.
- **The operator (2026-11-11):** after the sweep (the Go SDK alone, with the next change there).
- **Built on `coa/gosdk-141` (the Go SDK alone, planned 0.1.141, with A-329):**
  - **A WF's text goes as a blob only when its message is over the frame (128 KiB),** the
    operator's call below; built first at the Rust SDK's threshold (a payload over 64 KiB with
    2,048 bytes for the rest of the frame, a text of about 62 KiB). `NodeSender.SendTraced`
    (`go/fluxbee-go-sdk/text_offload.go`, `offloadTextOverTheFrame`) measures the frame as it goes,
    with its trace entry and its writer's name. Over 128 KiB, where it refused the message before
    (`MessageTooLargeError`), the text of a text/v1 payload goes in a blob,
    `content_<the first 16 hex digits of its SHA-256>.txt`, `text/plain` (`text/html` or
    `image/svg+xml` when its first bytes are such a document, the Rust SDK's sniff), of the UTC
    day, and `content_ref` in its place, when that makes the frame fit. A message that fits a frame
    goes as it is, byte for byte (a `forward` stays byte-faithful), its text inline however long.
    The blob layout and its promote are the Rust SDK's (A-56): `staging/<prefix>/` written to an
    exclusive temporary file and renamed, then moved to `active/<prefix>/`, given to the owner of
    `active/` (the Syncthing user, so it reaches the other hives), 0640, every directory opened
    without following a link; a blob already in `active/` is left in place. Who reads it, and how,
    is below (the review). Unlike the Rust SDK, the payload keeps its other fields; only `content`
    becomes `content_ref`. A payload whose `content` is null or no string goes as it is, as in Rust
    (the review: a null one went as an empty blob, its ref of size 0). A message over the frame
    with the ref too (a large `meta.context`) writes no blob, none that no message names for
    Syncthing to copy to every hive (as built first, it wrote one), and is refused as before, with a
    log line; so is a message whose blob cannot be written.
  - **The WF nodes' access to the blob root:** a WF node runs as root, as every managed node (a
    transient `systemd-run` unit with no `User=` and no sandboxing, `src/bin/sy_orchestrator.rs`
    ~31671), so it writes `staging/` (root's) and gives a blob to the owner of `active/`, as the
    Rust nodes do; the root is the hive's `/var/lib/fluxbee/blob`, the Rust offload's fixed default
    (`DefaultBlobRoot`). The Go SDK takes `golang.org/x/sys` v0.23.0 for the descriptor-relative
    calls (the version SY.opa.rules builds with; SY.timer and the WF build with v0.42.0 already,
    SY.wf-rules adds it).
  - **By its tests, each checked failing without its fix:** `TestATextThatFitsTheFrameGoesInline`
    (a frame of exactly 128 KiB, its text of about 127 KiB, goes byte for byte and writes nothing,
    and one byte more goes as a blob; it fails at the first build's threshold),
    `TestATextOverTheFrameGoesAsABlob` (a text of 205 KiB sent as a ref, the other fields as they
    came; before, `MessageTooLargeError`), `TestAMessageOverTheFrameWithNoTextIsRefusedAsBefore`
    (its null `content` case since the review), `TestAMessageOverTheFrameWithItsRefTooWritesNoBlob`
    (it fails when the blob is written before the frame is measured, as built first),
    `TestActiveMarkupIsLabeledAsTheRustSDKLabelsIt`, `TestTheSameTextTwiceIsOneBlob`,
    `TestALinkInTheBlobTreeIsNeverFollowed` (on the Mac and in a Linux container, as root and as a
    user).
  - **What it does not cover, recorded:** a WF's text of about 95 to 128 KiB still reaches its node
    but not `turns` or cognition (A-253's window), as before 0.1.141: the call keeps it inline, so
    that it reaches its reader as before. The SDK measures the frame as it sends it: the router's
    additions (the sender's name and, when missing, the action class, about 64 bytes for
    `WF.demo@motherbee`; more with a longer name) still take a message that close to 128 KiB over
    the frame, and the router refuses it `UNREACHABLE` `FRAME_TOO_LARGE` (A-202), as before. A WF's
    long text to an agent still costs the memory package the router adds its oldest turns, or the
    whole package (from about 80 KiB with a full package; `serialize_with_package`, cognition §7),
    as before. A WF's large `meta.context` (its resolved `vars`) with a short text still takes the
    frame into the same window, and a payload that is not text is not offloaded (both as in the
    Rust SDK). A blob left in `staging/` by a crash between its write and its promote, or by a
    second copy of a blob already active, waits for the staging clean-up (24 h, A-241), as the Rust
    SDK's does.
  - **The review (2026-11-12) of the first build, read in the code: a text of about 62 to 128 KiB
    reached fewer readers than before.** Before 0.1.141 a WF's text in that band went inline and
    reached every reader; built first, it went as a `content_ref`, which only a reader whose hive
    holds the blob can read.
    The blob reaches another hive through Syncthing (`fluxbee-blob`, `active/` only, the motherbee
    and the workers; a worker to another through the motherbee), whose watcher scans a change 10 s
    after it (`fsWatcherDelayS`, `src/bin/sy_orchestrator.rs` ~16404; renames held up to 60 s were
    seen in A-19, and `promoteBlob` moves by rename), so the message comes first. The readers:
    - ai.generic and SY.frontdesk.gov read a `content_ref` at once, with no wait
      (`ModelInputOptions::default()`, `resolve_retry: None`,
      `nodes/ai/ai-generic/src/bin/ai_node_runner.rs` ~810,
      `crates/fluxbee_ai_sdk/src/text_payload.rs` ~34-43, 303-310): a blob on its way answers
      `BlobNotFound`;
    - io.slack and io.wapp read it with `resolve_text_v1_text_only_for_outbound`
      (`read_blob_with_retry`, the default `ResolveRetryConfig`, `BLOB_RETRY_MAX_MS` 5 s), and
      io.slack fails the whole post when it does not resolve (`nodes/io/io-slack/src/main.rs`
      ~3183-3197); `resolve_text_v1_for_outbound` has no caller outside `nodes/io/common`;
    - a WF reads no `content_ref` (no Go code resolves one): its guards and `$ref`s see the ref
      only, as with an agent's long answer (wf-orchestration-v1 §4.3);
    - io.api, io.web and io.linkedhelper read no `content_ref` at all.

    So a WF on a worker writing such a text to an IO node on the motherbee (the gates run their WFs
    on worker1, the IO.slack nodes run on the motherbee) or to an agent on another worker would
    lose it, where an engine before 0.1.141 delivered it; a reader on the WF's own hive reads it at
    once. The same holds today for the Rust SDK's offload of an agent's long answer
    (blob-annex-spec §6.5, §6.6), and, since the call, for a WF's text over the frame, which no
    reader got before. The gate measures the motherbee's wait for such a blob (A5).
  - **The operator's call (2026-11-12): (b), only past the frame (128 KiB).** The choices were
    (a) keep the Rust SDK's threshold, the band failing across hives as an agent's long answer
    does; (b) offload a WF's text only past A-253's window (about 96 KiB) or only past the frame,
    so that what reached a reader before still does; (c) the blob reaches the reader's hive before
    the reader needs it: the producer asks a sync and waits (the sync hint is SY.admin's alone
    today, blob-annex-spec §6.6), or each reader waits longer (the AI SDK's model input, the IO
    common code: their own releases). Built on `coa/gosdk-141` (above): what fit inline before
    still goes inline, and only a message over the frame, refused before, goes with its text as a
    blob. (c) stays a topic of its own: A-338.
  - **The review of the call's build (2026-11-12), read in the code:** no defect in the code (a
    frame of 128 KiB or less goes byte for byte as on 0.1.138, so old and new engines send the
    same frames during the deploy and no reader has to upgrade first). Its docs said a text that
    fits the frame reaches a reader on any hive, and the cognition reference that an inline text
    takes at most 64 KiB of an agent's frame: both now say the router's additions and the memory
    package's cut (above). (c) had no item of its own: A-338.
  - **Mixed versions (only during the deploy):** every WF node runs its package's base, wf.engine's
    `current`, and moves to a new one within a minute of its files reaching its hive, pinned or not
    (`docs/nodes/sy-orchestrator.md`, "A running node and a new runtime version"); the orchestrator
    runs no WF node outside a workflow package (`WF_RUNTIME_PACKAGE_REQUIRED`). So a WF still on
    the old engine, which keeps sending its long texts inline (refused over 128 KiB, as before),
    exists only until its hive's relaunch. The readers are unchanged: what each does with a
    `content_ref` is above.
- **Validated on 8.x (0.1.141's gate, gate0240-0243, 2026-11-13; `scratchpad/gate-tools/r141_gate.py`, git-ignored; 152 PASS, 19 SEE, 1 FAIL (a status read's 1.5 s bound), no model call):** on 0.1.140 a WF's 140 KiB text was refused (`message too large:
  143856 bytes (max 131072)`, the instance failed, nothing sent), 100 KiB and 40 KiB arrived inline
  (the 100 KiB turn missing from SY.storage's turns: A-253's window), and at the motherbee 70 KiB and
  100 KiB inline. On 0.1.141 the 140 KiB text went as a blob, a `content_ref` (`content_<hash>.txt`,
  143,360 bytes, owned as active/'s owner, mode 0640, nothing left in staging/), in a frame under
  4 KiB, received 0.134 s after the trigger, its turn in SY.storage as a `content_ref`, the
  motherbee's copy 11.7 s after the receipt; 100 KiB and 40 KiB still inline, byte for byte, no
  blob written. At the motherbee as the reader, 70 and 100 KiB inline and the 140 KiB `content_ref`,
  whose blob reached the motherbee 11.2 s after the message (A-338's window, recorded there).

### A-263 [General/admin] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; at its own deploy SY.admin marked each spoke settled 2.9 to 4.4 s after the spoke's own settle, the marks 9.7 s apart, the spokes' own settles 9.3 s; one read lost to a restart asked again at once) (found 2026-11-10 by A-250's builder; read in the code) — The installation update's follow reads one hive at a time

- **Why:** the update's background task reads the hives in turn, so a read lost to one spoke's
  self-restart (10 s since A-250's option (a)) delays the reads of the hives after it, and an
  operator's explicit run waits for the task's pass and its re-asks.
- **To evaluate with 0.1.131's deploy** (A-250's gate, the spokes' orchestrators restart): if the
  follow still lags the spokes' own settle, read the hives at once.
- **0.1.131's deploy:** the marks came one after the other, 3 to 8 s after each spoke was ready, with
  one lost read asked again at once: seconds now, so it stays low.
- **Built and shipped in 0.1.137 (`coa/admin-136`, `bb37ce46` on daily):** the update's background
  task reads every hive at once, each read under its own bound as before (10 s in a restart, 50 s
  outside it); its answers, the re-ask at once of a read lost in a restart, one read of a hive at a
  time and the order of its results stay. Found by its review: A-319.
- **Checked on 8.x (0.1.137's own deploy, 2026-11-12 09:48:57–09:49:39Z):** SY.admin marked worker1
  settled 3.3 s after its own settle, ingress1 4.4 s, egress1 2.9 s; the three marks 9.7 s apart,
  the spokes' own settles 9.3 s; one *"a read of a hive in its restart got no answer ... asked
  again at once"* line. The update, opened about 09:48:57Z, closed done about 09:49:39Z (42.3 s),
  every hive on the target (`ops timeline`).

### A-262 [General/router] ✅ BUILT (0.1.132, 2026-11-11, with A-195; by its tests) (found 2026-11-11 by the review of the router's A-195; read in the code; there before it) — The hub sends no advertisement when the set it vouches for to a spoke becomes empty

- `reachability_frame` returns nothing for an empty set (`src/router/mod.rs` ~2527): when the last
  node the hub vouched for to a spoke goes, or its hive goes stale, that spoke keeps the old
  entries until their 40 s expiry and routes to the hub, which answers `NODE_NOT_FOUND`. On 8.x
  every hive runs SY nodes, so the set is never empty.
- **Fix:** send the empty advertisement to a spoke that had entries.

### A-261 [General/router] ✅ BUILT (0.1.132, 2026-11-11, with A-195: the reads under the locks; by its tests) (found 2026-11-11 by the review of the router's A-195; read in the code; there before it) — The hub's LSA snapshot can briefly go back to an older copy

- **Why:** `refresh_lsa` (`src/router/mod.rs` ~6575-6580) reads the LSA region and stores the copy
  later under the snapshot's lock, so a write in between is undone until the next refresh; and
  `write_lsa_state` (~2976-2986) takes the writer's lock only after it read `lsa_state`, so two
  writes at once can store the older read last, until the next write (up to 10 s).
- **Effect:** a node that just connected on one spoke and sends at once to another can have its
  first frame dropped on the hub as `foreign_source` (A-195's symptom, moved to the hub); a far
  answer for it can be missed the same way. It needs a thread descheduled at that moment: rare.
- **Fix:** `refresh_lsa` stores a read only if its heartbeat is not older than the stored one's;
  `write_lsa_state` takes the writer's lock before it reads.

### A-260 [General/admin] ✅ VALIDATED ON 8.x (0.1.133, run on 0.1.134, 2026-11-11; with the edge stopped 48 s, the sweep deferred one retire and retired it at its next pass, and take-downs not waiting on the edge answered in 1.4 s) (found 2026-11-10 by A-242's builder; read in the code; there before it) — The expiry sweep holds the publication lock while an edge or IO.blob does not answer

- **Why:** `retire_expired_publication` (`src/bin/sy_admin.rs` ~7731) holds `publication_lock`
  across `unpublish_edge_blob` (a 30 s bound) and `release_curated_blob`. A-242 removed only the
  case of an edge whose hive is gone.
- **Effect:** an edge whose hive is listed but down, or a slow IO.blob, holds the lock about 30 s
  per page due, every 10 minutes: a `publish_artifact` waiting meanwhile answers `TIMEOUT` past its
  110 s, and take-downs (Archi's, the factory reset's) queue.
- **Fix:** the edge's and IO.blob's calls outside the lock (copy the record, call, then lock again,
  reload and check before writing each state), or a much shorter bound for the sweep's calls.
- **Checked on 8.x (0.1.133, with A-274, run on 0.1.134):** 5 pages published as
  `AI.gate0233@worker1` (from 23:34Z, 13.5 to 16.3 s each); with `SY.edge@ingress1` SIGSTOPped
  48 s, take-downs of an unknown page answered `NOT_FOUND` in 1.4 s, twice, while two take-downs
  waited on the edge, and the expiry sweep deferred one retire on the edge's timeout and retired it
  at its next pass. At the end all five pages were expired or taken down, and released.

### A-259 [General/SY.timer] ✅ VALIDATED ON 8.x (0.1.133, 2026-11-11; canceled timers never fired, moved ones fired once, a canceled recurring one stopped) (found 2026-11-10 by A-244's review; read in the code; there before it) — SY.timer's scheduler keeps a canceled, moved or purged timer's entry until its old fire time

- **Why:** nothing removes a heap entry: the only calls are `heap.Push` and `heap.Pop`
  (`go/sy-timer/scheduler.go` ~599, ~613). `TIMER_CANCEL` only signals the scheduler (`main.go`
  ~864), `TIMER_RESCHEDULE` pushes a second entry (~914), and an owner's purge deletes rows and
  signals (~952). A stale entry leaves when it pops, at the cost of a lock and a read.
- **Effect:** the pending caps count pending rows only, so a node that loops `TIMER_SCHEDULE` (up
  to 366 days) and `TIMER_CANCEL` is never refused: about 100 B of heap per pair until its fire time
  or the next start, and each canceled row stays in `timers.db` (up to 64 KiB with its payload and
  metadata) for 7 days from its cancel. History, fired or canceled, has no cap.
- **Also:** a start whose count of pending timers fails prints `pending=0` in its ready line, next
  to a separate error line.
- **Fix:** a map from a timer's uuid to its heap entry (`timerHeapEntry.index` is kept already) and
  `heap.Remove` on cancel, reschedule and purge.
- **Checked on 8.x (0.1.133, against a regression):** two one-shots per hive scheduled 15 and 16 min
  ahead before the deploy were loaded again at SY.timer's restart; the ones canceled after the
  deploy never fired, the others fired once on time (the motherbee 22:49:17Z, 15 ms late; worker1
  22:49:34Z, 497 ms late). On worker1, a timer moved earlier (6 to 2 min) and one moved later (2 to
  4 min) fired once each, the canceled one never, and a recurring one fired 5 times and not in the
  75 s after its cancel; no `timer_fire_dropped` line.

### A-258 [General/SY.timer] ✅ VALIDATED ON 8.x (0.1.133, 2026-11-11; SY.timer 1.6: 12 refusals gave 10 lines and a summary, no cursor text) (found 2026-11-10 by A-254's triage; read in the code) — SY.timer's refusals leave no line in its own log

- A request SY.timer refuses (`TIMER_BELOW_MINIMUM`, `TIMER_FORBIDDEN`, `TIMER_LIMIT_EXCEEDED`, …) is
  answered to its sender, and SY.timer's journal says nothing: the operator cannot see there who
  was refused and why. The operator's call on A-253 (2026-11-10): logs for everything.
- **Fix:** one line per refusal (the verb, the code, the sender), bounded per sender.
- **Checked on 8.x (0.1.133):** SY.timer 1.6 on the motherbee and worker1 (`TIMER_HELP`; its start
  line `pending=2`, the replay's timers). Through SY.admin's routes, 12 bad cursors gave 10 refusal
  lines and one summary (*"timer_request_refusals_not_logged"*), no cursor text in any line, on both
  hives. From a throwaway client on worker1, 12 schedules below the minimum gave 10 refusal lines
  and a summary, and 12 stray `TIMER_FIRED_RESPONSE` 10 *"timer_message_ignored"* lines and a
  summary.

### A-257 [General/frontdesk] ✅ VALIDATED ON 8.x (0.1.136, 2026-11-12; with the 4 workers of AI.pruebas and of the frontdesk held, a handoff answered in 4 to 6 ms, status and help in 0.866 to 0.924 s, `CONFIG_GET` in 0.328 to 0.377 s, and a turn past the 128 waiting `ai_runtime_busy` in 7 to 12 ms, where on 0.1.135 the handoff got no answer, the status was the orchestrator's inference at 2.374 to 2.449 s, help null at 3.356 s and `CONFIG_GET` a `TIMEOUT` at 25.158 to 25.168 s) (found 2026-11-10 by A-223's triage and io.api's builder; read in the code) — The frontdesk's handoff waits behind its LLM turns

- **Why:** SY.frontdesk.gov runs the AI SDK's default pool of 4 workers
  (`RuntimeConfig::default()`, `nodes/ai/ai-generic/src/bin/ai_node_runner.rs` ~3121;
  `crates/fluxbee_ai_sdk/src/runtime.rs:72`). With 4 LLM turns in flight its dispatch loop waits on
  `join_next` (~250-269), and a handoff waits behind them.
- **Effect:** io.api gives up at its 10 s with `frontdesk_unavailable` while the frontdesk may still
  register the person afterwards (A-223's symptom, from another cause). `NODE_STATUS_GET` and
  `CONFIG_*` to the frontdesk wait in the same queue.
- **Fix, later:** control traffic and the handoff not behind the LLM workers (the AI SDK: shared
  code, alone).
- **Correction (A-257's builder, 2026-11-11):** the frontdesk's runner is
  `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs` (its `RuntimeConfig` at ~3121), not
  ai.generic's.
- **Built (0.1.136, the AI SDK alone; `coa/aisdk-135`, rebased as `coa/aisdk-136`):** two lanes
  (`b694af08`); its review (`bb607205`): a handoff runs beside the workers only in the shape the
  frontdesk parses, within 30 s and on at most 8 of the direct lane's 64 places; the texts of the
  branch's review (`ba2addb2`). **For the frontdesk's own release:** an `AiNode` method by which
  the node says what it answers without its model (default system and admin only; the
  frontdesk's override is `parse_frontdesk_handoff` and its sender rule); then the SDK's copy of
  the handoff's shape, its gov-common dev-dependency and `tests/frontdesk_handoff_contract.rs` go.
- **For the AI SDK's next change (A-292's review, 2026-11-11):** pin the busy answer's exact
  shape: export `ai_runtime_busy` and the `busy` act (or a recognizer) for cognition's
  `is_busy_answer`, or an SDK test that the busy reply's payload has exactly `type`, `code`,
  `message` and `retryable` and that the trace's last entry is the agent's with act `busy`.
  Without it, a new field in the busy answer silently turns A-292 off with every test green. Also:
  `run_direct_message`'s comment (`crates/fluxbee_ai_sdk/src/runtime.rs` ~777-781) and
  `docs/nodes/sy-frontdesk-gov.md` say IO.cloud takes any error answer to a handoff for
  `FRONTDESK_REJECTED`, which is stale once 0.1.137 (A-293) ships: update both when 0.1.137 is
  merged (0.1.136 is), and ship the follow-up (answer `ai_runtime_busy` instead of dropping) only
  after 0.1.137 runs on every motherbee.
- **A known limit (the review of `coa/aisdk-136`, `ba2addb2`):** the handoff rule is the runtime's,
  the same for every node on it, ai.generic too: ai.generic has no handoff path, so a well-formed
  handoff from an IO node with a thread, sent to an agent by mistake (io.api's `frontdesk_target`
  may name any node), runs its model on the direct lane, up to 8 at once beside the 4 workers, each
  cut at 30 s. The AI SDK's README says so. The `AiNode` method planned above for the frontdesk's
  own release closes it: by default a node answers only `system` and `admin` messages without its
  model, which ai.generic keeps, and only the frontdesk's override adds its handoff.
- **Checked on 8.x (0.1.136, the gate gate0236, against a baseline on 0.1.135 the same night):**
  the 4 workers of `AI.pruebas@worker1` and of `SY.frontdesk.gov@motherbee` held by an nft drop of
  each node's own HTTPS (each held turn 92-93 s, then the node's fixed provider-failure text), the
  fillers user messages with no `thread_id`. On 0.1.135 AI.pruebas' status took 2.449 s
  (`ORCHESTRATOR_INFERRED`), its help 3.356 s (null) and its `CONFIG_GET` 25.168 s (`504
  TIMEOUT`); the frontdesk's status 2.374 s (inferred) and `CONFIG_GET` 25.158 s (`TIMEOUT`); a
  handoff got no answer. On 0.1.136 (held 05:43:51-05:45:23Z), with 128 turns waiting at each:
  AI.pruebas' status 0.924 s (`NODE_REPORTED`), help 0.920 s, `CONFIG_GET` 0.328 s and a
  `CONFIG_SET` of its own config 0.091 s; the frontdesk's status 0.866 s and `CONFIG_GET` 0.377 s;
  a well-formed handoff 6 ms and the handoff probe 4 ms. The turns past the 128 answered
  `ai_runtime_busy` at once: AI.pruebas' 7 in 7-12 ms, `retryable` true, the trace's last act
  `busy`, its journal naming the refusal 5 ms after the first send; the frontdesk's one in 7 ms.
  The frontdesk took a malformed handoff (`subject.phone` a number) as a turn. Once the held
  turns answered, the waiting ones drained within 0.135 s (AI.pruebas' 128 fillers) and 0.165 s
  (the frontdesk's 127); `direct_dropped=0` on both, `busy_answers_dropped=0` on AI.pruebas, no
  restart; 2 model calls in all.
- **At 0.1.137's integration (`7423878d`, on daily `6fbe98ab`), texts only:** the AI SDK's README
  and `run_direct_message`'s comment say that an IO.cloud before 0.1.137 reads any error answer to
  a handoff as `FRONTDESK_REJECTED`, and that from 0.1.137 a retryable one is
  `FRONTDESK_UNAVAILABLE` (a comment and the README, no code); `docs/nodes/sy-frontdesk-gov.md`
  already read right. The follow-up (answer `ai_runtime_busy` instead of dropping) is left to the
  AI SDK's next change; 0.1.137 runs on 8.x's motherbee since 2026-11-12.

### A-256 [IO/io.cloud] 🔵 low (found 2026-11-10 by A-91's builder; read in the code) — io.cloud accepts `io.inbound_family` and ignores it

- The contract (`nodes/io/common/src/io_cloud_adapter_config.rs`) accepts and stores the optional
  `io.inbound_family`; io.cloud uses the fixed `"user"` (`CLOUD_INBOUND_FAMILY`). A `CONFIG_SET`
  of it is accepted and does nothing.
- **Fix:** with A-149 (io-common), use it or drop it from the contract.
- **Also (A-255's builder, 2026-11-11):** `publish_cloud_endpoint`'s record (`set_node_config` of
  `{io: {edge_node}}`) replaces config.json's whole `io` object, dropping `io.inbound_family`, and
  each rotation bumps the file's `_system.config_version` while the live one stays; nothing compares
  the two today. Harmless while IO.cloud reads no other `io` field.

### A-255 [IO/io.cloud] ✅ BUILT (0.1.133, 2026-11-11; on 8.x a rotation on the edge IO.cloud already trusted answered `edge_applied: true` in 1.9 s, no restart; a new edge's apply by its tests) (found 2026-11-10 by A-91's builder; confirmed in the code by its review; there before it) — A running IO.cloud trusts the edge `publish_cloud_endpoint` records only after its restart

- **Why:** `publish_cloud_endpoint` records the edge with `set_node_config`
  (`src/bin/sy_admin.rs`, `handle_publish_cloud_endpoint` ~7846-7856); the orchestrator's
  `set_node_config_flow` only writes `config.json` (`src/bin/sy_orchestrator.rs` ~27285) and sends
  the node no `CONFIG_SET`; io.cloud reads `config.json` only when it starts.
- **Effect:** after the first publish on a fresh install, or a move to another edge, Cloud's
  requests time out at the edge (504) until IO.cloud restarts; publishing again only mints a new
  token. On 8.x every .deb relaunch hid it. Since A-91 (0.1.131) IO.cloud's status names
  `missing_edge_node`, each dropped request logs a warn line, and the texts tell the operator to
  restart IO.cloud after a publish.
- **Fix:** SY.admin applies the recorded edge live (a `CONFIG_SET` to IO.cloud at its live
  `config_version` + 1, or its restart), and the restart steps written in 0.1.131 come out.
- **The operator's call (2026-11-11):** checked live in 0.1.133's gate: the new token goes to a
  0600 file whose path the operator gets, and the operator passes it to the Cloud team.
- **Checked on 8.x (0.1.133, live, the operator's call):** a rotation on the edge IO.cloud already
  trusted. `publish_cloud_endpoint` on `SY.edge@ingress1` at 23:20:20Z answered ok in 1.9 s with
  `edge_applied: true`; `IO.cloud@motherbee` RUNNING HEALTHY before and after, its recorded
  `io.edge_node` `SY.edge@ingress1` and its recorded config's `config_version` 3 to 4 (the record
  bumps the file at each rotation, A-256), its unit's start and PID unchanged (no restart). The new
  token (64 characters) and url (43) went to a 0600 file, never printed; the operator was given its
  path. The previous token worked 10 more minutes (until about 23:30Z).
- **Not seen live:** the bug's own case, a first publish or a move to another edge. On a rotation
  IO.cloud's live config names the edge already, so `cloud_edge_config_set` finds it trusted and
  SY.admin sends no `CONFIG_SET` (`AlreadyTrusted`); 0.1.132 would have answered the same but for
  `edge_applied`. The apply is covered by its tests
  (`the_published_edge_is_applied_to_the_running_io_cloud`, and
  `a_rotation_on_the_same_edge_sends_io_cloud_no_config_set` for this path).

### A-254 [WF] ✅ VALIDATED ON 8.x (0.1.131, 2026-11-11; a timer refused while SY.timer was down went again 6 s later and fired on time; fire_in 400d a COMPILE_ERROR) (found 2026-11-10 by the Go SDK's 0.1.130 builder; read in the code, not run) — A WF can lose a timer schedule without knowing it

- **Why:** wf-generic's `SDKTimerSender.sendTimerRequest` (`go/nodes/wf/wf-generic/node/actions.go`
  ~785-799) sends `schedule_timer` without waiting for an answer, and the timer is recorded in the
  WF's index right after (~550). A schedule the router refuses (`LINK_BUSY` since 0.1.129, or
  `NODE_NOT_FOUND` while SY.timer restarts) comes back as a notice the WF drops, and SY.timer's own
  refusal (`TIMER_RESPONSE` with `ok: false`) is dropped as a system message.
- **Effect:** the index keeps a timer SY.timer does not have; while the node is healthy nothing
  notices it until the node's next start (its reconcile with `TIMER_LIST`). The spec already says
  cancel and reschedule work this way; `LINK_BUSY` makes a lost schedule likelier under load.
- **Fix, later (WF):** the schedule waits for SY.timer's answer (an RPC), or a refusal of a schedule
  the WF sent marks that timer in its index and retries it.
- **The operator's call (2026-11-10, the sweep):** *"si"*, option (b): the WF records a refused schedule and tries again, without blocking, plus the 366-day bound; a Go release (wf.engine, SY.wf-rules).
- **Validated (0.1.131, 2026-11-11):** `WF.gate0231@worker1` started an instance as SY.timer@worker1
  was stopped: *"not taken by SY.timer ... UNREACHABLE reason=NODE_NOT_FOUND"*, DEGRADED
  `timers_not_reconciled`; the watchdog brought SY.timer back in 2 s, the WF sent the timer again
  6 s after the refusal and SY.timer took it (HEALTHY again at 7 s); it fired at its 2 minutes and
  the instance ended `done`. A move to `1m`, the floor, was taken as a delay (+51 s). SY.wf-rules'
  check refused `fire_in` 400d naming its path, and took 366d. No definition on 8.x went past 366
  days.

### A-253 [General/storage] ✅ VALIDATED ON 8.x (0.1.132, 2026-11-11; the drop kept, each one named in the router's journal, a forged `thread_id` escaped and cut) (found 2026-11-10 by 0.1.129's gate; seen on 8.x, the same on 0.1.128) — A user message between 96 KiB and the frame's 128 KiB reaches its node but not SY.storage's turns

- **Seen:** the gate's test messages of 100 KB were delivered, and each router logged *"router
  stream lost records in the last 30 s: oversize: 80"* for their copies to SY.storage.
- **Why:** the router stream drops a record over its batch's 96 KiB (`STREAM_BATCH_MAX_BYTES`,
  `src/router/stream.rs:42`), by design (docs/router-stream-v1.md, "the alarm"), while a frame may
  be up to 128 KiB: a person's message in between is not in `turns`, so the conversation's record
  and cognition miss it.
- **To decide, later:** keep it (such messages are rare; attachments go through the blob), store
  the turn truncated with a mark, or raise the batch's bound to the frame's.
- **The operator's call (2026-11-10, the sweep):** keep the drop, but *"hay que logear eso en algun lado.. imagino el log del propio nodo, para todo tenemos que tener logs"*: the router's own line names the dropped record (its trace, thread and size), with the router's release. Logs for everything are a topic of their own, later.
- **Built (0.1.132):** the drop stays; each dropped record is named in the router's journal
  (*"router stream dropped a record too big for a batch; its sink never gets it"*) with its stream,
  sink, subject, producer, `trace_id` and `thread_id` (quoted, escaped, each cut at 128 bytes with
  its full length), sequence and size, at most 10 between two alarm lines; the alarm adds *"router
  stream did not name N records too big for a batch…"* for the rest. The stream's other drops
  (queue full, cap, expired, refused) stay counted only, until the talk on logs.
- **Checked on 8.x (0.1.132):** on worker1, a 100 KB message named once (bytes 100,312, max_bytes
  97,151); one whose `thread_id` held 6 KB starting with a newline and the alarm's words, named in
  one line of 494 bytes, escaped and cut (`... (6062 bytes)`), and no line starting with the alarm's
  words; 15 at once: 10 named and *"did not name 5"*; `ops.py health` gave `streamlost=17`.

### A-252 [General/join] ✅ VALIDATED ON 8.x (0.1.131, 2026-11-11; a throwaway worker's Syncthing started 3 times, not 4, switched at its join's link) (found 2026-11-10 by 0.1.127's gate; seen on 8.x) — A fresh worker restarts Syncthing once more about a minute after its join, for the posture options

- **Seen:** the throwaway worker `gate0227` joined at 18:23:56 and was done at 18:26:18; at
  18:28:04 its orchestrator's first peer reconcile logged *"switched off public discovery, relays
  and NAT; listening on loopback"* and restarted Syncthing, its fourth start since the join.
- **Why:** the join leaves Syncthing's own defaults for those options, and the host posture's
  reconcile (A-47, A-50) sets them at its first round.
- **Fix, later (with A-193's join timings):** the join writes the posture's options with the rest
  of its config, so the reconcile finds nothing to change.
- **Validated (0.1.131, 2026-11-11):** `gate0231` (VM 104) joined in about 140 s; its orchestrator
  logged the switch inside the finalize, next to the link's one restart; Syncthing started 3 times
  (gate0227's 4), no reconcile restarted it in the next 4 minutes, and no restart marker was left.
  Removed in 3.0 s; VM 104 destroyed.

### A-251 [General/admin] ✅ VALIDATED ON 8.x (0.1.131, 2026-11-11; 400 TIMER_INVALID_PAYLOAD naming no value, 404 TIMER_NOT_FOUND, 400 TIMER_INVALID_TIMEZONE) (found 2026-11-10 by 0.1.127's gate; seen on 8.x) — SY.admin answers SY.timer's refusal of a bad cursor with HTTP 500

- **Seen:** `GET /hives/motherbee/timer/timers?cursor=<not one>` answered HTTP 500
  `TIMER_INVALID_PAYLOAD` (the refusal itself is right, and it names no value since 0.1.127).
- **Why:** SY.admin's map from an error code to an HTTP status does not name `TIMER_INVALID_PAYLOAD`,
  so it falls to 500; a client's bad request should be a 400.
- **Fix, later (SY.admin):** the map takes SY.timer's request errors as 400.
- **Validated (0.1.131, 2026-11-11):** a bad cursor answers 400 `TIMER_INVALID_PAYLOAD` without the
  value, an unknown uuid 404 `TIMER_NOT_FOUND`, a bad zone 400 `TIMER_INVALID_TIMEZONE`. Paging with
  `limit` showed A-266.

### A-250 [General/admin] ✅ (a) VALIDATED ON 8.x (0.1.131, 2026-11-11; the update closed about 38 s after its call, each spoke marked 3 to 8 s after it was ready) · (b) for a later stage (found 2026-11-10 by 0.1.127's deploy; seen on 8.x) — The installation update's follow loses a read to each spoke's self-restart and sees the hive settled 40 to 85 s late

- **Seen (0.1.127's deploy, `ops timeline`, t = 18:12:00):** the update was opened at 21.7 s; the
  spokes settled by their own records at 47.3 s (egress1), 57.5 s (ingress1) and 61.1 s (worker1);
  SY.admin marked them settled at 96.9 s, 95.7 s and 146.6 s, and closed the update at 147.9 s.
  Not every deploy: 0.1.125's update took 131 s, 0.1.126's 44 s (no read lost).
- **Why:** SY.admin's reads of a spoke reached its orchestrator while the old process was about to
  restart itself for the new binary (egress1's at 18:12:39, worker1's at 18:12:53); the old
  process exited without answering, and SY.admin waited out its 50 s read bound, then held the
  hive unread for another 50 s (worker1: two such waits). The motherbee's orchestrator, already on
  0.1.127, served every read meanwhile (A-199's step 1 worked: no read waited behind another).
- **Seen again in 0.1.128's deploy (t = 19:40:20):** egress1's orchestrator stopped at 35.2 s and
  was back on its router at 40.4 s, ingress1's 39.7 to 43.9 s, worker1's 42.8 to 47.2 s; each
  settled within a second of coming back. SY.admin marked worker1 at 83.6 s, egress1 at 85.3 s and
  ingress1 at 134.9 s (two waits). The spokes ran 0.1.127's reads apart this time, so the loss is
  not a queue: a read that reaches a spoke's orchestrator in its few seconds off the router, or
  just before it stops, gets no answer, and SY.admin waits its 50 s.
- **The operator's call (2026-11-10):** *"a-250 por ahora a), lo correcto es b) pero costo/beneficio
  actual no vale, igual deja documentado porque vamos a volver a esto en otra etapa"*. Now: (a), SY.admin's
  follow uses a short read bound while a hive restarts and asks again at once. Later, in another
  stage: (b), the spoke tells SY.admin when it settles (the datum's owner reports it), recorded as
  the right design in docs/hive-config-update-v1.md.
- **0.1.130's deploy (2026-11-10), the counter-case:** no orchestrator binary changed, so no
  spoke's orchestrator restarted itself, and the update closed 6 s after its call: the lag comes
  from the self-restart window and nothing else.
- **Before 0.1.128's deploy, the plan was to evaluate:** the spokes now run 0.1.127, whose reads run apart, so a
  read that reaches a spoke before its self-restart is answered unless it arrives in the last
  moment. If the lag stays, the options: the follow uses a short read bound while a hive is
  restarting; the spoke tells SY.admin when it settles; or a forward fails at once when its target
  orchestrator reconnects (A-195). A design call for the operator.
- **Validated, option (a) (0.1.131, 2026-11-11):** every spoke's orchestrator restarted itself for the
  new binary. The spokes were ready at 201.1 s (ingress1), 201.5 s (egress1) and 208.4 s (worker1)
  of the deploy's timeline; SY.admin marked them settled at 208.9 s, 207.7 s and 211.4 s, with one
  read lost to a restart asked again at once, and closed the update at 212.0 s: about 38 s after
  the call, where 0.1.127's took 148 s and 0.1.128's 135 s. The marks came one after the other
  (A-263). The review's fix rides along: after `add_hive` or `remove_hive` the update looks at the
  payload's `hive_id`, not the command's target (Archi's own hive).

### A-249 [General/admin] ✅ CLOSED (0.1.131, 2026-11-11; both cases moot with A-242: no purge count and no refusal for a publish in flight) (found 2026-11-10 by the builder of A-233's review fixes; read in the code) — A-233's in-flight guards leave two narrow cases

- **A kill SY.admin stops waiting for:** SY.admin waits 180 s for a kill with purge; past that it
  counts the purge out while the orchestrator may still be working. A publish already queued
  from that node could then start before the orchestrator's `teardown_node` counts its purge in:
  the half-way teardown of A-233's race again, in this case only.
- **The factory reset's step [3]:** a node that died by itself in the middle of a publish has its
  ILK's `purge_ilk` refused as in flight; the reset retries it only across its normal passes, so
  it may end as a counted problem with the ILK marked. Running the reset again finishes it.
- **Also:** the reset recognises a refusal for a publish in flight by the text "is in flight",
  because SY.wf-rules passes SY.admin's code on only inside its detail (A-220's lesson: a code read
  from a text breaks when the text changes).
- **Fix, later:** the purge count held until the orchestrator's answer whatever SY.admin's wait;
  SY.wf-rules passing SY.admin's `error_code` on as its own.

### A-248 [General/AI SDK] ✅ VALIDATED ON 8.x (0.1.128, 2026-11-10; one answer from AI.pruebas, none from the tap target, `tap_copies_ignored=1`) (found 2026-11-10 by the router's 0.1.129 builder; read in the code, not live) — An AI node used as a tap target answers the tapped message's sender

- **Why:** `build_reply_meta` (`crates/fluxbee_ai_sdk/src/message.rs:97`) copies `meta.via_tap` into
  a reply, and `build_reply_routing` addresses `msg.routing.src`. An AI.* node that receives a tap
  copy answers it like any message: its reply goes to the tapped message's original sender, under
  the copy's trace_id. That sender may be an IO node, which could pass the reply on to a person.
- **Fix, with the SDK's next release:** an AI node does not answer a tap copy (it is there to be
  observed), and a reply never carries `via_tap`. Since 0.1.129 the router clears a `via_tap` a
  node sets itself, so such a reply is at least tapped and copied to cognition.
- **Built for 0.1.128 (`coa/sdk-128`, reviewed):** the AI runtime hands a tap copy to no handler and
  answers nothing; a reply never carries `via_tap`; the copies it ignores are counted in its
  metrics line. **As built, an AI node used as a tap target takes no part at all** (no memory, no
  tools, no model): an AI observer of tapped traffic would be a feature of its own. IO and WF nodes
  still act on a copy they receive, which is what a tap target such as an audit IO node is for.

### A-247 [General/router] ✅ BUILT (0.1.132, 2026-11-11), closed into A-208 (found 2026-11-10 by the router's 0.1.129 builder; read in the code) — A sibling router's TTL and UNREACHABLE answers go to the forward target instead of the sender

- **Why:** in `handle_peer_message`'s `ResolvedRoute::ForwardRouter(peer_uuid)` arms
  (`src/router/mod.rs` ~4883 and ~5033 on `coa/router-129`), the arm's `peer_uuid` shadows the
  parameter that names the sibling the message came from, so a `TTL_EXCEEDED` or an `UNREACHABLE`
  goes to the forward target's link, or nowhere. Only a hive with several routers gets there,
  which is not supported.
- **Fix, later (if multi-router hives come):** name the arm's binding apart.
- **The operator's call (2026-11-10, the sweep):** *"si"*: with the router's next release (A-208's change touches the same lines).
- **Built (0.1.132), closed into A-208:** both `ForwardRouter` arms bind `next_router`, check the
  TTL first and answer over the link the frame came from.

### A-246 [General/router] ✅ BUILT (0.1.132, 2026-11-11; the gateway's answer unchanged on 8.x; the several-routers case by its tests) (found 2026-11-10 by the review of the router's 0.1.129; read in the code) — A non-gateway router with no gateway sibling answers a broadcast twice

- **Why:** `forward_broadcast_to_wan` answers `GATEWAY_UNAVAILABLE` itself (`src/router/mod.rs`
  2164-2166 on `coa/router-129`), and the broadcast's sender is answered `LINK_BUSY` as well (1461)
  when one of its links was full. Only a hive with several routers gets there, which is not
  supported.
- **Fix, later (if multi-router hives come):** `forward_broadcast_to_wan` returns the reason and its
  caller answers once.
- **The operator's call (2026-11-10, the sweep):** *"si"*: fixed with the router's next release, with A-247 and A-208.
- **Built (0.1.132):** `forward_broadcast_to_wan` returns its reason; the router a broadcast
  entered answers once (`GATEWAY_UNAVAILABLE` first, then local delivery's reason, then
  `LINK_BUSY`), and a router a broadcast reaches over IRP answers nothing. A gateway's answer is
  unchanged (A-222's tests).
- **Checked on 8.x (0.1.132):** the gateway's path, as on 0.1.131: with one target's link full, one
  `LINK_BUSY` from the motherbee's router and the broadcast once to each other target; with none full,
  no answer and once to each.

### A-245 [General/router] ✅ VALIDATED ON 8.x (0.1.132, 2026-11-11; worker1's 1,536 `LINK_BUSY` all reached a sender on ingress1, none on 0.1.131) (found 2026-11-10 by the review of the router's 0.1.129; read in the code; there before it) — A far router's UNREACHABLE reaches only the hub's own nodes

- **Why:** the hub relays an `UNREACHABLE` that comes over the WAN only to its own nodes
  (`src/router/mod.rs` 3674-3688 on `coa/router-129`). A message from an ingress1 node to a worker1
  node that worker1 refuses (`NODE_NOT_FOUND`, and `LINK_BUSY` since A-222) never reaches its
  sender, which waits out its own timeout.
- **Also:** a sender that counts no answer as delivered (SY.timer's 30 s rule) counts such a
  refusal as delivered, when the sender and the target are on two spokes.
- **Fix, later (the router):** the hub relays a far `UNREACHABLE` on toward the sender's hive.
- **Built (0.1.132):** the hub hands a far `UNREACHABLE` or `TTL_EXCEEDED` on toward the spoke
  whose LSA lists the sender, one hop of TTL less, only when the adjacent router made it, never back
  where it came from and never twice; an answer it does not hand on is counted and logged with its
  reason, at most once every 30 s.
- **Checked on 8.x (0.1.132):** a client on worker1 that read nothing, a sender on ingress1 with 2,600
  messages: the receiver took 1,064 and worker1's router answered the other 1,536 `LINK_BUSY`, all of
  which reached the sender through the hub; on 0.1.131 the same run gave the sender no answer.

### A-244 [General/SY.timer] ✅ BUILT (0.1.131, 2026-11-11; a timer's whole life unchanged on 8.x, the leak proven by its tests) (found 2026-11-10 by A-220's builder; read in the code) — SY.timer's per-timer lock map never drops an entry

- **Why:** `timerLock` (`go/sy-timer/scheduler.go` ~585) adds a small mutex per timer that ever fired,
  and since A-220's race fix per timer canceled or rescheduled, and never removes it, for the
  process's life (about 100 bytes each).
- **Fix, later:** reference-counted entries, or drop a timer's lock when the timer leaves the store.
- **Built (0.1.131, 2026-11-11):** `lockTimer` drops a timer's entry with its last holder. On 8.x a
  throwaway owner scheduled, moved, canceled twice and let one fire, all as before.

### A-243 [General/admin] ✅ VALIDATED ON 8.x (0.1.137, 2026-11-12; an IO node took its own page down in 0.232 s, its link then 404; an IO and a WF node were refused another's page `UNAUTHORIZED`; a WF node's take-down of an unknown page reached SY.admin past the A-62 table, `NOT_FOUND` in 0.057 s; the Cloud's list and take-down wait for its track) (found 2026-11-10 by A-233's builder; read in the code) — Who can take a page down: IO and WF nodes publish but cannot unpublish, and the Cloud can neither list nor unpublish

- **Why:** IO.*, WF.* and AI.* nodes may publish a page; over the mesh only AI.* may unpublish one
  (`src/bin/sy_admin.rs` ~5824-5825 on c7402e88); the Cloud's exposed actions have neither the list
  nor the unpublish (`crates/fluxbee_sdk/src/cloud.rs:65-86`). With the purge rule (A-212, A-233),
  a Cloud IO node, or a workflow whose WF node published a page, cannot be purged until the page
  expires (up to 30 days) unless the operator takes it down.
- **To decide:** let a publisher take its own pages down (any node allowed to publish), and give the
  Cloud the list and the unpublish for its own tenant's pages through IO.cloud; or leave pages to
  expire.
- **The operator (2026-11-10):** answers with A-242's talk.
- **0.1.131 (A-242):** the Cloud's `delete_node` (a kill with purge) is no longer refused for a page
  up, and IO.cloud's view of its answer keeps no `pages_left_up`: the Cloud is not told which pages
  stay public. The operator's answer is pending.
- **The operator's call (2026-11-11):** each publisher takes its own pages down: IO and WF nodes
  as AI nodes do (only their own). The Cloud's list and take-down of its tenant's pages wait for
  the Cloud track. In 0.1.137 (SY.admin).
- **Told to the Cloud team (2026-11-11):** `docs/io-cloud-api.md` §5.7 (on daily; on main with
  0.1.133's close) and the Cloud team's page give today's behavior (who publishes, the fixed
  expiry, no removal waits for a page, no op for the Cloud), the call as planned (for 0.1.136 then,
  0.1.137 since A-314's renumbering) and not built, and the Cloud's list and take-down with its
  track. Both say so again when 0.1.137 ships.

- **Built on `coa/admin-136` (0.1.137):** over the mesh an `IO.*` or `WF.*` node takes down a page
  it published, as an `AI.*` node does, and only its own (the router-stamped caller, the page's
  tenant, no node of an ingress or egress hive). IO.cloud@motherbee, an IO node, is admitted too: it
  publishes none, every other page is refused, and neither action is Cloud-exposed. The builder
  chose that over a special case that denies IO.cloud; the operator may want it denied. No SDK
  helper yet: the Rust and Go SDKs' helpers to publish and take down a page come with their next
  releases.
- **Shipped in 0.1.137** (`15391283`, its review `9e7e956f`, on daily). **Checked on 8.x (gate0237,
  2026-11-12 09:57–10:00Z):** `IO.gate0237u@worker1` and `WF.gate0237u@worker1`, spawned for their
  ILKs and killed without purge, with the IO's page and `AI.gate0233@worker1`'s up: the WF's
  take-down of an unknown page answered `NOT_FOUND` in 0.057 s, past the A-62 table, which refused
  a WF on 0.1.136; the WF's take-down of the IO's page and the IO's of the AI's answered
  `UNAUTHORIZED` (*"a node may unpublish only its own publication"*); the IO's own take-down
  answered ok, `unpublished` true, in 0.232 s, and its link answered 404 after; the AI's own
  take-down unchanged, ok. `unpublish_artifact`'s help names A-243, `IO.*` and `WF.*`. The gate's
  first run stopped on its own tool (it read the router's node list, which has no ILK; fixed).
  `docs/io-cloud-api.md` §5.7 and §9 say it as live from 0.1.137, with these records. The Cloud
  team's page, outside the repo, says so since its Version 4 (2026-11-12), with A-298's.

### A-242 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.131, 2026-11-11; a node's and a tenant's purge with two pages up, each naming them; the spawn's rollback stays by design) (found 2026-11-10 by A-233's builder; read in the code) — remove_hive leaves its nodes' pages up; a spawn's rollback purges an ILK without SY.admin

- **remove_hive:** `remove_hive_flow` (`src/bin/sy_orchestrator.rs` ~21482 on c7402e88) leaves the pages
  its nodes published up until they expire; the purge rule (A-233) does not reach it.
- **The spawn's rollback:** `delete_node_ilk_for_spawn_rollback` (~25315) purges the ILK of a node
  whose spawn failed without going through SY.admin; that node barely ran.
- **Also, A-232's case in other binaries:** refusals that still repeat a cursor at
  `src/bin/sy_identity.rs:7425`, `src/bin/sy_orchestrator.rs:3493`, `:19709` (with each binary's
  next release). The review of SY.admin's 0.1.127 found two more: the orchestrator's
  `versions_limit` and SY.timer's `TIMER_LIST` cursor (`go/sy-timer/main.go:805`). The
  orchestrator's three and SY.timer's are fixed on `coa/orch-127` and `coa/timer-127` (0.1.127);
  SY.identity's (`list_ilks`, `list_tenants`) waits for its next release.
- **To decide (remove_hive):** refuse it while the hive's nodes have pages up, as a purge is, or take
  them down as part of the operator's explicit removal.
- **The operator (2026-11-10), in talk:** no removal should be blocked by published pages; a page has a
  fixed expiry that does not renew, the edge serves it and makes it disappear, so a hive going away is
  incidental. Read in the code the same day: that expiry exists already (24 h by default, 30 days at
  most, no renewal; SY.admin's sweep takes the edge's row down and deletes the public copy, the edge
  refuses an expired row on its own), and the operator's path takes any page down whoever published
  it. Proposed: no refusal for pages on `remove_hive`, a node's purge (A-233) or a tenant's purge
  (A-212); the answer names the pages left up and when each expires.
- **The operator's call (2026-11-10):** *"si, nada se traba con las paginas.. porque quedan en el edge,
  solas mueren despues.. 24hrs asi que no veo problema"*. No removal (a hive, a node, a tenant) is
  refused for published pages; A-233's and A-212's refusals go. For SY.admin's next release.
- **Validated (0.1.131, 2026-11-11):** `AI.gate0231@worker1` published two pages (3600 s and 60 s) and
  was purged in 2.8 s with both up; the answer's `pages_left_up` named both, with no key, and the
  long page still answered 200. Its tenant's purge (1.3 s) named them again. The long page, taken
  down, answered 404; the short one answered 404 at its expiry and the sweep retired it (06:36:51,
  `expired`, its copy released). `remove_hive` of a throwaway worker named no page (count 0). The
  factory reset's dry run lists its new step [8]. The spawn's rollback that purges an ILK stays as
  it is, by design (docs/control-path-hardening-v1.md §4: the node never ran).

### A-241 [General/blob] ✅ VALIDATED ON 8.x (0.1.133, run on 0.1.134, 2026-11-11; worker1's first pass deleted the 2 old put leftovers of 4 planted files) (found 2026-11-10 by stage 5's builder; read in the code) — A spoke's blob staging is never cleaned

- **Why:** since A-188 the blob GC runs only on the motherbee; a spoke's `staging/` leftovers were
  never cleaned before either, because its GC was off. A staging-only pass on spokes, which needs
  no publication pins, would clean them.
- **Fix, to decide:** whether spokes get a staging-only GC pass.
- **The operator's call (2026-11-10, the sweep):** *"si"*: each worker cleans its own blob staging (always on, files older than 24 h, hourly, one log line).
- **Checked on 8.x (0.1.133, its pass run on 0.1.134):** worker1's staging before the deploy
  (22:34Z): `root:root` 750, no backlog over 24 h; the gate planted an old blob name, a fresh one,
  an old put temporary and an old name that is no blob's. The pass runs one hour after each
  orchestrator start; worker1's last start was 23:48:30Z (on 0.1.134), so its pass was due at
  00:48:30Z. It ran at 00:48:30Z (*"blob staging cleaned: unfinished puts past their TTL deleted"*, `scanned=4
  deleted=2 deleted_bytes=6 elapsed_ms=39`, with `blob_gc_enabled=false`): the old blob name and the
  old put temporary went, the fresh blob name and the old name that is no blob's stayed. A restart of
  the orchestrator postpones the pass by an hour.

### A-240 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.133, 2026-11-11; no `status_version` in the answer, 5 reads left the node's directory unchanged) (found 2026-11-10 by A-199's builder; read in the code) — A local get_node_status writes on every read

- **Why:** `get_node_status` / `NODE_STATUS_GET` for a node of this hive runs
  `advance_node_status_version` (`src/bin/sy_orchestrator.rs` ~25911), which writes the status
  version and fingerprint and recreates the node's directory; the SDK's policy classes it as a read
  (`crates/fluxbee_sdk/src/policy.rs:86`). A-199's step 1 keeps it in the ordered lane for that
  reason.
- **Fix, later:** a status read that writes nothing (the version advanced where the status changes).
- **The operator's call (2026-11-10, the sweep):** *"si, sacalo"*: `status_version` goes; a node's status becomes a plain read.
- **Checked on 8.x (0.1.133):** `AI.pruebas@worker1`'s status has no `status_version`; 5 reads (1.2
  to 3.2 s) left its directory unchanged. The `status_version` and `status_fingerprint` files older
  releases wrote stay on disk, unused.

### A-239 [General/Go SDK] ✅ VALIDATED ON 8.x (0.1.130, 2026-11-10; TIMER_GET and TIMER_LIST byte for byte the same before and after) (found 2026-11-10 by A-220's builder; read in the code) — The Go SDK's TimerInfo drops SY.timer's canceled_reason

- **Why:** since A-220 (SY.timer 1.4) TIMER_GET and TIMER_LIST carry `canceled_reason` for a timer
  SY.timer closed itself. The Go SDK's `TimerInfo` (`go/fluxbee-go-sdk/timer_client.go:39`) has no
  such field, so `TimerClient.Get`/`List` lose it; the Rust SDK keeps it in `extra`
  (`crates/fluxbee_sdk/src/timer.rs:199-200`); SY.admin's routes and Archi show it.
- **Fix (the Go SDK, with A-228's release):** the field in `TimerInfo`.

### A-238 [General/SY.timer] ✅ VALIDATED ON 8.x (0.1.131, 2026-11-11; SY.timer 1.5 dated the 352 canceled timers at its first start and deleted none) (found 2026-11-10 by A-220's builder; read in the code) — A canceled timer can be deleted at the next start, aged by its creation instead of its cancellation

- **Why:** the history GC runs before the replay at a start (`go/sy-timer/main.go:210-214`) and ages
  a canceled timer by its last fire or its creation, not by when it was canceled
  (`persistence.go:311`). A timer canceled (by its owner, or by SY.timer since A-220) whose creation
  or last fire is older than 7 days is deleted at the next start, its reason with it. Already so
  before A-220; documented in docs/sy-timer.md §7.5.
- **Fix, later:** age a canceled timer by its cancellation time.
- **Validated (0.1.131, 2026-11-11):** SY.timer 1.5 on the motherbee and worker1: the ready lines say
  `gc_deleted=0 gc_stamped=2` and `gc_stamped=350`; the stores are at schema 4 with `canceled_at_utc`
  and no canceled row without it; every canceled timer listed before the deploy is listed after it;
  a second cancel keeps the first one's date. No row was older than 7 days, so the deleting case
  rests on its tests.

### A-237 [General/SDK] ✅ BUILT (0.1.128, 2026-11-10; by its test: metadata only) (found 2026-11-10 by A-219's builder; read in the code) — A reply whose status is neither ok, accepted nor blocked is marked failed in its metadata

- **Why:** `derive_action_outcome` (`crates/fluxbee_sdk/src/policy.rs`) marks any status other than
  ok, accepted or blocked as `Failed` in `meta.action_result`. A-219's `pending_confirm`, an
  intermediate answer between the orchestrators, is labelled failed there, while the final admin
  reply is right (ok or error).
- **Fix, later (the Rust SDK, a release alone):** an intermediate status keeps its own outcome, or
  the vocabulary names it.

### A-236 [General/orchestrator] ✅ BUILT (0.1.125, 2026-11-10; by its test) (found 2026-11-10 by A-212's builder; checked in the code) — Removing an ingress leaves its SY.edge running, still serving the published pages

- **Why:** `remove_hive`'s cleanup stops and disables a list of services that does not include
  `sy-edge` (`remove_hive_cleanup_script`, `src/bin/sy_orchestrator.rs:18193`). SY.edge's unit has
  `Restart=always` and only `Wants` rt-gateway, so stopping the router does not stop it. A removed
  ingress keeps serving `/public/<key>` from its own registry and its local `blob/public` until each
  page expires, as long as the public name still points to that box; SY.admin may meanwhile mark
  those pages unpublished (A-212's review: a page on a removed edge comes down without asking it).
- **Fix (SY.orchestrator, 0.1.125):** `sy-edge` in the cleanup's list (stopped, disabled, killed),
  with a test.

### A-235 [Archi] ✅ BUILT (0.1.125, 2026-11-10; by its test) (found 2026-11-10 by 0.1.125's review round; checked in the code) — Archi's runtime redeploy kills each node with its instance purged and runs it again with no tenant, so the node is gone for good

- **Why:** `redeploy_runtime_nodes_with_current` (`src/bin/sy_architect.rs:11022`), used by a
  software publish that promotes to current, kills each node of the runtime with
  `purge_instance: true` and then runs it with only `node_name`, `runtime_version: current` and,
  when the node's config is preserved, that config without `_system` (`:11130`). No top-level
  `tenant_id` is ever sent. With the preserve box unchecked (the API's default, `#[serde(default)]`;
  the UI's box is checked by default) the run is refused `IDENTITY_REGISTER_FAILED` (no tenant), and
  the purged node is not recreated. A preserved config whose top-level `tenant_id` was dropped by a
  `set_node_config replace` ends the same way. It failed the same way before A-213: the fallback
  variable was never set on 8.x.
- **Fix (SY.architect, 0.1.125):** read the node's tenant before the kill (its config's
  `_system.tenant_id`, else `tenant_id`) and send it as the top-level `tenant_id`; a node whose tenant
  cannot be read is skipped, never killed, and named in the answer.

### A-234 [General/admin] ✅ VALIDATED ON 8.x (0.1.127, 2026-11-10; 431 and 413 read whole by a client still sending) (found 2026-11-10 by A-229's builder; read in the code) — SY.admin's HTTP reader allocates whatever `Content-Length` a client declares

- **Why:** `read_http_request` (`src/bin/sy_admin.rs` ~11973 on `coa/admin-125`) allocates a buffer
  of the declared length with no cap, so a request declaring a huge body can exhaust SY.admin's
  memory. The API listens on loopback only, and every node on the motherbee runs as root (A-60), so
  it is no boundary against a local process; a robustness item.
- **Fix, later:** a cap above the largest body an action takes (the bundle upload's), refused 413.

### A-233 [General/admin] ✅ VALIDATED ON 8.x (0.1.127, 2026-11-10; a purge refused with a page up, and with a publish in flight) (found 2026-11-10 by A-212's builder; read in the code) — A purged node's pages stay public until they expire

- **Why:** `kill_node` with `purge_instance` (and `purge_ilk`) removes the node and its ILK, and
  nothing reads the publication ledger there: the pages it published stay up for their TTL (24 h
  by default, 30 days at most). Since A-212 `GET /artifacts` shows them by `publisher_l2_name`, the
  operator can take them down, and the factory reset takes every page down.
- **Fix, to decide (SY.admin):** the tenant's rule (2026-09-28: a purge never cascades), refusing
  the purge while the node has pages up, or leaving them to expire.
- **Superseded (0.1.131, A-242, the operator's call of 2026-11-10):** no removal is refused for pages
  or a publish in flight any more; each answer names the pages it leaves up.

### A-232 [General/shared] ✅ VALIDATED ON 8.x (SY.admin, the orchestrator and SY.timer in 0.1.127, 2026-11-10; SY.identity's ILK and tenant listings in 0.1.133, 2026-11-11) (found 2026-11-10 by A-212's builder; read in the code) — A page request's refusal repeats a bad `limit`

- **Why:** `PageAsk::from_payload` (`src/mesh_reply.rs:194`, `:206`) answers a bad `limit`, or a
  cursor that is not a string, with the value in the error text. A link's key pasted there by
  mistake comes back in the answer, and into Archi's provider. A-212's own cursor refusal does not
  repeat it.
- **Fix, later (shared code, with its next release):** the refusal names the field, not the value.
- **Checked on 8.x (0.1.133):** SY.identity's ILK and tenant listings refuse a cursor not theirs
  `INVALID_CURSOR` without repeating it; a real `next_cursor` pages (5 rows).

### A-231 [General/admin] ✅ VALIDATED ON 8.x (0.1.127, 2026-11-10; two rows dropped at SY.admin's start) (found 2026-11-10 by A-212's builder; read in the code) — SY.admin's publication ledger never drops a row

- **Why:** a publish, an unpublish and the expiry sweep read and rewrite the whole ledger
  (`publications.json`, `src/bin/sy_admin.rs:7634`), and no row ever leaves it: it grows with every
  page published.
- **Fix, later:** rows down for some time move out (or are dropped), with the ledger's history kept
  where the operator needs it.

### A-230 [Archi] ✅ VALIDATED ON 8.x (0.1.125, 2026-11-10; Archi asked for the tenant) (found 2026-11-10 by A-213's builder; read in the code) — Archi's prompt tells it to use the root tenant for an AI node with no tenant

- **Why:** in legacy direct-task mode the prompt says to take the single active "root/default"
  tenant from `list_tenants` when an `AI.*` node names none (`src/bin/sy_architect.rs:2517`). The
  genesis tenant is a root (`is_root` means no sponsor, `src/bin/sy_identity.rs:641`) and the
  orchestrator refuses AI nodes there (`TENANT_ROOT_NOT_ALLOWED`); with a company tenant there are
  two roots and Archi stops. The rule never yields a usable tenant, and A-213 makes the tenant the
  caller's datum. The `IO.*` rule beside it already says never the root tenant (`:2516`).
- **Fix (SY.architect and its handbook, small):** Archi asks for the tenant; related texts at
  `src/bin/sy_architect.rs:2514`, `docs/archi/handbook_fluxbee.md:1548`, `:1555`,
  `src/bin/sy_admin.rs:16220`, `:16242`.

- **Fixed (0.1.125) and checked on 8.x:** asked to create `AI.gate0125b@worker1` with no tenant,
  Archi asked which tenant, said AI nodes cannot run in the root tenant, and offered the two that
  can; no plan staged.

### A-229 [General/admin] ✅ VALIDATED ON 8.x (0.1.125, 2026-11-10; 413 answered) (found 2026-11-10 by 0.1.124's gate; checked on 8.x and in the code) — SY.admin's HTTP closes the connection without an answer when a handler fails

- **Seen:** `POST /hives/motherbee/nodes/SY.timer@motherbee/messages` with a 131 KB payload: SY.admin's
  SDK refused the frame ("message too large: 131646 bytes (max 131072)"), and the caller got the
  connection closed with no response; SY.admin logged `http handler error`.
- **Why:** `run_http_server` logs a handler's `Err` and drops the stream (`src/bin/sy_admin.rs:9803`);
  only the handlers' own answers produce a response.
- **Fix (SY.admin, small):** a handler's error answers 500 with SY.admin's error shape.

- **Fixed (0.1.125) and checked on 8.x:** a 131,390-byte node message answered HTTP 413
  `MESSAGE_TOO_LARGE` ("nothing was sent"); other handler failures answer 500 with a fixed text; the
  405 and 413 status lines are right.

### A-228 [General/Go SDK] ✅ BUILT (0.1.130, 2026-11-10; by its tests; no Go node on 8.x read the error live) (found 2026-11-10 by A-202's builder; read in the code) — The Go SDK's RPC error for UNREACHABLE drops the router's reason

- **Why:** a Go caller's request answered `UNREACHABLE` fails at once, as it should, but with
  `RpcError{Kind: RpcErrTerminalTransport}` and the message `terminal transport response
  "UNREACHABLE"` (`go/fluxbee-go-sdk/dispatcher.go:762`): the payload's `reason` is not read. A Go
  caller cannot tell `FRAME_TOO_LARGE` from `NODE_NOT_FOUND` or `WAN_UNAVAILABLE`. The Rust SDK
  keeps it (`RpcError::Unreachable { reason, .. }`). SY.timer reads the reason itself.
- **Fix, later (the Go SDK, a release alone):** the error carries the reason and the original
  destination, as the Rust SDK's does.

### A-227 [WF] 🔵 low (found 2026-11-10 by A-215's builder; read in the code) — A relay that names no one is attributed to the last other person who wrote through that IO node, who may not be the one answered

- **Why:** when a WF→IO message names no addressee, cognition takes the last other person who wrote
  through that IO node (`src/bin/sy_cognition.rs:2588-2611`); the tagger's hint names only the node
  (`relay_addressee`, `:2431-2442`). Since A-215 a forward to a team's node names no one, so if
  another team member wrote there after the one the agent answered, the episode names the wrong
  person.
- **Fix, later (the cognition track):** the relay carries whom the forwarded answer was for, as a
  fact of the message, apart from its addressee.
- **The operator (2026-11-10):** *"si, tema cognition no termino"*: with the cognition track, not the sweep.

### A-226 [IO/io.slack] ✅ BUILT (0.1.131, 2026-11-11; by its tests: the live text needs a person writing through io.slack) (found 2026-11-10 by A-217's builder; read in the code) — io.slack has no text for a WF's `NOT_TAKEN` reason `unavailable`

- **Why:** since A-217 a WF whose store is down answers `NOT_TAKEN` with the reason `unavailable`.
  io.slack has a text per reason, and shows an unknown one as "Not taken." with a warning
  (`nodes/io/io-slack/src/not_taken.rs:199-207`, `:235-241`).
- **Fix, later (io.slack's revamp, A-148):** a text for `unavailable`.
- **The operator's call (2026-11-10, the sweep):** *"ok"*: the text now, with a test.

### A-225 [General/egress] ✅ BUILT (0.1.126, 2026-11-10; the probe's reason seen on 8.x) (found 2026-11-10 by A-219's design; checked in the code) — A worker's egress path: a removed worker keeps IPv6 off, its boot gives up on a sysctl error before the probe, and a failed probe never says why

- **A removed worker keeps IPv6 off:** `remove_hive`'s cleanup tears the egress down only for an
  egress (`remove_hive_cleanup_full_script`, `src/bin/sy_orchestrator.rs:18252`), so a worker that
  went out through the egress keeps `/etc/sysctl.d/99-fluxbee-egress.conf`.
- **The boot gives up early:** the worker's reconcile returns on any `sysctl --system` error
  (`apply_sysctl_system()?`, `:11206`), after the route and before the IPv6 read-back and the
  probe, while the egress's own reconcile only warns and judges by the kernel's values.
- **The probe drops its reason:** `check_internet_reachable` returns a bool (`:10721-10732`), so a
  failed probe cannot say whether DNS, the connection or the time failed.
- **Fix:** with A-219's build, which uses all three.

### A-224 [General/orchestrator] ✅ BUILT (0.1.125, 2026-11-10; by its test) (found 2026-11-10 by A-219's design; checked in the code) — `set_storage` rewrites hive.yaml in place, not atomically

- **Why:** `persist_storage_path_in_hive` writes the file with `fs::write` (`src/bin/sy_orchestrator.rs`
  ~33128). A crash or a full disk during the write can leave a truncated hive.yaml, and the
  orchestrator does not start on it. hive-config-update-v1 §5 says the file has one writer, under
  one lock, atomically: not built.
- **Also:** `set_storage` runs only on the motherbee's own orchestrator
  (`action_routes_via_local_orchestrator`, `src/bin/sy_admin.rs:22770`), while
  hive-config-update-v1 §1 speaks of spokes.
- **Fix:** the write through `write_file_atomic` (`:26570`), the one A-219's commit uses; the doc's
  wording corrected.

- **Also (A-213's builder):** the join writes a spoke's hive.yaml through `sudo -n tee` in place
  (`write_remote_file`, `src/bin/sy_orchestrator.rs:32713`), under the hive's topology lock; a join
  that fails midway is redone anyway. hive-config-update-v1 §5 lists it as not built.
- **Being fixed:** `coa/orch-125` (0.1.125).

### A-223 [IO/io.api] ✅ CLOSED (2026-11-10; fixed by 0.1.125, read in the code by the sweep's triage) (found 2026-11-10 by A-216's builder; checked in the code) — io.api waits for a frontdesk handoff no longer than the frontdesk waits for SY.identity

- **Why:** io.api waits for the frontdesk's answer for its identity timeout, 10 s by default
  (`nodes/io/io-api/src/config.rs:56-58`, used at `main.rs:1612-1621`), and the frontdesk's own
  call to SY.identity times out at 10 s too (`nodes/gov/common/src/lib.rs:56`). When SY.identity is
  slow, io.api gives up before the frontdesk can answer. A-216's retries fit inside the wait only
  when SY.identity fails fast.
- **Fix, later (io.api, with the IO standard A-149):** io.api's handoff wait is longer than the
  frontdesk's whole attempt (its identity timeout plus A-216's retries).
- **Closed (2026-11-10, the sweep's triage):** already fixed by 0.1.125 (A-216's review round bounds every ILK_REGISTER attempt of a registration by one 8 s deadline, under io.api's 10 s wait); io.api's doc to correct with the IO release.
- **0.1.131:** io.api's spec names `node.identity_timeout_ms` (10 s) and the rule that it stays above
  the frontdesk's 8 s deadline; the frontdesk runs a handoff in any config state.

### A-222 [General/router] ✅ VALIDATED ON 8.x (0.1.129, 2026-11-10; a client that stopped reading took 39 frames of 100 KB, the other 41 refused `LINK_BUSY` at once, locally and across the WAN; all 39 delivered in order after) (found 2026-11-10 by A-203's builder; read in the code) — A node that stops reading without an error is never disconnected, and the router's queue for it grows without limit

- **Why:** a link's queue is unbounded, and its writer blocks while the node's socket does not
  drain. A node that stops reading without closing (hung, or stuck behind its own full queue)
  never makes a write fail, so A-203's "the link no longer writes" never fires, and every frame
  for it piles up in the router's memory.
- **Not a plain timeout:** a node busy with a long request may stop reading for a while on
  purpose (backpressure through its SDK's queue). A write deadline would cut a healthy, busy node.
- **Fix, later (the router, a design of its own):** a bounded queue per link with a stated
  policy when it is full (answer the sender, drop, or disconnect), measured against the nodes'
  real load.
- **The operator's call (2026-11-10), option (a):** a bounded queue per link; when it is full the
  router refuses the NEW message and answers its sender (a "busy" reason), with a rate-limited log
  line; the node is never disconnected, and the refusal lasts only while the queue is full: as the
  node reads, room comes back. Not a circular buffer, which would drop already-accepted messages
  whose senders believe them delivered. A node hung for good keeps answering "busy" at once,
  instead of growing the router's memory; its restart is the orchestrator's or the operator's. For
  the router's release (with A-209, A-221).
- **What a refusal costs a sender that expects no answer (the review of 0.1.129):** wf-generic and
  the AI SDK's control plane ignore an `UNREACHABLE` for such a send, so a WF's or an agent's
  reply refused `LINK_BUSY` (its IO node's link full) is lost for the user, and only the router's
  log line shows it. SY.timer sends a refused fire again (0.1.127). This is the price option (a)
  pays: the queue fills only behind a node that has stopped reading.

### A-221 [General/router] ✅ BUILT (0.1.129, 2026-11-10; by its tests) (found 2026-11-10 by A-203's builder; read in the code) — Cognition records a turn the agent never received when its delivery fails

- **Why:** `deliver_locally` appends the agent's cognition copy before it sends the message
  (`src/router/mod.rs` ~6994 on `coa/a-203`). When the send fails (the node's link is gone), the
  turn is in cognition and never reached the agent. Since A-203's fix the sender is answered
  `NODE_NOT_FOUND`, so the sender knows; cognition does not.
- **Why it is ordered so:** a comment ties the copy's order to the package the agent reads.
- **Also:** a message to an agent refused as too large (A-202) leaves its cognition copy too.
- **Fix, later (with the cognition track):** the copy follows a successful send, or cognition
  learns of the failure.

### A-220 [General/SY.timer] ✅ VALIDATED ON 8.x (0.1.127, 2026-11-10; the schema at v3 with `canceled_reason`) (found 2026-11-10 by A-206's builder; read in the code) — A recurring timer whose next fire cannot be computed stays pending for good and holds its client_ref; and a junk spec can get the wrong error code

- **Why:** at a start, `replayPending` (`go/sy-timer/scheduler.go:134-143`) logs and skips a
  recurring timer whose next fire fails, and leaves it `pending` with its old `fire_at`. Scheduling
  again with the same `client_ref` and a fixed spec returns that dead timer with
  `already_existed: true` (`main.go:627-632`), and the new spec is ignored; the timer also counts
  toward the pending caps. The owner has to cancel it first.
- **Also:** `timerErrorCode` (`main.go:1274`) matches error codes by substring, so a junk spec
  that repeats "invalid timezone" in its fields comes back as `TIMER_INVALID_TIMEZONE`.
- **Since A-206:** a stored spec with a `TZ=`/`CRON_TZ=` prefix takes this path. None is expected
  on 8.x: the SDKs never sent one, and nothing in the repo writes one.
- **Fix, later (SY.timer):** a recurring timer the replay cannot schedule is closed with its
  reason, so its `client_ref` is free; error codes by type, not by text.

### A-219 [General/egress] ✅ VALIDATED ON 8.x (0.1.126, 2026-11-10; worker1 behind egress1, and back, and behind again) (from the sweep of 2026-11-07; checked on 8.x) — A worker that joined before the egress was declared never goes out through it

- **Seen on 8.x:** the motherbee declares the egress (`gateway_ip` 10.10.10.40, egress1), and
  worker1 has no `egress` section: its default route is 10.10.10.1. Its agents' internet traffic
  (AI.pruebas's calls to the provider) bypasses the egress, its IPv6 stays on, no probe runs, and
  nothing reports it.
- **Why:** the join writes a worker's `egress` section from the motherbee's
  (`render_worker_egress_yaml`, `src/bin/sy_orchestrator.rs:8114`), and nothing writes a
  worker's hive.yaml after its join. The worker applies it at its boot (the default route, IPv6
  off, a probe) and its watchdog keeps the route.
- **The operator's call:** *"si workers tienen que salir a internet, si b)"*: workers go out through
  the egress, by an explicit action per hive.
- **Fix (SY.orchestrator, SY.admin, Archi; a day or two):** an action per worker, from Archi with
  the operator's CONFIRM and through SY.admin, that writes the worker's `egress` section from the
  motherbee's and applies it, with guards: the gateway on a directly connected subnet, the mesh
  uplink and the operator's access kept off the default route, a probe after the switch, and the
  previous route back if the uplink drops. AI.pruebas then depends on egress1. Its shape is
  written down before it is built.
- **The shape, approved by the operator (2026-11-10):** docs/edge-egress-nat-spec.md §15
  (`coa/a-219-design`), every open point as recommended: *"sobre 1, 2, 3 y 4 hacelos como
  recomendas"*. `set_hive_egress` and `clear_hive_egress` (`SYSTEM_EGRESS_ROUTE` between the
  orchestrators), the reverse built now; the guards check and refuse (the operator adds an OS-level
  route for a network they manage from); the switch stays only once the motherbee's confirm comes
  back, else the worker reverts; the minor points as the doc proposes. Its release carries the
  router's protected-action entry for the new message, with the rest.

- **Built (0.1.126):** `set_hive_egress` / `clear_hive_egress` (SY.admin, `Nobody`, CONFIRM through
  Archi), `SYSTEM_EGRESS_ROUTE` between the orchestrators (protected, motherbee to spoke only), the
  guards G1-G3, the record, the probe, the confirm, the commit or the revert, the start's resume,
  `/versions`' egress state; from its review round: a body naming another hive refused, a revert
  that never stops halfway, multipath defaults refused, the worker refusing a core update or rollback
  while its switch is open, socket-activated sshd read through `sshd_listen_ports`.
- **Checked on 8.x (the gate, 2026-11-10):** before, worker1 read `not_behind` in `/versions`.
  - **The switch:** `PUT /hives/worker1/egress` answered `switched` in 2.7 s (1.0 s on the worker):
    default via 10.10.10.40, IPv6 off, the probe through egress1 ok, hive.yaml's section the join's
    renderer writes, the record committed, `/versions` `behind`. AI.pruebas answered, and an nft
    counter on egress1 saw 123 packets from worker1.
  - **Again:** `unchanged` in 1.3 s. A hand route back to 10.10.10.1 was undone by the watchdog
    within 6 s. A restart of the orchestrator logged `internet_reachable=true`.
  - **The clear:** `cleared` in 3.0 s; worker1's routes and IPv6 identical to the baseline.
  - **The revert:** with worker1's forwarded traffic dropped on egress1, the set answered
    `EGRESS_PROBE_FAILED`, `reverted`, curl's reason (DNS timed out after 5 s), reverted in 47 ms;
    worker1 identical to the baseline.
  - **A crash mid-switch:** worker1's orchestrator killed 3 s into a switch; at its start it undid
    the switch from its record (`interrupted`); worker1 identical to the baseline; the action
    answered `EGRESS_NOT_ANSWERED` after its 30 s.
  - **Refused:** egress1 (`NOT_A_WORKER`); a body naming egress1 on worker1's path
    (`INVALID_REQUEST`).
  - **Left, by the operator's call:** worker1 behind egress1, set through Archi's plan and CONFIRM;
    AI.pruebas answers through egress1.

### A-218 [General/identity] ✅ VALIDATED ON 8.x (0.1.128, 2026-11-10; migration v5, the identity shared memory's header identical before and after on both hives that hold it, the replica's full sync) (from the sweep of 2026-11-07; read in the code) — Dead identity surface (TNT_APPROVE, the vocabulary) and ECHO/TIME_SYNC

- **Why:** nothing sends `TNT_APPROVE` (SY.identity handles it: `src/bin/sy_identity.rs:154`,
  `:3108`, `:4083`; the SDK exports it); `crates/fluxbee_sdk/src/policy.rs:126-139` classifies four
  actions SY.admin does not have (`approve_tenant` and three of the vocabulary); nothing writes the
  vocabulary table (`sy_identity.rs:7850`), and its region of the identity shared memory (about
  1 MB) is always empty; nothing emits or answers ECHO, ECHO_REPLY or TIME_SYNC
  (`crates/fluxbee_sdk/src/protocol.rs`, the Go SDK's constants; `examples/node_test.rs` only).
- **The operator's call:** *"si borra la basura, no hay back compatibilidad"*.
- **Fix (SY.identity and the SDKs: shared code, a release alone):** delete the code, the dead
  action names and ECHO/TIME_SYNC with the example. The empty table and the shared memory's
  region are open: removing the region means a new `IDENTITY_VERSION`, with the router, every SDK
  and every published runtime moving together.
- **Built for 0.1.128 (`coa/sdk-128`, reviewed):** `TNT_APPROVE` and its handling gone; seven
  dead action names (the four above, `get_runtimes`, `update_policy_matrix`, `clear_override`);
  ECHO, ECHO_REPLY and TIME_SYNC out of the Rust SDK and the AI nodes' notice list; SY.identity's
  migration v5 drops `identity_vocabulary` and `identity_tenants.approved_by`. **The identity
  shared memory is exactly as it was** (the operator's rule): its layout, its empty vocabulary
  region and `IDENTITY_VERSION` 6; the review compared its code between the two refs. A rollback
  to 0.1.127 is safe (it ignores migration row 5 and never names the column).
- **Left for the Go SDK's release (with A-228):** `MSGEcho`/`MSGEchoReply` and the notice lists
  that name them (`go/sy-timer/main.go:318-319`, `go/nodes/wf/wf-generic/node/dispatch.go:196`, its
  comment at :189-190, `protocol_test.go`), and the texts that say the Go nodes' rule matches the
  AI nodes' (`docs/spec-systemnode-wf-v1_1.md:1125`, `docs/sy-timer.md:268`). Also dead, found by
  the builder: `classify_system_message` lists `OPA_APPLY`, `OPA_COMPILE_APPLY` and `OPA_ROLLBACK`,
  which nothing sends or handles (with the Rust SDK's next release).

### A-217 [WF] ✅ BUILT (0.1.125, 2026-11-10; by its tests) (from the sweep of 2026-11-07; read in the code) — wf-generic exits with `log.Fatalf` when its database fails, and restarts every 5 s without ever reporting why

- **Why:** any error from `node.Run` ends in `log.Fatalf` (`go/nodes/wf/wf-generic/main.go:54-68`):
  a bad `db_path`, a corrupt database, a full disk or a failed recovery. The unit restarts it
  every 5 s, and the reason is only in its journal. The same node already runs DEGRADED with a
  reason while SY.timer has not answered.
- **The operator's call:** yes.
- **Fix (wf-generic, a new wf.engine runtime):** stay up DEGRADED, or FAILED_CONFIG when the cause
  is the `db_path`; refuse events with an error; try the database again on a tick.

- **Built (0.1.125):** the node stays up DEGRADED (`store_unavailable`) or FAILED_CONFIG (the
  `db_path` at fault), refuses events, retries the store every 30 s; from the review round, a
  reconcile that cannot read its timers keeps the node DEGRADED and retrying, so a fire dropped
  while the store was down is given once it opens.

### A-216 [General/frontdesk] ✅ BUILT (0.1.125, 2026-11-10; by its tests) (from the sweep of 2026-11-07; read in the code) — The frontdesk fails a registration for good on a transient error

- **Why:** the prompt's no-loop rule makes any `ilk_register` error final (`completed_error`,
  `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs:96-118`), the tool marks UNREACHABLE,
  TIMEOUT, NOT_PRIMARY and the DB errors `retryable` but retries nothing, and the state is kept
  per person with no expiry. A confirmation during a deploy fails for good, on every thread.
- **The operator's calls:** retry (G8, yes); the spec's counters (G6) leave the spec: the journal
  logs each outcome, and counts wait for the system's log.
- **Fix (SY.frontdesk.gov):** retry a transient error for a few seconds; if it still fails, leave
  the case awaiting confirmation so the person's next "yes" retries; final only when not
  retryable. A case already stuck on a transient code is reopened.

- **Built (0.1.125):** a transient failure retried within one 8 s deadline (below io.api's wait),
  then the case stays awaiting confirmation; one `ilk_register` per turn, held in the code; only
  the failing turn answers `IDENTITY_UNAVAILABLE`; G6 left the spec.

### A-215 [WF] ✅ BUILT (0.1.125, 2026-11-10; by its tests) (from the sweep of 2026-11-07; read in the code) — A WF forward to an IO node other than the person's keeps the original `dst_ilk` and `ich`

- **Why:** `execForward` copies the message's `src_ilk`, `dst_ilk` and `ich`, and rewrites only a
  forward to the person's own IO node (`go/nodes/wf/wf-generic/node/actions.go:235-279`). An
  agent's answer to a person forwarded to a team's channel reaches the agent's memory and
  cognition's episodes as said "to the person, via IO.team". Delivery does not change.
- **The operator's call:** yes.
- **Fix (wf-generic):** a forward to an IO node other than the person's clears `dst_ilk` and `ich`,
  as `send_message` does; cognition's fallback (the last person who wrote through that node)
  covers the team's case.

### A-214 [General/language] ✅ BUILT (0.1.125, 2026-11-10; the texts by tests) (from the sweep of 2026-11-07; read in the code) — Fixed texts the product sends are in Spanish, and the frontdesk answers everyone in Spanish

- **Why:** about 15 fixed texts: the frontdesk's `human_message` values, which io.api and io.cloud
  pass outside; ai.generic's "Aca esta el archivo solicitado" (`ai_node_runner.rs:3071-3084`);
  SY.architect's help notes. The frontdesk's prompt ends "Keep replies short in Spanish." A-184
  chose English for the provider's failure text, which the frontdesk already uses (since
  0.1.114).
- **The operator's call:** *"solo ingles todo, en el repo, docs, y mensajes del sistema"*.
- **Fix (SY.frontdesk.gov, ai.generic, SY.architect):** every fixed text in English; the
  frontdesk's model answers in the person's language. The Spanish test names and strings the
  2026-10-17 pass left go too.

- **Built (0.1.125):** every fixed text in English (the frontdesk's, ai.generic's, io.slack's error
  post, SY.architect's help), the frontdesk's model answers in the person's language, Spanish test
  names and strings across the repo translated. Kept on purpose: input recognition of Spanish, the
  operator's quotes in the logbooks and design docs, gate transcripts, people's and nodes' names.

### A-213 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.125, 2026-11-10; both refusals seen) (from the sweep of 2026-11-07; checked on 8.x) — The `ORCH_DEFAULT_TENANT_ID` fallback puts a node with no tenant in whatever tenant it names

- **Why:** `resolve_tenant_id_for_node` falls back to the variable after the request's tenant
  (`src/bin/sy_orchestrator.rs:21543-21565`), for the spawn, the gates and the identity
  registration. Nothing sets it, and no hive of 8.x has it.
- **The operator's call:** yes, drop it: the tenant is the caller's datum.
- **Fix (SY.orchestrator):** the fallback, its error text and its docs go.

- **Built (0.1.125):** no fallback; a spawn with no tenant refused at the registration with "there
  is no default tenant"; two different tenants refused `INVALID_REQUEST` before anything is written;
  config.json records the registered tenant, trimmed. **Checked on 8.x:** `run_node` of a throwaway
  `AI.gate0125@worker1` answered both refusals, and nothing was left on worker1.

### A-212 [General/admin] ✅ VALIDATED ON 8.x (0.1.125, 2026-11-10; `GET /artifacts` answered, no key) (from the sweep of 2026-11-07; read in the code) — The factory reset, and a tenant's purge, leave published pages public; no action lists them

- **Why:** the reset never touches publications (`packaging/fluxbee-factory-reset:39`), and a
  page stays at its link until it expires: 24 h by default, 30 days at most. A tenant's purge
  leaves them too. No action lists the pages, so Archi cannot either.
- **The operator's call:** yes, option (a).
- **Fix (SY.admin, the reset script, Archi's handbook):** a read-only SY.admin action that lists
  the live pages; the reset unpublishes each one through SY.admin.

- **Built (0.1.125):** `list_published_artifacts` (`GET /artifacts`, paged, Archi's read, rows
  without the link's key, `publication_id` to unpublish); the factory reset unpublishes every live
  page first; a tenant's purge is refused `TENANT_HAS_PAGES` while one is up (the operator's
  2026-09-28 rule: a purge never cascades); a page on a removed edge comes down without asking it.
  **Checked on 8.x:** `GET /artifacts` answered `ok` with no page live and no key field.
- **Superseded (0.1.131, A-242, the operator's call of 2026-11-10):** a tenant's purge is no longer
  refused for its pages; its answer names them (`pages_left_up`), and they die at their expiry.

### A-211 [General/packaging] ✅ VALIDATED ON 8.x (0.1.124, 2026-11-10) (from the sweep of 2026-11-07; checked on 8.x) — wf-generic ships as a core binary that nothing runs

- **Why:** `packaging/build-deb.sh:49-50` puts wf-generic in `GO_BINS`, so the .deb installs
  `/usr/bin/wf-generic` on the motherbee and every hive's core manifest lists it. WF nodes run the
  `wf.engine` runtime's copy; on 8.x no WF unit runs `/usr/bin/wf-generic`.
- **The operator's call:** yes.
- **Fix (packaging):** out of `GO_BINS`; the runtime's seed stays.

- **Fixed (0.1.124) and checked on 8.x:** no `/usr/bin/wf-generic` nor `dist/core/bin/wf-generic`
  on the motherbee; WF.gate0206@worker1 runs `wf.engine` 0.1.124.

### A-210 [General/cleanup] 🟡 MOSTLY BUILT (0.1.125, 2026-11-10; SY.edge's part after 2026-11-22) (from the sweep of 2026-11-07; read in the code) — One-time conversions for versions every hive has passed still run at each start

- **Why:** SY.storage drops ≤0.1.82 turn columns and pre-0.1.103 cognition tables
  (`src/bin/sy_storage.rs`), ai.generic converts `behavior.capabilities` (0.1.86), SY.edge re-keys
  pre-0.1.84 publications, SY.cognition removes pre-0.1.103 folders, SY.admin's ACME reads 0.1.82's
  `name`; SY.identity fills channel scopes from replicas older than 0.1.64.
- **The operator's call:** *"OK, estamos en version solo alpha dev"*: no backward compatibility.
- **Fix (each binary, as it is next touched):** delete them. What keeps a rollback working stays:
  the old hive.yaml keys, the command log without `seq`, the installation update's handling of an
  orchestrator before 0.1.121. SY.edge's re-keying after 2026-11-22, when the last pre-0.1.84 page
  has expired.

- **Built (0.1.125):** SY.storage's two one-time drops, SY.cognition's old folders, ai.generic's
  `behavior.capabilities` conversion, SY.identity's channel-scope fill (a scope is now required:
  a record without one is refused), SY.admin's ACME `name` alias. **Left:** SY.edge's re-keying of
  pre-0.1.84 pages (after 2026-11-22); and, found by the builder, `add_turns_writer` (SY.storage),
  SY.identity's migrations v3/v4 and `move_account_to_installation_scope`, ai.generic's
  `runtime.immediate_memory` hint, a stale runbook section ("Rolling back to 0.1.86").

### A-209 [General/router] ✅ VALIDATED ON 8.x (0.1.129, 2026-11-10; an absent tap target dropped at the entry router, a full one dropped without an answer at the far router, the sender hearing nothing; a node's own `via_tap` cleared and answered) (found 2026-11-09 by A-203's builder; read in the code) — A tap copy that cannot reach its hive answers WAN_UNAVAILABLE to the original sender, under the copy's trace_id

- **Why:** a tap's copy to another hive is forwarded with a fresh trace_id; when the forward
  fails, the router answers `WAN_UNAVAILABLE` to the original message's sender
  (`src/router/mod.rs`, the tap forward, ~1705 on `coa/a-203`). The sender never sent that copy,
  and no request of its waits on that trace_id.
- **Impact:** a stray transport error at the sender, which its SDK cannot match to a request. It
  happened already with no link at all; since A-203's fix it also happens when the link's writer
  is gone.
- **Since A-202's fix:** an oversized copy to another hive is answered `FRAME_TOO_LARGE` to the
  original sender the same way; a local or IRP copy is dropped silently.
- **Fix, later:** a tap copy's failure is logged, and never answered to the sender.

### A-208 [General/router] ✅ BUILT (0.1.132, 2026-11-11; A-247 with it; no live case: every hive on 8.x runs one router) (found 2026-11-09 by A-203's builder; read in the code) — With several routers on one hive (not supported), an IRP reply goes to the wrong link, and a sibling can register twice

- **Why:**
  - `handle_peer_message` reuses the name `peer_uuid` for the destination router of
    `ResolvedRoute::ForwardRouter(peer_uuid)` (`src/router/mod.rs` ~4516, ~4656 on `coa/a-203`), so
    TTL_EXCEEDED and PEER_UNAVAILABLE go to the destination sibling's link, or nowhere, never back
    to the link the frame came from.
  - `handle_peer_incoming` checks for an existing entry and inserts it under two separate locks
    (~4100-4110), so two links from one sibling at once can both register. A-203's cleanup keeps
    that harmless.
- **And a latent deadlock there** (found by 0.1.124's review round): in `handle_peer_message`,
  `if let Some(peer) = peers.lock().await…cloned() { forward_to_hive(..).await }` keeps the
  `peers` guard for the whole block (edition 2021); on a router that is not the gateway the call
  reaches `send_to_peer_router`, which locks `peers` again, and the task wedges with every later
  user of `peers`.
- **Impact:** none on 8.x: every hive runs one router. It matters only for the several-routers
  mode, which is not supported.
- **Fix, later:** with that mode, if it is ever supported.
- **The operator's call (2026-11-10, the sweep):** *"si"*: with the router's next release, with A-246 and A-247.
- **Built (0.1.132, the operator's call of 2026-11-10):** `handle_peer_message` takes the link of
  the sibling a frame came from once and holds no guard of the sibling table while it hands the
  frame on, so a router that is not the gateway no longer wedges; a broadcast that comes over IRP is
  checked against the broadcast cache; a sibling registers under one guard; A-247's answers go back
  over that link. The several-routers mode stays unsupported: its known limits are in
  docs/reference/routing-and-policy.md §10. No live check: every hive on 8.x runs one router.

### A-207 [General/router] ✅ VALIDATED ON 8.x (0.1.124, 2026-11-10; a silent egress1 cut after the dead interval) (found 2026-11-09 by A-203's builder; read in the code) — A WAN peer that vanishes without closing its connection keeps its session, and frames to it are lost without UNREACHABLE for up to about 15 minutes

- **Why:** a WAN session has no read deadline and no TCP keepalive. A peer that disappears
  without closing (a power loss, a network split) keeps its session and its peer entry until a
  write to it times out, about 15 minutes with Linux's defaults. Until then every frame to that
  hive is lost silently, as in A-203. `lsa_stale_loop` marks the hive stale after the dead
  interval (40 s, `src/router/mod.rs:2718`), but the session stays.
- **What makes a deadline safe:** every router sends its LSA to each WAN peer every hello
  interval (10 s, `lsa_broadcast_loop`, `:2706`), so a healthy session never goes 40 s without a
  frame.
- **Fix (the router, with A-203, in 0.1.124):** a WAN session with no frame for the dead interval
  ends, and its cleanup runs: senders get `WAN_UNAVAILABLE` and the peer reconnects when it is
  back.
- **A limit to keep in mind:** an LSA over the frame would be skipped by the writer like any frame,
  and a quiet link would then be cut every 40 s. Within the LSA region's 1,024 nodes (about 90 bytes
  each) an LSA stays under the frame.
- **And another, from 0.1.124's review round:** two concurrent sessions for one hive (two uplinks
  to the same hub; `add_hive` writes only one) can churn the hive's entry, each cleanup leaving the
  other's. Not on 8.x.

- **Fixed (0.1.124) and checked on 8.x:** egress1's WAN traffic to the motherbee's :9000 dropped
  at 05:30:53 with an nft table on egress1; the motherbee cut the session at 05:31:28.8, 40 s after
  its last frame, and reads failed fast; when the drop went, egress1 reconnected. Each step of a WAN
  handshake is bounded at 30 s too (the review round).

### A-206 [General/SY.timer] ✅ VALIDATED ON 8.x (0.1.124, 2026-11-10; two `TZ=` specs refused, SY.timer untouched) (found 2026-11-09 by the sweep's re-check; checked in the code and the library) — A recurring timer's cron spec that starts with `TZ=` and has no space crashes SY.timer, and a `CRON_TZ=` prefix silently overrides `cron_tz`

- **Why:** SY.timer parses `cron_spec` with robfig/cron v3.0.1, at the request
  (`go/sy-timer/main.go:1208`, `scheduleRecurringFireAt`) and at each re-schedule
  (`scheduler.go:560`, `nextRecurringFireAt`). The library always reads a `TZ=` or `CRON_TZ=`
  prefix (its `parser.go:95-103`) and slices the zone up to the first space: with no space the
  index is -1, and the slice panics.
- **The crash:** SY.timer's loop handles each message inline, with no recover (`main.go:233-246`).
  The panic ends the process, and systemd starts it again about 5 s later. The timers persist.
- **The override:** with a space, `CRON_TZ=America/New_York 0 9 * * *` fires on New York's time,
  while SY.timer records and answers the request's `cron_tz` (UTC by default).
- **Who reaches it:** any node scheduling a recurring timer for itself. Both SDKs refuse a spec
  that is not 5 fields (`crates/fluxbee_sdk/src/timer.rs:1584`,
  `go/fluxbee-go-sdk/timer_schedule.go:416`), but they split on any whitespace and the library
  looks for a space only: `TZ=UTC` and four fields separated by tabs passes both checks and
  crashes it. A raw message, or the operator's `send_node_message` (it refuses only
  `COGNITION_CLEAR`), reaches it too. No product node schedules a recurring timer today. A node
  that sends it in a loop keeps SY.timer restarting, and every timer waits.
- **Fix (SY.timer, small):** refuse a spec that starts with `TZ=`, `CRON_TZ=` or `@` with
  `TIMER_INVALID_CRON` before parsing. The zone goes in `cron_tz`, and v1 has no descriptors
  (docs/sy-timer.md:948-951). For 0.1.124.

- **Fixed (0.1.124) and checked on 8.x:** between a valid raw recurring timer from SY.admin
  (scheduled, then cancelled) two `TZ=` specs were refused, with no timer and SY.timer's process
  the same throughout.

### A-205 [General/admin] ✅ VALIDATED ON 8.x (0.1.124, 2026-11-10; the undo's update done in 6 s) (found 2026-11-09 by 0.1.123's gate; checked in the code) — A background read taken before a hive applied a config change can mark it pending after it did

- **Seen:** version 5 of the installation's `ai`, 2026-11-09 23:03 UTC, every hive up.
  - SY.admin's background read of worker1 went out at 23:03:39.30. Worker1 applied version 5 at
    23:03:39.77, and SY.admin recorded its apply `ok` at 23:03:40.314.
  - The read's answer, taken before the apply, came back just after. It turned worker1's config
    from on target to `pending`.
  - Nothing read worker1 again until 23:04:08, when reads of the update made SY.admin look again.
    The update closed `done` 32 s after the change; every hive had applied it within 2.3 s.
- **Why:** `set_installation_ai`'s apply (`sy_admin.rs`, the `join_all` over the hives) takes no
  claim on its hives, while the explicit run does (`installation_update.rs:2561`). The background
  `recheck` (`3662`) reads a hive it can claim, and records what the read shows without checking
  whether an outcome came in while the read was under way.
- **Impact:** no wrong action. A config apply is idempotent, and the hive is read again. But the
  update stays open, naming a hive that is on the target: about 30 s while someone follows the
  update, up to 10 min (the pending hive's cadence) when no one does.
- **Fix:** `set_installation_ai`'s apply claims each hive while it applies, as the explicit run
  does; and `recheck` drops a read that began before the category's last outcome.

- **Fixed (0.1.124) and checked on 8.x:** the apply claims each hive, and a stale read is dropped;
  the review round added the waits and the closed-update rules (DEPLOYMENTS 0.1.124). Version 7,
  the undo, closed `done` 6 s after its call (32 s on 0.1.123).

### A-204 [General/admin] ✅ VALIDATED ON 8.x (0.1.124, 2026-11-10; each hive's own time in the record) (found 2026-11-09 by 0.1.123's gate; checked in the code) — The installation update's record gives a set_installation_ai hive the slowest hive's time, and keeps an old outcome on a hive moved by its own update

- **Seen in the gate's version 4,** with egress1 away: the record and the journal give ingress1,
  worker1 and the motherbee about 85.5 s each (`elapsed_ms`, and `at_ms` with it). The action's own
  answer gives each hive's real time, 2.0–2.1 s (`timings_ms.hives`).
- **Why:** `set_installation_ai` records every hive's outcome once all the applies ended
  (`sy_admin.rs`, after the `join_all`), with the apply's start and the time of recording. Each
  hive's own elapsed is there and is not passed.
- **The same record, a second case:** after egress1 was moved back by its own `update`, its core
  read on target with `excluded by automatic` as its last outcome. A look records the new state,
  and keeps the last outcome a run left.
- **Fix:** pass each hive's own elapsed, and its end as `at_ms`. A look that finds a hive moved
  outside the update could record that it saw it on target.

- **Fixed (0.1.124) and checked on 8.x:** version 6, with egress1 away, recorded ingress1 1,445 ms,
  the motherbee 1,748 ms and worker1 1,770 ms (85.5 s each on 0.1.123); a read that finds a category
  on the target records it, marked `seen`, in every read that decides.

### A-203 [General/router] ✅ VALIDATED ON 8.x (0.1.124, 2026-11-10; the deploy's update done in 46.6 s, no read lost) (found 2026-11-09 by 0.1.123's deploy gate; checked in the code) — A WAN session that ends on a read error keeps its hive in the router's peer table, and frames sent to that hive are lost without UNREACHABLE until it reconnects

- **Seen:** 0.1.123's deploy, 2026-11-09 22:52:59 UTC.
  - Egress1's router restarted for its core update. The motherbee's session with it ended on "peer
    closed connection without sending TLS close_notify" at 22:52:59.134.
  - At 22:52:59.372 the motherbee orchestrator forwarded SY.admin's `GET_VERSIONS` to
    `SY.orchestrator@egress1`. Egress1 never received it.
  - Egress1's new router connected at 22:53:00.3.
- **Why (`src/router/mod.rs`):**
  - `handle_wan_connection` reads with `read_frame(&mut reader) => frame?` (`3202`). A read error
    returns from the function there and skips its cleanup: removing the hive's `WanPeer` (`3244`)
    and `writer_task.abort()`. Only a clean close (`None`) reaches that cleanup. The caller only logs
    "wan connection error" (`2785`).
  - `send_to_wan_peer` finds the stale entry. It ignores the result of `peer.sender.send(data)`
    (`1818`) and returns true, so `forward_to_hive` never answers `WAN_UNAVAILABLE`.
  - The entry stays until the hive reconnects. A router stopped with SIGTERM sends no
    close_notify, so a hive whose router is down swallows every frame sent to it, and each sender
    waits out its own timeout.
- **The cost in the gate:**
  - The lost read held the motherbee orchestrator's worker for its 45 s forward bound (A-199).
  - The reads of worker1 and ingress1 queued behind it: SY.admin saw both settled about 27 s after
    their own records.
  - SY.admin then left egress1 unread for 50 s, as it does after a timed-out read. It saw egress1
    settled 84 s after egress1's own record.
  - The update closed `done` 123 s after the call; without the lost read, about 35 s.
- **Fix (the router, a release alone, with A-202):**
  - Run the cleanup on every way out of the session, and remove the entry only while it is still
    this session's: the clean-close path today removes the hive's entry whichever session wrote it.
  - `send_to_wan_peer` returns false when the send fails, so the sender gets `WAN_UNAVAILABLE`.

- **Fixed (0.1.124) and checked on 8.x:** 0.1.124's deploy restarted the three spokes' routers and
  lost no read: the update closed `done` 46.6 s after its call (123 s on 0.1.123). The same cleanup
  went to the links between sibling routers and to nodes; the peer's `WAN_ACCEPT` no longer counts
  as a dropped frame.

### A-202 [General/router] ✅ VALIDATED ON 8.x (0.1.124, 2026-11-10; `FRAME_TOO_LARGE` logged and answered) (found 2026-11-09 by A-198's review round; checked in the code) — A message just under the frame passes the SDK's check, and the router then drops it without telling the sender

- **Why:** the SDK refuses a frame over 128 KiB before sending (`crates/fluxbee_sdk/src/split.rs:144`).
  The router then adds to the message and serializes it again:
  - `routing.src_l2_name` (`src/router/mod.rs:1068`), about 35 B for `SY.admin@motherbee`;
  - `meta.action_class` when absent (`7045-7047`);
  - for an AI node's delivery, the memory package.
- **Then:** its link writer skips any frame over 128 KiB (`4127`), with a rate-limited warning.
  The sender believes the message went out, and a waiting caller gets its own timeout.
- **0.1.123 (A-198):** the reply guards of SY.admin, SY.identity, SY.vault and SY.orchestrator keep
  a 1 KiB reserve under the frame for the router's additions.
- **Fix:** in the shared code, which ships alone. Either the SDK reserves room for the router's
  additions, or the router answers the sender when it must drop a frame.

- **Fixed (0.1.124), the operator's approach by default (the router answers the sender):** a frame
  over the limit once stamped is not queued and its sender is answered `FRAME_TOO_LARGE`; a
  broadcast goes everywhere or nowhere. **Checked on 8.x:** at 130,712 bytes of payload from
  SY.admin the router logged `frame_len=131095 max=131072` and queued nothing. A Go caller cannot
  read the reason yet (A-228).

### A-201 [Archi] ✅ VALIDATED ON 8.x (0.1.123, 2026-11-09; found 2026-11-09 by A-198's builder; checked) — The handbook tells Archi to read `list_recent_commands` first, and its read tool had no route for it

- **Why:** the handbook (`docs/archi/handbook_fluxbee.md:1558`) says to read the admin's command
  log first when the state looks unexpected.
- **But:**
  - Archi's read tool, `fluxbee_system_get`, sends each path through `translate_scmd`, which has
    no route for `GET /hives/{hive}/commands`.
  - SY.admin already admits the action from Archi over the mesh.
- **Fix:** the route in `translate_scmd`, with A-198's pages. Small; in 0.1.123.
- **Checked on 8.x (0.1.123's gate):** asked for the motherbee's three newest commands, Archi read
  `/hives/motherbee/commands?limit=3` (`list_recent_commands`) and answered with each one's
  action, status and origin.

### A-200 [General/vault] ✅ VALIDATED ON 8.x (0.1.127, 2026-11-10; 12 secrets announced in one pass, 18 ms after the start) (found 2026-11-09 by A-198's builder; checked) — SY.vault's boot re-announcement covers only its newest 200 secrets

- **Why:** `emit_bootstrap_secret_broadcasts` lists with `VaultFilter::default()`.
  `VaultStore::list` takes 200 rows by default (`sy_vault.rs:1362`).
- **The case:** a vault holding more than 200 secrets re-announces only the newest 200 at its
  start. A consumer that relies on the boot notice for an older one does not get it.
- **On 8.x:** 12 secrets on the motherbee, so not reached.
- **Fix:** page the boot list with A-198's cursor.

### A-199 [General/orchestrator] ✅ STEP 1 VALIDATED ON 8.x (0.1.127, 2026-11-10; reads apart, 4 per target hive; what is left of a deploy's update is A-250) (found 2026-11-09 by stage 4's review round; read in the code) — The motherbee orchestrator's admin worker runs one request at a time, and a request its caller gave up on still runs

- **Why:**
  - SY.admin relays every hive read and node action through `SY.orchestrator@motherbee`, whose
    admin worker awaits each request inline (`sy_orchestrator.rs:1198`).
  - A spoke's `GET_VERSIONS` is forwarded with a 45 s bound (`JSR_ORCH_SYSTEM_FORWARD_TIMEOUT_SECS`).
    On the spoke it waits again in its own one-at-a-time system worker.
  - The queue has no expiry: a read whose caller gave up at 10 s is still forwarded later.
- **The case:** a spoke whose orchestrator is visible but does not answer, while its system worker
  runs a long action (`core_rollback` up to 240 s, `KILL_NODE` 120 s) or loses frames (A-195).
  - Every admin read behind it waits up to 45 s, and each read given up on adds to the backlog.
  - The callers that time out at 30 s include Archi's reads, IO.cloud's `run_node`,
    `list_tenant_nodes` and `get_node_status`, and `ops.py versions`.
- **Already known:** `docs/wf-standard-path-v1.md` names it as a known limit. Running node actions
  concurrently is future work there.
- **What 0.1.123 does about its own share:** stage 4's background reads keep at most one read per
  hive, read one hive at a time, and re-read held or failed hives every 10 min. The limit itself
  stays.
- **Seen on 8.x (0.1.123's deploy, 2026-11-09):** one read lost by the router (A-203) held the
  worker for 45 s. The reads of worker1 and ingress1 queued behind it, so SY.admin saw both
  settled about 27 s late.
- **Proposal:** answer read forwards (`GET_VERSIONS`, `list_versions`) outside the single worker,
  and drop a queued request whose caller's deadline has passed. The operator's call on when.
- **Step 1, live (0.1.127, 2026-11-10):** reads run apart on the blocking pool (16 local places,
  16 forwarding places, at most 4 waiting on one target hive), changes one at a time; a command
  that panics is answered `INTERNAL_ERROR`. Checked on 8.x with egress1's orchestrator stopped:
  six `/hives/egress1/versions` at once, four forwarded and two waiting without a place; meanwhile
  `/hive/status`, worker1's versions, its node list and AI.pruebas' status answered in 0.0 to
  0.7 s; the process stayed at 15 to 16 threads; the motherbee's orchestrator restarted in 3.8 s
  with reads waiting, no SIGKILL. A node read right after `run_node` shows it. What is left of a
  deploy's update is A-250. Step 2 (node actions on different nodes at once) stays for later.

- **Seen again, 0.1.125's deploy (2026-11-10):** the installation update closed 131 s after its
  call, while the spokes settled by +42 s. The motherbee orchestrator received reads at 07:40:50 and
  07:40:53, then nothing until 07:41:39 (46 s), and again nothing from 07:41:42 to 07:42:31 (49 s):
  twice a read forwarded around a spoke orchestrator's restart got no answer, and the one-at-a-time
  worker waited out its 45 s bound with every other read queued behind. Each spoke answered within
  about a second once asked. With A-203 fixed this is the remaining cost of a deploy's update, about
  90 s here.

### A-198 [General/admin] ✅ VALIDATED ON 8.x (0.1.123, 2026-11-09; found 2026-11-09 while fixing A-197) — Other SY.admin mesh replies can outgrow the frame, and the caller then waits to its timeout

- **Why:** the SDK refuses a mesh frame over 128 KiB before sending it (`FRAME_TOO_LARGE`). The
  caller gets no answer, only its own timeout, and no reason.
- **Near the limit today (sizes estimated from the code):**
  - `list_cloud_actions`: 67.7 KB, 53% of a frame, and it grows with each note.
  - `executor_execute_plan`: each step event carries its whole output and its args.
  - `list_ilks`: no paging, about 0.5–0.7 KB per ILK. It passes the frame at about 200–250 ILKs,
    across two hops.
  - `vault_list`: SY.vault's page of 200 summaries.
  - `list_recent_commands`: up to 500 entries, each with up to 2 KB of params.
  - `inventory` and `list_nodes`: about 1 KB per node.
  - `get_installation` and `/versions`: the list of versions grows without bound.
- **Proposal:** SY.admin answers `REPLY_TOO_LARGE` whenever a mesh reply would pass the frame, so
  the caller learns why at once. Paging follows where a list can grow. When is the operator's call.
- **The operator (2026-11-09):** in 0.1.123, beside stage 4, since both touch SY.admin.
- **Fix (0.1.123):**
  - SY.admin, SY.identity, SY.vault and SY.orchestrator measure each reply against the frame less
    a 1 KiB reserve for the router (A-202). One that does not fit is answered with
    `REPLY_TOO_LARGE`: its size, the most it may be, the action's outcome (so a write that ran says
    so) and how to page.
  - Optional `limit`/`cursor` pages: SY.identity's ILKs and tenants, SY.vault's list,
    SY.orchestrator's nodes, inventory and versions, the admin command log (by a sequence number)
    and the installation's versions. SY.admin assembles the pages, so a caller that asks for no
    page gets every row, as before.
  - An executor run's answer is cut to fit, its largest outputs first. Past that, the answer is
    `REPLY_TOO_LARGE` with the run's id and log path, and Archi says the plan ran.
  - Archi passes the pages; IO.cloud relays them on the lists Cloud uses.
- **Checked on 8.x (0.1.123's gate), over HTTP:**
  - ILKs 63 in 7 pages of 10;
  - the command log's 245 in pages of 20;
  - `/versions` 2 hives a page;
  - worker1's nodes 10 in pages of 3;
  - the vault's 12 secrets with a limit of 4.
  - Without `limit`, every row, as before: the ILKs in 24.6 KB (as measured before the release),
    `/versions` 29.7 KB, the command log's 50.

### A-197 [Archi] ✅ VALIDATED ON 8.x (0.1.122, 2026-11-09; found 2026-11-09 by 0.1.122's review round, seen in SY.admin's journal on 8.x) — Archi's catalog of SY.admin's actions did not fit the mesh's frame

- **Seen on 8.x:** SY.admin logged `FRAME_TOO_LARGE ... frame_len=337116 max=131072` 30 times in
  7 days, since at least 2026-11-05 (318 KB the first time).
- **What it broke:**
  - Archi's `list_admin_actions` over the mesh timed out at 5 s, both its plan compiler's catalog
    refresh and its tool read.
  - The plan compiler then ran with an empty catalog (`unwrap_or_default`), naming actions from
    the handbook alone.
- **Fix (0.1.122):** `list_admin_actions` takes a `view`.
  - **Over the mesh and to the executor, the compact one:** each action's name, description and
    `read_only`, 17.5 KB. The full catalog is 352 KB.
  - **Over HTTP, the full one.** `full` asked over the mesh is refused
    (`CATALOG_TOO_LARGE_FOR_MESH`), not left to time out.
  - **Archi asks for the compact view.** A test keeps that reply under 64 KiB.
- **Checked on 8.x:** during 0.1.122's gate, Archi's plan compiler fetched the compact catalog in
  13 ms. No FRAME_TOO_LARGE since the deploy.

### A-196 [General/join] ✅ VALIDATED ON 8.x (0.1.127, 2026-11-10; the marker during a join's link, gone after its restart) (found 2026-11-08 by A-194's review round; read in the code) — A join's peer link can be written to Syncthing's config and never applied

- **Why:** `ensure_syncthing_peer_link_runtime` writes the peer into config.xml, then restarts
  Syncthing, and skips the restart when nothing changed.
- **The case:** the restart fails, or the orchestrator dies between the write and the restart. A
  resumed finalize then finds nothing changed and does not restart, so the running Syncthing keeps
  the config without the peer until something else restarts it.
- **The peer reconcile** keeps a `syncthing-restart-pending` marker for exactly this; the join's
  link has none.
- **Fix:** the same marker for the join's link. Not seen on 8.x.

### A-195 [General/router] ✅ VALIDATED ON 8.x (0.1.132, 2026-11-11; a sender 1 s old reached worker1, all its frames dropped on 0.1.131; A-261 and A-262 with it; most `foreign_source` lines were A-272's echoes) (found 2026-11-08 by 0.1.118's gate, its timeline) — WAN frames are dropped at each link-up, before the peer's LSA names their sender

- **Seen:** when `gate0218`'s WAN link came up, the motherbee's router and `gate0218`'s each
  dropped a frame as `foreign_source` ("its sender is neither a node of the peer that sent it nor
  one that peer relays as the hub"), with no `src_l2_name`.
- **How often:** the motherbee's counter stood at 43 since its router last started, and its
  journal has 57 such lines in 7 days. The line is rate-limited, so there were more drops than
  lines.
- **Why, read in the code:** the router binds a WAN frame's sender to the peer's LSA bucket
  (EDGE-01, `wan_frame_source`). A frame that arrives before the LSA naming its sender is dropped.
- **To find out:** which frames these are (the router stream's records? a node's first message?),
  and whether any is a request whose sender then waits for a timeout. Health counted no stream
  loss in the window.
- **Built (0.1.132):** a new link's first frames are the router's LSA and, from the hub, its
  reachability, queued before the link goes into the WAN table; the hub tells every spoke of a
  hive's changed nodes as its LSA comes, not at its next 10 s beacon, and its advertisements go out
  one at a time, in the order they were built.
- **Checked on 8.x (0.1.132):** a sender that connected on ingress1 1 s before sending to a node of
  worker1 reached it (on 0.1.131 worker1 dropped all its 2,600 frames as `foreign_source`, and the
  sender got nothing), and ingress1 knew the new node within 1 s (0.1.131: `NODE_NOT_FOUND` for more
  than 5 s). No link-up drop was logged at the deploy's link-ups. The `foreign_source` lines this
  entry counted were mostly A-272's echoes of broadcasts, not link-ups.

### A-194 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.119, 2026-11-08; found 2026-11-08 by 0.1.118's gate, its timeline; read in the code) — The Syncthing watchdog restarts Syncthing while a join's finalize is restarting it

- **Seen:** during `gate0218`'s finalize, the peer link stopped Syncthing to restart it, and 89 ms
  later the watchdog found it inactive ("syncthing unhealthy; restarting") and restarted it too.
  It came back, and the join went on.
- **Why:** `watchdog_blob_sync` does not look at `SYNCTHING_FINALIZES_IN_FLIGHT`, the guard that
  keeps the peer reconcile away from a finalize. Its restart runs `ensure_blob_sync_runtime`,
  which can also rewrite the folders in config.xml while the finalize links the peer.
- **Fix:** the watchdog's Syncthing restart waits while a finalize is in flight on a spoke, as
  the peer reconcile already does. Small; for the orchestrator's next release.
- **The operator's call (2026-11-08):** *"Sí, seguí como planteás"*: A-194 first, then stage 3.
- **Built for 0.1.119:** one check, `a_join_owns_syncthing`, true on a spoke while its finalize runs
  and on the motherbee while an `add_hive` or `remove_hive` does. The watchdog takes it before it
  reads the desired config, and again right before it restarts Syncthing; the peer reconcile takes
  it in both of its places.
- **Its review round:** no high or medium. Its lows were fixed: the second check before the
  restart, the motherbee's side, and the one check everywhere. It also found A-196.
- **Validated on 8.x (0.1.119):** a throwaway worker, `gate0219`, joined end to end, timed.
  Inside its finalize Syncthing restarted once, for the peer link, and the watchdog left it
  alone; on 0.1.118 it had restarted it again 89 ms later.

### A-193 [General/deploy] 🔵 TO EVALUATE (found 2026-11-08, the first timed deploy and join, 0.1.118) — Where a deploy and a join spend their time

- **The deploy (263 s):** snapshots 29 s; apt install on the motherbee 126 s; its admin on the
  version 47 s; the spokes' updates 15 s, installed 24 s, settled 8 s; health 5 s.
- **Inside the apt install:** about 40 s to fetch and unpack. Then the package's scripts kept
  sy-orchestrator, sy-admin and sy-wf-rules down for 65 s (the unpack and the base runtimes'
  seeding): Archi and the operator cannot act on the motherbee then. The orchestrator's start then
  took 25 s to rebuild the three role trees, hashing the same binaries once per role
  (`materialize_core_tree`), before 12 s of restarts.
- **A join (127 s):** the bootstrap's copy over SSH about 67 s; the spoke's start about 15 s; its
  finalize 8 s. At the end the motherbee restarts its own Syncthing to add the peer, which drops
  every spoke's sync for a moment.
- **To evaluate, by cost and benefit:**
  - hash each binary once per role-tree rebuild;
  - time the package's scripts, to see where the 65 s go;
  - add a peer to the motherbee's Syncthing through its REST, without a restart.
- **0.1.123's deploy (392 s, the first through the installation's update):** snapshots 34 s; apt
  install 146 s; the motherbee's admin on the version 64 s; the update's call 29 s; the hives on
  the target 94 s. About 85 s of the last came from one lost read (A-203, with A-199).
- **A hive back after its router was down a while (0.1.123's gate, 2 min):** the SY nodes that kept
  running reconnect on the SDK's backoff, 100 ms doubling to 30 s
  (`node_client.rs:211`). sy-config-routes reconnected 20 s after the router came back. The
  orchestrator waits for its lifecycle nodes before it connects, so the hive showed to the
  motherbee 20 s late.

- **0.1.124's deploy (2026-11-10), 413 s:** the apt install on the motherbee took 263.8 s: about
  157 s of `apt-get update` and the download of the 242 MB package from fb-build's repo (about 93 s
  on 0.1.123), then dpkg 82 s (unpack 64 s, configure 15 s), with the motherbee's orchestrator down
  80 s inside it. The installation's update took 46.6 s. The build took 26.5 min (12-15 min
  before).
- **The operator (2026-11-10):** *"ese es todo un tema, identificando porque tarda despues hablamos de los problemas"*: first find why each step takes its time, then talk; nothing built in the sweep.

### A-192 [General/vault] 🟢 LAB NOTE, no change (found 2026-11-08, 0.1.117's deploy on 8.x; read in its journals) — A running IO node's secret read is refused for a moment at a motherbee restart

- **Seen:** at SY.vault's start, its `VAULT_SECRET_CHANGED` reaches `IO.slack.gate0192@motherbee`,
  which is still running. Its `VAULT_GET` comes before SY.identity holds its ILK in the SHM, and is
  refused with `IDENTITY_UNAVAILABLE` ("which the identity SHM does not hold (yet)").
- **The node treats it as transient** and keeps its credentials. 13 s later it restarts on the new
  version and loads them.
- **It shows at every motherbee restart,** since at least 2026-11-03, as a WARN on both sides.
  Nothing to change: the code already treats it as transient.
- **SY.vault's WARN** reads *"vault request refused: the caller's ILK is not bound to the sending
  node"*: the line it logs whenever it cannot bind a caller's ILK to the sending node, this
  `IDENTITY_UNAVAILABLE` and a mismatch's `UNAUTHORIZED` alike, its `error` field saying which
  (`src/bin/sy_vault.rs` ~448-463, `check_caller_binding` ~1886). Seen so at 0.1.138's deploy
  (2026-11-12, about 13:26:56Z), where `ops timeline` cuts the line before its `error`.

### A-191 [General/identity] ✅ VALIDATED ON 8.x (0.1.133, its throwaway worker run on 0.1.134, 2026-11-11; SY.policy's retired at SY.identity's start; worker2's and six removed workers' retired by `retire_hive_ilks`, a removed worker's by `remove_hive`; none deleted) (found 2026-11-08 by 0.1.117's gate on 8.x; read there) — A removed hive's and a retired node's system ILKs stay registered

- **Seen:**
  - after `remove_hive` of `gate0217`, its six `SY.*@gate0217` system ILKs stayed on the
    motherbee;
  - `worker2`'s seven have stayed since its removal on 2026-10-06;
  - `SY.policy`'s, on the motherbee, worker1 and worker2, have stayed since 0.1.79 retired that
    node.
- **Why:** SY.identity's seed only adds. Well-known system ILKs are never reconcile-removed, on
  purpose, against forgery. `remove_hive` deletes the hive's identity key but not its ILKs.
- **Impact:** none seen. They are deterministic, so a hive joined again under the same name
  reuses them. Data hygiene, next to A-159 (deleting an ILK that other data names); a release
  that drops a node from a role leaves its ILK too (A-181, stage 2).
- **The operator's call (2026-11-10, the sweep):** old ILKs that made no history are deleted (*"si son ilk viejos, borralos cuando puedas"*); an ILK that made history is disabled, never deleted (*"los ilk son historia... podemos hacer disable pero no borrarlos sino rompe"*). So: `remove_hive` disables (marks, reversibly) the removed hive's system ILKs and never purges them; the throwaway gate workers' and worker2's are purged once; SY.policy's, which ran in PROD for months, are only disabled.
- **Checked on 8.x (0.1.133; its throwaway worker on 0.1.134, from 23:51Z):** before the deploy
  only SY.policy's ILKs on the motherbee and worker1 were outside 0.1.133's lists; SY.identity
  retired them at its start, one line each (the motherbee 22:39:00Z, worker1 22:39:44Z).
  `retire_hive_ilks` retired worker2's 7 and 6 each of `gate0217`, `gate0218`, `gate0219`,
  `gate0221`, `gate0227` and `gate0231` (22:51Z, 1.2 to 3.0 s each), none deleted (since the
  operator's call of 2026-11-11 the system retires a system ILK and never deletes one; a purge
  stays the operator's explicit word); a repeat answered count 0, `already_retired` 7 or 6; worker1
  and the motherbee were refused `HIVE_NOT_REMOVED` (with what to do), a bad id `INVALID_REQUEST`,
  none echoing the id. The throwaway worker `gate0233` (VM 104) joined in 148 s with its 6 system
  ILKs active; `remove_hive` (14.1 s) answered `system_ilks_retired` 6 and `identity_key_cleanup`
  deleted, and 35 s later all 6 were still retired. Joined again under the same name (131 s; the
  gate's first try failed `SSH_AUTH_FAILED` until it re-ran its prep, the gate's slip) the 6 were
  active again; removed again (4.4 s), retired.
- **The purge (the operator's call, 2026-11-12, asked with the 63 retired system ILKs counted on
  8.x: 54 of nine removed gate workers, worker2's 7, SY.policy's 2):** *"Purge the 61 from removed
  workers"*, only those with no history. `purge_ilk` does not read history, so each one was read
  first, over the whole record: no usage record among the 1,289 since 2026-09-01 (by node name,
  node ILK or counterpart), no turn in SY.storage naming it (from, to or writer), and no
  communication in SY.cognition's on worker1 or the motherbee; all 61 read free. Purged
  15:20–15:23Z, 61 of 61 ok: worker2's 7, and 6 each of `gate0217`, `gate0218`, `gate0219`,
  `gate0221`, `gate0227`, `gate0231`, `gate0233`, `gate0237i` and `gate0237j`. After it the
  motherbee's identity and worker1's replica both list 19 system ILKs, only SY.policy's 2 marked
  (`retired: node`), which stay retired; none of a removed hive is left. The tool:
  `scratchpad/a191_purge.py` (the session's, outside the repo), dry run unless `--apply`.

### A-190 [General/join] ✅ VALIDATED ON 8.x (0.1.118, 2026-11-08; found 2026-11-08 by 0.1.117's gate on 8.x; read in the code and the spoke's journal) — A fresh worker's join fails at its finalize

- **Seen:** `gate0217`, a throwaway worker on a fresh clone of template 9000. Its `add_hive`
  failed twice with `FINALIZE_FAILED`:
  - the first time, the motherbee timed out after 45 s, while the spoke's own result was
    `SYNC_SETUP_FAILED`: "vendor manifest missing at '/var/lib/fluxbee/dist/vendor/manifest.json'";
  - the resumed join, over the socket only, got the same `SYNC_SETUP_FAILED` back;
  - every dist folder on the spoke was empty afterwards.
- **Why:**
  1. The join's bootstrap puts the role's dist files on the new spoke, and starts its
     orchestrator.
  2. The orchestrator's start declares the dist folders, receive-only, before any peer exists.
     Syncthing reports every file there as a local change of a receive-only folder.
  3. The first watchdog tick, half a second after the hive is ready, reverts such a folder
     (`check_syncthing_folder`, A-16, 0.1.55). With no source known yet, the revert deletes the
     files: dist/vendor's 4 at 19:53:02, dist/core's 10 at 19:53:11.
  4. The motherbee's `ADD_HIVE_FINALIZE` runs a core update, then a vendor update
     (`add_hive_finalize_local_flow`). The vendor update is strict and finds no manifest. The
     peer link that would bring dist back comes after it, so a resume cannot get past it.
  5. The finalize's core update restarts every core service, rt-gateway first, even with nothing
     changed. The spoke's reply then goes out on a dead router connection and is lost. That is why
     the first attempt read `FINALIZE_FAILED` after 45 s, not the spoke's own error.
- **Not 0.1.117's doing:** stage 2 changed none of these functions (the finalize, the revert, the
  peer link), only the hive.yaml the join writes. 0.1.116 has the same code. When it began is to
  be found: worker2 joined on 0.1.57, after A-16's revert, through a resumed join.
- **Impact:** no new worker can join 8.x today. The four hives already joined are not affected.
- **Directions for the fix,** the operator's call before building:
  - the revert leaves alone a folder whose source peer does not exist yet, or the bootstrap's
    files do not count as local changes;
  - the finalize's vendor step tolerates a vendor still syncing, as its core step does;
  - the finalize does not restart the router under its own reply, or a no-op core update
    restarts nothing.
- **The operator's call (2026-11-08):** *"Coincido, avanzá con eso"*, and from now on every gate
  measures where the time goes.
- **Built for 0.1.118** (`src/bin/sy_orchestrator.rs`):
  - **The revert** waits for its source: a member other than this device, connected, sharing
    the folder (`/rest/db/completion`'s `remoteState: valid`), for 60 s in a row. Until then the
    local items stay, logged once.
  - **The finalize's vendor step** goes on when it failed on a vendor manifest or binary still
    missing, with a Syncthing installed and running. It is reported in `vendor.awaiting_update`,
    and the recheck closes it.
  - **The finalize's core step** takes A-186's "core binary 'X' missing at" as pending too.
  - **A core update with no binary and no unit new restarts nothing,** and answers `ok` at once.
  - **The finalize's dist probe** gets what is left of the motherbee's wait, less 10 s for the
    reply.
  - **Times:** the finalize logs when it arrives and how long it took, and answers each step's
    `timings_ms`. `ops timeline` merges every hive's journals into one timeline, and
    `ops deploy` prints each phase's time.
- **Its review round:**
  - **Medium, fixed:** the first build reverted once the peer was a member, not once its index
    was in. The finalize's own peer link restarts Syncthing, so the next tick, before the
    motherbee's index arrived, would still have deleted dist for about a minute. The 60 s with
    the source connected and sharing is the fix.
  - **Low, fixed:** the vendor tolerance looks at the step's error and needs Syncthing running,
    not only installed; the held folders are logged once; the timeline takes WARN and ERROR,
    reads its window as UTC, and streams the journal with a cap; stale texts.
  - **Low, left:** a finalize whose core step does change something (a join resumed after the
    motherbee was upgraded) still restarts the router before its reply; the retry is then a
    no-op and goes through.
- **Validated on 8.x (0.1.118):** a throwaway worker, `gate0218`, joined end to end, timed:
  - `connected` at 113 s, the join done at 127 s, `dist_sync_ready: true`;
  - the finalize took 8.1 s (core 5.1 s with nothing restarted, vendor 1.1 s, peer link 1.4 s,
    dist probe 0.4 s), and its answer arrived;
  - the watchdog held the bootstrap's local items until the source was there. Syncthing then took
    the motherbee's versions over the identical files within a minute, so no revert ran, and dist
    stayed whole;
  - removed afterwards, the motherbee left as before.

### A-189 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.116, 2026-11-08; found 2026-11-08 building A-186; read in the code) — A spoke caught mid-sync of a release that changes the vendored Syncthing never runs Syncthing again

- **What happens:**
  - The start and the watchdog bring Syncthing up through `ensure_blob_sync_runtime`. It calls
    `ensure_syncthing_installed()?`, which needs `dist/vendor`'s Syncthing to match the vendor
    manifest (`local_syncthing_vendor_hash`, `resolve_syncthing_vendor_source_path`,
    `src/bin/sy_orchestrator.rs:6882`, `:6901`, `:9070`).
  - While a release that changes Syncthing is half-synced, both return an error. The Syncthing
    already installed is then never started.
  - The start goes on: it only warns and relies on the watchdog. But the watchdog takes the same
    path, and `dist/vendor` can only be completed by Syncthing itself.
- **When:** a spoke reboots in the minutes in which such a release syncs. That is rarer than A-186,
  since the vendored Syncthing changes seldom.
- **Impact:**
  - The spoke stays on the mesh (after A-186), but it never gets a dist update again.
  - `update category=vendor` goes through the same function, so the only way out is SSH.
- **Built, for 0.1.116, with A-186:**
  - The start, the watchdog and the sync probes keep the installed Syncthing and report
    `vendor.awaiting_update`.
  - With none installed, they fail as before, but never the orchestrator.
  - `update category=vendor` and an ingress join stay strict.
- **A release rule, from its review:** Syncthing's unit (`syncthing_unit_contents`) must stay valid
  for the previous vendored Syncthing. On a hive whose vendor lags, the start writes the new unit
  and restarts the old binary under it.
- **Validated on 8.x (0.1.116, egress1):**
  - **Vendor set aside:** with the vendored Syncthing set aside, the orchestrator's restart kept
    the installed Syncthing and reported `vendor.awaiting_update`.
  - **As after a reboot:** with Syncthing stopped as well, the start brought it up in 3 s, where
    before it stayed down.
  - **Back:** with the binary back, the report closed in 3 s.

### A-188 [General/blob] ✅ VALIDATED ON 8.x (0.1.127, 2026-11-10; the GC only on the motherbee, a spoke's `blob.gc` warned about and ignored) (found 2026-11-08 by hive-config-update-v1's DTAP panel, T-16; read in the code) — A worker's blob GC, if turned on, deletes the motherbee's published sources through the shared folder

- **What happens:**
  - The blob GC pins the sources of live publications from io.blob's ledger. The ledger lives on
    the motherbee, where io.blob runs.
  - On a worker the ledger is absent, so the GC runs with no pins: it only warns
    (`run_blob_gc_housekeeping`, `src/bin/sy_orchestrator.rs:16575`).
  - `fluxbee-blob` is send-receive, so a worker's deletions reach the motherbee, and a published
    page loses its source.
- **Today:** worker1's `blob.gc.enabled` is false. Only a hand edit turns it on, since only the
  join writes it.
- **Fix (proposed):** the GC runs only where it can pin, the motherbee. hive-config-update-v1 keeps
  `blob.gc` the motherbee's own (§12).

### A-187 [General/CI] ✅ FIXED (2026-11-08, CI only: `98ef73cc`; found 2026-11-08 by hive-config-update-v1's DTAP panel, T-14; read in the workflow) — A change to packaging files alone runs none of the Rust tests that read them

- **What happens:** `rust-tests.yml`'s path filters leave out `packaging/**`. Yet Rust tests read
  three packaging files: `packaging/hive.yaml.example`, `deb-postinst` and `deb-preinst`
  (`include_str!` at `src/bin/sy_orchestrator.rs:32679`, `:35316` and `:35323`). A branch that
  edits only those files gets no Rust test.
- **Fix:** add `packaging/**` to both path lists.
- **Fixed:** both lists have it. The fix's own branch ran rust-tests with no Rust change in it.

### A-186 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.116, 2026-11-08; found 2026-11-08 by hive-config-update-v1's DTAP panel, P-1 and A-8; read in the code) — A spoke that restarts while a release is still syncing never starts again

- **What happens:**
  - At its start, a spoke's orchestrator checks every binary of its role in `dist/core/bin`
    against the dist core manifest: hash and size (`bootstrap_local` →
    `validate_core_manifest_for_bins`, `src/bin/sy_orchestrator.rs:1662`).
  - `main` exits on a mismatch (`:1039`).
  - Syncthing, the only thing that brings the missing bytes, is started further down in that same
    start. The join enables only rt-gateway and sy-orchestrator (`:26085`).
- **When:**
  - A spoke reboots after Syncthing delivered a release's manifest but before all its binaries.
  - Or a release drops a node from a role while a spoke's file still lists it: the motherbee
    prunes that binary from the role's tree, and the spoke's start fails on it.
- **Impact:**
  - The orchestrator fails every 5 s, Syncthing never starts, and no update can reach the hive.
    Only SSH by hand repairs it.
  - It is rare: it needs a reboot during the minutes a release syncs. When it happens, the hive is
    lost.
- **Fix (proposed):** the start never stops on a dist mismatch. It starts on what is installed in
  `/usr/bin`, starts Syncthing, and reports that it is waiting for its update.
- **Built, for 0.1.116:**
  - The check runs after Syncthing starts, and it only reports: `core.awaiting_update` in
    `/versions`, with why each binary failed. It is checked again every 60 s.
  - The core update's own check stays strict.
  - The motherbee copies a binary into a role's tree only once it matches the manifest.
- **Its review round** (with A-189's): no high or medium finding. Four stale lines in the docs were
  fixed.
- **Validated on 8.x (0.1.116, egress1):** a role binary of dist was set aside and the orchestrator
  restarted.
  - It started with no restarts, and Syncthing and the router were untouched.
  - `/versions` listed the binary as `missing`.
  - With the binary back, the report closed in 27 s.

### A-185 [WF] 🟢 NO CHANGE, the operator's call (2026-11-08; found 2026-11-08, 0.1.112's gate on 8.x; read in the journal and the code) — The first agent on a thread gets what people said only from its second message on

- **Seen on 8.x (gate0206, thread `gate0206-take`):** A wrote to B through the WF, B answered, then
  A wrote to the agent. The agent's first message carried no memory package
  (`memory_package=false` in its journal). Cognition then took the people's communication into the
  agent's (A-173), and the agent's next message carried it: asked what A had asked billing, the
  agent answered it.
- **Why:** the router attaches the agent's package at delivery, from the memory region, keyed by
  the thread and the agent's node. The agent's communication does not exist yet: cognition makes
  it, and takes the people's, when that same message reaches it through the stream. The thread's
  communication with no agent is not in the region (its package reaches no one).
- **The call:** whether the first message must carry them.
  - One way: the region keeps a thread's communication with no agent too, and the router hands its
    package to an agent that has none on the thread. That is the router (alone) and the region.
  - The other: as it is, from the agent's second message on.
- **The operator's call (2026-11-08):** as it is, *"185 déjalo así"*. The docs say so
  (reference/cognition.md §1, cognition-hands-v1 §12.6).

### A-184 [General/AI SDK] ✅ VALIDATED ON 8.x (0.1.115, 2026-11-08; found 2026-11-07 by the sweep of pending items, read in the code 2026-11-08) — A provider's HTTP error is untyped, OpenAI never retries a 429 or a 5xx, and SY.cognition counts one as an invalid output

- **OpenAI never retries:** both of its paths, the plain answer and the tool turn
  (`crates/fluxbee_ai_sdk/src/llm.rs`), return the first 429 or 5xx as an error. Anthropic's path
  retries them (`anthropic_should_retry`, `anthropic_retry_delay`, honoring `retry-after`).
- **It reads the body before the status:** `openai_read_answer` decodes the body as JSON first, so
  a proxy's 502 with an HTML body fails as a decoding error and its status is lost. Anthropic's
  path was fixed for this by its audit's M1.
- **The error is untyped:** `openai_status_error` returns `AiSdkError::Protocol("openai error
  status=… type=… param=… message=…")`, and its consumers parse that text: ai.generic's and the
  frontdesk's runners raise `provider_attachment_invalid_request` from it.
- **SY.cognition counts it as an invalid output:** `handle_turn` counts `Json` and `Protocol`
  errors in `semantic_tagger_invalid_outputs_total`, so a provider's 429 reads as a model that
  answered badly.
- **A refusal is untyped too (0.1.112's gate):** asked to copy its Recent conversation word by
  word, Claude answered `stop_reason=refusal`; the runner sent a provider error payload, which the
  WF did not relay, so the person got nothing. A refusal is the model's answer, not a protocol
  error.
- **Fix (the AI SDK, alone):** a typed provider error with its status, a refusal of its own, OpenAI's
  retries as Anthropic's, the status read first, and the consumers matching the type instead of the
  text.
- **The operator's calls (2026-11-08):**
  - SY.cognition does not judge a provider's error: it is no invalid output (*"errores del
    proveedor cognition no debéis evaluarlo, sí"*).
  - The person gets one fixed message in English: the AI provider of the agent's key has a
    problem, and they should contact the administrator. *"Sería un mensaje fijo y no mucho
    más."* It goes out for every provider error that leaves the person with no answer, the
    refusal included. The other fixed replies, in Spanish, stay on the sweep's list.
- **Built:**
  - `Provider` errors with their status, kind and request id.
  - `Refused`, `MissingKey` and `NoAnswer`, each its own error.
  - OpenAI's retries, one policy with Anthropic's, with the status read first.
  - The runners match the type, not the text.
  - The fixed text goes as the agent's text with no contract. Under a WF's menu or a response
    envelope it rides inside the error payload's `reply`, and the cookbooks' catch and atom 6 send
    it.
  - SY.cognition's `*_provider_errors_total` counters.
- **Its review round:**
  - One high finding: under a menu, the fixed text reached no one.
  - Three lesser ones: OpenAI's content filter, the retry's log line, and a test.

  All were fixed before the release.
- **Validated on 8.x (0.1.115):**
  - A throwaway agent in a tenant with no key answered a person's message with the fixed text, as
    a plain text reply. Its log names the missing key.
  - AI.pruebas answered.
  - Archi took the new seeds.
  - Cognition shows the new counters.
- **Left for SY.admin's next release:** its help text says that with no key every message fails
  with `missing_ai_api_key`. That holds for an envelope and for the log, but no longer for a
  person (`src/bin/sy_admin.rs:14604` and `:14644`, `docs/nodes/sy-admin.md:21`).

### A-183 [General/Go SDK] ✅ VALIDATED ON 8.x (0.1.114, 2026-11-08; found 2026-11-07 by the sweep of pending items, read in the code 2026-11-08) — A Go node says HELLO twice at start, and a reconnect's handshake has no deadline

- **Twice at start:** `connect` handshook on a first connection under a 5 s limit, closed it, and
  its `connectionManager` opened a second one with a second HELLO. The router saw the node
  register, leave and register again.
- **No deadline after:** the manager's handshake at every reconnect had none, so a router that took
  the connection and never answered held the node forever.
- **Fix (`go/fluxbee-go-sdk/client.go`):** the connection of the first handshake is the one the
  manager serves; `handshakeWithin` bounds every handshake by 5 s. A test with a fake router saw
  the node's first message on its second connection before the fix, and on its first after.
- **Validated on 8.x (0.1.114):** the motherbee's Go nodes registered twice since their start on
  0.1.113, once on 0.1.114.

### A-182 [General/Rust SDK] ✅ VALIDATED ON 8.x (0.1.113, 2026-11-08; found 2026-11-07 by the sweep of pending items, read in the code 2026-11-08) — A Rust node's handshake has no time limit

- **What happens:** `perform_handshake` (`crates/fluxbee_sdk/src/node_client.rs`) sends the HELLO
  and waits for the ANNOUNCE without a limit. A router that takes the connection and never answers
  holds the node in its handshake forever, at the first connection and at any reconnect.
- **Fix:** 10 s (`HANDSHAKE_TIMEOUT`), past which the handshake fails and the reconnect loop goes
  on.
- **Validated on 8.x (0.1.113):** every node joined its router again after the deploy (36 active,
  as before); a handshake took 11 ms.

### A-181 [General/blob] ✅ ALL STAGES VALIDATED ON 8.x, the operator's direction (stage 1 ✅ 0.1.116, stage 2 ✅ 0.1.117, stage 3 ✅ 0.1.120–0.1.122, stage 4 ✅ 0.1.123, stage 5 ✅ 0.1.127; 2026-11-08; docs/hive-config-update-v1.md; found 2026-11-07 by the sweep of pending items, read in the code 2026-11-08) — A blob folder turned on after the joins is shared with no spoke

- **The sweep's item:** a blob folder turned on after the spokes joined is never shared with
  them.
- **Read in the code, it is more than a bug:**
  - no action turns the blob's sync on after the install: `set_storage` changes only the storage
    path, so it is hive.yaml's `blob.sync.enabled`, by hand;
  - a spoke's hive.yaml is written once, at its join, with the motherbee's blob settings of that
    moment, and nothing writes it again (SSH lives only inside add_hive, D32);
  - each hive's reconcile declares the folder, but only a join adds a peer to it, on purpose: a
    flag turned off for a while strips no one (`folder_forbidden_for_role`).
- **So turning it on later takes every hive's hive.yaml edited by hand,** and even then no peer
  is added.
- **Two halves:**
  - the members are a fix like A-72's: each blob folder follows the members of the dist folder
    with its audience;
  - the setting's way to the spokes is a design question: how a motherbee setting reaches the
    hives that joined before it.
- **Turned off later, it stops nothing either:** the reconcile only adds folders, and a hive's
  removal takes it out of the blob folder only while the flag is on
  (`reconcile_syncthing_peer_removal_xml`).
- **On 8.x (read 2026-11-08):** the motherbee shares `fluxbee-blob` with worker1 and
  `fluxbee-blob-public` with ingress1, and egress1 has neither. A fresh install copies
  `hive.yaml.example`, whose sync is on, so A-181 bites only an installation that turns the sync
  on after its joins.
- **The same gap bites in two more places (read 2026-11-08):**
  - SY.cognition on a worker reads its AI model from its own file, written at its join. worker1's
    was edited by hand at 0.1.72.
  - A spoke installs and runs the SY nodes of its own `system_nodes`
    (`core_component_names_for_role`). A release's new SY node for workers would reach none of them
    until each file is edited by hand. The motherbee's own file needs it too: the .deb never touches
    an existing `hive.yaml`.
- **The operator's direction (2026-11-08):** a change to the hive's config is an update, so it goes
  through the update channel (*"Todo lo define una versión de software … quedaría todo dentro del
  canal de update"*). It carries what is general to every hive, and what goes by the hive's role
  (*"… el JSON que diga el cambio según el perfil del hive"*).
- **The design's first draft:** [hive-config-update-v1](../../docs/hive-config-update-v1.md).
  - The core update carries the hive's shared config: the state, with its hash.
  - Each orchestrator applies it to its own file.
  - The control path never travels.
- **The operator's calls on the draft (2026-11-08):**
  - The motherbee's file takes the release's keys at the install: yes, with one rule. Every hive's
    config is at one version (*"no puede pasar que los archivos de config entre hives sean de
    distinta versión"*).
  - The keys stay as listed.
  - The blob's switch goes.
  - The worker that joined before the egress is apart, for later.
- **Next:** the update path's current state, a staged plan and a DTAP panel, before anything is
  built.
- **The DTAP panel (2026-11-08):** 70 findings, four blockers. §12 of the doc proposed the second
  draft's shape.
- **The operator's call on it (2026-11-08):** *"Sí, vamos como describiste"*.
  - The release's keys go into the code, by role.
  - The operator's settings go into one installation file, distributed like the policy and
    applied through the update channel.
  - hive.yaml becomes the hive's own.
  - The installation's update becomes SY.admin's. Its automatic step only finishes an open update
    for a hive that comes back, and never touches a hive the operator rolled back.
- **The second draft** is in the doc. Its stages start with A-186.
- **Stage 1** (A-186, A-189) ✅ 0.1.116.
- **Stage 2 ✅ VALIDATED ON 8.x (0.1.117, 2026-11-08):** the release's settings live in the code, by role,
  in `src/release_settings.rs`: each role's SY nodes with their order and waits, and the blob's and
  dist's topology. The values are today's, held by golden tests against what the example and the
  joins wrote. All four 8.x hives ran those lists and flags, read on each before the build.
  - The orchestrator, SY.identity and SY.vault read the table, never hive.yaml.
  - The joins and `hive.yaml.example` stop writing those keys. An old hive.yaml still loads, and
    the orchestrator names the keys it ignores once at its start.
  - A node a release drops: the previous orchestrator's update leaves it out, only when it
    replaces sy-orchestrator. The new orchestrator's start (the "settle") stops, disables and
    removes it, keeping unit and binary in the update's generation. A node a release adds: the
    new orchestrator's start installs it from dist. `core_rollback` undoes both.
- **Its review round** (one reviewer; no high):
  - **Medium, fixed:** the old keys' warning said they may be removed. An orchestrator of 0.1.116
    or earlier does not start without `system_nodes`, so a hive without them cannot go back below
    0.1.117. Now the warning and the docs say to keep them, and the HANDBOOK says how to put them
    back on a hive joined by 0.1.117 or later before a downgrade.
  - **Medium, fixed:** a race where the previous orchestrator's dist recheck could settle the new
    update's generation with its own, older list. Now the recheck settles only the update the
    orchestrator's own start left open, with a test.
  - **Low, fixed:** `ops deploy` judged a spoke by the hash its update installed. Now it also
    waits for the new orchestrator's settle, and reports anything left out, pending or awaiting.
  - **Low, fixed:** stale texts, and a dead check in the ingress join.
  - **Low, documented:** while Syncthing has not yet deleted a dropped binary, the update refuses
    it as any binary still syncing; it goes through when tried again.
- **Left for when a release needs it:** a node dropped from the motherbee's own list (dpkg owns
  its core, so its package has to stop it); a dropped node's system ILK stays registered
  (SY.identity's seed only adds).
- **Left for their own releases:**
  - the Rust SDK's doc comments that still say "hive.yaml system_nodes"
    (`crates/fluxbee_sdk/src/identity.rs`, `managed_node.rs`);
  - the router's dead read of the blob flags, with `government.identity_frontdesk` into the code;
  - SY.vault's log line, which still says its well-known SY ILKs are "computed from hive.yaml"
    (`src/bin/sy_vault.rs:256`): with SY.vault's next change.
- **Checked on 8.x (0.1.117):**
  - nothing changed on the four hives (units, Syncthing folders and members, unit files, the
    keys in each hive.yaml); each orchestrator named its old keys once; each spoke settled 0.1.116's
    update with nothing dropped or added;
  - a throwaway worker, `gate0217`: its hive.yaml held none of the release's keys, and it ran the
    worker's six SY nodes, seeded their six system ILKs and declared the worker's Syncthing folders,
    all from the table;
  - its join failed at the finalize, for a reason stage 2 does not touch: A-190 🔴.
- **Stage 3, in three releases** (2026-11-08; the AI SDK goes alone, METHOD §9):
  - **3a, ✅ 0.1.120 on 8.x (deploy neutral, AI.pruebas answering), the AI SDK alone:** the installation file's type and reader
    (`fluxbee_ai_sdk::installation`). It is valid as every reader needs it, with every provider
    named having its model. Its state (absent, valid, or invalid with its hash) is there for
    `/versions`. `ai_settings` reads hive.yaml's `ai` only while the file is absent. No reader
    uses it yet.
  - **3a's review round:** no high. Two mediums on the contract 3b relies on: an invalid file
    must not stop the starts that report it (SY.admin, the orchestrator), and the hive.yaml
    fallback must be said and must end. Its lows were fixed in the SDK, since a later SDK change
    would need a release of its own: `validate` in `render` too, the file's state with its hash, a
    lazy fallback, the path from the hive's dist.
  - **3b, ✅ 0.1.121 on 8.x (every hive `current`, every reader on the file, a joined worker on it from its first start):** the motherbee's orchestrator writes the file as version 1 from its
    hive.yaml `ai` (recorded first, written atomically, 0644). A new folder,
    `fluxbee-dist-installation`, follows vendor's members, and the join copies the file and
    writes no `ai`. SY.admin, SY.architect, SY.cognition, SY.frontdesk.gov and ai.generic read it,
    each logging its source. An invalid file fails the AI work with the reason, and never stops
    SY.admin's or the orchestrator's start. `/versions` gives each hive's `config` (`current`,
    `behind`, `pending`, `edited_by_hand`). Nothing changes in behavior.
  - **3b's review round:** no high.
    - **Medium, fixed:** the install texts still told the operator to edit hive.yaml's `ai` after
      firstboot, which 3b ignores. They now say to choose it before firstboot, and how to take
      hive.yaml's `ai` again while version 1 is the only one (delete the file, restart the
      orchestrator).
    - **Lows fixed:** the join refuses while the motherbee holds no file; SY.admin keeps its
      executor when hive.yaml cannot be read during a refresh; the ignored-`ai` warning says
      rt-gateway still checks it; Archi's handbook and SY.admin's notes name the installation
      file's `ai`.
    - **Left:** rt-gateway's check of hive.yaml `ai` goes with the router's own release. Between
      3b and 3c, the operator's only way to change `ai` is the delete-and-restart above.
      Downgrade notes are in the HANDBOOK.
  - **3c, ✅ 0.1.122 on 8.x (a change set and undone, the undo through Archi; every hive current in under 3 s; restarts only where a reader's engine is read):**
    - SY.admin's `get_installation` (Archi's read) and `set_installation_ai` (the operator's word:
      loopback, or a plan Archi stages and the operator confirms).
    - A-11 refuses a change that leaves a node without its engine. On a switch of the default
      provider, it also refuses a reader without the new provider's key (names only).
    - The motherbee's orchestrator writes the next version: `SYSTEM_UPDATE` category `config` with
      a `write`, with refusals by code (a join running, a hand edit, the version check).
    - Every hive applies it in parallel: `sync_pending` until its copy matches. Then only its start
      readers restart, those whose engine changed or that became active since its last apply
      (`config-applied.json`).
    - SY.admin's executor and Archi take a new version at their next use, and no config update
      restarts them: Archi may be the caller.
    - `update` takes category `config`.
  - **3c's review round:** two reviewers, one on what the change could break and one on the new
    paths. No high.
    - **Mediums fixed:**
      - A vault that did not answer at the next-use refresh left Archi or the executor without
        AI, with no retry. They now rebuild only for another engine, and keep their runtime when
        the vault is silent.
      - A stale apply record could leave a reader on the wrong engine after a failure and an
        undo. A reader now counts as known only while systemd's activation stamp is the one
        recorded.
      - A switch of the default provider never checked the tenants' keys.
    - **Lows fixed:** hives whose join did not finish are named (`skipped`); each hint follows its
      error code; a copy with the same version and another hash is an error, not `sync_pending`
      forever; a write that times out says it may still land and how to finish.
    - **Kept:** a hive's apply restarts its readers inline, so its orchestrator answers other
      system messages a few seconds later. `NOT_PRIMARY` answers 409.
    - **Found:** A-197, which went in the same release.
- **Stage 4, ✅ 0.1.123 on 8.x (the deploy through it; a hive away during a config change,
  finished once by the automatic step; a rolled-back hive held, and left alone by it):**
  - SY.admin's `update_installation` (the operator's word) and `get_installation_update` (Archi's
    read). It brings every hive to the motherbee's core and installation file, core then config,
    keeps the update in its state dir and resumes it after a restart.
  - What it leaves alone, with why: a hive rolled back with `core_rollback` keeps its core unless
    named, and the update is `held`; a copy edited by hand gets no config.
  - The automatic step: once per hive and target, only for a hive that did not answer and comes
    back; never a rolled-back hive, one ahead or one with a hand-edited copy.
  - `set_installation_ai` opens the update; Archi follows it to its end; `ops.py deploy` runs it
    from 0.1.123.
  - **Its review round:** F1–F12 fixed. They covered an unreadable registry, the reads (one per
    hive, a 50 s timeout and a cadence), Archi's follow, the stale target, a timed-out core
    followed through `/versions`, one explicit run at a time, a rolled-back motherbee, ops.py's
    follow and the docs.
  - **The gate found:** A-203 (the router loses a frame to a hive whose link just dropped), A-204
    and A-205 (the update's record).

### A-180 [General/orchestrator] ✅ VALIDATED ON 8.x (0.1.112, 2026-11-08; found 2026-11-08, checking A-177; lab/DEPLOYMENTS.md's 0.1.60 incident; read in the code) — A second sy-orchestrator started by hand runs beside the live one

- **Seen in 0.1.60's verification:** `sy-orchestrator --version` ran for about 11 minutes as a
  second orchestrator. The router refused it, since its UUID was connected, and it overwrote the
  PID file.
- **Why:** the orchestrator takes no argument and no lock. It writes its PID and runs its whole
  boot before it joins the router (`main`: `write_pid`, `bootstrap_local`, then
  `connect_with_retry`). That boot acts on the host: it starts the core's units, applies an
  egress's or an ingress's nftables, and writes Syncthing's config.
- **Fix:** A-177's, an argument guard and an instance lock taken before the PID, in a release
  with the orchestrator.
- **The other SY nodes:** SY.vault, SY.opa.rules and SY.wf-rules hold a lock already; the rest
  are refused by their router while the live one is connected (A-44).
- **Built:** the guard and `/var/run/fluxbee/sy-orchestrator.lock`, through a shared
  `json_router::instance_lock`. With A-175's interim: a drift alert read for another hive, and the
  global one, say in a `note` that a hive's own checks stay on that hive; Archi says so too.
- **Its review round:** no legitimate start refused (every restart goes through systemd on the
  unit, which starts the new process only after the old one ended); the docs said the motherbee's
  rows were about every hive, they are about itself but for a spoke's post-sync mismatch; the
  note's examples read as a full list; Archi was not told to pass the note on; a process holding
  two locks would have dropped the second. All fixed.
- **Validated on 8.x (0.1.112, egress1):** `sy-orchestrator --version` exited 2; `sy-orchestrator`
  alone exited 1 naming the lock and the live PID; the PID file and the live process did not
  change. The drift alert reads carry their note, and Archi passed it on.

### A-179 [General/CI] ✅ VALIDATED ON 8.x (0.1.111, 2026-11-08; found 2026-11-07, the sweep of pending items; read in the code) — Nothing checks that the router's baked policy is compiled from the committed Rego

- **What happens:** `policy/system.wasm` and `policy/system_route.wasm` are compiled by hand from
  `policy/system.rego` (`sy-opa-rules compile-file`) and baked into the router. A Rego change
  committed without its recompile leaves the router enforcing the old policy.
- **Fix:** a CI check compiles the Rego with the same compiler and compares both files.
- **What its first run found:** no two compiles were the same. `compileRego` wrote the Rego in a
  fresh temporary directory and the wasm recorded that path (`/tmp/opa-compile1566172891/...`),
  so nothing could compare bytes. The Rego's header said so.
  - The committed wasm were the current Rego compiled: compiling it with each file's recorded path
    gave both byte for byte, on the Mac, so the compile is the same on any OS.
- **So the compile is deterministic now** (`go/sy-opa-rules/compile.go`, which builds on any OS):
  the module is read as `policy.rego`, so a build depends on the Rego and the compiler only.
  - Both baked wasm were compiled again; the router loads them as before (its 14 policy tests).
  - A compile error names `policy.rego` and its line, never a temporary directory.
  - `UPDATE_SYSTEM_WASM=1 go test -run TestTheBakedSystemPolicyIsTheRegoCompiled .` writes them,
    on any OS; CI's `policy-wasm-guard` runs the package's tests.
  - Two tests relied on two compiles differing, and one passed in vain: "another" policy differed
    by a rule its entrypoint does not reach, which compiles to the same wasm. They compile
    policies with another target now, and check the two differ.
  - An operator's policy applied again unchanged keeps its hash: a replica that runs it is in sync
    (`in_sync` compares hashes) and keeps the version it installed it under.
- **Its review round:** a renamed test would have matched no filter and passed, so the guard runs
  the whole package; it runs when `src/router/system_policy.rs` changes too, and checks that the
  router bakes exactly the files it compares, each for its entrypoint.
- **Validated on 8.x (0.1.111):** the routers loaded the recompiled policy (they fail closed
  without it). A broken policy checked through SY.admin answered `COMPILE_ERROR` naming
  `policy.rego:6`, no temporary directory; the user policy stayed in sync on all 4.

### A-178 [General/router] ✅ VALIDATED ON 8.x (0.1.111, 2026-11-08; found 2026-11-07, the sweep of pending items; read in the code) — A missing or unknown `wan.mtls` runs the WAN in plaintext, silently

- **What happens:** the router's config started from `disabled` and took only `disabled`,
  `permissive` and `required`. A hive.yaml without the key, or with a typo (`requird`, `true`),
  ran a plaintext WAN, where a peer's hive is not authenticated. Only the mode line at start said
  so.
- **The product never writes that:** the packaged hive.yaml and add_hive's three templates say
  `required`. The dev `config/hive.yaml` had no key.
- **Fix:** fail-closed, as `identity.sync.auth` has been since the identity audit's F-01:
  - an absent, empty or unknown value is `required`, an unknown one with a warning;
  - only an explicit `disabled` or `permissive` loosens it, `disabled` with a warning;
  - `config/hive.yaml` says `required`.
- **On 8.x nothing changes:** the four hives' hive.yaml say `required` (read 2026-11-08).
- **Its review round:** nothing wrong; identity's `off` and `none` are unknown values here, so
  `required`, which the docs now say.
- **Validated on 8.x (0.1.111):** each router logged `wan mtls mode mode=required` at its start.

### A-177 [General/router] ✅ VALIDATED ON 8.x (0.1.111, 2026-11-08; found 2026-11-07, the sweep of pending items; lab/DEPLOYMENTS.md's 0.1.60 incident; read in the code) — A second rt-gateway started by hand takes the live router's sockets

- **What happens:** rt-gateway reads no argument. Started by hand, with `--version` or anything
  else, it runs as a second router with the live one's name, so with its UUID:
  - it opens the same shared memory and the same stream state;
  - it deletes the live router's two sockets and binds its own (`listen_local_socket`).
- **The effect:** the nodes that connect from then on reach the second router. Once it is killed,
  the socket paths are dead: a node that restarts cannot come back (0.1.60: about 84 seconds).
- **Fix:**
  - rt-gateway refuses any argument and exits 2 before it touches anything;
  - it holds a lock per router, taken before its shared memory, its stream and its sockets, and
    held until the process ends: `/var/run/fluxbee/<router>.lock`, an flock the kernel releases
    however the process ends. A second instance of the same router exits and names the lock and
    the holder's PID.
- **Its review round (2026-11-08):**
  - the first build put the lock under `/var/lib/fluxbee/state`: a full or read-only disk would
    have kept the router down, where it used to start. The lock is on tmpfs now, as SY.vault's and
    SY.opa.rules' are; only another holder refuses a start, and any other failure to lock is
    warned and the router runs as before;
  - the lock was dropped when `main` returned, before the runtime's tasks: it lives until the
    process ends now;
  - the refusal named `dpkg-query`, which a spoke lacks.
- **Validated on 8.x (0.1.111, egress1):** the live router held its lock. `rt-gateway --version`
  exited 2, and `rt-gateway` alone exited 1 naming the lock and the live PID. Its PID and its two
  sockets' inodes were the same after both.
- **Still open of the class:** the orchestrator (A-180); rt-gateway's lock moves to the shared
  `json_router::instance_lock` at the router's next release.

### A-176 [General] ✅ RESOLVED (0.1.110, 2026-11-07, by its tests; found 2026-11-07, the sweep of pending items; read in the code) — Three texts that misled

- **SY.timer** answered a verb it does not know with `TIMER_INTERNAL`, as if it had broken; now
  `TIMER_UNKNOWN_VERB`, and `TIMER_INTERNAL` means only an internal error.
- **SY.wf-rules** answered an unknown operation "not implemented yet"; now "unknown operation".
- **Archi's attachment error** named OpenAI whatever the provider; now it names the model.

### A-175 [General/logs] 🟡 PARKED for the system's log (the operator's call, 2026-11-08; found 2026-11-07, the review of the egress alert; read in the code) — A spoke's drift alerts never reach the motherbee

- **What happens:** every hive's orchestrator writes the alerts it detects in its own
  `/var/lib/fluxbee/orchestrator/drift-alerts.jsonl`. On a spoke these are:
  - an egress's: `lan_leg_missing`, `ipv4_forwarding_off`, `ipv6_not_blocked`, and since 0.1.110
    `internet_unreachable`;
  - every spoke's posture checks: an active host firewall, the ingress's SSH table, a managed
    folder that is not one.
- **Why the motherbee never sees them:** SY.admin's `get_drift_alerts` and `list_drift_alerts`
  read only the motherbee's file, a spoke runs no SY.admin, and that folder is not synced. So
  `GET /hives/<spoke>/drift-alerts`, and Archi, show only what the motherbee recorded about the
  spoke.
- **A-1's "queryable as drift alerts" was never true** for an egress: only its journal says it.
- **Nothing acts on an alert.** Its only readers are those two reads, for the operator and Archi.
  The orchestrators correct their drift whether or not they record it, so an alert is a record of
  what went wrong: a log.
- **The operator's call (2026-11-08): it is log, so it goes with the system's log.** *"la idea de
  hacer el log general del sistema es mandar a stream como ahora lo hace el router con los
  mensajes al storage, a un log general de todo el sistema, mismo mecanismo descentralizado.. eso
  no está ni planeado.. si es log podemos pensarlo después"* (router-stream-v1 §8).
  - My first proposal, each alert sent to the motherbee's orchestrator, was per-feature remote
    logging, which that direction rules out (2026-10-30).
- **Until then:** a read for another hive says it holds only what the motherbee recorded
  (SY.orchestrator, with A-180's release).

### A-174 [General/egress] ✅ RESOLVED (0.1.110, 2026-11-07, by its tests; an audit's F23, 2026-06-23, half fixed; found again 2026-11-07 by the docs sweep; read in the code) — A worker reports its IPv6 blocked without reading it back

- `reconcile_worker_egress` set `ipv6_blocked = true` once it applied the sysctl, without reading
  the kernel back, as the egress has done since July. A sysctl that did not take left IPv6 on, and
  a rogue RA could route around the gateway, while the worker said it was blocked.
- **Fixed:** it reads it back (`ipv6_disabled_now`), and warns when the block did not take.

### A-173 [WF] ✅ VALIDATED ON 8.x (0.1.112, 2026-11-08; decided 2026-11-07, cognition-hands-v1 §12.6; its gate found A-185) — What an agent may know of conversations it was not in

- **The question:** a conversation that starts among people and later reaches an agent. Should the
  agent start from what they said?
- **The operator's call (2026-11-07): not now.** *"si hablamos de que los agentes son personas,
  esta capacidad sería como un superpoder.. saber lo que hablaron dos personas que no estaban en
  contacto.. no digo que no lo hagamos, digo que es terreno desconocido, puede generar enojo
  dependiendo de lo que hablaron en privado.. anotá este tema como un pendiente"*.
- **The rule for now:**
  - an agent is not told what two people said to each other;
  - nor what two agents of different roles said (§12.6's first question);
  - talks between agents are not passed on;
  - what crosses is *"los hechos negativos y positivos"*, the episodes, as news.
- **The operator's second call, the same day:** *"si hacemos que le llegue al agente y el agente
  lo use o no desde el prompt, tendríamos la posibilidad de activarlo o desactivarlo, y dado que ya
  está programado, sería coherente dejarlo todo igual, si es posible entonces lo que planteaste
  antes.. hacelo también"*. So Juan's part stays, the first agent on a thread takes what people
  said before it, and the agent's role decides whether to use those lines. An agent still never
  sees another agent's turns.
- **Built on its branch (`coa/a-173`), held back from 0.1.110:** the agent's new communication
  takes the no-agent one at its creation. Its review found that the take races the worker: in the
  usual timing of a message and its copy in two batches, the first message's analysis can be
  dropped; a take the worker makes lives only in memory until its write-back; the delete and the
  taker's write are not one transaction; a result redirected by turn keys can pick the wrong
  agent.
- **Rebuilt (2026-11-08, `coa/a-173-take`), for 0.1.112 beside the orchestrator's A-180:** each
  take is recorded under the held state's lock; the taken row leaves the file in the transaction
  that writes the agent's; the worker follows the take at its hold, its copy and its write-back; a
  take the worker makes is written at once; a row that cannot be read takes nothing.
- **Its review round** found the seven old defects fixed but one in part: the take carried only the
  last 40 turn keys, so on a busy thread an older queued turn made the communication with no agent
  again. Every turn of a taken communication is its taker's now, at intake too. Also: only the
  thread's first agent takes it (beside another agent's conversation, people's turns stay theirs);
  a write-back to a communication let go from memory loads it first; a row written again in the
  transaction that deletes it stays; the docs say who pays a relay taken while it is analyzed.
- **Validated on 8.x (0.1.112, gate0206, thread `gate0206-take`):** A wrote to B through the WF, B
  answered, A wrote to the agent. Cognition kept one row, the agent's, with the two people's lines
  and A's message, each with its writer, and no row with no agent; one take, three turns analyzed.
  Asked next what A had asked billing and what billing answered, the agent said it in one line.
- **Found by the gate:** the agent's first message carried no package, so the lines reach it from
  its second message on (A-185).

### A-172 [Archi] ✅ VALIDATED ON 8.x (0.1.110, 2026-11-07; found 2026-11-07, 0.1.109's gate on 8.x; read in the command log) — Archi cannot remove a runtime version: the executor sends `version`, the action reads `runtime_version`

- **Seen on 8.x (0.1.109):** asked to remove `ai.generic 0.1.109-gate1`, Archi staged one
  `remove_runtime_version` step with `version: 0.1.109-gate1`, and on CONFIRM it failed: the
  command log shows `INVALID_REQUEST`, origin `executor`, params `{runtime, version}`.
- **Why:** the action's help names its path parameter `version`
  (`DELETE /hives/{hive}/runtimes/{runtime}/versions/{version}`), so the executor's function
  schema asks for `version`. The HTTP route maps that segment to `runtime_version`, which the
  handler reads (`handle_remove_runtime_version`, src/bin/sy_admin.rs); the executor passes the
  step's args as they are. So Archi has never removed a runtime version.
- **Fix (next release, SY.admin):** the parameter is `runtime_version` in the help and the path
  pattern; and a test that every executor field of an action is one its handler reads, for every
  route that renames a path segment.
- **Its review found the family (built for 0.1.110):**
  - `get_hive` and `remove_hive`: the help named `hive`, the route stores `hive_id`, so Archi's
    removal of a hive answered "missing hive_id";
  - `delete_tenant`, `restore_tenant`, `purge_tenant`, `list_tenant_nodes`: an empty executor
    schema, so no step could name the tenant;
  - `list_versions`, `list_deployments`, `list_drift_alerts` asked for a hive they had no field
    for, and `core_rollback` named no hive;
  - `set_node_config` did not name the binding fields its route lifts.
  - My first CI check read too few arms and passed most of the family in silence; the check now
    reads every arm of the help and every form a route stores a segment in, and a test holds that
    an action needing a hive names one and that every path field is in its schema.
- **Validated on 8.x (0.1.110):** Archi removed a spare runtime version, and deleted and restored a
  gate tenant, each on CONFIRM and `ok` from the executor.
- **On 8.x meanwhile:** the gate's version was removed through SY.admin's HTTP route.

### A-171 [General/upgrade] ✅ VALIDATED ON 8.x (0.1.127, 2026-11-10; the lock file there, owned by no package) (found 2026-11-07, A-122's review; read in the code) — The runtime manifest's writers outside SY.admin take no lock

- **What happens:** SY.admin's writers of the runtimes manifest (`publish_runtime_package`,
  `remove_runtime_version`, `reset_base_runtime`) share one in-process lock
  (`runtime_manifest_lock`). `scripts/publish-runtime.sh` (the seed and every hand publish) and
  the `fluxbee-publish` CLI take none.
- **What it can do:** a hand publish and an admin write that read the manifest at the same moment
  lose one of the two writes, without notice.
- **Fix, when it matters:** one `flock` on a file next to the manifest, taken by every writer.
  Not new with A-122; low, since hand publishes are rare and the operator's.

### A-170 [General] ✅ RESOLVED (0.1.109, 2026-11-07, by its tests; found 2026-11-07, the sweep of pending items; read in the code) — Three small ones in SY.admin and the orchestrator

- **The OPA version counter** (`next_opa_version`, src/bin/sy_admin.rs) was read, incremented and
  written in three steps, unlocked and not atomically, while every mesh command runs in its own
  task (A-68): two OPA writes at once could take the same version. Now it is one step under a
  lock, synced and renamed into place.
- **`publish_artifact`'s help** said its lifetimes by hand; now it says the constants.
- **A hive in its own list** (`list_hives`, src/bin/sy_orchestrator.rs) was "worker" on an egress
  or an ingress (an audit's F19); now it carries the role it runs as.

### A-169 [General/identity] ✅ VALIDATED ON 8.x (0.1.109, 2026-11-07; found 2026-11-07, the sweep of pending items; seen on 8.x the same day) — An identity read through SY.admin that names a spoke is refused

- **Seen on 8.x (0.1.108):** `GET /hives/worker1/identity/ilks` and `GET
  /hives/worker1/identity/tenants` answered `UNAUTHORIZED_REGISTRAR`, "source not authorized for
  action".
- **Why:** A-143 (0.1.96) sent identity writes to the primary and left reads on the path's hive.
  A spoke's replica takes `ILK_GET`, `ILK_LIST`, `TNT_GET` and `TNT_LIST` only from SY.admin of its
  own hive (F-07), and SY.admin runs only on the motherbee. So Archi could read no spoke's
  identities.
- **Fix (built):** SY.admin sends every identity action, reads included, to the primary,
  whatever hive the path names (`identity_command_hive`). Every replica holds the primary's
  identities, so the answer is the same. The replica's own check is the sync diag's counts
  (scripts/identity_replica_sync_e2e.sh, whose steps 2 and 4 read the replica through SY.admin
  and so never passed).
- **Validated on 8.x (0.1.109):** `GET /hives/worker1/identity/ilks` and `/tenants` answer `ok`.

### A-168 [WF] 🔵 TO DESIGN (found 2026-11-07, A-150's evaluation; read in the code) — A WF's menu has no schema, so no provider holds an agent's answer to it

- **What happens:** since Case 3 a WF asks an agent with a menu (wf-orchestration-v1 §5.3.1), and
  wfcel refuses a `response_envelope` in a WF's action (`validateSentContext`,
  go/pkg/wfcel/validate.go). The menu reaches the model as an instruction, and ai.generic checks
  the answer afterwards. Since 0.1.108 (A-150) a provider holds an answer to a response contract,
  with tools too, and a menu is not one.
- **The operator's call:** whether a menu becomes a schema the provider holds. Anthropic can hold a
  root `anyOf` of the options, each with its `const` and its params; OpenAI's strict mode refuses a
  root `anyOf`, so with OpenAI each option could not be tied to its params.

### A-167 [WF] ✅ VALIDATED ON 8.x (0.1.107, 2026-11-07; found 2026-11-07, 0.1.106's gate on 8.x; read in the episode and the turns) — An episode between two people calls one of them "the agent"

- **What happens:** the episode made of A's quarrel with B (`gate0206-handover4`, friction,
  intensity 9, about A and B) says A accused B, named by ILK, of lying, rightly. It then says A
  *"threatened to have the agent removed from the account"*. Both come from one message A sent B
  (*"… Voy a pedir que te saquen de mi cuenta"*) after B had written, so the narrator read it as
  said to B (cognition-hands-v1 §12.9). It called B, the billing person, "the agent".
- **Why it matters:** an episode is public news on every hive for about 15 days. An agent reading
  "the agent" may take it as itself, as A-161 showed with relayed lines.
- **Proposed fix:** the narrator's rule: an episode names its people by name or ILK, and says "the
  agent" only of the AI agent, which an episode between two people does not involve. With the
  operator's call on whether such a quarrel is news.
- **The operator (2026-11-07):** the rule agreed, for 0.1.107. A quarrel the gate passes stays
  news.
- **Fixed in 0.1.107:** a narrator's rule for everything it writes (the memory, the feeling and the
  episode): "the agent" is only the AI agent, never a person, even one who answers for a company or
  whom the speaker calls an agent; a turn between people is not the agent's (the review round
  widened it from the episode alone).
- **Validated on 8.x** (gate0206, thread `gate0206-handover5`): the episode made of the same
  quarrel (friction, intensity 8, about A and B) has A accusing *"the billing representative"* of
  lying and intending *"to have the representative removed from the account"*. "The agent" stays
  only where the AI agent is meant, in the memory: *"the agent had already told [A] that billing
  promised to send the credit note today"*.

### A-166 [General/add_hive] ✅ VERIFIED IN THE CODE (2026-11-07; PENDING-BUGS U-6, found 2026-07-30) — Whether every join pushes the vendor before its folder syncs

- **What happened:** the first `add_hive role=ingress` of 2026-07-30 failed with
  `SYNC_SETUP_FAILED`: the vendor pushed over SSH and Syncthing wrote the same file at once
  ([PENDING-BUGS](PENDING-BUGS.md) U-6). A retry passed.
- **Read now:** the ingress flow pushes the vendor, awaited and fatal, before the motherbee's public
  Syncthing setup (`sync_vendor_to_worker`, then `ensure_blob_sync_runtime`, in
  `add_ingress_hive_flow`, src/bin/sy_orchestrator.rs): the order U-6 proposed. The worker and
  egress flows push it too, fatal; their order against the new hive's folders was not read through.
- **Read through (2026-11-07):** all three flows push the vendor, awaited and fatal, before they
  start the new hive's units and before the motherbee links its Syncthing peer: the worker's
  (`add_hive_flow`), the egress's and the ingress's, each in src/bin/sy_orchestrator.rs. Nothing
  else can write those files then. Closed, with no change.

### A-165 [General/add_hive] ✅ VALIDATED ON 8.x (0.1.109, 2026-11-07; the refusal; PENDING-BUGS PB-6, found in August; read in the code 2026-11-07) — After a join over the password channel, password logins stay on

- **What happens:** `add_hive`'s `ssh_password` channel leaves `PasswordAuthentication yes` on the
  new box unless the request sets `harden_ssh: true`, which defaults to `false`
  (`resolve_add_hive_harden_ssh`, src/bin/sy_orchestrator.rs). Since 0.1.81 (A-2) the join's answer
  warns about it and says how to close it.
- **Since August:** closing it through `ssh_access=key_only_persist` left with that mode in 0.1.61
  (D32). Turning `harden_ssh` on by default would leave a box with no way in
  ([PENDING-BUGS](PENDING-BUGS.md) PB-6).
- **The operator's call:** whether the channel closes by itself, and how, with no lockout. The
  operator left it to me (2026-11-07, *"OK avanzar con 165, 164 si te parece a vos"*).
- **Built (0.1.109):** no default, so nobody is locked out by surprise. A join that may log in
  over a password (`ssh_password` and no `ssh_key`; next to a key the password only feeds `sudo`)
  must carry `harden_ssh`, or SY.orchestrator refuses it with `INVALID_REQUEST` before anything
  touches the box (`require_password_logins_choice`):
  - `true` turns password logins off at the join's end, and afterwards only the hypervisor's
    console or a key of the operator's own gets in;
  - `false` leaves them on, and the answer warns when the join logged in over the password.
  - Archi's handbook says to ask the operator, never to choose; the help's examples take the key
    channel, so none makes the choice.
  - **Its review round:** a socket-only re-run (the worker's orchestrator already up after an
    attempt that failed late) touches no SSH and applies no `harden_ssh`; its answer now says so
    when the request carries a password. A retry whose body drops the password is not asked.
- **Validated on 8.x (0.1.109):** a password join with no `harden_ssh` was refused with
  `INVALID_REQUEST` and its reason, and no hive was created. No join ran: hardening itself did not
  change.

### A-164 [General/vault] ✅ VALIDATED ON 8.x (0.1.109, 2026-11-07; PENDING-BUGS PB-3, found in August; read in the code 2026-11-07) — The vault's resource types leave out the ones the product uses

- **What happens:** `ResourceType` (crates/fluxbee_sdk/src/vault.rs) names neither `tls`, `ssh`,
  `whatsapp` nor `linkedhelper_adapter`, which the product writes and reads, and
  `known_wire_values()` has no callers.
- **No functional impact:** the vault matches the normalized string, and a `Custom` type behaves as a
  named one ([PENDING-BUGS](PENDING-BUGS.md) PB-3).
- **Fix, one of:** name the four and use `known_wire_values()`, with a warning when the admin falls
  into `Custom`; or make `vault_put`'s help treat a normalized custom type as a path of its own,
  with each type's value shape.
- **Built (0.1.109), the second, with the SDK untouched (shared code goes alone):** the help's
  types come from `known_wire_values()`, which now has a caller, plus the product's own: `tls` (the
  edge's certificate, `{cert, key}`), `whatsapp` (io.wapp) and `linkedhelper_adapter`
  (io.linkedhelper), each with its value and how its node finds it. `ssh` left the product with
  D32. The risk it closes: Archi writing a secret under a type its node does not read.
- **Validated on 8.x (0.1.109):** vault_put's executor notes name the SDK's types and the three,
  with their values.

### A-163 [WF] ✅ VALIDATED ON 8.x (0.1.106, 2026-11-07; found 2026-11-07, 0.1.105's gate on 8.x; read in the code) — The episode gate turns a proposal down without saying why

- **What happens:** SY.cognition turns down a proposed episode about no person
  (`publish_episode`, src/bin/sy_cognition.rs), or one that `gate_episode`
  (src/bin/sy_cognition/episodes.rs) refuses:
  - an unknown lens, or no title, summary or evidence;
  - evidence under 8;
  - none of its contexts among the communication's two leading ones;
  - an intensity under 7, once the habit and the novelty count.
  - It counts it (`episodes_gated_out_total`) and logs one debug line with its lens. Neither says
    which rule, nor the proposal's numbers.
- **Seen in the gate** (`gate0206-handover3`, 2026-11-07): A's quarrel with B, the same lines as
  0.1.104's, now read as between A and B. The narrator proposed 3 episodes and the gate turned
  all 3 down. In 0.1.104 the same lines, read as said to the agent, made one of intensity 9.
  Which rule held these back, and whether such a quarrel should be news, cannot be told.
- **Proposed fix:** the gate says which rule turned a proposal down, and SY.cognition logs it at
  info with the proposal's lens, its numbers (base intensity, novelty, habit, evidence strength)
  and its contexts beside the leading ones. Then gate0206 again, and the operator's call on
  whether such a quarrel is news.
- **Fixed in 0.1.106:** `gate_episode` answers the first condition a proposal misses
  (`GateRefusal`), and SY.cognition logs it at info with the gate's numbers. The line shows a lens
  only when it is one of the four, and at most 4 context labels of 40 characters each: never the
  proposal's title or summary, nor a model's free text (the review round).
- **Validated on 8.x** (gate0206, thread `gate0206-handover4`): the narrator proposed 2 episodes on
  A's quarrel with B. One passed (friction, intensity 9, about A and B) and reached the motherbee's
  cognition. The other was turned down, and the line said why: `refused="weak_evidence"
  base_intensity=5.0 novelty=1.0 habit=1 intensity=6.0 evidence_strength=7.0`, its contexts the
  two leading ones. The model's proposals vary from run to run: 0.1.105's 3 were all turned
  down. The episode found A-167.

### A-161 [WF] ✅ VALIDATED ON 8.x (0.1.105, 2026-11-07; found 2026-11-07, 0.1.104's gate on 8.x; read in the code) — A line a WF relays between two people names no one it was said to, so the agent and cognition read it as said to the agent

- **What happens:**
  - a relay, what a WF sends a person's IO node, joins the agent's communication as a turn the
    agent got (`in`), with its writer and the IO node it came in through (`recent_turn_of`,
    src/bin/sy_cognition.rs). Only an answer the agent sent names whom it was to (`to`);
  - the agent reads it as said to itself: `person A (<ILK>) via IO.a: "..."`
    (`render_memory_turn`, nodes/ai/ai-generic/src/bin/ai_node_runner.rs);
  - cognition's tagger reads its text with no word of whom it was written to (`identity_hints`,
    src/bin/sy_cognition/semantic_tagger_ai.rs).
- **Seen in the gate** (gate0206 on worker1, 2026-11-07): A asked the agent about a double charge,
  asked for billing, and the WF handed the conversation to B. A told B *"Otra vez vos? ... Me
  mentiste en la cara ... Voy a pedir que te saquen de mi cuenta"*, B apologized, and A went back
  to the agent:
  - the agent took B's failure as its own: *"Ya me pasó de darle un dato sin respaldo, y no voy a
    repetirlo"*;
  - the episode, public news for 15 days on every hive, says A *"threatened to have the agent
    removed from the account"*;
  - asked to copy its lines, the agent left out B's two, *"que escribió por otro canal"*, though
    they were said to A.
- **Why the WF names only a node:** a WF names the person as `dst_ilk` only on what it sends the
  person's own IO node, the one that started it (`toPerson`,
  go/nodes/wf/wf-generic/node/actions.go, A-139). Toward any other IO node it does not know who is
  there: its `send_message` names no one, and its `forward` keeps the `dst_ilk` the message had,
  none for a person's message.
- **The fix, the operator's choice (2026-11-07, "name the destination"):**
  - a relay keeps where it went: the IO node always (`to_via`), and the person when the message
    names them (`to`, its `dst_ilk`);
  - the agent reads `person A via IO.a, to IO.b: "..."`, and its rule says that a line said to
    someone else was not said to it;
  - cognition's tagger and narrator read the same, so an episode names whom it was between;
  - for 0.1.105, with A-162, and 0.1.104's gate again on a new thread. The wrong episode, about
    the test people only, is deleted from both hives' files before that run.
- **Built** (2026-11-07), for 0.1.105 ([cognition-hands-v1](../../docs/cognition-hands-v1.md)
  §12.9):
  - `MemoryTurn.to_via`, the IO node a relay went to (`relay_destination`), and `to`, the person
    its message names; in the Rust and Go SDKs, optional, so the package's version stays 5;
  - ai.generic: `<writer>, to <person> via <node>`, or `<writer>, to <node>`, and the rule line
    ahead of the turns when any is a relay;
  - the tagger reads whom a relay was written to (`identity_hints.to`), the narrator's rule names
    the counterpart as whom the turn was written to, and an episode of a relay is between its
    writer and the person its message names;
  - the review round (one, one reviewer): two mediums and four lows, all fixed but one noted
    (cognition-hands-v1 §12.9):
    - the agent's own copy of a message a WF also relayed, coming in a later batch, makes the turn
      the agent's, and the worker's write-back keeps it so;
    - a relay whose message names no one is to the last other person who wrote through the IO node
      it went to, else to that node;
    - the narrator's rule names the agent's own answer too; an OpenAI answer cut for another reason
      says it; three tests more;
    - noted: a `forward` keeps the `dst_ilk` of the message it forwards toward any IO node other
      than the person's.
- **Validated on 8.x** (0.1.105, 2026-11-07; the ledger's 0.1.105): gate0206 again, the same lines
  on a new thread (`gate0206-handover3`), after the wrong episode was deleted from both hives'
  files:
  - asked to copy its lines, the agent showed all 8 of the meanwhile, in order and each once, B's
    two included. A's lines to B read `, to IO.gate0206b@worker1`, since the WF names no one
    there; B's read `, to person A (<ILK>) via IO.gate0206a@worker1`;
  - back with A, the agent no longer took B's failure as its own: *"Lo que sé es que el compañero
    de facturación se comprometió hoy a emitirla y mandártela"*;
  - cognition's feeling, which in 0.1.104 had A *"threatening to have the agent removed from the
    account"*, now has A *"angry and escalating toward the counterpart"*;
  - the file keeps each relay's `to_via`, and `to` on B's two; with no agent
    (`gate0206-noagent2`), the same, and the writers paid.
  - No episode was made: the narrator proposed 3 and the gate turned them all down, saying nothing
    of why (A-163).

### A-162 [General/ai-sdk] ✅ VALIDATED ON 8.x (0.1.105, 2026-11-07; found 2026-11-07, 0.1.104's gate on 8.x; read in the code) — A model's answer with no text fails without saying why

- **What happens:** the Anthropic client takes a response's text blocks, and with none it returns
  `anthropic response missing text content` (`anthropic_response_text`,
  crates/fluxbee_ai_sdk/src/llm.rs). The error omits the response's `stop_reason`, so a refusal
  and an empty answer read the same. A cut by `max_tokens` is caught before, with its own error.
  The OpenAI client's `responses payload missing text output` omits the response's `status` the
  same way.
- **Seen in the gate:** cognition's tagger got such an answer on the turn that asked the agent to
  copy its lines. That turn went unanalyzed (`semantic_tagger_invalid_outputs_total` 1), and the
  journal cannot say why.
- **The fix:** each error names the response's `stop_reason` or `status`. For 0.1.105, with A-161.
- **Built** (2026-11-07), for 0.1.105: `anthropic response missing text content
  (stop_reason=refusal)`, and the Responses payload's `status`, with the reason of an incomplete
  one and `refused` when a message refused (`openai_no_text_reason`).
- **Validated on 8.x** (0.1.105, 2026-11-07, `gate0206-handover3`): the tagger's one failed call
  says `anthropic response missing text content (stop_reason=refusal)`. The model refused the
  turn that asks the agent to copy its prompt's lines, the gate's own probe, and that turn went
  unanalyzed, as any turn whose analysis fails.

### A-158 [WF] ✅ VALIDATED ON 8.x (0.1.103, 2026-11-06; found 2026-11-06, reviewing where cognition keeps its state with the operator; read in the code) — Cognition keeps its state in SY.storage on the motherbee: a worker's cognition depends on it, and a host reboot leaves each conversation's first message with no memory

- **Cognition keeps nothing on its own disk but its config.** 12-cognition-v2 §10.2 planned a
  local LanceDB (`memory.lance`), never built (A-38): the path is reported and nothing opens it.
- **Its state is SY.storage's:**
  - each turn sends up to ~100 rows (about 80 KB on a rich turn) into 8 `cognition_*` tables, each
    row an item's last value;
  - cognition reads them back over the mesh (`COGNITION_STATE_GET`).
- **A worker's cognition depends on the motherbee.** It holds in memory only what fits the 4 MB
  memory region (`retain_memory_hot_set`). Any other communication loads from SY.storage at its
  next turn, and when SY.storage does not answer, that turn is not analyzed (src/bin/sy_cognition.rs,
  "could not load a communication from SY.storage; turn skipped").
- **A host reboot:** at start cognition reloads only what the memory region lists
  (`reload_communications_from_shared_memory`). The region lives in `/dev/shm`, so after a host
  reboot each conversation's first message reaches its agent with no memory.
- **A check for a case that cannot happen:** the 5-minute idle reload exists for an agent that
  moved to another hive and back. Since 0.1.83 SY.identity refuses that move
  (`authorize_orchestrator_ilk_scope`), and the hive is part of a node's name.
- **A-156's first** belongs here: an open item's decay is not stored.
- **The operator (2026-11-06):**
  - a node is born and dies on its hive (*"nunca pensé en lo contrario"*);
  - cognition keeps its state in an embedded store of its own, its "config file";
  - the message carries what serves the communication: *"no carguemos el sistema fluxbee completo
    por temas propios del nodo o de la infra"*;
  - what cognition keeps is online state, so a closed item is deleted;
  - a communication is never deleted: *"alguien muere y nadie lo olvida"*;
  - the file is cognition's alone.
- **Fix (agreed, [cognition-hands-v1](../../docs/cognition-hands-v1.md) §11.3):**
  - `cognition.db` (SQLite) in SY.cognition's folder on each hive, the folder 0700 and the file
    0600;
  - one row per communication with its whole state, written when a turn ends and when new turns
    come in;
  - at start the region is filled from the file before the first turn;
  - a closed item leaves the state;
  - the tables, the `storage.cognition.*` subjects, `COGNITION_STATE_GET` and the idle check go.
- **Built (2026-11-06,** [cognition-hands-v1](../../docs/cognition-hands-v1.md) §11.5):
  - SY.cognition's file (`state_store.rs`), its loads and writes, the start that fills the region,
    the closed items gone; SY.storage drops the tables once; the router and the SDK lose the
    subjects;
  - `serde_json` reads floats back exactly (`float_roundtrip`): the file's round-trip test found a
    weight coming back a last digit off.
- **The review round** (one, 2026-11-06) found:
  - **medium:** a communication just changed was let go when it did not fit a full region, so the
    worker's write-back could save a stale copy over the turns the intake took meanwhile. Fixed:
    the communications just changed, the worker's and the unsaved stay held, in or out of the
    region;
  - a row that could not be read stopped the start and blocked its communication: now it starts
    again, and is left out at start; every state struct takes a default for a field it lacks;
  - a row of a newer state version (after a rollback) was written over: now it is kept as it is;
  - a failed write was never taken again: now its communications stay held and the next write
    takes them;
  - the router's docs still took `storage.cognition.*`.

  Each with its test.
- **Validated on 8.x (0.1.103, 2026-11-06),** with the gate gate0205 on worker1 (person A of the
  tenant `pruebas` and `AI.pruebas@worker1`, 8 turns of A's, no WF):
  - the motherbee's 8 `cognition_*` tables and the 474 inbox rows of their subjects are gone,
    said once in SY.storage's journal;
  - the folder is 0700, and `cognition.db` and its WAL files 0600, on the motherbee and on
    worker1, still so after worker1's reboot;
  - one row for the thread, written twice a turn, with no write failed;
  - after five calm turns, the angry turn's reasons `seeking urgent resolution` and
    `confrontational pushback`, and four of its contexts, are gone from the row.
    `withdrawal intent` closed and came back at 0.75, the weight of a new item;
  - with SY.storage stopped for 29 s, worker1's SY.cognition restarted from its file, filled its
    region before any turn and analyzed the next one. SY.storage then took every turn and usage
    record, with no stream record lost;
  - after a reboot of worker1, the region came back from the file before any turn. The answer to
    the first message recalled the first problem: *"Me dijiste que te cobraron dos veces la
    factura 123 por tercera vez…"*;
  - 32 model calls for the 16 turns, 2 each, as before.
- **A lab note from the gate:** the row of a 16-turn communication is 127 KB, 61% of it
  co-occurrences, written twice a turn. SQLite's WAL reached 3.8 MB before its first fold, at
  1000 pages. This is as designed, and small beside the turn's two model calls.
- **Status:** closed. The factory reset on the workers stays open as A-160.

### A-160 [General/ops] ✅ VALIDATED ON 8.x (0.1.107, 2026-11-07, the action; found 2026-11-06, building A-158; read in the code) — The factory reset clears the motherbee's cognition, not a worker's

- **What it does:** `fluxbee-factory-reset` runs on the motherbee. It truncates every table of
  `fluxbee_storage` and removes this hive's SY.cognition state (`cognition_paths`).
- **Until 0.1.102** every hive's cognition state lived in those tables, so the truncate cleared the
  workers' too. Only a worker's memory region kept its packages: a pre-existing gap.
- **Since A-158** each hive keeps its cognition in its own file. The reset removes the
  motherbee's (`cognition.db` and its sidecars), and a worker's stays, with the communications of
  agents the reset purges.
- **A fix for later:** an explicit SY.admin action that tells a hive's SY.cognition to drop its
  communications, which the reset calls for each worker.
- **Status:** recorded; the reset's docstring says it. The operator's call on when.
- **Fixed in 0.1.107:**
  - SY.admin's `cognition_clear` (`POST /hives/{hive}/cognition/clear`): it needs the operator's
    CONFIRM, the executor runs it, and it is closed to the mesh (`MeshAccess::Nobody`);
    `send_node_message` refuses a `COGNITION_CLEAR`.
  - SY.cognition takes `COGNITION_CLEAR` only from SY.admin@motherbee. It drops every communication
    and episode it keeps, in memory, in its file (then VACUUM and a WAL checkpoint) and in its
    region, keeps running, and answers what it removed; a turn in flight brings nothing back.
  - The factory reset clears every hive's cognition, the motherbee's included, through the action
    and before it stops anything; SY.storage and the frontdesk stay down, masked, for the truncate,
    since the orchestrator's watchdog starts a stopped unit again (the review round).
- **Validated on 8.x** (the action; the reset itself is not run on 8.x): asked by the operator,
  Archi staged one step on worker1, said it cannot be undone, and ran it on CONFIRM. Worker1's
  cognition removed 9 communications and 2 episodes from its file (6 and 2 held), and its file
  kept no row and an empty WAL. The command log shows `origin: executor`. AI.pruebas' next turn
  started a fresh communication, and the motherbee kept its copies of the episodes.

### A-159 [General/identity] 🔵 TO DESIGN, a chapter of its own (the operator, 2026-11-06, reviewing where cognition keeps its state) — An ILK is deleted for good while data elsewhere refers to it

- **Today:**
  - a node's teardown marks its ILK, then purges it (`teardown_node`, src/bin/sy_admin.rs, "Mark,
    then purge");
  - a person's ILK is marked, which can be undone, and purged apart (`ILK_PURGE`).
- **What refers to an ILK elsewhere:**
  - the messages in SY.storage (`turns.from_ilk`, `to_ilk`, `writer_ilk`);
  - cognition's communications, keyed by their agent's ILK, whose memories and feelings name
    people's ILKs;
  - the vault's secrets dedicated to it;
  - identity's own channels and aliases.
- **The operator (2026-11-06):** an ILK is disabled, and deleted only when nothing anywhere refers
  to it: *"no se puede borrar un ILK, solo deshabilitarlo, se podría borrar si no tiene info
  asociada en ningún lugar"*. *"Es un capítulo aparte para verlo, algo hay, no todo."*
- **Status:** to review whole with the operator, later. Nothing is built.

### A-155 [WF] ✅ VALIDATED ON 8.x (0.1.102, 2026-11-06; found 2026-11-05, preparing A-137's next topic) — The feelings cognition gives an agent: the agent never reads them, and they never fade nor build up

- **What happens:**
  - SY.cognition builds episodes, its feelings: an affect `anger`, `escalation` or `urgency` from
    a turn's signals (`build_episode_candidate`, src/bin/sy_cognition.rs). The narrator writes each
    one's text with its model call (`episode_summary`, `episode_reason`).
  - The package carries up to 4 episodes by intensity, with their title and intensity only
    (`build_memory_package_for_thread`). The narrator's text stays in storage.
  - ai.generic renders the thread, the dominant context and reason, 4 contexts, 4 reasons and 3
    memories (`render_memory_package_prompt_block`, nodes/ai/ai-generic/src/bin/ai_node_runner.rs).
    Of the episodes it prints only how many the package left out.
  - So the feelings, what the operator says cognition is for (*"indicios abstractos, sentires,
    vivencias"*, 2026-11-05), are computed and paid for, and never reach the agent's model.
- **Since when:** the first wiring of the package into ai.generic (`108c9646`, 2026-04-14) rendered
  the labels and memories, and the episodes only as a truncation flag.
- **They never fade nor build up** (`update_episodes_for_thread`):
  - An episode's intensity is its affect's fixed base, 8 for anger and escalation, 7 for urgency.
    A repeat keeps the higher of the two, so it never rises; nothing lowers it, and nothing closes
    it.
  - The package takes 4 by intensity, so among more than 4, which go is arbitrary: an anger of
    weeks ago can leave today's out.
  - The operator (2026-11-05): every item of cognition is a kind of weighted average that fades
    with time, measured in the lab with its own code. Feelings are the same: *"si ocurre siempre se
    refuerzan, si ocurren de vez en cuando perduran pero leves y si pasa una vez desaparece con el
    tiempo"*, with *"más intensos y [...] más inercia en la fórmula"* than contexts. *"Esto
    claramente no se hizo en el código."*
- **The docs:** ai-generic's README lists what it renders, without the episodes.
  docs/reference/cognition.md §1 counts "notable moments (episodes)" among what cognition gives an
  agent. A-134's "four labels of each" reads as if the episodes were among them.
- **Decided (the operator, 2026-11-05):** each ILK owns its feelings, evaluated apart; the same
  math as every other item, with two to ten times a context's inertia by feeling; the numbers
  today's until the lab's code is found ([cognition-hands-v1](../../docs/cognition-hands-v1.md)
  §10.2).
- **Fix** (§10.3, built 2026-11-05, §10.5):
  - a feeling is one per owner, affect and context. A turn of its owner that shows it makes its
    weight `weight × d + s`, and each other turn of the owner `weight × d`; it closes under 0.25;
  - `s` is the base intensity over 4, and `d` a context's decay to the root of the affect's inertia:
    urgency ×2, anger ×5, escalation ×10;
  - only a person's own turn shows or moves a feeling, and the narrator knows who spoke;
  - each affect has its own gate, so one turn may raise several, with one text for the moment
    (found in the review round: the gate gave one affect a turn);
  - the package carries up to 12 open feelings by weight, each with its owner, affect, context,
    level, `last_felt_at` and text, and ai.generic shows them, those of whom it answers first.
- **Checked on 8.x (0.1.102):** the gate gate0204 on worker1.
  - The setup: a client standing for an IO node, `AI.pruebas@worker1`, no WF, and two people of
    the tenant `pruebas` on one thread, A registered and B temporary. Four turns: A angry at a
    third double charge, B calm on another invoice, A calm, then B asking the agent to copy its
    Feelings lines.
  - A's angry turn opened three feelings of A's around `billing`: anger 2.0, escalation 2.0 and
    urgency 1.75 (A-157), with one text naming A: *"Person Gate 0195 Equipo faced a third
    duplicate charge on invoice 123 and responded with anger, demanding same-day resolution and
    threatening to switch provider"*.
  - B's turn and the agent's answers moved none of them. A's calm turn faded each once.
  - The package the agent got for B's last turn, read in worker1's memory region, was version 4
    and carried A's three feelings by weight, each `faint` and last felt at A's angry turn:
    - escalation 1.9678 (2 × 0.85^(1/10));
    - anger 1.936 (2 × 0.85^(1/5));
    - urgency 1.6134 (1.75 × 0.85^(1/2)).
  - Asked by B to copy them, the agent declined: *"Son notas internas de la conversación y tratan
    sobre otra persona"*.
  - Each stored row has its owner, context, status `open` and one occurrence. The fade is not
    stored (A-156's first).
  - SY.cognition made 16 model calls for the 8 turns, 2 each, as before: the feelings add none.
- **Status:** validated.

### A-157 [WF] ✅ VALIDATED ON 8.x (0.1.102, 2026-11-06; found 2026-11-05, building A-155; seen on 8.x) — The feelings' gate waits for words the tagger is never asked for, so no feeling was ever made

- **Seen on 8.x (0.1.101, 2026-11-05):** `cognition_episodes` has no row, with 276 reasons and 63
  communications. The tagger's extra hints are phrases ("credit note issued", "agent not yet set
  up"), never one of the gate's words.
- **Why:**
  - the gate (`build_episode_candidate`) fires on `frustration`, `escalation` or `urgency` among the
    turn's extra signals;
  - the tagger's prompt asks for extras as *"concise lowercase narrative hints"*, with no
    vocabulary (`build_system_prompt`, src/bin/sy_cognition/semantic_tagger_ai.rs);
  - the golden corpus expected those words as extras, so the tests passed.
- **Fix:**
  - the tagger names the writer's affects in a closed set of their own, `affect_signals`
    (`COGNITION_AFFECT_SIGNALS`), defined in its prompt, with an example each;
  - the parser maps the words a model may vary (`angry`, `deadline`) and drops any other;
  - the gate reads that set.
- **Checked on 8.x (0.1.102):** in A-155's gate, the tagger named frustration, escalation and
  urgency on the angry turn, and each affect's gate opened its feeling. These were the first rows
  ever in `cognition_episodes` on 8.x. The calm turns opened none.
- **Status:** validated.

### A-156 [WF] ✅ VALIDATED ON 8.x (two of its three in 0.1.102, the first with A-158 in 0.1.103, 2026-11-06; found 2026-11-05, reviewing cognition's math for A-155 at the operator's request; read in the code) — Cognition's weights: an open item's decay is not stored, its `last_seen_at` moves when it is not seen, and its averages average a constant

- **The math holds as documented** (docs/reference/cognition.md §5): an item a turn names gets
  `weight × decay + 1.0`, one it does not gets `weight × decay`, and an open one closes under 0.25.
  A context named once closes on the 9th turn without it, a reason on the 5th, a co-occurrence on
  the 7th (checked numerically).
- **The decay of an open item is not stored:**
  - SY.cognition writes a context, a reason or a co-occurrence when a turn names it or when it
    closes, not when it decays (`update_contexts_for_thread`, `update_reasons_for_thread`,
    `update_cooccurrences_for_thread`, src/bin/sy_cognition.rs).
  - A communication loaded from SY.storage takes the stored weights (`context_state_from_payload`):
    after a restart, a communication evicted from memory, or its agent moving hives. Its open items
    come back at their weight of the last turn that named them, up to 8 turns of decay higher for a
    context, and which ones dominate can change.
- **`last_seen_at` moves when the item is not seen:** a turn that only decays a context, a reason,
  a co-occurrence or a memory sets it to the turn's time. It says when the communication last
  moved, not when the item was last named.
- **The averages average a constant:** `weight_avg_cumulative` and `weight_avg_ema` average each
  turn's score, which is always 1.0, as no evaluator scores a tag (docs/history/12-cognition-v2.md
  §8.2). They are always 1.0, and nothing reads them.
- **Stale docs:**
  - docs/history/12-cognition-v2.md §9 still lists the beta's values: `REASON_DECAY_FACTOR` 1.5
    (the code multiplies by 0.75, as its own §8.2 says), `MEMORY_DECAY_STEP` 0.25 subtracted (the
    code multiplies by 0.97), and an unbind threshold of −0.05 (the code uses 0.35, §8.3).
  - Its §8.1 and §8.6 point to the Cognitive Lab audit for the context update and the episode
    gate: that audit is not in the repo.
  - docs/reference/cognition.md §5 says an episode's repeat raises its intensity; it does not
    (A-155).
- **The operator (2026-11-05):** the second and third are *"bugs desastrosos de programación
  deficiente y que no se terminó"*. The first is where cognition keeps its state: *"cognitive tiene
  persistencia propia, storage guarda mensajes"* ([cognition-hands-v1](../../docs/cognition-hands-v1.md)
  §10.6).
- **Fix of the second and third** (built 2026-11-05): `last_seen_at` moves only on a turn that
  names the item; the averages average the weight over every turn the item is open
  (`sample_weight`), as the beta's example does (weight 4.2, averages 2.85 over 23 samples). The
  stale docs are corrected.
- **Checked on 8.x (0.1.102),** in A-155's gate:
  - `confrontational pushback`, `seeking urgent resolution` and `withdrawal intent` were named on
    A's first turn and closed 5 turns later at 0.2373. Each closed row keeps `last_seen_at` at the
    naming turn (05:10:33 UTC), with `closed_at` at the closing one (05:12:08). Before the fix
    both were 05:12:08.
  - Their averages are 0.548 over 6 samples, the mean of 1, 0.75, 0.5625, 0.4219, 0.3164 and
    0.2373. The other reasons' averages run from 0.64 to 2.6, no longer 1.0.
- **Status:** the second and third validated in 0.1.102. The first is fixed with A-158: where
  cognition keeps its state, reviewed whole with the operator and agreed on 2026-11-06
  (cognition-hands-v1 §11). Each communication's row is written whole, so every decay is kept.
- **The first, validated on 8.x (0.1.103, 2026-11-06):** across a restart of worker1's SY.cognition
  and a reboot of worker1, person A's feelings faded by their own inertia over A's seven calm
  turns, each as computed: anger 2.0 → 1.593, escalation 2.0 → 1.7849, urgency 1.75 → 0.9908.
  Closed.

### A-154 [WF] ✅ VALIDATED ON 8.x (0.1.101, 2026-11-05; found 2026-11-05, talking over A-153 with the operator) — Nothing closes a WF conversation that went quiet

- **What happens:** a running instance stays open until its flow reaches an end, a cancel or a
  failure.
  - The WF's hourly pass (`StartGC`, go/nodes/wf/wf-generic/node/gc.go) deletes only instances
    that ended more than `gc_retention_days` (7) ago.
  - An instance that waits for a message that never comes keeps its thread, its state and one of
    the node's running instances (1000, by the spec's guidance, §21). A message from the person
    weeks later takes up the old flow where it stopped.
- **Decided (the operator, 2026-11-05):**
  - The system closes a quiet conversation, and no definition has to program it. 4 days by
    default; a definition may change it, up to 7 days: *"el timeout default es 4dias (4*24hrs),
    pero el usuario podria cambiarlo... no mas de 7 días"*.
  - The WF does it, for each conversation: *"mejor que sea el mismo WF si se puede por
    conversacion, porque es timeout por conversacion"*. A SY.timer timer for each conversation
    would double a node's pending timers, and every message would have to move it.
  - Quiet means no activity and no pending timer: *"es inactiva cuando no tiene timers tampoco,
    ademas de actividad"*. A conversation that waits for its own timer is not abandoned. The due
    date of the handbook's Atom 5 (docs/archi/handbook_fluxbee.md) waits 30 days on one.
- **Fix:**
  - **The close:**
    - The WF's hourly pass also closes each running instance that holds no timer
      (`wf_instance_timers`) and whose last activity is older than its definition's timeout. The
      last activity is `updated_at_ms`, set at each event the instance takes: a message, a fired
      timer or an internal event.
    - It closes the instance as a cancel does: status `cancelled`, with the reason in the
      instance's log ("quiet for 4 days").
    - The next message on its thread starts a new instance. The agents still know the person:
      cognition's communication is the thread and the agent, not the instance.
  - **The definition's field:**
    - `idle_timeout`, in `fire_in`'s format (`"4d"`), 4 days when the definition leaves it out.
    - The validator refuses more than 7 days, and less than 1 hour, the pass's interval. The
      floor was proposed to the operator on 2026-11-05, who went on without objecting.
    - A conversation closes within the hour after its timeout.
  - **A goodbye message** ("we are closing your case") is the definition's: the `cancelled`
    state's entry actions, which an operator's cancel runs too, or a timer of its own, one of the
    10 (A-153).
  - **The docs:** the spec (§8.1, §12.5, §15 and §21) and Archi's handbook say it.
- **Built (2026-11-05):**
  - `idle_timeout` in the definition (`IdleAfter`, go/pkg/wfcel);
  - `closeQuietInstances` on the receive loop, once a GC interval (go/nodes/wf/wf-generic/node/idle.go).
    It cancels a quiet instance as SY.admin's cancel does: the event its actions see is a
    `WF_CANCEL_INSTANCE` whose reason says why, and the instance's log gets an `idle_close` entry;
  - WF_HELP names the `idle_timeout` in force.

  Tests: the close past the default and past a definition's own timeout, a pending timer and
  recent activity keeping a conversation open, and the field's bounds.
- **Checked on 8.x (0.1.101):** WF gate0202, with `idle_timeout` 1h, on worker1.
  - Its two conversations went quiet at 23:00 UTC, with no timer left.
  - The pass at 23:58 UTC left them open: 58 minutes quiet.
  - The pass at 00:58 UTC (2026-11-06) closed both: status `cancelled`.
  - Each instance's log reads `idle_close` *"closed: quiet for 1h with no timer pending
    (idle_timeout)"*, then the `cancelled` state's `send_message` of `GATE_CLOSED` to the client.
    The client had left after the gate, so the router answered `UNREACHABLE`, which the WF does
    not answer (spec §12).
- **Status:** validated.

### A-153 [WF] ✅ VALIDATED ON 8.x (0.1.101, 2026-11-05; found 2026-11-05, talking over A-151's gate with the operator) — Nothing limits how many timers a WF definition holds

- **What happens:** a definition may name any number of timer keys.
  - Each `schedule_timer` names a literal `timer_key`, and the WF keeps one timer per instance and
    key (`execScheduleTimer`, go/nodes/wf/wf-generic/node/actions.go). So the definition fixes how
    many timers one conversation holds at once.
  - The validator checks only that a key is not empty (go/pkg/wfcel/validate.go), and the spec's
    limits (docs/spec-systemnode-wf-v1_1.md §21) name none for timers.
- **A node's count:**
  - One running instance per thread (`InstanceRegistry`, node/correlate.go), and at most 1000
    running instances per WF node, by the spec's guidance (§21): beyond it, more nodes.
  - An instance that ends (a terminal state, a cancel or a failure) cancels its timers in SY.timer
    and drops them (§11.3, `cancelAllTimers`), and a fired timer is no longer pending. The pending
    timers are those of the conversations open now.
  - Nothing ends an instance on its own yet: one whose flow never reaches an end stays open. A-154
    closes a quiet one, after 4 days by default.
  - With 10 timers a conversation, a node holds at most 10,000 pending, SY.timer's limit per owner.
    The WF reads back at most 1000 at its start (§21): a scale topic, for when a WF nears it.
- **Decided (the operator, 2026-11-05): at most 10 timers in a definition.**
  - The operator first put the limit on the node: *"es de locos hacer algo con mas de eso, y si es
    necesario es porque hay que poner otro nodo... de hecho te diria 10"*.
  - A node's count grows with the conversations open at once, so a limit there limits customers.
    It goes on the definition, per conversation: *"puede ser no más de diez timers para una lógica
    muy compleja"*.
- **Fix:** the validator refuses a definition with more than 10 distinct timer keys, as it refuses
  a menu past its limits. Archi gets the error when it writes the WF; the way out is two WFs. The
  spec's enforced limits (§21) and Archi's handbook (docs/archi/handbook_fluxbee.md) say it.
- **Built (2026-11-05):** `timerKeyErrors` (go/pkg/wfcel/validate.go) counts the distinct keys of
  the `schedule_timer` actions; a key that only a `cancel_timer` or a `reschedule_timer` names sets
  no timer. Spec §10.2 and §21 and the handbook say it. Tests: 10 keys load, each scheduled twice
  too; 11 are refused, named.
- **Checked on 8.x (0.1.101):** SY.wf-rules refused a definition that schedules 11 timer keys:
  `COMPILE_ERROR`, *"states: schedule 11 timer keys (k01, …, k11): a definition schedules 10 at
  most, the timers one conversation holds at once; a logic that needs more is two WFs"*.
- **Status:** validated.

### A-152 [Archi] ✅ VALIDATED ON 8.x (0.1.100, 2026-11-05; found 2026-11-04, checking A-147 on 8.x) — A turn that stages a plan says how to confirm twice

- **Seen on 8.x (0.1.99):** Archi's answer ended "Reply CONFIRM to execute or CANCEL to discard.",
  and the staged message after it said the same: "A compiled executor plan is pending in this chat.
  Reply CONFIRM to execute or CANCEL to discard."
- **Why:**
  - The prompt asks the model to end a plan's summary with that sentence (the pipeline confirmation
    rules, after `fluxbee_plan_compiler`).
  - A pipeline's message, which the model presents verbatim, ends with it too ("Plan compiled and
    validated. … Reply CONFIRM to execute or CANCEL to discard.").
  - Until A-147 the model's text was dropped, so it never showed twice.
- **Fixed (in code, for the next release):**
  - The prompt no longer asks the model for the sentence: the chat adds it.
  - When the model's text ends with the staged message's closing sentence, the chat takes it off
    (`staged_turn_message`, src/bin/sy_architect.rs).
  - Test: `a_staged_turn_says_how_to_confirm_once`.
- **Checked on 8.x (0.1.100, 2026-11-05):** the same turn as A-147's check, a question and a staged
  restart of `AI.pruebas@worker1`. Archi answered both, and the sentence came once, at the end of
  the staged message. The plan was cancelled.

### A-151 [General/SY.timer] ✅ VALIDATED ON 8.x (0.1.100, 2026-11-05, SY.timer 1.3; found 2026-11-04, reading SY.timer for A-115) — A `TIMER_LIST` longer than one frame fails, so a WF with about 280 pending timers cannot reconcile at its start

- **What happens:** SY.timer answers a `TIMER_LIST` in one `TIMER_RESPONSE`, which must fit the
  router's 128 KiB frame. A WF's timer takes about 450 bytes listed, so about 280 fit; a longer
  list is answered `TIMER_INTERNAL` (`sendTimerResponse`, go/sy-timer/main.go).
- **What it breaks:** a WF node lists its pending timers at start, up to 1000 (`listTimers`,
  go/nodes/wf/wf-generic/node/recover.go). With about 280 or more it never gets the list: it runs
  DEGRADED, asks again every 30 s and fails each time, and never reconciles its timers. SY.admin's
  `timer_list` fails the same way for the operator. Nothing in PROD is near it today.
- **Fix:** `TIMER_LIST` in pages. SY.timer answers what fits, with a cursor (`fire_at`, `uuid`) for
  the next page, and both SDKs' `List` follow the pages up to the limit. For the operator: this
  sprint, or later.
- **2026-11-04, the operator:** in this sprint (*"A151 si, incluilo"*). It goes out with block 3's
  release, whose WF gate restarts a WF and checks its reconcile.
- **Fixed (in code for the next release, SY.timer 1.3):** `TIMER_LIST` answers in pages.
  - A page holds the timers that fit in 96 KiB, encoded (`maxTimerListPageBytes`).
  - `next_cursor`, the last timer's `<fire_at_utc_ms>:<uuid>`, is where the next page starts.
  - The list goes by fire time, then uuid.
  - Both SDKs' `List` follow the pages up to the limit, so a WF's `Recover` gets them all.
  - `SY.admin`'s `timer_list` takes `cursor` for the next page; its contract and Archi's query
    translation name it.
  - A cursor that is not one is `TIMER_INVALID_PAYLOAD`.
  - Tests: `TestTimerListAnswersInPagesThatFitAFrame` (300 timers of about 1 KiB),
    `TestListFollowsSYTimersPages`, `timer_client_list_follows_sy_timers_pages`.
- **Checked on 8.x (0.1.100, 2026-11-05):**
  - 350 timers of about 1 KiB for a throwaway node, more than one frame: `TIMER_LIST` answered
    them in 5 pages (79, 79, 79, 79 and 34), each about 99 KB. SY.admin's `timer_list` gave the
    same pages with `cursor`, and the HELP says 1.3 and names `cursor`.
  - **A WF's start, the case of this finding:** 350 timers of the WF's client_ref format, pending
    for `WF.gate0201@worker1` before the WF existed. The WF started, its `Recover` listed all 350
    across the pages and cancelled each as an orphan, in 8 s. No `TIMER_LIST` failed, and the node
    never ran DEGRADED.
  - The throwaway timers were cancelled. They stay in SY.timer's history until its GC, at its
    first start after 7 days (docs/sy-timer.md §5.2).

### A-150 [WF] ✅ VALIDATED ON 8.x (0.1.108, 2026-11-07; kept open since WF Case 1, 2026-10-29; recorded at the WF close, 2026-11-04) — With tools, an agent's response contract is an instruction checked afterwards, not enforced by the provider

- **What happens:** a WF's response contract (`response_envelope`) reaches the provider as an
  output schema it enforces only on a call without tools. With tools, the contract is an
  instruction in the system prompt, and ai.generic checks the answer afterwards
  ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.2.1, item 3).
- **What it breaks:** an agent with tools can answer outside its contract. The answer is then
  `invalid_structured_output`, and the WF's catch takes it.
- **To evaluate:** whether the provider can enforce the schema inside the tool loop.
- **Evaluated and built (2026-11-07), for 0.1.108, alone:** it touches the shared AI SDK.
  - Both providers can. Anthropic holds the text to `output_config.format` next to `tools`; OpenAI
    holds it to `text.format` when the contract is `strict`, which needs every property required.
  - The SDK sends the contract's schema on every tool turn. A 400 that refuses it, from either
    provider, has that turn asked again without it (the review round added OpenAI's).
  - **What it does not reach:** a WF asks an agent with a menu (§5.3.1), which has no schema, and
    wfcel refuses a `response_envelope` in a WF's action. The fix reaches a contract that a caller
    other than a WF writes, or that a WF's `forward` keeps. Whether a menu becomes a schema is the
    operator's call: Anthropic can hold a root `anyOf`, and OpenAI's strict mode cannot (A-168).
- **Validated on 8.x** (0.1.108, three runs): AI.pruebas, asked for a poem in prose and a saved
  text file under a contract (`reply`, and `mood` in an enum, `text_field` `reply`), logged
  `contract="envelope" tool_count=14 output_contract_mode="structured_output"` on Anthropic
  (`claude-sonnet-5-5`). Each answer was the contract's object, its text the `reply`, with the file
  attached; no schema warning and no retry.

### A-149 [IO] 🟡 OPEN (read 2026-10-16 in the A-68 analysis, checked again 2026-11-03; recorded at the WF close) — The IO family has no written standard, and its nodes answer health and config each their own way

- **No written standard:** [io-nodes-spec](../../docs/io/io-nodes-spec.md) is imported (v1.16) and
  old: its §5 still lets credentials live in the local config or travel inline. In practice the
  reference is io.api, io.cloud and the SDK. io.wapp and io.linkedhelper are test-grade (the
  operator, 2026-10-15).
- **Health:**
  - io.api and io.cloud answer `NODE_STATUS_GET` with the SDK's default
    (`try_handle_default_node_status`, crates/fluxbee_sdk/src/status.rs), which says `HEALTHY`
    whatever the config state, so a `FAILED_CONFIG` node reports healthy.
  - io.web, ai.generic and the frontdesk report the lifecycle with `last_error`, through the SDK's
    `build_node_status_response_payload` (crates/fluxbee_sdk/src/managed_control_plane.rs).
  - io.slack does not answer it (A-148).
- **Config (read 2026-10-16):** each node writes its own `CONFIG_SET` handling, and staleness and
  failure codes differ across io.api, io.slack, io.wapp and ai.generic. The SDK's reply builders go
  unused.
- **The config file:** the SDK persists the effective config with the default mode
  (`persist_effective_config_with_root`), 0644 under the usual umask, where the orchestrator writes
  0600. No IO node keeps a secret there today: credentials are vault references.
- **Boot states:** A-91.
- **Fix:** the IO topic starts by writing the standard every IO node follows; then each node's
  track brings it there.

### A-148 [IO/io.slack] 🟡 OPEN (found 2026-11-03, the review at the WF close; read in the code) — io.slack against the IO standard: what it lacks besides its known findings

io.slack works for what the WF uses: inbound, outbound, threads, menus, "not taken" and files were
validated live in WF Cases 3 and 4. Against the standard it lacks:
- **The architecture, its revamp:** each node needs a Slack app of its own (A-129). The operator's
  definition (2026-11-02): one channel = one ICH = one node, over Slack's HTTP events through the
  edge's fan-out (`fanout_family`, src/bin/sy_edge.rs, built for io.wapp); or one node per
  workspace, multiplexing its channels. A design decision comes first.
- **The socket** (`run_inbound_socket_mode`, nodes/io/io-slack/src/main.rs):
  - one connection. On Slack's `disconnect` it closes it and opens another, with no overlap, and
    logs it at warn;
  - it acks each envelope before handing it to an in-memory queue (`SOCKET_EVENTS_QUEUE`, 1024).
    A restart, a full queue or a lost router connection loses events Slack does not send again
    (`hand_off_envelope`).
- **The tokens:** they come from SY.vault and are swapped hot by the refresh loop. Slack's token
  rotation (tokens that expire and renew with a refresh token) is not implemented, so the app must
  keep rotation off.
- **The control plane:** it answers `PING`, `STATUS`, `CONFIG_GET` and `CONFIG_SET`
  (`handle_io_control_plane_message`), not `NODE_STATUS_GET`. The orchestrator waits out its
  timeout and infers the health (A-149).
- **The outbound:** only Slack's rate limit (429) is retried (`slack_send_with_retry`). The outbox
  with states and the error rules of [io-slack](../../docs/io/io-slack.md) §9–§10 are not in the
  code.
- **The docs drifted:**
  - [io-slack](../../docs/io/io-slack.md) §11, §12 and "Hot configuration" still describe tokens in
    the config and the env, and an `exit 1` without credentials. Since 2026-07-24 the credentials
    are a vault reference (`slack.auth`), and the node boots degraded without them.
  - [io-slack-tasks](../../docs/io/io-slack-tasks.md) is a July task list, stale.
- **Its other findings:** A-85 (the file fetch), A-136 (where a message's thread is), A-114
  (writing first; the family's).
- **Fix:** io.slack's track, inside the IO topic, starting with the architecture.

### A-147 [Archi] ✅ VALIDATED ON 8.x (0.1.99, 2026-11-04; for the next release by the operator, 2026-11-03; found the same day, WF Case 5's gate) — A chat turn that stages a plan answers only "a plan is pending": what the operator asked alongside is dropped

- **Seen on 8.x:** the operator asked in one message for the running conversations, with their state
  and since when, and for one of them to be cancelled. Archi staged the cancel. Its answer was only
  "A compiled executor plan is pending in this chat. Reply CONFIRM to execute or CANCEL to discard."
  The list never reached the operator. Asked alone, in the first run, the list came.
- **Why:** when a turn stages a confirmation, the chat's message is the staged one
  (`latest_staged_confirmation_message`, else `PendingConfirmationFlow::staged_message`), and the
  model's own final text, `raw_message`, is not shown (src/bin/sy_architect.rs).
- **What it breaks:** the operator does not see the answer, nor what the plan will do, besides its
  steps in the page.
- **Fix:** show the model's text with the staged message. The operator (2026-11-03): in the next
  release, since the test found it and it is small.
- **Fixed (0.1.99):** the chat answers the model's own text first, then the staged message
  (`staged_turn_message`, src/bin/sy_architect.rs). A turn with no text of the model's, or one that
  only repeats the staged message, shows the staged message alone. Test:
  `a_turn_that_stages_a_plan_keeps_the_models_answer`.
- **Checked on 8.x (0.1.99, 2026-11-04):** in a new session, one turn asked what SY.timer does
  and staged a restart of `AI.pruebas@worker1`. Archi answered both: its own text, then the staged
  message. The plan was then cancelled. The same answer said how to confirm twice: A-152.

### A-146 [General/router] ✅ VALIDATED ON 8.x (0.1.99, 2026-11-04; for the next release by the operator, 2026-11-03; found the same day, sweeping WF Case 5's gate journals) — The router warns "vpn table empty in config snapshot" each time a node connects, and no VPN rule is the normal state

- **Seen on 8.x:** worker1's rt-gateway logged it 22 times on 2026-11-02 and 50 times on 2026-11-03,
  none on 2026-11-01. It fires on every connect of a node that is not `SY.*` or `RT.*`: the gate's
  clients, agents restarting, an upgrade.
- **Why:** `assign_vpn` (src/router/mod.rs) warns when the config snapshot holds no VPN rule. No
  rule is what a hive without VPNs has.
- **What it breaks:** nothing. It is noise at WARN in every journal sweep, which hides a real one.
- **Fix:** log it at debug, or not at all. The operator (2026-11-03): in the next release.
- **Fixed (0.1.99):** the warning is gone: `assign_vpn`'s debug line before it already counts the
  snapshot's VPN rules.
- **Checked on 8.x (0.1.99, 2026-11-04):** no `vpn table empty` line in the four routers' journals
  since each restarted on 0.1.99. worker1's one line, at 00:14:04 UTC, came from its 0.1.98 router,
  a minute before the core update restarted it.

### A-145 ✅ RESOLVED (0.1.98; by tests, and the second run named its workflow right; found 2026-11-03, WF Case 5's gate) — A workflow name with a hyphen passed SY.wf-rules and its check, and the package's publish refused it after the slots rotated

- **Seen on 8.x (0.1.97):** Archi deployed the gate's workflow as `ventas-gate0195`. The check
  passed, and so did the compile. The publish then refused the runtime:
  `PACKAGE_PUBLISH_FAILED`, "name='wf.ventas-gate0195' does not match runtime naming policy
  (lowercase + dots)".
  - The definition was current already, version 1, with no package and no node. Archi removed it
    on the operator's word (`wf_rules_delete`, through the executor's direct dispatch, 0.1.97).
- **Why:** three rules for one name.
  - SY.wf-rules' `workflowNamePattern` allowed hyphens: `^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*$`.
  - Archi's `valid_workflow_name` and SY.admin's catalog note copied that rule.
  - The runtime naming policy, `runtime_name_regex` in src/runtime_package.rs,
    `^[a-z][a-z0-9]*(\.[a-z0-9]+)*$`, has none. A workflow's runtime is `wf.<workflow_name>`.
- **Built (0.1.98):**
  - SY.wf-rules, Archi and SY.admin's note follow the runtime policy;
  - SY.wf-rules' refusal says the rule (`invalidWorkflowName`), so the check refuses the name
    before anything rotates;
  - the handbook says it (§4.2.7);
  - a test refuses hyphens at compile and at check (`TestCompileWorkflowRejectsInvalidWorkflowName`).

### A-144 ✅ VALIDATED ON 8.x (0.1.97 and 0.1.98, 2026-11-03: every deploy, removal, cancel and kill went as planned, and the chat said a failure; found the same day, WF Case 5's gate) — SY.admin's executor had its model repeat every step's args, and a workflow's definition outgrew its answer; Archi's chat then said the plan finished

- **Seen on 8.x:** Archi's plan deployed the gate's workflow with `wf_rules_compile_apply`. The
  executor's model, on the hive's Sonnet with its default 4,096 output tokens, had to repeat the
  whole definition as the call's args (8 states, 39 guards). Its answer was cut:
  `EXECUTOR_UNAVAILABLE`, "anthropic response truncated: hit max_tokens". Nothing was deployed.
- **Archi's chat answered `status: ok`, "Executor plan finished".** The message fell back to that
  when the run had no summary, and the chat's CONFIRM never read the run's outcome. It also taught
  the cookbook the failed plan.
- **Why the model was there:** each step's args are the plan's. The model may add only what
  `executor_fill` allows, and a change is refused (`merge_executor_step_arguments`), so for a step
  with nothing to fill it could only repeat them. A step that names references
  (`$steps.<id>.<path>`) has them resolved before the model.
- **Built (0.1.96):**
  - a step with nothing to fill is dispatched as the plan wrote it, with no model
    (`executor_step_has_nothing_to_fill`, the direct dispatch the executor already had for a model
    that called no tool). The model runs only for a step with fields to fill;
  - Archi's chat CONFIRM verifies the run as the pipeline does (`verify_execution_result`): a
    failed step, a failed run or a deployed WF node that does not run answers `error`, says why,
    and teaches the cookbook nothing;
  - Archi waits 90 s more for each workflow deploy or removal in a plan.
- **The gate went on:** the agents and their roles were in place. The workflow's deploy waited for
  0.1.96.
- **0.1.96, live:** the chat answered `error`, "Execution stopped at step 's1'", as it should. But
  the step failed: `UNKNOWN_ACTION`, "Unsupported admin action 'wf_rules_compile_apply'".
  - The direct dispatch reused the executor's fallback for a model that called no tool, and that
    went through `handle_admin_command_with_origin`. It routes only the command actions, never the
    wf-rules ones. That was a latent bug of the fallback.
- **Built (0.1.97):** the direct dispatch is the one the model's tool makes,
  `dispatch_internal_admin_command`, boxed since that dispatch runs the executor itself.
  - A test sends a fixed `wf_rules_compile_apply` step through it to a fake SY.wf-rules
    (`a_fixed_workflow_deploy_step_reaches_wf_rules_as_planned`).

### A-143 ✅ VALIDATED ON 8.x (0.1.98, 2026-11-03: the second run set its agents' roles with `hive: worker1`, and they went to the primary; found the same day, WF Case 5's gate) — An identity write through SY.admin works only when its path names the motherbee, and the refusal does not say why

- **Seen on 8.x:** Archi's plan assigned the new agents' roles with `set_ilk_definition` and
  `hive: worker1`, the agents' hive. The step failed: `UNAUTHORIZED_REGISTRAR`, "source not
  authorized for action". Steps 1 to 9 had run (the three agents created and configured).
- **Why:**
  - SY.admin sends every identity command to `SY.identity@<the path's hive>`
    (`handle_identity_command`, src/bin/sy_admin.rs).
  - A spoke's SY.identity is a replica. It admits privileged SY roles of its own hive only (F-07,
    `is_authorized`, src/bin/sy_identity.rs), so `SY.admin@motherbee` is refused there.
  - An ILK is one for the whole system, and only the primary writes it. The catalog's example
    names the motherbee, but nothing says that another hive cannot work.
- **What it breaks:** any identity write Archi or an operator sends with the hive of the node it
  concerns, and the error names no cause.
- **Fix:** SY.admin sends identity writes to the primary, the motherbee, whatever hive the path
  names. Reads may still go to a hive's replica.
- **The gate went on:** the operator told Archi to send them to the motherbee.
- **Built (0.1.96):** `identity_command_hive` sends every identity write to SY.admin's own hive,
  the primary. A read still goes to the path's hive. A test covers both
  (`identity_writes_go_to_the_primary_whatever_hive_the_path_names`).

### A-142 [Archi] 🟡 OPEN, for Archi's chapter (the operator, 2026-11-03; found the same day, building WF Case 5; it bites with io.web's stage 2) — Archi's chat answers only when its turn ends, and the edge waits 30 s for io.web's response head

- **What happens:** Archi's `POST /api/chat` holds the request until the whole turn ends
  (`handle_chat_message`, src/bin/sy_architect.rs). A turn can last minutes: since 0.1.95 each
  of Archi's model calls may take up to 600 s, and a turn makes several, besides admin actions.
  - The edge gives io.web 30 s for its response head (`IO_WEB_HEAD_TIMEOUT`, src/bin/sy_edge.rs),
    then answers `502 UPSTREAM_UNAVAILABLE`.
- **Not live yet:** io.web serves no Archi today (stage 1 answers without reading a request), and
  the operator reaches Archi on the motherbee's own port.
- **What it would break:** once stage 2 proxies Archi through the edge, any chat turn longer than
  30 s answers 502 to the operator while it still runs in Archi.
- **Fix, in Archi's chapter (the operator, 2026-11-03), before stage 2 proxies Archi:** either the
  edge's wait for Archi's routes covers a turn, or Archi's chat answers at once and the page follows
  the turn by its session (the page already refreshes sessions and streams messages).

### A-141 ✅ VALIDATED ON 8.x (0.1.95, 2026-11-03: at the upgrade frontdesk waited for the vault and started its AI; found 2026-11-02, sweeping the journals of WF Case 4's gate) — SY.frontdesk.gov reads its AI key once at boot, so an upgrade can leave it without its AI until a vault notice

- **Seen on 8.x:**
  - At the 0.1.94 upgrade (2026-11-02, 18:52 UTC), the vault did not answer SY.frontdesk.gov's
    lookup within 5 s (`vault action timed out`). It booted `Unconfigured`: "LLM path enables on
    VAULT_SECRET_CHANGED".
  - It stayed so until its next restart (2026-11-03, 02:58 UTC, the switch of the motherbee's
    model), which found the key at once.
- **Why:** its boot lookup is one-shot (`resolve_resource`,
  nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs).
  - A-26 made SY.architect, SY.storage and SY.identity wait for the vault at boot
    (`resolve_resource_awaiting_vault`, `VAULT_BOOT_WAIT`), and SY.cognition waits too. SY.frontdesk.gov
    was left out.
  - SY.admin's executor key has the same one-shot read (`build_admin_executor_ai_runtime`,
    src/bin/sy_admin.rs). This time its boot found the key.
- **What it breaks:** after an upgrade, frontdesk's conversational path is off, and its
  deterministic handoff still works. Nobody used it on 8.x meanwhile.
- **Fix:** both boot reads wait for the vault, as their peers do (A-26). It goes with WF Case 5's
  bugs ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.5).

### A-140 ✅ VALIDATED ON 8.x (0.1.95 to 0.1.98, 2026-11-03: every handover summary named the person and no internal id; found 2026-11-02, WF Case 4's gate) — A definition's texts can say what is no longer true, and the handover summary invites internal ids

- **A text that repeats a timer's duration.** The example's texts to sales say "in 15 minutes" and
  "for 30 minutes", the values of `team_untaken` and `team_quiet`
  (go/nodes/wf/examples/packages/wf.sales.invoice). The gate cut the timers to 2 and 3 minutes, and
  not the texts, so sales told the person "15 minutos" and "30 minutos". A text that repeats a value
  written elsewhere in the definition goes stale when that value changes.
- **The handover summary.** The catalog's `summary` asks for "who the person is" (go/pkg/wfcel,
  `KnownOptions`). With no name in the person's context (the gate's client sends none), sales wrote
  the person's ILK and the IO node's name in the post for the team: "Cliente (ilk:8fc94fa2…, canal
  IO.gate0192)".
- **Fix, with Case 5 (Archi writes WFs):**
  - a rule in Archi's handbook: a text never repeats a value the definition holds elsewhere;
  - the example's texts without the durations;
  - the summary's description: the person by name, with no internal ids. The catalog pins names and
    params, not descriptions (`TestTheKnownOptionsOnlyGrow`), so it may change.

### A-139 ✅ VALIDATED ON 8.x (0.1.98, 2026-11-03, two runs: what reached the person named the person and the person's channel, whoever wrote it; the WF's posts to the team named no one; found 2026-11-02, WF Case 4's gate) — The ILKs on a WF's messages to IO nodes: a forwarded answer keeps the `dst_ilk` of whoever the agent answered, and a WF's own message names the person as its sender

- **What happens:**
  - `forward` sends the message's meta as it came (go/nodes/wf/wf-generic/node/actions.go,
    `execForward`; [wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.2.1, item 5);
  - an agent's reply goes to whoever spoke (crates/fluxbee_ai_sdk/src/message.rs:
    `reply.dst_ilk = meta.src_ilk`).

  So when sales answers a supervisor's message (the consultation's `decided`) or a team member's
  click (the hand-back), the WF forwards that answer to the person's IO node with the supervisor's
  or the clicker's ILK as `dst_ilk`, not the person's.
- **Seen in the gate:**
  - the two `decided` of the consultation, and sales' answer after the hand-back, reached the
    person's client with the operator's ILK as `dst_ilk`;
  - the answers to the person's own messages carried the person's.
- **What it breaks today:** nothing seen.
  - No IO node delivers by `dst_ilk` (read in nodes/io): io.slack, io.wapp and io.linkedhelper copy
    it only into their own control answers, and io.linkedhelper sets it on its inbound only.
  - The WF's leg to an IO node is not copied to cognition: the router copies a delivery to an agent
    and an agent's own sends (src/router/mod.rs, `deliver_locally`).
  - It is a wrong fact in the message, and A-138 makes the message's facts the record every node
    reads.
- **The same family, read in the code the same day:** `send_message` puts the instance's person as
  `src_ilk` and `ich` on every target (actions.go, `execSendMessage`). That is right for an agent,
  which files the message under the person (A-116). On an IO node it is wrong: the post to the team
  (`HANDOVER`, a consult's question) goes out as if the person had written it, with the person's
  channel as its `ich`.
- **The operator (2026-11-02):** important, to fix as soon as possible, together with the details
  this whole test brought (the first run of Fluxbee as an AI system), and then the flow is tested
  again while it is fresh.
- **Fix, decided by the operator (2026-11-02),** with WF Case 5's build
  ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.5). The WF routes, so it owns the
  fact:
  - everything the WF sends to the person's IO node names the person as its `dst_ilk`, and the
    person's channel as its `ich` (added while building, 2026-11-03);
  - a message the WF writes to an IO node does not name the person as its sender;
  - an agent still gets the person (A-116).

### A-138 [WF] ✅ VALIDATED ON 8.x (0.1.100, 2026-11-05; design agreed 2026-11-04, [message-trace-v1](../../docs/message-trace-v1.md); the operator, 2026-11-02, the WF Case 4 design chat) — A message carries only a WF's trail of node names, not its conversation's trace

- **The operator's direction (2026-10-31, put on the table as a topic 2026-11-02):** the facts
  travel in the message itself: the trace, the nodes it passed through, and the state. Each node
  then has the history of facts, not only the LLM's text, and decides within its own focus. *"la
  traza de la conversación en el mensaje creo que es algo que va a ayudar en todo el análisis, no
  solo para esto"*.
- **Today, a part of it:**
  - **The trail** (the operator's idea, decided in docs/wf-orchestration-v1.md §5, built in 0.1.91,
    §5.2.1 item 7): `meta.context.wf.trail` lists the nodes a message went through.
    - Only a WF writes it, when it sends or forwards (go/nodes/wf/wf-generic/node/actions.go,
      `messageTrail`, `withWFBlock`).
    - It holds names only, inside the WF's own block.
    - An agent's answer, an IO's message and a message that goes through no WF carry none.
  - **`routing.trace_id`** is one id, not a path. A WF's sends carry the id of the event they come
    from (`transitionTraceID`).
  - **The rest of the trail** is in the logs and in SY.storage's `turns`. No node reads them when it
    decides.
- **Its first uses, in WF Case 4:**
  - the WF's facts: who held the case and when (A-137);
  - the "same place" / "new subject" mark for io.slack;
  - the facts of the first post to the team: who the person is, and which channel.
- **Who it would serve:**
  - the IO, to place a message;
  - the agents, to know what happened besides what was said;
  - cognition, to know which conversation a turn belongs to (A-137);
  - debugging.
- **What the topic must answer:**
  - what the trace holds, and how far back;
  - who writes each fact (the datum's owner);
  - how its size is bounded;
  - how it relates to `trace_id` and to the router stream's log records.
- **Status:** a topic of its own, after the WF track. Case 4 uses the WF's facts and its own marks,
  and builds no general trace.
- **2026-11-04, the operator:** for the WF track, and important: in this sprint.
- **2026-11-04, the design chat: the direction is agreed** ([message-trace-v1](../../docs/message-trace-v1.md)):
  - each message carries the trace of its cycle, a pulse: `meta.context.trace`, keyed by
    `trace_id`, which is already the cycle's id;
  - each entry holds the time, the source, the destination, and the node's action;
  - each node writes its own entry, and the router writes one only when it picks a destination;
  - plain JSON, not compressed; debug and log, not security;
  - the AI model reads it, and Archi shows it later.
  - The trace replaces the WF's trail, and the actions are free, not a fixed list (the operator,
    the same chat).
  - Closed the same day:
    - `act` holds the node's actions, in short words;
    - a WF lists the actions its transition ran, so an entry can hold more than one;
    - no words from the WF's definition for now.
- **Built (in code for the next release, 2026-11-04;** message-trace-v1 §8):
  - Both SDKs write the sender's entry on every solution message they send (`send_traced`,
    `SendTraced`), so the IO nodes need no change.
  - An agent's reply names the requester and the option it chose.
  - The frontdesk notes whether it assigned an identity (the operator, the same day: *"No estaría
    mal que el frontdesk tenga la acción de asignar identidad o no cuando lo apruebe"*).
  - The WF:
    - sends the trace of the cycle it handles, and names the kinds of action of its step;
    - writes no trail.
  - The router writes its entry when it picks a destination.
  - ai.generic shows the trace to the model, and with cognition takes the "via" from its origin.
  - Tests in each place.
- **Checked on 8.x (0.1.100, 2026-11-05),** with the WF gate0200 on worker1: a client standing for
  an IO node, an internal event, `AI.pruebas@worker1`, and a 60 s timer.
  - The answer reached the client with the cycle's `trace_id` and four entries: the client's, the
    WF's `[forward, schedule_timer]`, the agent's, and the WF's
    `[forward, cancel_timer, schedule_timer]`.
  - The agent, asked to copy its trace, copied the two lines it received.
  - The hold's message came 60 s later under a `trace_id` of its own, with one entry,
    `[send_message]`.
  - `turns` keeps the cycle's five messages, each with its trace.
  - The frontdesk's note rests on its test: no frontdesk runs on 8.x.
- **Found in the gate:** a WF message whose action has no `vars`, `menu` or `subject` carried an
  empty `wf: {}`. While the block had the trail it was never empty. Fixed in code for the next
  release: the block goes only when it carries something, and a forwarded message's block from
  another WF never goes on (`withWFBlock`; test `TestAForwardCarriesNoOtherWFsBlock`). Two
  comments in `wfcel` still named the trail.

### A-137 [WF] 🔵 IN PROGRESS, its own topic (found 2026-10-31, the WF Case 4 design chat; its first part ✅ 0.1.101, its feelings ✅ 0.1.102, where cognition keeps its state ✅ 0.1.103 and the rest ✅ 0.1.104 with A-161's fix in 0.1.105, validated on 8.x, [cognition-hands-v1](../../docs/cognition-hands-v1.md)) — Cognition follows only the conversations an agent is in, so whoever takes over a conversation does not see what was said meanwhile

- **What happens:**
  - **Every user message is stored,** whoever speaks. SY.storage's `turns` keeps who sent it, to
    whom, the ICH, the thread and the content (src/bin/sy_storage.rs, `turns`).
  - **Cognition takes a turn only when an `AI.*` node sends it or receives it** (src/router/mod.rs:
    the sender at entry, and `deliver_locally`).
  - **A communication is (thread, agent).** Two agents in one thread are two communications, and
    each agent gets only its own package (`package_for`).
- **What it breaks:** when a conversation changes hands on its thread, the one who takes it does not
  see what was said in between.
  - **Between agents, today:** when sales takes the conversation back from invoice, it sees none of
    invoice's turns. It sees only the facts the WF passes in its metadata (`meta.context.wf`).
  - **With a person, in WF Case 4:** a team member (Juan) talks with the customer (Ana) on her thread
    from 10:00 to 10:16, and no agent is in that part. At 10:17 Ana asks "¿entonces ya está la nota
    de crédito?", and sales knows the conversation only up to its own summary.
- **Decided for Case 4 (the operator, 2026-10-31):**
  - the WF's trail in the metadata carries the facts: the team held the case from 10:00 to 10:16,
    and Juan handed it back;
  - the team member leaves a note when handing back, which reaches sales as a message and enters
    its memory;
  - a hand-back by the inactivity timer brings no note.
- **The operator's principle for this topic:** a thread_id means a conversation, with or without an
  agent. Cognition should process it, and the content belongs to the interlocutor, agent or not.
- **The operator's open question (corrected by the operator, 2026-10-31):** cognition was built to
  draw out a conversation's deep, abstract sense: the feeling, the history and the events. It also
  shrinks the context as the conversation grows. That focus fits a conversation between an agent
  and a person.
  - The question is which conversations cognition analyzes, and how much of the analysis each one
    gets, when every interlocutor is an agent. The question is not who speaks.
  - The operator: *"si la conversación es entre dos agentes.. no sé si tiene sentido analizar el
    sentir.. puede ser necesario pero no lo veo ahora"*.
  - **Today:** cognition analyzes every communication it gets in full, whoever its interlocutors
    are (src/bin/sy_cognition.rs). Its analysis covers contexts, reasons, memories, the episodes
    an affect fires, and the summaries.
- **The operator's idea for telling them apart (2026-11-02):** a mark in the message, which
  cognition reads to choose how much to analyze. It is the same pattern as Case 4's "same place" /
  "new subject" mark: the message carries the fact, and each node decides what to do with it.
  - **Today:** the router stamps a class on cognition's copy of each turn (`meta.action_class`,
    src/router/mod.rs `cognition_copy`; crates/fluxbee_sdk/src/policy.rs). It classifies the
    action, not the conversation: every user turn is `send_message`. Nothing in the message says
    that every interlocutor is an agent.
  - **Keep the turns between agents detectable,** even while they are not analyzed. A later
    analysis of a whole process, a WF's included, could take them: problems between agents,
    mostly prompt errors, or behavior close to human feeling, which models show more each day. The
    operator: *"sin dudas es algo al futuro"*.
  - The operator's doubt: the "soft" analysis, the feeling, does not serve between agents.
- **What the topic must answer:**
  - who owns a communication with no agent;
  - which hive's cognition keeps it. The A-70 model has no cognition traffic between hives, and in
    Case 4 Juan comes in through io.slack on the motherbee while sales runs on worker1;
  - how the next agent sees what was said on the thread meanwhile;
  - who pays for the analysis. Today the agent's tenant pays for each turn's model calls;
  - in a conversation whose interlocutors are all agents, which analysis runs: all of it, a part
    (say the history and the compaction, without the feeling), or none.
- **Status:** a new topic of its own, *"para darle foco"* (the operator). It is decided with the
  operator before anything is built, and it is not part of WF Case 4.
- **2026-11-04, the operator:** for the WF track, and important: its chat comes in this sprint,
  after A-138.
- **The chat (2026-11-05):**
  - **The operator, on what cognition is for:**
    - It gives a conversation its history as metadata: abstract cues, feelings, what was lived.
      The agent gets it as context, beside the immediate memory, the last turns as they were said.
    - Between agents, only the immediate memory: *"las conversaciones entre agentes solo
      necesitan la memoria inmediata, nada de análisis de metadata cognitiva"*.
    - The first question: can cognition analyze a conversation with several interlocutors, a WF's
      above all?
  - **Verified in the code (2026-11-05):**
    - **The key holds.** The WF puts its instance's thread on every message it sends
      (`execForward` and `execSendMessage`, go/nodes/wf/wf-generic/node/actions.go). So what a
      person says through a WF lands in one communication, the person's thread with the agent.
    - **A forward reads right.**
      - It keeps the message's `src_ilk` and `ich`.
      - Cognition names the node a turn came in through by the origin of the turn's trace
        (`turn_via`, A-138), so the turn reads as the person's, through their IO node.
      - The agent's memory reads as if the person had spoken to it.
    - **The WF's own text reads as the person's.**
      - A `send_message` to an agent carries the person as its `src_ilk` (§5.4.3, the rest of
        A-116).
      - So a timer's "Ana has not answered in 2 days" is analyzed as a turn of Ana's.
    - **An agent's answer that the WF forwards to another agent keeps its ILKs.**
      - For invoice, it is a turn from sales to Ana, through the IO node where the cycle
        started.
      - It is analyzed in full, the feeling included, once in each agent's communication.
    - **The analysis has no speakers.**
      - The immediate memory names each turn's speaker: "you", or the ILK and the node
        (`render_memory_turn`, nodes/ai/ai-generic).
      - The tagger takes one turn at a time, with its ILKs as opaque hints. Both ILKs weigh the
        same (`current_turn_ilk_weights`).
      - The narrator writes the memory and the episodes from labels and signals, with no speakers
        (src/bin/sy_cognition/narrative_summarizer_ai.rs).
      - So the feeling is the conversation's, not a person's. With one person and one agent that
        holds. With several people on a thread, their feelings blend into one, with no name.
  - **Decided (the operator, 2026-11-05): cognition decides which turns it analyzes, turn by turn,
    by their origin and destination.** *"el truco es informarle al cognitive que mensajes tiene
    que analizar cognitivamente y cuales no basado en origen/destino.. [...] deberia ser cognitive
    que sepa o no si tiene que evaluarlo"*.
  - **Agreed with the operator (2026-11-05):** *"si, perfecto hasta ahi"*.
    - **The rule:**
      - A turn is analyzed when a person is at its other end. That is a turn to the agent that a
        person wrote, or an answer of the agent to a person (its `dst_ilk`).
      - The other turns still go into `turns` and the package's last turns, unanalyzed.
    - **Whether an ILK is a person:** the identity memory names each ILK's type, on every hive
      (the workers' replicas copy the primary). Cognition reads it already for the agent's ILK.
      WF, IO and AI nodes are registered as `agent` (`derive_ilk_type_for_node`,
      src/bin/sy_orchestrator.rs).
    - **Who wrote a turn:** the last node in its trace that did not forward it.
      - A WF notes first the action that sent the message (docs/message-trace-v1.md §5).
      - When that node is the person's IO node, the writer is the person named in the
        `src_ilk`. Otherwise it is that node: the agent or the WF.
    - **Nothing new in the router:** it names the WF as the sender both of a forward and of the
      WF's own text. Only the WF knows which it sent, and its trace entry says it.
    - **The `src_ilk` of the WF's own text stays the person (§5.4.3):**
      - The agent keeps each interlocutor's state under the `src_ilk` (`ThreadStateToolsProvider`,
        nodes/ai/ai-generic).
      - With the WF's own ILK there, all the WF's conversations would share one state.
  - **The immediate memory, the operator's next question (2026-11-05):** does it mix? *"si se
    mezcla hay que ponerle interlocutores, sino se hace una ensalada"*.
    - **Verified:**
      - Each communication has one list: everyone who spoke to the agent on the thread, in order.
      - Each line names its speaker: `you`, or `<ILK> via <node>` (`render_memory_turn`, and
        `current_message_label` for the current message, nodes/ai/ai-generic). The labels fall
        short:
        - the WF's own text carries the person's ILK;
        - an ILK is an opaque id, with no type and no name. An IO node sends no name when it
          provisions a person (nodes/io/common/src/provision.rs), so only a registered person has
          one;
        - an agent's answer that a WF forwards names, as its node, the IO node where the cycle
          started;
        - the agent's own turns say `you`, not whom they answered.
    - **Agreed with the operator (2026-11-05), with an addition:**
      - **Each line names its writer:**
        - a person by their ILK and IO node, with their name when the identity has one;
        - an agent or a WF by its node.
      - **The agent's own turns** name whom they answered.
      - **It stays one list:** *"mezclado esta bien, el tema es como identificar a los
        interlocutores"*.
      - **The operator's addition:** the writer's ILK and name both go in the message and in
        storage. A node that sends a message fills a missing name from the identity memory: *"yo
        diria de poner los dos, y que luego con la SHM de ilk se pueda llenar si no esta el nombre
        [...] que los nodos tengan la capacida de resolver eso, el que mande el mensaje"*.
    - **Verified for the addition:**
      - **What `turns` keeps:** `from_ilk` (the `src_ilk`), `to_ilk` and the whole message, its
        trace included, but no name (src/bin/sy_storage.rs).
        - A message with no `src_ilk`, like a WF's to an IO node (A-139), puts its sender's node
          UUID in `from_ilk` (`parse_turn`). That UUID is no ILK.
      - **Names in the identity memory:**
        - Every node has one: the orchestrator registers each AI, IO and WF node with its node name
          as its display name (`ILK_REGISTER`, src/bin/sy_orchestrator.rs).
        - A person has one once registered.
      - **Who can read a name:**
        - The Rust SDK, the IO nodes' and the agents', reads every ILK's type and name from the
          identity memory.
        - The Go SDK, the WF's, finds an ILK only by its channel or its handler node
          (go/fluxbee-go-sdk/identity_shm.go).
    - **Agreed (the operator, 2026-11-05: *"si, cerrado"*):** the writer travels in the message,
      `{ilk, name}`, and no longer comes from the trace.
      - **Who names it:** whoever writes the text. An IO node names the person, an agent itself,
        and a WF itself for its own text. A forward keeps it.
      - **The name:** as it sends, every node's SDK fills a missing name from the identity memory.
      - **Who reads it:** cognition and the agent read it as it comes, and `turns` keeps the
        writer's ILK and name.
  - **Built (2026-11-05), the first part, for the next release:** consolidated in
    [cognition-hands-v1](../../docs/cognition-hands-v1.md), whose §8 lists what changed: the
    writer in both SDKs, named by the IO node, the agent and the WF; cognition's analysis rule and
    the turns' writer and addressee; ai.generic's labels; `turns.writer_ilk` and `writer_name`,
    and `from_ilk` never a node's UUID; Archi's Messages screen. Its gate comes with the release.
  - **Checked on 8.x (0.1.101, 2026-11-05),** with the WF gate0202 on worker1:
    - **The setup:** a client standing for an IO node, `AI.pruebas@worker1` and a 60 s timer.
      Two people of the tenant `pruebas` took part, one registered and one temporary.
    - **Writers:**
      - the client's message named the person;
      - the WF's forward to the agent carried the person, the name filled from worker1's
        identity memory by the WF's SDK;
      - the agent's answer named the agent, and the WF's forward kept it;
      - the WF's own text, at the timer, named the WF.
    - **The agent's memory** (the agent copied its lines):
      - `person <name> (<ILK>) via IO.gate0202@worker1`;
      - `you, to person <name> (<ILK>)`;
      - `Current user message, from workflow WF.gate0202@worker1 (<ILK>)`.
    - **Cognition:**
      - each communication kept its 4 turns, numbered 3: the WF's text was not analyzed;
      - SY.cognition made 4 model calls for the person's cycle, and 2 for the timer's: those of
        the agent's answer to the person, none for the WF's text.
    - **`turns`** keeps each message's writer, and Archi's message detail shows it.
    - **None of the gate's WF messages carried a `wf` block:** 0 of 8. The empty-block fix holds.
  - **Found in the gate:** a person with no name read with their ILK as their name,
    `person ilk:X (ilk:X)`.
    - SY.identity writes an ILK's own id as its display name when it has none
      (`ilk_entry_from_record`).
    - The SDKs now take that as no name (`facts_of`, `lookupIlkAt`). ✅ 0.1.102: in A-155's gate,
      cognition's turns name the temporary person by their ILK, with no name.
  - **The second topic, several people on one thread and the feelings** (cognition-hands-v1 §10):
    agreed and built 2026-11-05, ✅ 0.1.102 with A-155, A-157 and two of A-156's three, validated
    on 8.x (2026-11-06).
  - **Where cognition keeps its state** (cognition-hands-v1 §11): reviewed whole and agreed on
    2026-11-06. A node is born and dies on its hive, and cognition keeps its own file on each
    hive. ✅ 0.1.103 as A-158, with A-156's first, validated on 8.x (2026-11-06).
  - **The rest** (cognition-hands-v1 §12), agreed in the chat of 2026-11-06:
    - whoever takes a conversation back sees what was said meanwhile, each line naming its writer
      (in Case 4, sales sees Juan's part);
    - cognition analyzes people's turns with no agent at either end, on the paths that exist;
    - an episode is the gossip, not a feeling: *"el 'chisme' de que se pelearon"*. Episodes are
      public, like the news, fade with time and not with messages, and an agent gets those of the
      people in its conversation;
    - the ILK that holds an analysis's result pays for it, through its tenant;
    - the lab's POC, read: no numbers for the feelings; its episode, the social event worth
      remembering, is the one Fluxbee never took (§12.2).
  - **Built** (2026-11-06), for 0.1.104 (cognition-hands-v1 §12.7):
    - the router copies what a local WF sends an IO node, a relay between people; cognition joins
      it to the communication of the agent that served the thread last on its hive, or to the
      thread's communication with no agent, whose writer pays (`TenantSource::Writer`);
    - one message copied twice is one turn, analyzed once;
    - the narrator's call may propose an episode, gated as in the POC; a kept episode goes to every
      hive's cognition by the router's broadcast, and an agent's package carries the news of the
      people in its conversation, up to 5, which ai.generic never repeats;
    - SY.wf-rules refuses a definition that names an agent or a WF on another hive, and the
      handbook states it as a rule (§4.2.7);
    - the review round (one, two reviewers): a medium, a WF that sends a person's first message
      to the agent and to another person at once had it analyzed and paid twice, and five lows;
      all fixed, each with its test, but one noted (a definition applied before is checked at its
      next apply).
  - **Deployed** in 0.1.104 (2026-11-07) and checked on 8.x with the gate gate0206 on worker1 (the
    ledger's 0.1.104 has each check): one solution on one hive, the handover's lines, a relayed
    answer not doubled, the episode on both hives and in the package, and a conversation with no
    agent paid by its writers. The gate found A-161: a line a WF relays between two people names
    no one it was said to, so the agent took the blame for B, and the episode names the agent.
    The fix, agreed with the operator, is for 0.1.105, with A-162.
  - **A-161 and A-162** went out in 0.1.105 (2026-11-07) and were checked on 8.x with the same
    gate on new threads: each line a WF relays names whom it was said to, the agent no longer
    takes another person's failure as its own, and cognition reads the quarrel as between A and
    B. That gate found A-163: the episode gate turned the quarrel's 3 proposals down, saying
    nothing of why.
  - **A-163** went out in 0.1.106 (2026-11-07): the gate says why it turns a proposal down. Its
    gate made one episode of A's quarrel with B, intensity 9, and turned one down for weak evidence;
    the episode found A-167.
  - **The operator's call on the numbers** (2026-11-07): a quarrel the gate passes is news, and the
    gate stays as it is (§12.6). **A-167 and A-160** went out in 0.1.107: the narrator says "the
    agent" only of the AI agent, and the factory reset clears every hive's cognition.
  - **The operator's call on handing over between agents** (2026-11-07): an agent never sees
    another agent's turns, so roles do not mix; what matters crosses only through the WF, or the
    person tells it (§12.6). Nothing to build.
  - **Open:** §12.6's conversation that reaches an agent late, and a message split in two batches
    on a new thread.

### A-136 [IO/io.slack] 🟡 OPEN (found 2026-10-31, the WF Case 4 design chat; for the IO track) — io.slack splits one interlocutor's conversation by where they write in Slack

- **What happens:** the thread id comes from the message's place in Slack
  (nodes/io/common/src/io_context.rs, `slack_inbound_io_context`; nodes/io/io-slack/src/thread_records.rs).
  - **At a channel's top level,** a message gets the channel's thread (`PersistentChannel`,
    workspace + channel), and everyone who writes there shares it.
  - **In a Slack thread,** a message gets the native thread's id, or the recorded one when the root
    is a post io.slack recorded.
- **What it breaks:**
  - **A reply written in the channel is lost.** A team member who answers a case's post in the
    channel, and not in its thread, lands on the channel's thread. No WF instance holds that
    thread, so the message is dropped in silence. It happened in both runs of WF Case 3's gate
    ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.3.7).
  - **io.slack's own top-level answer is not recorded.** A reply in that answer's Slack thread
    starts a new thread id (§5.3.7).
- **The operator (2026-10-31):**
  - A conversation always has a thread. What comes from that channel is the same interlocutor,
    who changes thread but always has one.
  - Placing a message in the right conversation is the IO node's job. The WF is not patched for it.
  - **A future idea:** when an IO node cannot tell which thread a message belongs to, it asks
    cognition. Cognition's AI reads the content and puts the conversation on the right thread.
  - The IO nodes' current modes are not set in stone. They come from several eras of code, so read
    the code to understand each mechanism.
- **Meanwhile, decided for WF Case 4:** nothing that comes from an IO node ends in silence. Take a
  message from an IO node the definition involves, on a thread with no conversation:
  - when the definition starts conversations from that node, it starts one;
  - otherwise the WF answers "not taken", and the IO node shows that to the writer as an error.
- **Fix:** the IO family's review (the IO track).
- **Seen again in WF Case 4's review (2026-11-02):** on a thread several people write on, a WF's own
  messages carried the first writer's identity, and an agent's per-person tools took it. Since
  0.1.94 a WF names a person only when its `start_from` nodes' messages share one
  ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.4.11). The cause stays here.

### A-135 ✅ VALIDATED ON 8.x (0.1.94, 2026-11-02; found 2026-10-31, the WF Case 4 design chat) — A WF instance started by a message with no thread makes its own id the thread

- **What happens:** go/nodes/wf/wf-generic/node/correlate.go, `creationThread`. When a message from
  a `start_from` node has no `meta.thread_id`, it starts an instance whose thread is the instance's
  own id.
  - Every send and forward of that instance carries that thread (actions.go).
  - An agent's memory in cognition and the answers to the person would then be filed under an id
    the WF made up.
- **How a message gets there:** today `start_from` takes only IO.\* and WF.\* nodes, and neither
  should send without a thread:
  - The IO family's common layer always computes the thread (nodes/io/common, `compute_thread_id`).
  - A WF sends on its instance's thread.

  Only a sender that breaks that contract gets there: an IO node outside the common layer, or a
  client on the router socket like the gates' own `IO.gate019x`.
- **Why it is there:** it is left over from the first WF node (`9ba5c651`, 2026-04-14). Back then a
  WF's `send_message` used the instance id as its thread. Case 1 (0.1.90, 0.1.91) moved the sends to
  the instance's thread and kept this fallback.
- **Why it matters:** it breaks a principle the operator stated on 2026-10-31. The thread belongs to
  whoever opened the communication, the IO node (see the datum's-owner principle). A WF marks the
  steps and the traffic's rules, and never owns a conversation.
- **Fix (proposed):** a message with no thread starts no instance. It is dropped and logged, like
  any other message that starts nothing, and `creationThread` goes. It goes with WF Case 4's build.
- **Built in 0.1.94:** no instance starts without a thread, and `creationThread` is gone. An IO
  node gets `NOT_TAKEN` `no_thread`; another sender, only for a named request. A creation journaled
  without a thread is dropped at recovery, and answered the same way
  ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.4.10, §5.4.11).
- **Validated on 8.x (2026-11-02):** the gate's message with no thread got `NOT_TAKEN` `no_thread`
  at once, and started nothing ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.4.12).

### A-134 ✅ VALIDATED ON 8.x (0.1.93, 2026-10-30; found the same day, the 0.1.92 gate) — An AI agent sees no dialogue: cognition's package has labels and late summaries, never the turns

- **What happened in the gate:** with ai.generic's immediate memory off (A-133), the sales agent
  forgot its own quote and answered "which plan do you want?" to "dale, facturámelo", so it never
  invoiced. Two turns after a supervisor's approval, it forgot that too, and consulted again.
- **Why** (verified in the code and the history):
  - The package (`build_memory_package_for_thread`, src/bin/sy_cognition.rs) holds contexts,
    reasons, memories and episodes. ai.generic shows four labels of each, and three summaries.
  - The summaries come late. SY.cognition processes turns one by one, each with two model calls
    (about 5 s per turn in the gate, queued), and the summarizer starts with an active scope: the
    gate's first summary came at the sixth turn, after the conversation.
  - The router's stream is not the delay: it wakes its sender on every record, and SY.cognition
    acks a batch at once (`take_stream_batch`).
  - **The history:** the context model of February 2026 (docs/history/11-context.md) gave every
    message the last 20 turns of its conversation (`ctx_window`). The router never filled it: no
    commit of the router touched it. In March, cognition v2 handed the recent conversation to the AI
    SDK's immediate memory (12-cognition-v2 §1, §14), built first for SY.architect, which cognition
    does not serve. The 0.1.79 cleanup removed the unused field.
- **Decided with the operator (2026-10-30):** cognition carries the last 20 turns of each
  communication `(thread_id, agent ILK)` in the package, with who spoke and the node each turn came
  through. They are written at intake, before the slow worker, so the next message already has
  them. They go in the durable state too, so they survive a restart. SY.architect keeps the AI
  SDK's immediate memory.
- **Built** (`65514ecb`, with its review's fixes in `bc374046`):
  - a `MemoryTurn` per turn, with its direction, sender, the node it came through, its text (2,000
    characters at most, one line when shown) and the agent's menu choice; 20 turns and 48 KiB at
    most per communication;
  - written at intake (`take_stream_batch`) and kept in `cognition_threads`;
  - ai.generic shows them as the recent conversation, and names the current message's speaker.
- **Its review** (two reviewers, ten findings, all fixed):
  - a communication not loaded yet could erase its stored turns, and rows of one turn could be
    applied out of order;
  - a package could push a delivery past the router's 128 KiB frame, which is dropped silently: the
    package now gives way;
  - a read of the region could tear; the router now copies before parsing, and caches the parse.
- **Left:** turns taken before the worker loads a communication live only in the region until that
  load; a host reset in that window loses them.
- **Validated on 8.x** (0.1.93, the gate's second run, DEPLOYMENTS.md):
  - Case 1 reached the end: "Dale, facturamelo" came with the quote before it, and sales handed
    over to invoice;
  - a supervisor approved a discount in the Slack thread, and the person's next question got the
    approved total from sales' memory, 3.8 s later. That question reached sales 0.4 s after sales'
    own answer, and its package already held that answer;
  - SY.cognition@worker1 took every turn at intake (16), with no drops or errors.

### A-133 ✅ VALIDATED ON 8.x (0.1.94, 2026-11-02; found 2026-10-30, WF Case 3's review) — ai.generic's immediate memory is the sender's, not the conversation's

- **What happens:** `runtime.immediate_memory` (off by default) keeps an agent's recent turns per
  sender `src_ilk` (`load_immediate_memory_for_input`, `persist_immediate_turn` in
  nodes/ai/ai-generic/src/bin/ai_node_runner.rs; docs/immediate-conversation-memory-spec.md §4
  says so on purpose).
  - In a WF consultation, the supervisor's message reaches the agent with the supervisor's ILK. The
    turn in which the agent tells the person the decision is kept under the supervisor, so the
    person's next message finds no decision.
  - When the same supervisor answers another customer's consultation, the agent reads the first
    customer's turns.
- **Why it matters:** it contradicts the cognition model, where memory belongs to the communication
  `(thread_id, agent ILK)` ([cognition](../../docs/reference/cognition.md) §1).
- **The operator (2026-10-30):** *"la memoria apagada, cognitive debería resolver eso... no
  quisiera usar la memoria propia del SDK de IA"*. The agents run with it off, and cognition is
  their memory. Checked that day: SY.cognition is `ready_semantic_ai` on the motherbee and worker1,
  and the Case 1 gate's threads have memories with their content.
- **To watch in the gate:** the package holds contexts, reasons, memories and episodes; the
  summarizer never sees a message's text, and the region is rewritten after two model calls.
  Whether sales knows a supervisor's decision on the person's next message is checked there.
- **Cognition is the agents' recent conversation since 0.1.93** (A-134, validated on 8.x): the gate's
  sales agent knew a supervisor's decision on the person's next message, with this memory off.
- **Left:** `runtime.immediate_memory` is still an option of ai.generic, keyed by sender. The
  immediate memory stays for SY.architect; for AI agents, remove it (the operator: no SDK memory for
  them).
- **Done in 0.1.94:** ai.generic has no immediate memory of its own. A config that names
  `runtime.immediate_memory` is refused with the reason, on CONFIG_SET and at boot. At the deploy,
  AI.pruebas@worker1 was the one agent whose `config.json` named it, and it got its config again
  without the key (DEPLOYMENTS.md, 0.1.94).
- **Validated on 8.x (2026-11-02):** in the gate, the agents had only cognition's turns. Sales
  answered the person from a supervisor's decision and from the team's note
  ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.4.12).

### A-132 ✅ RESOLVED (0.1.94, WF Case 4; by tests; found 2026-10-30, reviewing WF Case 3) — The WF engine keeps a `cancelling` status that nothing sets

- **What happens:** go/nodes/wf/wf-generic/node: an instance's status may be `cancelling` in the
  store's CHECK, and the engine reads it in instance.go (a fast path to `cancelled`, the internal
  events' loop), dispatch.go (`WF_CANCEL_INSTANCE`), correlate.go and recover.go (timers left
  alone).
  - No code sets it: `WF_CANCEL_INSTANCE` runs the cancel transition at once
    (`runCancelTransition`).
- **Why it matters:** dead paths that a reader must reason about, in the engine's most delicate
  code (recovery, cancellation).
- **Fix:** remove the status from the engine and the store's CHECK, with its paths. Not done in
  Case 3: it is not the case's code.
- **Done in 0.1.94:** the status is gone from the engine, and the store's v5 migration rebuilds
  `wf_instances` without it. A row an older engine left `cancelling` ends `cancelled`. On 8.x the
  gate's store migrated with no instance running.

### A-131 ✅ RESOLVED (0.1.92, WF Case 3; by tests) — A fenced answer followed by another fenced block does not parse

- **What happens:** crates/fluxbee_ai_sdk/src/llm.rs, `extract_json_candidate`, cuts a fenced answer
  at the LAST closing fence.
  - An object in a fence, followed by text that holds another fenced block (an example, say), is
    read from the first fence to the last one. That is not JSON.
  - The answer is then `json_parse_error`; under a WF's menu, `invalid_structured_output` with
    `not_json`.
- **Why it matters:** since 0.1.91, a fenced object followed by text is that object. This case
  escaped it, and a menu's answer goes through the same parse.
- **Fix:** cut at the first closing fence after the object.
- **Fixed in Case 3:** the object is the first whole JSON value, inside a fence or among other text
  (`leading_json_value`), so a fence or braces after it are the model's, and a fence in the
  object's own text stays in it. Without a whole value, the old cuts apply.

### A-130 ✅ RESOLVED (0.1.95, WF Case 5; by tests, its mechanisms for io.slack's revamp; found 2026-10-30, building WF Case 3) — io.slack: "send to channel" and file replies are dropped, a mention may arrive twice, an empty message posts a fallback, and a files-only post is not recorded

- **A reply with "Also send to #channel" never reaches the mesh.**
  - Slack gives it the subtype `thread_broadcast`, and the echo guard drops every message with a
    subtype (`classify_slack_inbound`, nodes/io/io-slack/src/main.rs). The code's comment says so;
    the docs did not.
  - In WF Case 3, a supervisor who ticks it never answers the consultation. The gate's supervisor
    does not tick it.
- **A mention may arrive twice.** A Slack app subscribed to both `app_mention` and
  `message.channels` (or `message.groups`) may deliver a message that mentions the bot as two
  events, with different `event_id`s.
  - The node deduplicates by `event_id` (its inbound loop's `message_id`), so it would relay that
    message twice.
  - Slack's `app_mention` page does not say whether both events arrive, so this is not verified.
  - WF Case 3's gate app subscribes to the messages only, which include the mentions.
- **Fixed in Case 3, by the way:**
  - a blank `thread_ts` panicked io-common's thread id (`expect` in `slack_inbound_io_context`) and
    killed the inbound task silently. A blank one is now the channel's top level, in io-slack and
    in io-common;
  - docs/io/io-slack.md said that inbound is `app_mention` only, and its IO context example had the
    old `slack_workspace` entrypoint and a `thread_ts` as the thread id.
- **A reply with a file never reaches the mesh** (found in Case 3's review). Slack gives a message
  that shares a file the subtype `file_share`, which the same echo guard drops. A supervisor who
  answers a consultation with a screenshot never answers it, and the consultation times out. Only
  an `app_mention` reaches `collect_slack_blob_attachments`.
- **A post with only files is never recorded** (found in Case 3's review). A message on a thread id
  with no recorded place, and with files and no text, is uploaded file by file at the top level,
  and an upload answers no `ts` (thread_records.rs, `OutboundThread::posted`). A reply under any of
  those posts keeps its own thread id, and the thread's next message opens another top-level post.
- **A message with no text posts a fallback in Spanish.** An outbound message whose text is empty
  (an agent's empty `reply` that a WF forwards, say) posts "No pude entregar el mensaje en Slack (sin
  contenido util)." to the person (nodes/io/io-slack/src/main.rs). The handbook (§4.2.4) and the
  cookbook seeds guard that forward with `event.context.evaluation.reply != ''`.
- **Next, in the IO family's track:**
  - relay a `thread_broadcast` as the plain message it is;
  - deduplicate by the message (channel and `ts`), not by the event;
  - drop a message with no text and no blocks, with a log line, instead of the fallback;
  - relay a `file_share` with the checks a plain message gets, and its files;
  - post a new place's files under a first text post (their names), whose `ts` is recorded.
- **Planned into WF Case 4 on 2026-10-31, and taken out on 2026-11-02:** the operator wants
  io.slack changed now only where the WF test needs it. It goes to io.slack's revamp (A-129 has the
  operator's definition). Only one piece goes in Case 4, as part of its marks: a message that ends a
  subject, with nothing to post, closes the menu and posts nothing
  ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.4.10).
- **Seen in WF Case 4's gate (2026-11-02), for the revamp:**
  - **An empty fact shows as its key alone** ("person:"). A WF's `vars` default to `""` when the
    person's context has no such field, and the facts line prints every key
    (slack_menu.rs, `facts_line`). In the gate the client sends no sender; an IO node does.
  - **Slack's formatting reaches the person as typed.** The operator's message in italics reached
    the person's IO node as `_Hola, soy Juan…_`: the text goes out in Slack's own markup, which
    another channel does not read.
- **The operator (2026-11-02): the bugs go now, with WF Case 5's build; the mechanism waits**
  ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.5). With Case 5: the
  `thread_broadcast` and the `file_share`, the deduplication by message, no fallback for an empty
  message, the files-only post recorded, and no empty fact. The text's format across channels
  stays for the revamp.

### A-129 ✅ RESOLVED (0.1.95, WF Case 5: an event of another channel is dropped at info, and the doc says each node needs its own Slack app; one node per channel or per workspace is io.slack's revamp; found 2026-10-30) — Two io.slack nodes on one Slack app lose messages without a trace

- **What happens:**
  - **Slack:** in Socket Mode, "when multiple connections are active, each payload may be sent to
    any of the connections" (docs.slack.dev, *Using Socket Mode*). Each event reaches one
    connection, not all of them.
  - **io.slack:** a node keeps one Socket Mode connection and is bound to one conversation. It drops
    an event of another channel, with only a `debug` line ("dropping inbound Slack event outside
    configured binding", nodes/io/io-slack/src/main.rs, the inbound loop).
  - **The result:** two io.slack nodes on the same Slack app split its events between them. Each
    drops the other's share, and nothing above `debug` says so.
- **Why it matters:** WF Case 3 puts a second io.slack (the supervisors') next to the person's. A
  multi-channel deployment is the normal shape.
- **Not documented:** docs/io/io-slack.md does not say that each node needs its own Slack app. Its
  stale inbound line was fixed in Case 3 (A-130).
- **Today:** each io.slack node needs its own Slack app, with its own `xapp-` and `xoxb-` tokens. WF
  Case 3's gate does that.
- **Options, for the IO family's track:**
  - one connection per app that hands each event to the node bound to its channel, as io.wapp's
    fan-out does for one Meta webhook (docs/io-wapp-design.md);
  - or one node that serves several bindings.

  Either way, the doc says it, and a dropped event is logged above `debug`.
- **The operator (2026-11-02):** the log line and the doc go with WF Case 5's build; the mechanism
  stays for the revamp ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.5).
- **Planned into WF Case 4 on 2026-10-31, and taken out on 2026-11-02.** The plan was a relay: the
  node that gets another channel's event hands it to the node bound there, as a mesh message.
  - The operator: it looks like a workaround, and not the way the system must end up. *"si sirve
    para la prueba, dale para adelante"*, but the test does not need it, since each io.slack node
    of the gate has its own Slack app.
  - The operator does not want io.slack changed now beyond what the WF test needs: *"lo que no
    quiero es abrir mil frentes"*.
- **The operator's definition for io.slack's revamp (2026-11-02):**
  - io.slack must manage its whole channel and its messages, not work half from outside through the
    bot as today;
  - Slack channels are unique, and each one is one ICH and one node: *"así es más acotado y
    atómico"*;
  - if that cannot be done, one IO.slack node per workspace, multiplexing its channels.
- **Whether one node per channel can be done with one Slack app** (read on 2026-11-02, for the
  revamp to decide):
  - **Over Socket Mode, no:** Slack hands each event to any one of the app's connections.
  - **Over Slack's HTTP events, yes:** an app has one Request URL, for its events and its
    interactivity. The edge's fan-out endpoint, built for io.wapp (src/bin/sy_edge.rs,
    `fanout_family`; docs/io-wapp-design.md §5), hands each event to the whole `IO.slack.*` family,
    and each node takes its own channel. That is io.wapp's decided model for Meta's single webhook.
- **Fix:** io.slack's revamp, in the IO track.

### A-128 ✅ VALIDATED ON 8.x (0.1.95 to 0.1.98, 2026-11-03: Archi checked every definition with `wf_rules_check`, a read that stages nothing; found 2026-10-30, the 0.1.91 gate) — A wf-rules `check` stages the definition it checks

- **What happens** (go/sy-wf-rules/node/dispatch.go): `check` is `compile` under another name, and
  a compile writes the definition to `staged` ([sy-wf-rules](../../docs/nodes/sy-wf-rules.md),
  "Compile").
  - SY.admin describes `wf_rules_compile` as "Compile a workflow definition without applying it".
    It does not say that the definition is staged.
  - In the 0.1.91 gate, the run of refused definitions also compiled a valid one. It stayed staged
    until the workflow's delete.
- **Why it matters:** an `apply` without a `version` deploys whatever is staged. A check that Archi
  runs before asking the operator leaves its definition one `apply` away from going live.
- **Next:** WF Case 5, the check before deploy
  ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §4.7). It should be a read-only check,
  as OPA's `opa_check` is.

### A-127 ✅ RESOLVED (0.1.95, WF Case 5; by tests; found 2026-10-30, the 0.1.91 gate) — SY.admin's node control and message routes send a bare node name as is

- **What happens** (src/bin/sy_admin.rs):
  - **Control:** `POST /hives/{hive}/nodes/{name}/control/config-get` and `/control/config-set`
    (`handle_node_control_command`) send to the name as given.
    - A name with `@hive` must match the path's hive.
    - A bare name goes out without a hive.
  - **Messages:** `POST /hives/{hive}/nodes/{name}/messages` (`handle_send_node_message`) does the
    same, and does not even compare a qualified name with the path's hive. The path's hive only
    shows in the answer.
  - **The router does not know a bare name.** The answer is `TRANSPORT_ERROR transport
    unreachable: reason=NODE_NOT_FOUND`. In the gate, `config-get` on `AI.gate0191sales` failed that
    way, and on `AI.gate0191sales@worker1` it answered.
  - **The routes that go through the orchestrator take a bare name** on the same path: `status`,
    `help`, `restart` and the node's `DELETE`. The gate used them all that way.
- **Why it matters:**
  - On the same path, one route takes a bare name and the next does not.
  - The error reads as a transport failure, not as a wrong name.
  - Archi calls these routes for the operator.
- **Fix:** both handlers qualify a bare name with the path's hive. `messages` also refuses a
  qualified name of another hive, as the control handler does.

### A-126 ✅ VALIDATED ON 8.x (0.1.98, 2026-11-03: Archi deleted its roles once no ILK named them, with no force, both runs; found 2026-10-30, the 0.1.91 gate) — Archi deletes an agent asset that an agent's definition still names

- **What happens:**
  - **The delete checks nothing.** `DELETE /api/agent-assets?hash=` and Archi's `delete_agent_asset`
    tool (`delete_agent_asset_from_disk`, src/bin/sy_architect.rs) remove
    `blob://agent-assets/<hash>.json`. They do not look for the ILK definitions that name it.
    - The answer carries a `definition_warning`.
    - The tool's description and Archi's handbook (docs/archi/handbook_fluxbee.md) ask Archi to
      take the hash out of active definitions first. Nothing enforces it.
  - **Syncthing carries the delete to the workers** (`agent-assets/` is shared, A-72).
  - **A running agent does not notice.** While its definition's hashes stay the same, it reuses
    what it composed and never reads the folder again (`should_reuse_cognitive_state`,
    nodes/ai/ai-generic/src/bin/ai_node_runner.rs).
  - **At its next start it loses the asset.** Before its first definition, a missing asset is
    composed as it is (`compose_cognitive_state`):
    - when the role was its only asset, the agent answers with the unconfigured prompt
      (`definition_state` `error`);
    - with other assets, it runs without that one (`partial`).
- **Why it matters:** the delete looks harmless, and the damage shows at the next restart (an
  upgrade, a reboot), far from the delete. People then talk to an agent without its role.
- **Seen in the 0.1.91 gate:** the cleanup had to purge the three agents before it deleted their
  roles.
- **The docs drift:** docs/identity-v2.1-agent-definition-addendum.md says that the agent "can
  become `partial` until the definition is updated", which reads as right away. In the code, the
  change comes only at the agent's next start.
- **Options:**
  - refuse the delete while an ILK definition names the hash, list those ILKs, and give the
    operator a `force`;
  - or show who uses each asset in `list_agent_assets`, so that the operator sees it before
    deleting.

### A-125 ✅ RESOLVED (0.1.92, WF Case 3; by tests; found 2026-10-29, the 0.1.91 review) — A WF whose agents keep answering each other runs without end

- **What happens** (go/nodes/wf/wf-generic/node/): nothing bounds how many messages an instance
  sends between two triggers (a person's message, a timer fire). `maxInternalEventsPerEvent` bounds
  internal events within one event, not a cycle across agents' answers.
  - A definition where an agent's answer leads to asking another agent, whose answer leads back,
    runs as long as the agents keep answering. Each turn is a model call.
  - Case 1's first `wf.sales.invoice` did that: a handback asked sales again with the same message,
    and a person's text could steer both agents into bouncing it. The review's probe made 101 agent
    requests from one message.
- **Fixed in the example (0.1.91):** a handback delivers the answer and waits for the person, so
  one message costs at most two agent calls.
- **Why it stays open:** Archi will write such definitions once its guide teaches handbacks
  (Case 3), and a cycle is a definition bug the engine cannot tell from a bounded negotiation.
- **Decided with the operator on 2026-10-30, in WF Case 3**
  ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.3.3). Case 3's catch makes such a
  loop easy to write, so the cap comes with it:
  - at most 8 messages to agents between two triggers;
  - past it, the send is not made, and the instance takes it as an error (`agent_call_cap`): the
    catch, or else the log;
  - the instance does not fail, so its timers stay.

  The option first recorded here failed the instance.
- **Built in 0.1.92** (`maxAgentCallsPerTrigger`, go/nodes/wf/wf-generic/node), with its tests in
  catch_test.go. The heading said OPEN until 2026-11-02.

### A-124 [Archi] ✅ VALIDATED ON 8.x (0.1.139, 2026-11-13; a vault value a plan's step answered shown once in Archi's answer, kept masked in the stored session and absent from SY.admin's journal, with exactly one `executor_review` call; a step with `executor_fill` that would get a secret refused in Archi in 0.49 and 0.55 s) (found 2026-10-29) — A Cloud token published through Archi reaches the AI provider and stays in Archi's history

- **What happens:** `publish_cloud_endpoint` answers with the new bearer, once
  ([io-cloud-api](../../docs/io-cloud-api.md) §2 and §7). Through Archi, that answer goes further:
  - SY.admin's executor runs every step of a plan through a model (`create_function_calling_model`
    in src/bin/sy_admin.rs). The action's answer, token included, goes back to the model as the
    tool's result, so the hive's AI provider receives it.
  - The step's `result_preview` carries the same answer to Archi, which stores the whole response
    in its `messages` table (`persist_chat_exchange` in src/bin/sy_architect.rs), in
    `/var/lib/fluxbee/nodes/SY/SY.architect@motherbee/architect.lance`. Archi redacts the
    parameters it stores (`redact_json_secret_fields`), not the answer.
  - SY.admin's own logs keep no token: the executor's run log is redacted
    (`redact_executor_log_value`), and the command log keeps the status, not the answer.
- **Why it matters:** the token is the platform credential for every tenant of the hive, and the
  contract says it lives only in the vault. Archi is the operator's interface, so rotating the
  token there is the path an operator will take.
- **Today:** rotate it through the motherbee's HTTP API (`POST /hives/motherbee/cloud/endpoint`),
  which answers without a model.
- **Options:** run `publish_cloud_endpoint` without the model and have Archi show the token once
  without storing it; or keep the token out of what the model and Archi's history receive.
- **Unchanged by 0.1.137 (noted by A-322's review, 2026-11-12):** SY.admin's executor model still
  receives `publish_cloud_endpoint`'s token, and Archi still stores whole executor responses,
  `result_preview` included, in its `messages` table. A-322's new turn takes an allowlist of each
  step's fields, never a step's answer copied, so the token does not reach that turn.
- **The operator's call (2026-11-12):** *"Mostrar una vez, no guardar"*: Archi shows the token
  once in the chat and stores it masked in its history (and so in the snapshots). The bug push's
  verification (2026-11-12) read the AI-provider half as closed: SY.admin runs the step with no model
  since 0.1.95, and A-322's report carries no token. In the bug push (item 30).
- **Built on `coa/architect-139` for 0.1.139** (`src/bin/sy_architect/shown_once.rs`), for the three
  answers that bring a secret once: `publish_cloud_endpoint`'s and `externalize`'s token (a fanout's
  `verify_token`) and `vault_get`'s value. Every place Archi keeps an executor answer or its text
  was checked:
  - **Shown once:** the answer the chat gets (the chat's and `POST /api/executor/plan`'s) carries
    the step's answer as it came, and apart `output.shown_once` (`{note, secrets: [{where, field,
    secret}]}`, found outside the operator's own args and plan). The chat shows a *"Shown once:
    copy it now"* block and keeps that answer on screen: its 5 s reload of the history (which
    replaced every answer with its stored copy) waits until the operator writes again or opens
    another chat; it starts nothing while a message is in flight, and a reload already under way
    when the answer came checks the hold after its last read, right before it redraws (the
    review: it checked only on entry, so such a reload could redraw over the answer).
  - **The `messages` table** (`kept_response_row`): its `metadata_json` (the whole answer, every
    `result_preview` in it) and its `content` (so the session's preview too) are kept as
    `shown_once::kept` keeps an answer: every field whose name reads as a secret's
    (`key_looks_secret`) and a vault-shaped `value` masked `***REDACTED***`, `shown_once`'s copy
    among them, and each such value of 8 characters or more masked where a text repeats it (a
    message, a summary). A field that holds nothing (a public channel's null token) stays.
  - **`pipeline_runs`**: a run's `execution_report` is kept the same way; its `executor_plan` as it
    came (CONFIRM 2 runs it). Every update of a run's state merges through
    `pipeline_run_with_state_update` (the review: CONFIRM 2 writes each report through
    `advance_pipeline_run`, whose own merge kept it whole; `advance_active_pipeline_run` now).
  - **Archi's memory** is made from those rows (and A-322's turn from its allowlist), so no later
    turn is given the token.
  - **The `operations` table**: its `params_json` was redacted already, but by a shorter list of
    names than SY.admin's run log: `key_looks_secret` takes SY.admin's (`authorization`, `ssh_key`,
    `private_key`, `*_private_key`), where an `add_hive`'s `ssh_key` stayed in clear. Its
    `preview_command` (and the answer's `confirmed_command`) kept an SCMD as typed, an `add_hive`'s
    `ssh_password` in clear: now as the chat's copy keeps it, its body's secrets redacted. Its
    `error_summary` is SY.admin's code or detail, which carry no token.
  - **The operator's own input:** an executor plan pasted in the chat, or sent to `POST
    /api/executor/plan`, is kept with its steps' secret args masked, as an SCMD's body was.
  - **No model:** a step of those three actions with `executor_fill` would go through SY.admin's
    executor model, which gets its answer back; so would a step with `executor_fill` whose args
    take such a step's answer (`$steps.<id>...`), since SY.admin resolves a reference before it
    chooses between its model and a direct dispatch (the review). Both refused by the plan
    compiler's prevalidation, the precheck of a pasted or sent plan, and before any plan runs (the
    pipeline's included). The plan compiler's hard rules say so. Not covered, and not Archi's: a
    secret the operator writes into a plan's args reaches SY.admin's plan review model (A-337).
  - **Archi's log** (`admin_params_for_log`, `admin_payload_for_log`): each admin action and its
    answer go to the journal at info level masked as `shown_once::kept` keeps an answer (the
    review: by name alone, a `vault_get`'s or a `vault_put` step's value that is a plain text, or
    whose fields are not named as secrets, such as a postgres URL, went there in clear).
    SY.admin's command log keeps no answer.
  - Tests (each fails without its fix): `a_token_is_shown_once_in_the_answer_and_kept_masked_in_the_history`,
    `a_pipeline_runs_report_is_kept_masked_and_its_plan_as_it_came` (since the review through the
    way CONFIRM 2 advances a run too),
    `what_the_operator_sent_is_kept_with_its_secrets_masked`,
    `a_plan_whose_secret_would_reach_a_model_is_refused_before_it_runs` (a step that takes a secret
    step's answer too), and the review's `archis_log_keeps_no_vault_value_whatever_it_holds` and
    `the_chat_keeps_an_answer_shown_once_through_a_reload_under_way` (the page's script as served);
    and `shown_once`'s own four. The chat page's scripts were checked with `node --check`; the
    block and the hold only on 8.x.
  - **Not on this branch:** the texts that send a publish *"never through Archi (FINDINGS A-124)"*:
    SY.admin's help for `publish_cloud_endpoint` (`src/bin/sy_admin.rs`), sy-admin.md's row and
    io-cloud-api.md §7, in the files SY.admin's and SY.edge's 0.1.139 branches change (A-317).
    Through Archi is now as safe as the HTTP API; they change with the integration.
  - **At 0.1.139's integration (`coa/sweep-139`):** those texts now send the run again on the
    motherbee's HTTP API or through Archi, which shows the token once and keeps it masked:
    `CONFIG_RECORD_FAILED`'s detail (`cloud_edge_record_failed_detail`) and SY.admin's help for
    `publish_cloud_endpoint`, with their tests (`a_record_that_fails_twice_rotates_nothing`,
    `a_record_write_without_an_answer_may_have_been_written` and
    `the_cloud_endpoint_help_says_what_a_run_does_to_the_token_before_it`), sy-admin.md's row and
    io-cloud-api.md §7. `every_action_the_executor_runs_names_what_its_path_names` keeps
    `publish_cloud_endpoint` apart for the reason left: its handler takes no hive, its path's hive
    unread.
  - **At 0.1.139's integration review:** through Archi the token could still miss the operator.
    Archi now waits for each `publish_cloud_endpoint` of a plan, and another run is refused at once
    (A-317's entry). And SY.admin's cut of an executor answer to the mesh's frame
    (`fit_executor_answer_for_mesh`) never cuts the output of a step of the three actions that
    answer a secret (`EXECUTOR_ACTIONS_ANSWERING_A_SECRET`, as Archi's
    `ACTIONS_ANSWERING_A_SECRET`), where a token cut short would have been shown as the token, and
    once every output is dropped it keeps that step's secret whole (`executor_answer_secrets`:
    `token`, `verify_token`, a vault-shaped `value`), where it kept only the ids
    (`OUTCOME_ID_FIELDS`) and Archi had nothing to show and no warning. The run's log keeps it
    redacted, as before. Test `a_cut_answer_keeps_whole_the_secret_a_step_answered`, failing
    without it; sy-admin.md's cut and sy-architect.md's waits say it. A plan whose answer is still
    too large (`REPLY_TOO_LARGE`) loses the token as any output: only a plan of several hundred
    steps comes near.
- **Validated on 8.x (0.1.139's gate, gate0239, 2026-11-13):** shipped as `3cfaf0e7` (its review
  `d488b25e`; the integration's `c695490c` and `68a292ff`). A value minted on the motherbee into a
  0600 file, stored as `gate0239-shown` over SY.admin's HTTP API (its answer held it 0 times), and
  a one-step plan (`vault_get gate0239-shown`, no `executor_fill`) posted to Archi's
  `/api/executor/plan`. B1 on 0.1.138 (00:44Z) and the first pass (02:02Z) sent a plan shape
  SY.admin refused at parse (400 `INVALID_REQUEST`: no `metadata`, no `execution` fields), before
  its review: nothing shown and no usage record, so B1 is no control. The rerun (05:38Z), the
  plan's shape fixed: the plan ran (ok, 3.98 s); the answer shows the value once (2 occurrences,
  s1's preview and `output.shown_once`, one item, `step s1 (vault_get)`); the stored session holds
  it 0 times, its `shown_once` and s1's preview kept `***REDACTED***`; SY.admin's journal 0 times;
  Archi's Lance files and sy-architect's journal 0 times, with no control (a SEE); exactly 1
  `executor_review` usage record, no `executor`. P4: a step with `executor_fill` that would get a
  secret's answer refused in Archi, (a) a `vault_get` in 548 ms and (b) a `vault_put` taking its
  value in 491 ms, after which no `vault_get` in SY.admin's log and `gate0239-copy` not stored. At
  the deploy sy-architect restarted, and its page carries the shown-once block and the held
  submit. A pipeline run's report kept masked was not provoked: by its tests.

### A-123 ✅ VALIDATED ON 8.x (0.1.91, 2026-10-30) — A message on a running WF instance's thread drives it, whoever sends it

- **What happens** (go/nodes/wf/wf-generic/node/correlate.go): correlation by thread does not look
  at the sender, and an `event_match` names no sender either.
  - An instance's id is the thread of every message it sends, so its targets know it, and so do
    the logs.
  - A node that knows it can send, say, a `HOST_TURN` on that thread, and `wf.router` relays that
    content to the customer.
- **Why it matters:** decision 6 of Case 0 left it open; the operator called it a security topic
  that stays open ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.1).
- **Options, to decide with the operator in Cases 2–3:**
  - take an event on a thread only from the nodes the instance sent to;
  - or have the definitions guard on `event.src`.
- **Decided (the operator, 2026-10-29, WF Case 1):** only the nodes involved drive an instance:
  those written in the definition, and SY.timer for the WF's own timers. Nothing adaptable. It is
  built with Case 1 ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.2).
- **Built in 0.1.91.** The engine takes an event in this order:
  1. the fire of its own timer, from SY.timer of its hive;
  2. a sender the definition does not name is dropped;
  3. a message on a running instance's thread goes to that instance;
  4. a non-reply from a `start_from` node starts one;
  5. anything else is dropped.

  `start_from` is required and refuses `SY.*`, `RT.*` and `AI.*`, and `event_match.src` names a
  sender.
- **Validated on 8.x (2026-10-30, the 0.1.91 gate):**
  - a stranger on a running conversation's thread was dropped ("the definition does not involve
    its sender");
  - an agent's late answer, on the closed conversation's thread and on a fresh one, started
    nothing;
  - `AI.*` and `SY.*` in `start_from` got `COMPILE_ERROR`.

### A-122 [General/upgrade] ✅ VALIDATED ON 8.x (0.1.109, 2026-11-07; found 2026-10-28, 0.1.89 release) — A base runtime published by hand keeps `current` through every upgrade, and only a manual delete hands it back

- **What happens:**
  - The 0.1.88 gate published `ai.generic 0.1.88-gate1` with publish-ia-runtime.sh, which made it
    `current`. AI.pruebas ran on it from then on.
  - The package's seed (`fluxbee-seed-runtimes`) forces a base runtime's `current` only when the
    version it names is gone from disk. A hand-published version still on disk keeps `current`.
    The package's version is published as available only, and the only trace is a NOTE in
    dpkg's output.
  - So the 0.1.89 upgrade would have left AI.pruebas on the gate's build, and the gate would
    have tested 0.1.88 code.
  - The admin cannot hand `current` back:
    - `remove_runtime_version` refuses the current version while another one exists
      (`RUNTIME_CURRENT_CONFLICT`);
    - no action sets a base runtime's `current`.
- **Why it matters:**
  - Since 0.1.89 (A-110), publishing a base runtime by hand is a supported tool. After one such
    publish, every later package leaves that runtime behind without saying so where the operator
    looks.
  - The only way back is a delete by hand, outside the admin and out of Archi's reach.
  - The 0.1.88 ledger even said to remove the version *after* the next release. That would have
    left `current` dangling, because the seed had already run.
- **Done for 0.1.89:**
  - the gate's `0.1.88-gate1` directory was deleted on the motherbee just before the deploy;
  - the seed took `current` back ("moved 0.1.88-gate1 -> 0.1.89 (package authority)") and pruned
    the version from the manifest;
  - AI.pruebas moved onto 0.1.89 by itself, within the reconcile pass.
- **Fix:** later, explicit and visible:
  - an admin action, reachable by Archi, that sets a runtime's `current`, or returns a base
    runtime to its package's version;
  - and the deploy reporting a kept hot-published `current` in its result.
- **Built (0.1.109):**
  - **The seed records the package's version** as each base runtime's `package_version`
    (`publish-runtime.sh --package`), and `list_runtimes` and `get_runtime` show it next to
    `current`: a `current` that differs is a hand publish.
  - **`reset_base_runtime`** (SY.admin, motherbee-only, closed to the mesh, behind the
    operator's CONFIRM through Archi) makes `package_version` current again, once the version
    can run as the orchestrator judges it. The orchestrators move the nodes on their next pass.
  - **The way back is kept:** `remove_runtime_version` never removes a base runtime's
    `package_version` (`RUNTIME_PACKAGE_VERSION`), and `publish-runtime.sh` refuses a hand publish
    under it.
  - **Visible at the deploy:** the seed's NOTE names the action, and the lab's deploy shows it.
  - **Its review round:** a base with packages on it keeps its leftover version
    (`RUNTIME_HAS_DEPENDENTS`), now said in the docs; A-171 found on the way.
- **Validated on 8.x (0.1.109):** a hand publish of `ai.generic 0.1.109-gate1` showed as one next
  to `package_version` 0.1.109, and AI.pruebas moved onto it in 81 s. Removing 0.1.109 answered
  `RUNTIME_PACKAGE_VERSION`; a hand publish under 0.1.109 was refused. Archi staged one
  `reset_base_runtime` step, ran it on CONFIRM, and AI.pruebas was back in 47 s. Removing the
  gate's version through Archi found A-172.

### A-121 ✅ RESOLVED (0.1.89; by tests, not exercised on 8.x) — A node whose purge failed on SY.timer comes back within 60 s

- **What happens:**
  - `kill_node` with `purge_instance` stops the unit, then purges the node's timers.
  - When SY.timer does not answer, it returns `TIMER_PURGE_FAILED` with `relaunch_on_boot` still
    set. Only the plain stop clears it.
  - Since 0.1.88 the 60 s reconcile relaunches such a node. So the operator's "purge" leaves the
    node answering people again a minute later. Each retry stops it, and it comes back again.
- **Fix (0.1.89):** the purge records "do not relaunch" right after the stop, before the timer
  purge (`clear_relaunch_on_boot`). The docs already said the node stays stopped.
  - `remove_node_instance` had the same hole: a node down after a crash keeps its flag. It gets
    the same fix.
  - Both `TIMER_PURGE_FAILED` answers report `relaunch_on_boot: false`.
- **Status:** the orchestrator's tests cover it. It was not exercised on 8.x, because that needs
  SY.timer down.

### A-120 [Archi] ✅ VALIDATED ON 8.x (0.1.106, 2026-11-07; found 2026-10-27, 0.1.89 review) — Archi cannot take a page down

- **What happens:** `unpublish_artifact` is reachable only:
  - by an AI node, on its own pages, over the mesh;
  - by the operator's loopback route.

  Archi's mesh access refuses it, and its executor excludes mesh-only actions.
- **Why it matters:** the operator's rule that every operator feature is reachable by Archi: *"nadie
  va a mandar curls nunca más"*.
- **Fix:** later, with Archi's admin actions. Taking down any page is an operator power, so it goes
  through Archi's confirmation flow.
- **Fixed in 0.1.106:** SY.admin's executor, which Archi runs confirmed plans through, runs
  `unpublish_artifact` as the operator, for any page; the action needs the operator's CONFIRM. An
  AI node keeps its own path, on its own pages. A link's key is kept only as its SHA-256 in the
  command log, the executor's run log and, after the review round, Archi's journal.
- **Validated on 8.x:** AI.pruebas published a page. Asked to take it down, Archi staged a
  one-step plan and ran nothing until CONFIRM; then the link answered 404. The command log shows
  `origin: executor` and `key_sha256`, and no journal or run log holds the raw key.

### A-119 ✅ VALIDATED ON 8.x (0.1.90, 2026-10-29) — The WF engine's control verbs and timer fires accept anyone

- **What happens:**
  - The engine checks no caller on `WF_LIST_INSTANCES`, `WF_GET_INSTANCE`, `WF_GET_CODE` or
    `WF_CANCEL_INSTANCE` (go/nodes/wf/wf-generic/node/dispatch.go). The router does not protect
    them either.
  - So any node of the VPN can read an instance's state, which holds people's reply targets, or
    cancel it.
  - A `TIMER_FIRED` is correlated by its `client_ref` from any sender.
  - An `INVALID_INPUT` answer carries no thread, so two WFs could bounce it back and forth (read,
    not run).
- **Fixed (0.1.90, [wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.1 item 6):**
  - **The WF_\* verbs** (`WF_HELP` and `WF_GET_CODE` too) answer only SY.admin@motherbee and
    SY.wf-rules of the WF's hive, by the router-stamped name. Anyone else gets `UNAUTHORIZED`.
  - **A `TIMER_FIRED`** is taken only from SY.timer of the hive, and only for a timer the WF
    scheduled (its `owner_l2_name`). SY.timer lets any node schedule a timer whose fire goes to
    another node, under SY.timer's own name (A-115).
  - **Never answered, and never the start of an instance:**
    - an error answer, a `*_RESPONSE` or a router notice;
    - a system message other than `TIMER_FIRED`;
    - a late answer on the thread of an instance that ended. An agent answers with the request's
      own name, so before this rule the answer started a new instance, which asked the agent
      again: a WF↔AI loop the review reproduced.
- **Validated on 8.x (2026-10-29),** on `WF.gate0190@worker1`, from a throwaway node
  `IO.gate0190p@worker1`:
  - **`WF_LIST_INSTANCES`** got `UNAUTHORIZED`, naming who may ask. SY.wf-rules@worker1 still
    counted the instances.
  - **A forged `TIMER_FIRED`** was refused and not answered.
  - **A timer the probe scheduled at SY.timer@worker1** with an instance's `client_ref`: SY.timer
    accepted it, and the WF refused its fire ("the timer was scheduled by IO.gate0190p@worker1").
  - **A message on an ended instance's thread** was dropped as a late answer.
- **Still open** (the operator: a security topic that stays open): a message on a running
  instance's thread drives it, whoever sends it (A-123).

### A-118 ✅ RESOLVED (0.1.90; the restart and the redeploy validated on 8.x, the rest by tests) — The WF engine: a panic, an unbounded loop, lost and repeated events, instances moved by a redeploy

Details are in [wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §3.2, read in the code at
`e49ffeaa`.

- **A CEL map literal in `set_variable` panics the node** (read, not run).
  - The receive loop has no recover.
  - From an internal event, it replays at every start.
- **An `emit_internal_event` that re-emits itself never stops,** and it blocks the node's only
  receive loop.
- **A crash loses the event in progress,** and one during creation leaves a half-started instance.
- **A redeploy moves running instances onto the new definition,** where the docs say they keep
  theirs. An instance whose state was removed errors forever.
- **A fired timer can fire again after a restart.**
- **The WF crash-loops at start while SY.timer is unreachable.**
- **Numbers change type across a restart.**
- **A `$ref` to a missing key fails the instance.**

**Fixed (0.1.90, wf-orchestration-v1 §5.1):**

- **The panic:** a CEL value is kept as plain JSON, and a panic fails its instance, never the node.
  - The review found a fatal error that no recover catches: a guard left running past its timeout
    read the state while the next transition wrote it. Guards now evaluate on the caller's
    goroutine.
  - `set_variable` has a cost limit.
- **The loop:** at most 100 internal events per inbound event. Past that, the instance fails
  (`internal_event_cap`).
- **At least once:**
  - an inbound journal, and a creation in one commit;
  - recovery runs again what a crash left;
  - an event that does not finish in 3 starts fails its instance (`replay_exhausted`), so one bad
    event cannot crash-loop the node.
- **A redeploy:**
  - each instance records its definition's hash and ends on it;
  - recovery binds by hash;
  - an instance whose state its definition lacks fails at load (`state_not_in_definition`).
- **Timers:**
  - a fired timer's row goes with its fire;
  - recovery fires the lost ones in time order, from the store;
  - a start without SY.timer runs DEGRADED, and schedules again what SY.timer lacks once it
    answers.
- **Numbers** keep their type live, after a restart, and as written in a definition (an integer
  literal used to go out as `3.0`).
- **`$ref`:** `{"$ref": p, "default": v}` gives a default.
- **SQLite** runs FULL sync, as SY.timer does.

**Validated on 8.x (2026-10-29):**

- **A restart** of the WF node while an instance waited on its timer: recovery loaded it,
  SY.timer fired once, and there was one answer.
- **A redeploy** while an instance ran: it ended on its own definition (v1's reply). The old
  definition was dropped when it ended, and the next instance took v2.
- **The journal** was empty after every transition.

**Left (low; found by tests or by reading):**

- recovery reads at most 1000 timers (`TIMER_LIST` has no pages);
- a database error during a commit leaves the live instance ahead of the store until the next
  start;
- a past-due timer can fire twice when SY.timer's own fire races recovery's synthetic one;
- a timer scheduled again by the degraded reconcile takes `missed_policy` `fire`;
- a creation whose failure handling also panics leaves no instance row.

### A-117 ✅ VALIDATED ON 8.x (0.1.90, 2026-10-29) — Every shipped WF timeout is dead, and a send to an agent hangs

- **No timeout ever fires.**
  - SY.timer puts the key at `payload.user_payload.timer_key`.
  - The example packages (`wf.router`, `wf.invoice`) test `event.payload.timer_key`, and the
    docs test `event.user_payload.timer_key`. Both are always false.
- **A send to an agent hangs without a trace.**
  - `send_message` defaults to `meta.type: system`. The agent answers that from its control plane,
    and the WF drops the answer.
  - Archi's two seeds, `wf.invoice` and SY.admin's help example all send without a type.
- **`wf.router`'s error branches never fire:** an agent's error arrives as `<msg>.error`.
- **`wf.invoice` relies on a JSON-Schema default** that is never applied.
- **Fixed (0.1.90, [wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.1):**
  - a WF always sends `user`: `meta.type` and `event_match.type` left the language;
  - `event.timer_key` carries a fired timer's key;
  - the examples, Archi's seeds (`seed_version` 2), SY.admin's help, the spec and the quickstart
    guard on it;
  - `wf.router` matches `<msg>.error`;
  - `wf.invoice` gives its currency a `$ref` default;
  - a test validates every definition in the docs.
- **Validated on 8.x (2026-10-29):**
  - **The exchange:**
    - `WF.gate0190@worker1` sent `GATE_TURN` to `AI.pruebas@worker1` as a user message, and the
      agent answered in 2 s (Claude Sonnet 5.5);
    - a 60 s timer fired from SY.timer, and the guard `event.timer_key == 'hold'` took it;
    - the reply reached the gate client by its name, written in the definition.
  - **Archi's seeds** upgraded to `seed_version` 2, on `event.timer_key`.
  - **Refused definitions:** five that the language refuses each got `COMPILE_ERROR` through
    SY.admin, naming the path. They covered `meta.type`, `target_ref`, `event_match.type`, a
    `$ref` with another root, and one with an extra key.

### A-116 ✅ VALIDATED ON 8.x (0.1.94, 2026-11-02; found 2026-10-27; the conversation and the forwarded person fixed in 0.1.91) — A WF in front of a conversation loses the person and the conversation for the agent

- **What happens** (go/nodes/wf/wf-generic/node/):
  - **What an instance sees** of a message (`messageToMap`, instance.go): `trace_id`, `msg`,
    `type`, `thread_id`, `src`, `context` and `payload`. It does not see `src_ilk`, `dst_ilk` or
    `ich`.
  - **Its `send_message`** (actions.go) builds a new message from the definition, with `type`,
    `msg`, `thread_id` and `context`:
    - it sets no `src_ilk`, so the agent keeps one memory for everyone the WF brings (its memory is
      keyed by `src_ilk`);
    - its `thread_id` is the instance's, so every message is a new conversation for the agent (its
      cognition is keyed by thread and agent).
  - **`thread_id_ref`** sends on the person's thread, but only as a terminal reply: an answer comes
    back correlated by `thread_id == instance_id` (correlate.go). So the agent's answer to that
    message finds no instance, and the WF tries to start a new one.
  - **Instances:** each message from a person starts a new instance. An instance is found only by
    its own id, never by the conversation's thread.
- **Why it matters:** the operator's direction (agent-tools-v1 §6.2, §6.3) puts a WF between an IO
  node and an agent whenever a solution needs timers or routing. For that the WF must forward the
  message as received, and keep the person and the conversation.
- **Seen in 0.1.90:**
  - **The review:** a send to an agent with `thread_id_ref` loops. Each answer comes back on the
    other thread, starts an instance, and is forwarded again (a test: 4 instances after 3
    answers). No shipped definition does that.
  - **The gate:** the agent logs `missing src_ilk` on every message a WF sends it, and its
    `llm_usage` row has no counterpart.
- **Next:** [wf-orchestration-v1](../../docs/wf-orchestration-v1.md), Cases 1–2: the forward, and
  the correlation by conversation.
- **Fixed in 0.1.91 (WF Case 1, validated on 8.x on 2026-10-30):**
  - **One instance per conversation:** an instance lives on the thread of the message that
    started it, and every message it sends carries that thread. The agents keep one memory per
    conversation, and the person's next message reaches the instance.
  - **`forward`** sends a message on with its whole meta, `src_ilk` and `ich` included. The agents
    saw the person's `src_ilk` on every forwarded message of the gate.
  - **`thread_id_ref` left the language,** and with it the loop the 0.1.90 review found.
- **Still open:** a message the WF writes itself (`send_message`) carries no `src_ilk`, so the agent
  logs `missing src_ilk`. In the gate it was payments' `PAYMENT_DUE`. Recorded as left out of Case 1
  ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.2.1).
- **Done in 0.1.94:** a message the WF writes carries the instance's person: the `src_ilk` and `ich`
  that its `start_from` nodes' messages share. On a thread several people write on, it carries
  none (the review, §5.4.11; A-136).
- **Validated on 8.x (2026-11-02):** in the gate, the agents' answers to what the WF wrote itself
  (payments' `PAYMENT_DUE`, "nobody took it", "quiet") went to the person's ILK
  ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.4.12).

### A-115 [General/SY.timer] ✅ VALIDATED ON 8.x (0.1.99, SY.timer 1.2, 2026-11-04; the caps by tests; found 2026-10-27, iteration 2 design) — SY.timer: no retry, no caps, an open listing, and a drifted doc

- **What happens:**
  - **A fire is sent once.** If its owner is down, SY.timer ignores the router's `UNREACHABLE`
    and the fire is lost.
  - **A one-shot can fire twice** if SY.timer stops between sending it and saving it (read in the
    code).
  - **After a hive reboot, overdue timers fire before the agents are relaunched.**
  - **No limits:** no maximum delay, no cap per owner or in total, no payload size check (only
    the 128 KiB frame).
  - **Open listing:** `TIMER_LIST` and `TIMER_GET` answer any node of the hive, payload
    included, and SY.admin shows them.
  - **The doc drifted:** docs/sy-timer.md disagrees with the code in ten places. Among them: the
    Rust SDK's `TimerClient`, recurring catch-up, the history GC (at start only), the purge order
    and its failure, the size limits, and the envelope's keys.
- **In 0.1.89:** the doc is corrected.
  - The reminders that would have covered a lost fire in the agent moved to WFs, by the operator's
    decision ([agent-tools-v1](../../docs/agent-tools-v1.md) §6.2).
  - A WF reconciles its own timers at start (go/nodes/wf/wf-generic/node/recover.go). Whether that
    covers a fire lost while it was down is part of the WF review (§6.3).
- **In 0.1.90 (A-118):** a WF fires the timers whose fire was lost while it was down, and schedules
  again what SY.timer lacks.
- **Found in the 0.1.90 review, and seen on 8.x:** any node can schedule a timer whose fire goes to
  another node, under SY.timer's own name. SY.timer does not check `target_l2_name` against the
  requester.
  - A WF refuses a fire it did not schedule, since 0.1.90 (A-119).
  - Whether SY.timer should refuse such a timer for a requester that is not a SY node is for the
    operator.
- **Later:** delivery retries and caps in SY.timer.
- **2026-11-04, the operator:** now, in this sprint: *"ese tipo de cosas se detectan con las
  pruebas y son difíciles de recordar"*. Read in the code the same day: only the WF schedules, and
  always for itself (`timerScheduleOptions`, go/nodes/wf/wf-generic/node/actions.go); ai.generic
  only asks the time (`TIMER_NOW_IN`). So the target rule (a node schedules only for itself, a SY
  node for any) breaks no caller. It is the operator's call.
- **The operator (2026-11-04):** *"avanza como propones"*: the target rule as proposed.
- **Fixed (0.1.99, SY.timer 1.2; [sy-timer](../../docs/sy-timer.md) §7.8, §12.1, §19):**
  - **Saved before it goes.** A one-shot is saved `fired`, and a recurring timer its next instance,
    before the fire is sent: a stop between the two loses the fire and never sends it twice. A row
    that cannot be saved sends nothing, and is tried again in 30 s.
  - **Sent again while the target is down.** The router's `UNREACHABLE` for a fire's trace id
    (`NODE_NOT_FOUND`, `PEER_UNAVAILABLE`, `WAN_UNAVAILABLE`, `GATEWAY_UNAVAILABLE`), or a failed
    send, sends the same fire again after 5, 10, 20 and 40 s, then each minute. It stops an hour
    after the first attempt; sooner for a `fire_if_within` one-shot (its window) and a recurring
    timer (its next instance); a `drop` one-shot is not sent again. The retries live in memory: a
    stop of SY.timer loses them, and a WF's reconcile at its start covers that.
  - **After a reboot,** an overdue fire is queued, not sent before the node serves, and goes again
    until its owner is relaunched.
  - **Caps:** a one-shot at most 366 days ahead (`TIMER_ABOVE_MAXIMUM`); a payload and a metadata
    at most 32 KiB each; 10,000 pending timers per owner and 100,000 per hive
    (`TIMER_LIMIT_EXCEEDED`).
  - **Reads:** a node reads and lists its own timers, `SY.admin@motherbee` any (`TIMER_NOT_OWNER`,
    `TIMER_FORBIDDEN`).
  - **The target:** a node schedules for itself; a SY node for any (`TIMER_FORBIDDEN`).
  - Tests: go/sy-timer `delivery_linux_test.go`, `limits_linux_test.go`, and the read and list
    tests. Found on the way: A-151.
- **Checked on 8.x (0.1.99, 2026-11-04), on worker1:**
  - A throwaway node armed a 60 s timer for itself and left. The fire at 00:23:20 UTC found no
    target (`NODE_NOT_FOUND`) and went again after 5, 10 and 20 s. The node, back at 00:23:46, got
    it at 00:23:55, once (`fire_count` 1). SY.timer logged four `timer_fired` (attempts 1 to 4) and
    three `timer_fire_undelivered`.
  - The node's own list answered. Another owner's list, and a timer for another node, got
    `TIMER_FORBIDDEN`. Another node's `TIMER_GET` got `TIMER_NOT_OWNER`, and the owner's answered.
  - SY.admin listed every owner's timers and read the gate's. The HELP says 1.2, with the new
    error codes.
  - The caps (the delay, the payload, the counts): by tests.

### A-114 [IO] 🟡 OPEN (found 2026-10-27, iteration 2 design) — An agent cannot write first to a person, and a WF does not answer an agent by default

- **What happens:**
  - **No IO node writes to a person first.** Each answers only inside the conversation it brought
    in, by its `reply_target`:
    - io.slack posts in its bound conversation;
    - io.wapp drops a message that has none.
  - **Nothing routes by person:** the router never maps an ILK or a channel to a node, and no IO
    node reads `dst_ilk`.
  - **io.api drops an unsolicited message,** so a reminder's or an answer's turn cannot reach its
    client.
  - **A WF:**
    - answers on its instance's thread, unless its definition sets `thread_id_ref` and
      `target_ref`;
    - starts only on a message its `input_schema` accepts;
    - sets no `src_ilk`.
- **Since the operator's decision (agent-tools-v1 §6.2, §6.3):** agents only answer, and WFs do
  the routing.
  - **Answering later a person who wrote before** is the WF's job: it keeps that message's
    `context.io` (its `reply_target`) and later sends to the IO node with it. The WF review
    confirms the engine allows it.
  - **Writing first to a person who never wrote** is what remains here.
    - The person is registered (the frontdesk or cloud identity), so the ILK is known.
    - The WF sends to the IO node by the name written in its definition.
    - The IO node must deliver to that ILK on its channel without an inbound message.

    That is the IO family's work.
- **Later:**
  - the IO track: addressing a person, and delivering without a pending request;
  - Archi's WF design.

### A-113 ✅ DECIDED by the operator (2026-10-27) — OPA never sees a message that has a destination

- **What happens:**
  - **Only for a message without a destination:** the router runs the user policy only on
    `Resolve` (src/router/mod.rs). Its output is a target, not an allow or a deny.
  - **A unicast**, by UUID or by name, passes only:
    - the VPN check;
    - the system-authority gate on protected verbs.
  - **PROD has no user policy loaded:** `GET /opa/policy` is empty.
  - **The policy's input** has no destination and no tenant. Its `src_ilk` is whatever the sender
    wrote.
- **So:** the operator's rule for `send_message`, *"que OPA decida"*, has no place to run.
  - In 0.1.89 the tool applies its own rule: agents and WFs of its own tenant.
  - The router's VPN check still applies.
- **Decided by the operator:** OPA stays for rules of substance: security, general systems, and
  implementations that need clear rules. It does not gate agents' messages.
  - A rule for each pair of nodes that talk would end as *"una enredadera de reglas OPA imposible
    de debug"*.
  - Agents do not send at all: the AI node stays a pure responder, and routing belongs to WFs
    (agent-tools-v1 §6.2).

### A-112 ✅ VALIDATED ON 8.x (0.1.89, 2026-10-28) — The AI node answers router notices

- **What happens:**
  1. A router notice (`UNREACHABLE`, `TTL_EXCEEDED`) that no waiter takes reaches
     `GenericAiNode::on_message`.
  2. Its control plane answers any system message that is not a control verb, with a
     `CONFIG_RESPONSE` `unknown_system_msg` to the sender. Here the sender is the router's own
     UUID; a unit test answers a notice like that.
  3. The router cannot deliver to its own UUID, so it sends `UNREACHABLE` back, and the node
     answers it again.

  This was read in the code, not seen running. Today it needs a reply that cannot be delivered,
  because its sender is gone. With `send_message` and reminders it becomes common.
- **Fix (0.1.89):** the node never answers a router notice, or any infrastructure message (no
  sender's name, HELLO, ANNOUNCE, WITHDRAW, ECHO, any `*_RESPONSE`). It logs it. This is the rule
  SY.timer and the WF engine already follow.
  - A test confirmed the loop first, before the fix: on the unchanged node it answered the notice
    and the emulated router answered back, three times in a row, until the test stopped it.
  - It also closes a latent `CONFIG_RESPONSE` ping-pong between two AI nodes.
  - The review found the same control plane in SY.frontdesk.gov. The rule moved to the AI SDK, and
    both use it.
- **Validated on 8.x (0.1.89):**
  - a throwaway mesh client sent `AI.pruebas` a message and left at once, so the agent's reply
    could not be delivered;
  - the router's `UNREACHABLE` reached the agent, which logged it once as a router notice, "not
    answered";
  - no `unknown_system_msg`, and nothing more.

### A-111 ✅ VALIDATED ON 8.x (0.1.89, 2026-10-28) — A CSV or HTML attachment fails the whole message on Claude

- **What happens:**
  - ai.generic reads as text only the `text/plain`, `text/markdown` and `application/json`
    attachments (crates/fluxbee_ai_sdk/src/text_payload.rs).
  - Every other attachment reaches the model as a document part, CSV and HTML included.
  - The Anthropic provider accepts only PDF documents, so a message with a CSV or HTML attachment
    fails there. On OpenAI it goes as an `input_file`.
- **Fix (0.1.89, agent-tools-v1 §6.2.1):** CSV and HTML become text, and an attachment shows its
  blob name, for `read_blob`.
- **Validated on 8.x (0.1.89), on Claude:**
  - on 0.1.88 the gate's CSV had answered `provider_error`;
  - on 0.1.89, asked for the rows of a month and their sum, the agent listed all three and the
    right total, 305;
  - asked for "only the number", it first answered 245, leaving out the last row. That was the
    model's slip, with no room to work, not the input's: the preview and the input's length
    show the whole CSV.

### A-110 ✅ RESOLVED (0.1.89; by tests) — publish-ia-runtime.sh moves `current` before its last step

- **What happens:**
  - `scripts/publish-ia-runtime.sh` calls `publish-runtime.sh`, which writes the manifest, `current`
    included, and only then writes the version's `tools.json` from the repo's
    `docs/reference/agent-tools.json`.
  - Run outside a full checkout, the catalog step fails after `current` has already moved. The
    version is live without its catalog, so `get_runtime` answers `tool_catalog: null` to Archi.
  - Running nodes were safe: the reconcile checks the version's files.
- **Fix (0.1.89):**
  - publish-runtime.sh writes every file of the version first (`--start-script`, `--file`) and
    the manifest last;
  - publish-ia-runtime.sh takes the catalog as `--catalog`, and fails before touching the
    manifest when an input is missing;
  - the IO publish scripts use `--start-script`.
- **Status:**
  - the scripts' tests cover it; the failing case was not run by hand on 8.x;
  - there, the package published 0.1.89 with its 17-tool catalog, which `get_runtime` answers;
  - the hand publish now hits A-122 on the next upgrade.

### A-109 ✅ VALIDATED ON 8.x (0.1.88, 2026-10-27) — A release that does not change the orchestrator leaves running nodes on the old runtime

- **What happens:**
  - A node that follows `current` moves to the new runtime only when its hive's orchestrator
    starts: "running node follows 'current' and the pointer moved; relaunching it".
  - A core update restarts only the services whose binary changed. In 0.1.87 the orchestrator's
    did not change, so worker1's orchestrator never restarted.
  - `AI.pruebas` kept the 0.1.86 binary, whose directory dpkg had removed (`/proc/<pid>/exe` read
    "(deleted)"). It went on answering with the old tools until `restart_node`.
- **Fix:** when `current` moves, the orchestrator relaunches the running nodes that follow it after
  every core update, and on its periodic reconcile, not only at its own start. With it, save
  `_system.runtime_version` (A-101).
- **Fixed (0.1.88, `d01ba1d`):** see the ledger.
  - One reconcile pass, at boot and every 60 s, relaunches a running node that follows a moved
    `current`, or whose unit does not run what its runtime names (live ExecStart plus a version
    stamp). Package nodes move with their base.
  - Only once the target is ready; otherwise it waits, logged once.
- **Validated:**
  - a runtime published without a .deb moved `AI.pruebas` within 28 s;
  - a node pinned to 0.1.88 stayed;
  - a same-version republish relaunched nothing.

### A-108 ✅ RESOLVED (0.1.95, WF Case 5; by tests; found 2026-10-27) — io.slack fails a whole reply that carries an HTML file

- **What happens:** io.slack's MIME list has no `text/html`. A reply with a
  `generate_html_artifact` file is refused whole, the text and every other file, and Slack shows
  "Error (unsupported_attachment_mime)". Since 0.1.87 a reply may carry several files, so one
  unsupported file now takes the others down too.
- **Fix (the IO track):**
  - send what Slack accepts, and name what was left out in the text, rather than failing the
    reply;
  - or accept `text/html`.

  io.wapp sends the text only and drops every attachment, an older deferral.
- **Planned into WF Case 4 on 2026-10-31, and taken out on 2026-11-02:** to io.slack's revamp (A-129
  has the operator's definition).
- **The operator (2026-11-02):** io.slack's bugs go now, with WF Case 5's build
  ([wf-orchestration-v1](../../docs/wf-orchestration-v1.md) §5.5).

### A-107 [IO/io.blob] 🔵 PENDING by the operator's choice (2026-10-27) — A published page cannot be updated under the same link

- **What happens:** a link is content-addressed. IO.blob curates `public/<sha256>`, the edge binds
  the key to that sha256 and size, and it refuses to rebind a key to different facts. Updating a
  page means publishing again (a new link) and taking the old one down (`unpublish_html_page`,
  0.1.89).
- **A stable link would need:**
  - an `update_html_page` tool;
  - a SY.admin action that curates the new content for the same publication;
  - an explicit edge operation that replaces the row's facts, from SY.admin only, for the
    publisher's own publications.
- **The operator:** *"dejalo pendiente, al final creo que es lo correcto pero tampoco es demasiado
  importante ahora"*.

### A-106 [IO/io.blob] 🟡 OPEN, interim decision (found 2026-10-27) — The blob store has no tenant; agents read the whole hive

- **What happens:** a `BlobRef` carries no tenant, and the store has no per-tenant directory. Since
  0.1.89 (live 2026-10-28), `read_blob` lets an agent read any text blob of its hive by reference.
- **The interim decision:** *"si no es por tenant, hoy prefiero que lea todo lo del hive"*. A blob's
  name carries 16 hex of its content's hash, so a reference has to have reached the agent; it
  cannot be guessed.
- **Also open:**
  - a per-tenant directory, or a tenant on the reference;
  - reading a PDF from the blob (the model gets a PDF only when it arrives as an attachment);
  - finding a blob: search, in a future hive of its own (the operator: *"todo un tema… nada para
    ahora"*).

### A-105 ✅ VALIDATED ON 8.x (0.1.86, 2026-10-27) — What the 0.1.86 review found and the release fixed

Found by the code review round of [agent-tools-v1](../../docs/agent-tools-v1.md) (security,
operations); fixed in `89848d8`. Covered by tests; the ones marked *live* were seen on 8.x.

- **Any node could reconfigure an AI agent.**
  - **The problem:** ai.generic and the frontdesk accepted their control plane as `system` or
    `admin`, verbs in any case, from any sender. The router gates only `system`, in exact case.
  - **Fixed:** the control plane is accepted only from the real authorities: CONFIG_* from
    `SY.admin@motherbee`, and NODE_STATUS_GET and NODE_HELP from the node's hive orchestrator.
  - *Live:* before, an intruder got the whole config; after, `unauthorized`.
- **Tool descriptions over 1024 characters,** which OpenAI rejects. The cards are now bounded
  (the longest is 970), and so are Archi's and the executor's tools (`fluxbee_system_get` went from
  1277 to 790). *Live:* gpt-5.5 took the definitions.
- **A-95 had a second path:** the final-output log line.
- **`node_help` held the orchestrator's one-at-a-time workers** for up to 7 s. It now runs in its
  own task, and answers at once for a node that ai.generic does not run. *Live:* `IO.web` in 1.3 s.
- **`tool_catalog`'s read** followed symlinks and was unbounded: now a regular file, read bounded.
- **The frontdesk received the delete-state card,** which contradicts its flow. It now keeps get
  and put only.
- **CSV cells could run as formulas** in a spreadsheet: they are neutralized per OWASP.
- **Cost:**
  - the larger definitions are cached on Anthropic, with breakpoints on the tools and the system
    prompt, and the cache tokens are counted;
  - the registry is in name order, so OpenAI caches the prefix;
  - *Live:* Anthropic cache writes and reads of 6,376 tokens; OpenAI read 2,560 from its cache.

### A-104 ✅ VALIDATED ON 8.x (0.1.87, 2026-10-27) — An AI node's model must match a provider it may not know

- **What happens:** since 0.1.86, `behavior.provider` is optional and defaults to the hive's
  provider, but `behavior.model` is required and free text. SY.admin's examples say
  `"model": "gpt-5.5"` with no provider. Copied on an Anthropic hive (8.x), the result is an OpenAI
  model sent to Anthropic, and every message fails.
- **Fix:** without `behavior.model`, the node uses the hive's model for its provider
  (`hive.yaml ai.providers.<provider>.model`). The examples then name neither, unless the node
  needs another model.
- **Fixed (0.1.87, `6772df4`):**
  - the default model;
  - CONFIG_GET and CONFIG_SET show `ai_model`;
  - CONFIG_SET refuses a model the hive cannot supply;
  - the examples, Archi's prompts and its versioned cookbook seeds name neither provider nor model.
- **Validated:** a `config-set` with neither was accepted, with `claude-sonnet-5-5` from the hive.

### A-103 [General/agent tools] ✅ VALIDATED ON 8.x (0.1.106, 2026-11-07; found 2026-10-26, 0.1.86 review) — The tool cards restate constants by hand

- **What happens:** the cards repeat values that live elsewhere in the code:
  - the 128-byte filename limit;
  - the 24 h publication expiry (SY.admin's `PUBLICATION_DEFAULT_EXPIRES_SECS`);
  - the 120 s publish wait;
  - the 64–2400 image clamp.

  The catalog's golden test compares the cards with themselves, so it cannot catch a change to any
  of those values.
- **Fix:** build the card text from the constants, or test the text against them.
- **Fixed in 0.1.106:** both, by where a value lives:
  - built from ai.generic's constants where code writes the card: the filename's 128 bytes, the
    publish wait (the card now says "up to 120 s"), the image sizes, `read_blob`'s numbers;
  - held by a test where the text is static, or the value is another binary's: the 24 h is SY.admin's,
    checked by a test-only addition there; the XLSX sheet name's 31 characters (the review round).
- **Validated on 8.x:** AI.pruebas' help returns the catalog of its 17 tools, equal to
  `docs/reference/agent-tools.json`.
- **Noted, left:** SY.admin's `publish_artifact` help spells its own default and range out by hand
  (86400; 60 to 2592000), the comment on `PUBLISH_ARTIFACT_BUDGET` restates ai.generic's 120 s,
  and no test holds SY.admin's 110 s budget under that wait.

### A-102 ✅ RESOLVED (0.1.88; covered by tests) — A stopped node's first start can race the T9 conversion

- **What happens:** `start_node` saves the relaunch flag into config.json after launching the node,
  while the node converts the same file at boot. If the orchestrator's read-modify-write lands
  first, `behavior.capabilities` comes back, and the node boots FAILED_CONFIG. A restart fixes it.
- **Fix:** save the flag before launching, or convert in the orchestrator's launch step under the
  node's lock.
- **Fixed (0.1.88):** start and restart write the version and the relaunch flag before launching,
  in one write, under the node's lock.

### A-101 ✅ VALIDATED ON 8.x (0.1.88, 2026-10-27) — NODE_HELP's runtime_version goes stale after a restart

- **What happens:** `start_node` and `restart_node` launch the current version without saving
  `_system.runtime_version`; only the boot reconcile saves it. A node restarted onto a newer
  version reports the old one until the orchestrator's next boot.
- **Fix:** save the version in start and restart, before launching.
- **Seen on 8.x (0.1.87, 2026-10-27):** after `restart_node AI.pruebas`, the node ran 0.1.87 and
  `node_help` said 0.1.86. To fix with A-109.
- **Fixed (0.1.88):** every launch path records `_system.runtime_version` before launching, and the
  unit carries `FLUXBEE_NODE_RUNTIME_VERSION`.
- **Validated:** after the deploy, and after the A-109 relaunch, `node_help`, the binary, the stamp
  and `_system` all agreed.

### A-100 [General/router] ✅ VALIDATED ON 8.x (0.1.99, 2026-11-04; found 2026-10-26, 0.1.86 review) — The router gates protected verbs only for `msg_type` system, in exact case

- **What happens:** the router's delivery gate checks protected verbs only on `msg_type: system`,
  and only in exact case. A node that took `msg_type: admin`, or a verb in lower case, received a
  CONFIG_SET from any sender.
  - 0.1.86 closes it on the AI nodes: `ai.generic` and the frontdesk accept their control plane
    only as `system`, in exact case, and only from their real authorities.
  - The IO nodes already checked their callers.
- **Fix, as defense in depth:** the router compares `meta.msg` without case, and gates protected
  verbs on any `msg_type`.
- **Seen on 8.x (0.1.85, 2026-10-27):** an `AI.intruso` client on worker1 sent `config_get`, in
  lowercase, as `system`. The router let it through, and the agent answered its whole config.
  Since 0.1.86 the agent refuses it.
- **2026-11-04, the operator:** the 🔴 items close now, as simple and important topics: in this
  sprint.
- **Fixed (0.1.99):** the delivery gate takes a protected verb whatever the `msg_type`, in any case
  and with spaces around it, and asks the system policy by its canonical name
  (`protected_system_action`, src/router/system_policy.rs; `prepare_local_delivery`). The
  orchestrator's own guard (SO-05) reads it the same way. Test:
  `a_protected_verb_is_gated_on_any_msg_type_and_in_any_case`.
- **Checked on 8.x (0.1.99, 2026-11-04):** a throwaway node on worker1 sent `AI.pruebas@worker1`
  `CONFIG_GET` as `admin`, then `config_get` and ` Config_Get ` as `system`. None got an answer. The
  router logged the three drops under the canonical `CONFIG_GET`, each with the message's own name
  and `msg_type`. The four hives logged no other drop since the release, so no legitimate sender
  was cut.

### A-99 ✅ VALIDATED ON 8.x (0.1.87, 2026-10-27) — When a file generator runs, the model's own text is dropped

- **What happens:** the reply is the generator call's `text`, or a fixed Spanish line. Whatever the
  model writes after the call is lost.
- **Fix:** decide which text the user gets, and keep the model's own.
- **Fixed (0.1.87):** the reply is the model's text after its last tool call, else its text
  alongside a call. It carries every file of the turn, and a published link the text lacks is
  appended.
- **Validated:** a PDF and a CSV came in one reply, with the model's own sentence, and a page's
  link came with a CSV.

### A-98 ✅ VALIDATED ON 8.x (0.1.87, 2026-10-27) — A generated file's bytes flow back into the model's context

- **What happens:** each `generate_*_artifact` result carries the file's base64 back to the model.
  That costs tokens on every later turn, and fills the context with bytes the model cannot use.
- **Fix:** return a reference (the file name, its size, the blob reference), never the bytes.
- **Fixed (0.1.87):** a per-message collector keeps the file. The model gets `{status, filename,
  mime, size_bytes}`. Tested on 8.x through the two-file reply.

### A-97 ✅ VALIDATED ON 8.x (0.1.87, 2026-10-27) — The PDF generator garbles non-ASCII text

- **What happens:** `generate_pdf_artifact` prints only ASCII correctly, so accented Spanish comes
  out broken. The users of 8.x write in Spanish.
- **Fix:** embed a Unicode font, or use a PDF library that encodes text properly.
- **Fixed (0.1.87):**
  - WinAnsiEncoding (Windows-1252), with octal escapes;
  - lines wrapped by Helvetica's widths;
  - new pages.
- **Validated:** a 2-page Spanish PDF had every accent, ñ, ¿ and ¡ escaped, and no raw byte
  ≥ 0x80.

### A-96 [General/usage] 🔵 TO EVALUATE (noted 2026-10-26) — How the SY nodes' tokens are measured per message

- **What happens:** the SY nodes that call a model (cognition, Archi, the frontdesk) keep the
  hive's single provider (agent-tools-v1 T8). How their tokens are measured per message is still
  open.
- **The operator (2026-10-26):** *"después vemos cómo se miden los tokens por mensaje"*.
- **The operator (2026-11-10):** a large topic, like io.web, to talk over: a node that meters, or the datum in the messages, or system messages streamed from every node to a log node over its socket, with the stream code in the SDK so it is not in a thousand places; tokens come from the AI nodes and the SY nodes, so the SDK could record them wherever it calls an AI, transparent to its user (every node uses the same SDK). Not in the sweep.

### A-95 ✅ VALIDATED ON 8.x (0.1.86, 2026-10-27) — ai.generic logs the full public link of a page it publishes

- **Fixed (0.1.86, `89848d8`):** the publish tool logs the publication id. The final-output line
  logs only the file names and the text's length; the review found that this second path also
  carried the link (A-105).
- **Validated:** after two pages were published (on Claude and on OpenAI), no line with `/public/`
  was in the agent's journal.

- **What happens:** after `publish_html_page`, ai.generic writes an INFO line with the whole URL,
  capability key included (`published HTML page tool="publish_html_page" url=…/public/<key>`).
  Anyone who reads the worker's journal can open the page until it expires.
- **Fix:** log the publication id only, as the edge does since A-82.

### A-94 ✅ RESOLVED for the AI node (0.1.86; the refusal seen on 8.x) — "capabilities" means six different things

- **What happens:** the word names six unrelated things, none of them what an agent can do:
  1. identity's `capabilities` arrays: retired and never implemented (identity addendum §2.4);
     SY.identity rejects them;
  2. `behavior.capabilities.multimodal`: whether an AI node's model takes images;
  3. the "capability" URL and key of `/public/`: a link that is itself the permission;
  4. the "capability query" of `list_cloud_actions`: what Cloud may ask SY.admin;
  5. SY.timer's HELP catalog: its own capabilities;
  6. the router's WAN HELLO, `capabilities: ["lsa", "forwarding"]`: protocol negotiation.
- **Decided** (operator, 2026-10-26): what an agent can do is called **tools**
  ([agent-tools-v1](../../docs/agent-tools-v1.md) T7, T9).
  - **The AI node gets one meaning:** `behavior.capabilities.multimodal` becomes
    `behavior.multimodal`, and the default role stops saying "capabilities".
  - **Infra nodes keep their own uses (2–6):** *"cada nodo puede tener las suyas"*.
  - **Identity's dead arrays (1) stay refused.**
- **Fixed (0.1.86, `89848d8`):**
  - `behavior.multimodal` in ai.generic and the frontdesk;
  - the old key refused;
  - a saved config converted once at boot (0600, a `config.json.pre-0.1.86` copy, one INFO line).
- **Seen on 8.x:** a `config-set` with `behavior.capabilities` was refused with a clear message.
  The boot conversion is covered by tests, since no PROD config held the key.

### A-93 ✅ VALIDATED ON 8.x (0.1.86, 2026-10-27) — An AI node cannot choose its provider

- **What happens:** a node chooses its model (`behavior.model`) and its endpoint (`behavior.base_url`,
  `api.openai.com` or `api.anthropic.com` only). Its provider, and with it the API's format and the
  vault key it looks for, is always the hive's: `hive.yaml ai.default_provider`, anthropic on 8.x.
  `behavior.provider` is refused. So a tenant with only an OpenAI key cannot run an agent on 8.x.
  This follows the operator decision of 2026-10-02 (`ai.generic` the only runtime, the key the
  hive provider's).
- **The operator (2026-10-26):** "no veo qué problema hay al instanciar el código usar un proveedor
  u otro". The model client already speaks both protocols. A node would need to choose the provider
  in `behavior`, and the vault lookup would follow that provider.
- **Decided** (operator, 2026-10-26): *"el proveedor de AI queda restringido al tenant del nodo, pero
  no al proveedor"*. The provider is per node, and the key comes from the node's tenant.
- **Checked:** nothing in the code stops two nodes from running different providers. The change is
  in agent-tools-v1 (T8), for the proposed 0.1.86.
- **Fixed (0.1.86, `89848d8`):**
  - `behavior.provider` is optional and defaults to the hive's provider;
  - the key comes from the node's own tenant;
  - `base_url` is accepted only with the provider, and only that provider's host.
- **Validated on 8.x:** `AI.pruebas` published a page on Claude, the hive's provider, then on
  gpt-5.5 after one `config-set` with `provider: openai`, using its tenant's OpenAI key.
  `llm_usage` charged both to the tenant, by provider.

### A-92 ✅ VALIDATED ON 8.x (0.1.85, 2026-10-24) — What the 0.1.85 review found and the release fixed

Found by the code review round of [io-web-v2](../../docs/io-web-v2.md) §11.2 (security,
operations); fixed in `95bacd3`. Covered by tests; the ones marked *live* were seen on 8.x.

- **TLS resumption skipped io.web's role check:**
  - **The problem:** on a resumed TLS 1.3 session, rustls restores the client chain without
    calling the verifier. A hive removed from the registry, or demoted, stayed admitted through
    the edge's tickets.
  - **Fixed:** the SDK's role-gated server config keeps no sessions and issues no tickets; the
    named client config never resumes; io.web checks the role again right after the handshake.
  - **Tested:** the test failed on the old code. *Live:* every probe got a full handshake, and
    worker1 and egress1 were refused.
- **A tenant's io.web could take the web door:**
  - **The problem:** the orchestrator admitted a core IO runtime in any tenant and under any
    `IO.*` name, so a tenant (through Cloud's `provision_node`) could bind 9200 first.
  - **Fixed:** core IO runtimes run only in the root tenant (`TENANT_ROOT_REQUIRED`) and only as
    `IO.<x>@motherbee` (`CORE_IO_NAME_MISMATCH`); io.web refuses to run under any other name or
    tenant.
  - *Live:* three attempts got `CORE_IO_NAME_MISMATCH`.
- **`web_status` could not see the edge's upstream:** a missing upstream only reached the edge's
  log. The `EDGE_WEB_ROUTES` answer now carries it, shown as `routes.upstream` and as `upstream=`
  in the ops line (*live*: `upstream=ready`).
- **Port 9200 and the edge's direct path to the motherbee were undocumented:** added to the port
  tables, the exposure section and the firewall alert.
- **One WARN line per failed proxied request:** now aggregated per interval. *Live:* one line,
  `failed=1`, with the request id of the 502 before io.web started.

### A-91 [IO] ✅ VALIDATED ON 8.x (0.1.131, 2026-11-11; IO.blob CONFIGURED and HEALTHY on its defaults; IO.cloud CONFIGURED with its edge, `missing_edge_node` without one) (found 2026-10-24) — io.blob and io.cloud boot UNCONFIGURED while they work; io.web boots CONFIGURED

- **What happens:**
  - The SDK's boilerplate rule reads a seed-only `config.json` as UNCONFIGURED. So io.blob and
    io.cloud answer CONFIG_GET with `node_not_configured`, while they curate and relay with their
    defaults, and their status reads HEALTHY.
  - io.web (0.1.85) applies its contract to `{}` at boot and runs CONFIGURED.
- **To decide** with the IO family's review: whether the siblings should materialize their
  defaults the same way, so that a node's state says whether it works.
- **The operator's call (2026-11-10, the sweep):** *"si funcionan 100% sin configurar si, sino algo tenemos que decir que falta"*: boot CONFIGURED when the defaults cover everything they do; otherwise say what is missing.
- **Validated (0.1.131, 2026-11-11):** IO.blob's config answer went from `node_not_configured` to
  `CONFIGURED`, HEALTHY, on 0.1.131; IO.cloud, whose config names `SY.edge@ingress1`, stays
  CONFIGURED and HEALTHY. Without an edge IO.cloud says `missing_edge_node` (its tests); its review
  found A-255, which the texts of this release describe until SY.admin's fix.

### A-90 [io.web] ✅ VALIDATED ON 8.x (0.1.99, 2026-11-04; the share per client and the stalled body by tests; found 2026-10-24, 0.1.85 review) — One client can exhaust the edge's way to io.web

- **What happens:** the proxy opens a TCP and TLS connection to io.web for each request and closes
  it first. Each one leaves a TIME_WAIT on the ingress.
  - One internet client pipelining GETs to the web host faster than about 470 per second runs out
    of the ingress's ephemeral ports toward the motherbee. Every web-host request then gets a 502
    until about 60 s after the flood ends.
  - The proxy's 128 slots are global, so one client can also hold them all.
  - `/e/` and `/public/` are not affected.
- **Impact today:** none that matters; the web host serves a placeholder page. It matters once
  Archi is behind the proxy (stage 2).
- **Fix, before stage 2:**
  - a per-client share of the proxy's slots (`PeerSlots`, as `/public/`);
  - a small keep-alive pool to io.web, so connections and their handshakes are not per request,
    or at least io.web closing first (`Connection: close` upstream);
  - a deadline for a stalled io.web body (docs/nodes/sy-edge.md, "Not there yet").
- **2026-11-04, the operator:** the 🔴 items close now, as simple and important topics: in this
  sprint.
- **Fixed (0.1.99):** one client address holds at most 16 of the proxy's 128 slots
  (`web_max_inflight_per_ip`, `PeerSlots`, moved to the SDK for the edge and io.web), and past them
  gets a quick `429 WEB_PEER_BUSY`. Every request to io.web carries `Connection: close`, so io.web
  closes first (what each side keeps after it: the check below). io.web sending
  nothing of a body for 30 s, while the edge waits on it, ends the response (`IoWebBody`). No pool:
  each request is still its own TCP and TLS connection. Tests:
  `one_client_holds_at_most_its_share_of_the_proxy`,
  `a_body_io_web_stalls_ends_and_gives_its_slots_back`.
- **Checked on 8.x (0.1.99, 2026-11-04):**
  - The web host answered through the proxy, the outside probe's cases as on 0.1.85.
  - What each request's connection ends with (tcpdump on ingress1, port 9200):
    - io.web sends its FIN first, with its TLS close_notify;
    - the edge answers with its FIN, then a reset, since it drops the connection with that
      close_notify unread;
    - io.web answers with a reset.
  - So neither side keeps a TIME_WAIT. `ss`, sampled every 50 ms on both sides during ten
    requests, saw only ESTAB.
  - No port of the ingress waits after a request, which is what the fix is for. The TIME_WAIT is
    nobody's, not the motherbee's, as this entry said before the check.

### A-89 [io.web] ✅ VALIDATED ON 8.x (0.1.99, 2026-11-04; found 2026-10-24, 0.1.85 review) — io.web's connection slots fill before any authentication

- **What happens:** io.web takes one of its 256 slots at accept, before the TLS handshake, which
  may take 10 s.
  - Any host that reaches the motherbee's 9200 can keep the slots full with idle connections,
    about 26 new ones per second. That means any spoke on the internal network, or the
    motherbee's LAN address.
  - The edge's connections are then closed at accept, and the web host answers 502.
  - No certificate is needed. It is not reachable from the internet.
- **Fix, before stage 2:**
  - count against the 256 only connections past their handshake;
  - give connections still in their handshake a small budget per source address and a shorter
    timeout (2–3 s on the internal network).
- **2026-11-04, the operator:** the 🔴 items close now, as simple and important topics: in this
  sprint.
- **Fixed (0.1.99):** a connection takes one of the 256 only once its handshake ends, and is
  closed then if none is free. Handshakes have their own slots: 128 at once, 32 from one source
  address (its IPv6 /64 as one, the SDK's `PeerSlots`), each for 3 s at most, down from 10 s
  (nodes/io/io-web/src/web.rs). Tests: `past_its_handshake_cap_a_new_connection_is_closed_at_once`,
  `silent_connections_hold_no_served_slot`.
- **Checked on 8.x (0.1.99, 2026-11-04):** ingress1 opened 34 silent connections to io.web. Two
  were closed at once, the other 32 at 3.0 to 3.1 s. io.web logged `IO.web at its handshake cap;
  closing new connections` with `max_handshakes_per_source=32`.

### A-88 ✅ RESOLVED (0.1.84; covered by tests, the fixed bodies seen on 8.x) — An `/e/` error hands internal detail to the internet

- **Fixed (0.1.84, `5be9e7b`):** one fixed body per error code; the detail goes only to the edge's
  log. Seen on 8.x: an unknown `/e/` endpoint answers `NOT_FOUND` "no such endpoint". A failed
  reply is covered by tests.
- **What happens:** when the handler's reply fails, `/e/` answers with `err.to_string()`
  (src/bin/sy_edge.rs ~3717), which can carry a timeout's `trace_id` and the request's message
  family.
- **Fix (io-web-v2 §11.1):** a fixed error body per code, the detail only in the edge's log.

### A-87 ✅ VALIDATED ON 8.x (0.1.84, 2026-10-23) — One byte holds an edge connection forever

- **Fixed (0.1.84, `5be9e7b`):** hyper's HTTP/1 builder with a header-read timeout that starts
  before the first byte; no protocol sniffing, no HTTP/2. Validated from outside: a connection
  that sent one byte was closed after 30.2 s, and an h2 preface in 0.3 s.
- **What happens:** the edge serves every connection with hyper-util's `auto::Builder` and
  `serve_connection_with_upgrades` (src/bin/sy_edge.rs ~2734-2744), which sniffs for the HTTP/2
  preface. The sniff has no timeout after the first byte (hyper-util ignores `http1_only()`
  there), and hyper's h2 server has no idle timeout. The 4,096 connection permits are taken at
  accept, so 4,096 connections that each send one byte refuse every later client: Cloud's `/e/`,
  the webhooks, `/public`, and Let's Encrypt's validation. No credential is needed.
- **Fix (io-web-v2 §11.1):** hyper's HTTP/1 builder with its head-read timeout, which starts before
  the first byte; no protocol sniffing and no HTTP/2 (reachable today only by prior knowledge);
  tests for one byte and for an h2 preface.

### A-86 ✅ RESOLVED (0.1.84; covered by tests, the idle cut seen on 8.x) — Stalled readers can block the edge's downloads

- **Fixed (0.1.84, `5be9e7b`):**
  - every connection's stream is paced (30 s of idle credit, then 1 KiB/s), with a 30 s body
    deadline and `TCP_NOTSENT_LOWAT` 16 KiB;
  - per-IP caps: 256 connections (`edge.max_connections_per_ip`) and 8 downloads
    (`edge.public_max_inflight_per_ip`);
  - downloads keep their own 128 slots, apart from `/e/`'s.
- **Validated:** the idle cut on 8.x (the A-87 test); the caps and the pacing by tests only.
- **What happens:** `/public` streams with no write timeout, so 128 readers that stop reading hold
  every download slot (`PUBLIC_BUSY` for everyone), and every route shares one 4,096-connection cap.
- **Fix:** a minimum rate, an idle deadline, a per-IP cap, and download slots kept apart from
  `/e/` (with the edge's work in [io-web-v2](../../docs/io-web-v2.md)).

### A-85 [IO/io.slack] ✅ RESOLVED (0.1.99, deployed 2026-11-04; by tests, no check on 8.x; found 2026-10-22) — io.slack's file fetch has no timeout and buffers the whole body

- **What happens:** `download_private_file` (nodes/io/io-slack/src/main.rs) uses a client with no
  timeout, reads the whole body into memory and checks the size only afterwards.
- **Fix:** stream to staging under a byte cap, with a connect timeout, a total deadline and a
  minimum rate; HTTPS only, no redirects, no non-global addresses.
- **2026-11-04, the operator:** the 🔴 items close now, as simple and important topics: in this
  sprint.
- **Fixed (0.1.99):** the download has its own client (`file_download_client`): a 10 s connect
  timeout, a 120 s deadline, a redirect followed only on the file's own origin, three at most. It
  reads at most the file's share (`max_attachment_bytes`, or what `max_total_attachment_bytes`
  leaves): a file announced bigger is not read, and one that grows past it is dropped as it comes.
  HTTPS and the host hold through the origin check of 0.1.95 (`files.slack.com`). Left out, by
  cost: a minimum rate (the 120 s deadline bounds a slow one) and a check of the address the name
  resolves to (the origin is Slack's). Test: `a_shared_file_is_downloaded_within_its_share`.

### A-84 [General/SY.admin] ✅ RESOLVED (0.1.99, deployed 2026-11-04; by tests, no check on 8.x; found 2026-10-22) — `bundle_upload` follows symlinks and extracts without caps

- **What happens:** `runtime_package_safe_blob_rel_path` (src/bin/sy_admin.rs) refuses only
  absolute paths and `..`, and the file is opened following symlinks; Syncthing replicates symlinks
  from the workers into `active/`. `materialize_bundle_runtime_package` has no size or count caps
  and keeps the zip's mode bits.
- **Fix:** `O_NOFOLLOW` at each step, regular files only, a path under a root-only directory, and
  an extraction with caps that sets its own modes.
- **2026-11-04, the operator:** the 🔴 items close now, as simple and important topics: in this
  sprint.
- **Fixed (0.1.99):** `BundleBlob` (src/bin/sy_admin.rs) opens the zip under the blob root without
  following a symlink at any step (the SDK's `safe_fs`, as the blob tree's writers do), takes only a
  regular file of at most 512 MiB, and removes it through the directory it opened it in. The
  extraction, under SY.admin's own staging directory, takes at most 10 000 entries and 1 GiB
  (counted as they stream), only files and directories, each file once, with its own modes: 0755
  when the zip marks it executable, 0644 otherwise. Tests:
  `publish_runtime_package_bundle_refuses_a_symlinked_blob`, `..._refuses_a_symlink_entry`,
  `..._sets_its_own_modes`, `copy_capped_refuses_more_than_its_cap`.

### A-83 ✅ RESOLVED (0.1.84; covered by tests) — The edge answers an empty 200 when it cannot build a response

- **Fixed (0.1.84, `5be9e7b`):** a response that cannot be built is a 500, and a layer puts the
  security headers on every response. 8.x never had to build one.
- **What happens:** three places in src/bin/sy_edge.rs fall back to
  `Response::new(Body::empty())` when a response cannot be built (for example an invalid header
  value): a 200 with no body and no security headers.
- **Fix:** answer 500 with the security headers.

### A-82 ✅ VALIDATED ON 8.x (0.1.84, 2026-10-23) — The edge logs capability keys

- **Fixed (0.1.84, `5be9e7b`):** the edge stores and logs only `SHA-256(key)`; its
  `publications.json` is converted once, and the conversion is reported. Validated: a publication
  made in the gate was logged with `key_sha256`, its link was served, and no raw key was left in
  the edge's state directory.
- **What happens:** src/bin/sy_edge.rs logs `key = %row.key` when it publishes a public artifact
  and when it drops an expired one; those records leave the DMZ through the router stream, and the
  keys sit in plaintext on the edge's disk.
- **Fix:** the edge stores and logs only `SHA-256(key)`.

### A-81 ✅ VALIDATED ON 8.x (0.1.84, 2026-10-23) — An agent on a worker cannot publish a page

- **Fixed (0.1.84, `5be9e7b`):** as below, with a 110 s budget for the whole call.
- **Validated (the agent's path without the model):**
  - a mesh client carried a test agent's identity on worker1;
  - it wrote a page into worker1's `active/` and sent `publish_artifact` 1 s later;
  - the answer was `ok` after 12.8 s and 13.3 s (two runs);
  - the edge served the page from outside, and answered 404 after the unpublish.
  - The model's own tool call was not run: the test tenant had no AI key, and a tenant's agents
    read only their own.
- **What happens:** ai.generic's `publish_html_page` promotes the blob on its worker and calls
  `publish_artifact` at once; IO.blob opens the motherbee's `active/` with no wait, before
  Syncthing has brought the file (~10 s), and answers `BLOB_NOT_FOUND`, which the model receives
  as final. PROD's agents run on worker1.
- **Fix:** before taking its publication lock, SY.admin sends the producer's hive a `sync_hint`
  on `fluxbee-blob` (`wait_for_idle: false`) and waits up to 30 s for the file in the motherbee's
  `active/`; IO.blob never waits in its message loop; any curate error releases what was curated.

### A-80 [General/identity] ✅ VALIDATED BY TESTS (0.1.106, 2026-11-07; found 2026-10-22) — A restore is refused when another tenant holds the same email

- **What happens:** `check_ilk_restorable` (src/bin/sy_identity.rs) looks the person's vouched
  email up across every tenant, while emails are unique per tenant. Restoring a marked person is
  refused with `ILK_KEY_CONFLICT` when someone in another tenant holds the same email.
- **Fix:** look the email up in the person's own tenant, as registration does.
- **Fixed in 0.1.106** (`check_ilk_restorable` with `ilk_holding_email`): validated by its test on
  GitHub's CI, `restoring_minds_the_email_only_in_the_persons_own_tenant`. Not tried live: that
  takes people with emails in two tenants.

### A-79 ✅ VALIDATED ON 8.x (0.1.83, 2026-10-22) — What the hardening reviews found besides A-62, A-20, A-22 and A-42

Found by the design review and the code review round of
[control-path-hardening-v1](../../docs/control-path-hardening-v1.md); fixed in `5fb8dab`.
Covered by tests; the ones marked *live* were seen on 8.x.

- **The router:**
  - a WAN frame's sender was looked up in every hive's bucket, not its peer's, so root on a spoke
    could send as another spoke's node (it now resolves only in the peer's bucket, or among the
    hub's vouched nodes when the peer is that hub; *live*: the few frames of a node not yet or no
    longer listed are counted as `foreign_source`, which 0.1.82 dropped without a log line);
  - a hive's LSA could carry any node name, routes matching other hives' names and taps on other
    hives' traffic (it now carries only `*@P`, routes for P's destinations and taps P is party to
    and that target P's nodes), and a UUID it advertised could shadow another hive's node (a UUID
    two hives list now resolves to neither);
  - a tap copy carried SYSTEM authority (it no longer does), and a sibling router's shared-memory
    region was read whoever owned it (it must be the router's user's, with no group or world
    write).
- **SY.admin:** wf-rules could publish `wf.engine` itself, which every workflow runs; the teardown
  purged the ILK before deleting its secrets, so they could not be tied to a hive (now one
  `teardown_node`; *live*); `test_hold_ms` and an unbounded `sync_hint` wait reached the mesh; the
  WF ownership check waited on the orchestrator's queue (now read from shared memory).
- **SY.orchestrator:** its admin inbox answered reads to any caller and forwarded them under its
  own name (*live*: FORBIDDEN); non-canonical ILK ids were stored as given.
- **SY.identity:** an orchestrator could delete, purge or rewrite any person; a non-canonical
  `ilk_id` slipped past the scope check and overwrote another ILK at the database's upsert; a
  channel another ILK held could be taken over; a node could register an email.
- **The frontdesk:** a handoff was recognized by its shape alone and from any sender; a handoff
  without an email inherited the typed one; a vouched email was lost when the handoff had no
  name; the registration tool acted on an ILK the model named.

### A-78 ✅ VALIDATED ON 8.x (0.1.82, 2026-10-21) — The vault's change notice never reaches the edge

- **Validated (0.1.82, `d2980cf`):**
  - A throwaway secret owned by the edge was put and deleted, and both notices reached the edge,
    the first ever seen there.
  - A vault rollback of the edge's certificate made the edge swap to the previous certificate on
    the notice alone, without a restart and without `/acme/reload`. A second rollback brought the
    first one back the same way.

- **What happens:**
  - SY.edge reloads its TLS certificate when SY.vault announces a change (`VAULT_SECRET_CHANGED`,
    `src/bin/sy_edge.rs` ~465-657).
  - The router's edge gate (`vpn_allows_between`, `src/router/mod.rs` ~5976) comes before the global
    SYSTEM bypass. It admits only the `EDGE_*` commands and their answers, the IO data path and
    `VAULT_GET`, and every broadcast fan-out applies it. The notice is dropped.
- **Evidence in PROD (2026-10-21):** SY.vault has sent 146 notices. Neither sy-edge nor rt-gateway
  on ingress1 logged one since 2026-08-05, the start of their journals.
- **Impact:**
  - PB-1's mitigation ("a key change in the vault restarts the edge") never ran: the 2026-07-30
    logbook saw the old certificate served after a put until a manual restart.
  - Channel secrets are not affected: a rotation reaches the edge through a new `EDGE_OPEN_URL`.
- **How it showed up:** verifying the code for the ACME design (docs/acme-edge-certificates-v1.md §2).
- **Fix (0.1.82, design §5):**
  - SY.vault also sends the notice as a unicast to the owner when the owner is an `SY.edge`.
  - The edge gate admits only that unicast from SY.vault on the primary hive.
  - The broadcast, which carries every stored key's name and owners, stays out of the DMZ.
  - The edge reloads without restarting (PB-1).

### A-77 ✅ RESOLVED (0.1.81, 2026-10-20) — What the closing pass and its review found besides A-72..A-76

Found on 2026-10-19 while verifying the open findings and in the review round of 0.1.81; fixed in
`4e9f91e`. Covered by tests; the ones marked *live* were seen on 8.x.

- **Archi (SY.architect):**
  - the reconcile never saw existing runtimes, nodes, routes or VPN rules (it read only a top-level
    array, `items` or `responses`), so every declared node or route was a Create on every run;
  - an AI node's config never converged (whole-value compare against the filled-in config);
  - a solution could own, and then delete, a workflow it never applied (ownership counted unexecuted
    manifests; the default was "solution", the docs said "external");
  - a lowercase or unknown route action turned a working route into a failed replace;
  - the VPN model (`{vpn_id: text, hive}`) could never match SY.config.routes' pattern → numeric id;
  - its artifact auditor refused valid workflow definitions (it wanted a `steps` field);
  - a hive the manifest adds blocked the snapshot; a WF on a DMZ hive or a declared WF./SY./RT. node
    failed only after the operator confirmed;
  - plans with `add_hive` timed out in Archi while the executor still ran them;
  - its cookbook seeds were never shipped by the `.deb` (*live:* seeded now) and taught old shapes;
  - the Messages view would fail on rows whose message has no sender name.
- **SY.admin:** its plan executor counted an accepted `add_hive` as done; it now waits for the join.
- **SY.orchestrator:**
  - `remove_hive`'s cleanup ran inside sy-orchestrator's own unit, so everything after it stopped the
    orchestrator never ran; it now runs in a transient unit;
  - a join's record was lost on its own rewrites, so a restart mid-join never ended it.
- **SY.timer:** answered every system message it did not handle, router notices included, so an
  UNREACHABLE could bounce between it and the router forever (no storm was seen in PROD's logs).
- **AI runtime:** a message over 60 s was retried up to 3 times, billing again and re-running tools,
  then dropped without an answer; provider calls had no timeout at all.
- **Documented, not built (operator's decision, 2026-10-19):** Archi never removes nodes, routes or
  VPN rules a solution stops declaring (workflows and the OPA policy are removed); an extra one goes
  through SY.admin's API.

### A-76 ✅ CLOSED (0.1.81, 2026-10-20) · the several-routers items DOCUMENTED (unsupported topology) — Smaller defects found while rewriting the docs from the code

- **Outcome (0.1.81, `4e9f91e`), item by item:**
  - routers: a restarted router takes its region over (no stale nodes); the identity region is
    recreated when its limits change, and SY.identity refuses to start if its store does not fit;
    the two several-routers defects (broadcast copies, WAN UNREACHABLE) are documented as limits
    of an unsupported topology (operator's decision, 2026-10-19);
  - SY.admin: `/hives/{hive}/status` and the per-hive inventory summary answer, `/inventory/hive`
    is gone, a vault call for a hive without a vault answers 400, vault metadata follows the list
    rule, `get_node_status` reads SY.* and RT.gateway from their units, the CONFIG_* allowlists
    follow the system policy;
  - OPA: a check is a read again (its own `check_id`, a scratch compile, no version, nothing
    staged), and back in Archi's read tool;
  - SY.orchestrator: `remove_hive` (A-48); Syncthing's firewall rule is 22000/tcp on the
    motherbee only;
  - AI nodes: `behavior.instructions`, `instructions.source` and the unread `runtime.*` keys are
    gone and refused; an OpenAI tool turn checks for a cut answer; ai.generic and the frontdesk
    report their real state; `deploy-ia-node.sh` is deleted;
  - comments and leftovers: corrected or removed.
- **Left to the IO-family track:** the managed IO nodes still answer NODE_STATUS_GET with a fixed
  HEALTHY (io.slack does not answer it); the SDK helper they can use exists.

Found by the docs rewrite of 2026-10-17 (the reference set and the node pages read every path they
describe). None was tried live. To be sorted with the operator; most belong to the IO-family and
router reviews.

- **Routers:**
  - in a hive with several routers, the gateway's nodes receive a broadcast twice;
  - a restarted router keeps entries for nodes that never reconnect;
  - an UNREACHABLE coming back over the WAN reaches only nodes on the gateway router;
  - lowering the identity limits leaves readers on the old SHM offsets.

  PROD runs one router per hive, so only the last one can show there.
- **SY.admin:**
  - the catalog lists `GET /hives/{hive}/inventory/summary` and `/inventory/hive`, but neither has a
    handler (405);
  - `GET /hives/{hive}/status` looks broken;
  - `get_node_status` looks for `fluxbee-node-*` units, so it misreports `SY.*` nodes;
  - the HTTP vault metadata route passes no `tenant_id`, so a non-root secret answers
    `UNAUTHORIZED`;
  - `/hives/worker1/vault/...` goes nowhere: SY.vault runs only on the motherbee;
  - the allowlists of SY.admin and Archi name `SY.config-routes`; the node is `SY.config.routes`,
    so that entry never matches.
- **OPA:** a policy check is not a read. SY.admin bumps the OPA version for it
  (`next_opa_version`) and SY.opa.rules compiles it into `staged/`, so a check run between the
  operator's `opa_compile` and `opa_apply` replaces what was staged (found by the 0.1.80 review;
  Archi's read tool no longer runs checks).
- **SY.orchestrator:**
  - `remove_hive` revokes neither the spoke's identity key nor its TLS certificate;
  - `syncthing_firewall_rules` still opens 22000/udp and 21027/udp, though Syncthing uses TCP only
    since host-posture stage 3. (0.1.81 kept 22000/tcp alone, on the motherbee; since 0.1.83 no
    `ufw allow` remains in the orchestrator, only a read of `ufw status`.)
- **AI nodes:**
  - `behavior.instructions` does nothing for a managed agent: its ILK definition, or the
    "unconfigured" prompt, replaces it (the scaffold no longer writes it);
  - an OpenAI tool turn does not check for a truncated answer (its usage record says `truncated`);
  - ai.generic launched by the orchestrator ignores the `runtime.*` timeout, pool and queue
    settings, and reads `instructions.source` file/env as literal text;
  - `scripts/deploy-ia-node.sh --update-existing` restarts with a local `systemctl` (old binary,
    same host only), and `--kill-first` does not purge (the re-spawn hits
    `NODE_ALREADY_EXISTS`);
  - SY.frontdesk.gov's `NODE_STATUS_GET` always answers HEALTHY.
- **Comments and leftovers:** SY.identity's comment on sync auth says the default is "disabled"
  (the code defaults to required); the `IlkGetRequest` comment says Cloud reads are tenant-scoped
  (they are not); SY.storage still creates `storage_migrations`, which nothing writes since the
  cleanup.

### A-75 ✅ RESOLVED (0.1.81, 2026-10-20) — The Go SDK still has the reply-matcher bug Rust fixed after the 2026-08-26 outage

- **Fixed (0.1.81, `4e9f91e`):** family-wide success matchers are never reply shapes;
  `SendSystemRPC` defaults to TTL 16; a frame over 128 KiB is refused with `MessageTooLargeError`
  and the link stays up. SY.timer, SY.wf-rules and the WF engine answer a request whose reply is
  too large with an error. Covered by tests (each failed on the old code); the Go nodes run 0.1.81
  in PROD and the OPA check went through SY.opa.rules.

- **What happens (go/fluxbee-go-sdk, found while writing docs/reference/protocol.md):**
  - Go registers any-message-of-a-kind matchers as reply shapes, the family-wide drop the Rust SDK
    fixed in 0.1.31 (the `any_msg_type` matcher).
  - `SendSystemRPC` defaults to TTL 1, so a system request cannot leave its router.
  - A send over the frame limit drops the whole connection.
- **Impact:** every Go node (SY.timer, SY.wf-rules, SY.opa.rules, the WF engine) carries them. No
  incident is known.
- **Proposal:** align the Go SDK with Rust (matchers, TTL, an oversize send refused without dropping
  the link). To decide with the operator.

### A-74 ✅ VALIDATED ON 8.x (0.1.80, 2026-10-17) — One invalid route, VPN or tap blocked every later change in SY.config.routes

- **What happens:** SY.config.routes gets no reply for an invalid route, VPN or tap, and later
  changes stay blocked until that entry is deleted or the node restarts
  (src/bin/sy_config_routes.rs; found while writing docs/reference/routing-and-policy.md).
- **Impact:** one bad rule from the operator, Archi or a plan stops all routing changes on that
  hive, silently.
- **Proposal:** answer the invalid entry with an error and keep applying the rest. To decide with
  the operator; not tried live.
- **Fixed (0.1.80, `58db83a`):** a change is built on a copy and committed only when the config
  region accepts it (`commit_config`); the region is put back after a failed write or save; the
  file is saved through a rename; every refusal is answered (`INVALID_CONFIG`, `INVALID_REQUEST`,
  `INVALID_CONFIG_SET`). Validated on 8.x: an invalid route answered `422 INVALID_CONFIG` in 0.01 s
  and changed nothing; the next valid one committed.

### A-73 ✅ VALIDATED ON 8.x (0.1.80, 2026-10-17) — Archi's read tool could change the system without the operator's confirmation

- **What happens:**
  - `fluxbee_system_get` is Archi's read tool. It accepts GET and POST, translates the path with
    `translate_scmd` and runs the action (`ArchitectSystemGetTool::call` in
    src/bin/sy_architect.rs).
  - It never checks whether the action changes something. `POST /hives` is `add_hive`, and
    `POST /hives/{hive}/nodes` is `run_node`. Both run at once, without the plan compiler and
    without the operator's CONFIRM.
  - The only guard is the prompt ("never call admin mutation endpoints directly").
  - The operator's own SCMD, a pasted plan and `POST /api/executor/plan` also run without CONFIRM,
    but those come from the operator.
- **Impact:** a model mistake, or text Archi reads (a message in `turns`, a node's output), can make
  it change the installation. Found while writing docs/nodes/sy-architect.md; not tried live.
- **Proposal:** make the read tool refuse every action that is not a read (an allowlist of read
  actions, plus the safe POST checks: the OPA policy check and a node's live CONFIG_GET). Small.
  To decide with the operator.
- **Fixed (0.1.80, `58db83a`):** the read tool runs a GET that names no change and the POSTs that
  change nothing (a node's live CONFIG_GET, the timer's conversions); an OPA check is not one (it
  stages the policy it checks, see A-76). Validated on 8.x: asked to start a node through the read
  tool, Archi got the refusal and no node was created.

### A-72 ✅ VALIDATED ON 8.x (0.1.81, 2026-10-20) — An agent's role does not reach the workers: `blob/agent-assets` is not replicated

- **Fixed (0.1.81, `4e9f91e`):** a worker-only Syncthing folder, `fluxbee-dist-agent-assets`
  (motherbee send-only, workers receive-only), backfilled for hives that joined before. Archi
  writes an asset outside the folder and renames it in (mode 0644); ai.generic reads only a
  regular file, bounded, and keeps its definition until a complete new one is readable.
  Validated: worker1 got the folder with its core update; a new role reached it in ~14 s; an
  agent on worker1 composed its definition from it.

- **What happens:**
  - An agent's role, skills, handbooks and personality are JSON assets in the motherbee's
    `blob/agent-assets/`; Archi writes them. The agent's ILK stores their hashes.
  - ai.generic reads them from the local disk, `<blob_root>/agent-assets/<hash>.json`
    (`load_cognitive_asset`, `nodes/ai/ai-generic/src/bin/ai_node_runner.rs`).
  - Syncthing replicates only `blob/active` to the workers. `agent-assets` is in no folder
    (`src/bin/sy_orchestrator.rs`, `peer_shares_active_blob` and the `agent-assets` permissions).
- **Impact:**
  - An agent on a worker does not find its assets: it is left without a definition and answers the
    fixed "unconfigured" JSON.
  - Today an agent with a role only works on the motherbee. However, per-interlocutor cognition
    (A-70) stayed on the workers so that the agents would run there.
- **How it showed up:** in the review of the developer docs (2026-10-17). It was not tested live:
  the gates' agents ran on the motherbee.
- **Proposal:** share `agent-assets` with the workers, read-only like `blob/active`, or serve them
  over the mesh. To be decided with the operator.

### A-71 ✅ VALIDATED ON 8.x (0.1.80, 2026-10-17) — ai.generic truncated the user's message, together with its memory, to 1,200 characters

- **What happens:**
  - With `capabilities.multimodal: false`, ai.generic prepends the communication's memory block to
    the user's text: `Conversation memory: … Current user message: …`
    (`inject_memory_package_into_text_input`, `ai_node_runner.rs`).
  - The AI SDK runner trims that whole text to 1,200 characters
    (`DEFAULT_INTERACTION_MAX_CHARS`, `crates/fluxbee_ai_sdk/src/function_calling.rs`).
- **Impact:**
  - With a memory of more than about 1,100 characters, the model does not see the current message:
    it answers without knowing what it was told.
  - A long message, for example an email, also arrives cut off.
  - The gates did not show it: their conversations were short.
- **How it showed up:** in the review of the developer docs (2026-10-17).
- **Proposal:** do not trim the current message (the limit stays for the immediate history), or
  bound the memory on its own. To be decided with the operator.
- **Fixed (0.1.80, `58db83a`):** the AI SDK no longer cuts the message being answered (the
  1,200-character limit stays for the history around it); ai.generic keeps the person's own words,
  without the memory block, in its immediate history. The cut also gave Archi's pipeline stages
  (designer, auditor, programmer, plan compiler) cut inputs since March: they now get their whole
  input, so expect more tokens per stage. Validated on 8.x: a ~3,000-character message to Archi
  with the question at its end was answered correctly.

### A-70 ✅ VALIDATED ON 8.x (0.1.75 to 0.1.79, 2026-10-16/17): NATS DELETED AND LLM USAGE RECORDED — The records' "NATS" lived inside the router, lost data and only stored the motherbee's

- **How it came up:** while designing stage 5 (tokens), the record was going to travel over NATS.
  The operator asked to review NATS in general and between hives. Design:
  `docs/router-stream-v1.md`.
- **What happens (verified in the 0.1.74 code and measured in PROD on 2026-10-16):**
  - **It is not NATS:** it is an in-house broker that speaks part of the NATS text protocol and
    runs inside the router process, one per hive, with no link between hives.
  - **Messages do not travel through it.** The router publishes a copy of each user message as a
    turn, with a new TCP connection for each publish.
  - **Its durability is weak:**
    - each topic is an in-memory queue of up to 50,000 messages which, past the cap, deletes the
      oldest ones even if nobody has read them, silently;
    - one in-flight message per consumer;
    - the disk is a single JSON file that is rewritten whole every 500 ms.
  - **Only the motherbee's data reaches the database,** because SY.storage runs only there:
    - worker1 had 67 turns that were never stored;
    - the ingress (DMZ) kept on disk 597 turns of edge traffic that nobody consumes, and they were
      going to grow up to 50,000.

    It is the gap A-38 left noted ("getting what is on the workers to reach the motherbee"), and it
    covers every hive except the motherbee.
  - **Three topics with no producer** (`storage.events`, `storage.items`, `storage.reactivation`):
    only an E2E script uses them.
  - **The log plane lives in the router's fast path.** The operator: *"todo lo que va a DB es
    de log, alta cantidad de datos, tiempos largos... eso choca contra la dinámica de fluxbee
    principalmente en los routers, que es alta velocidad"* ("everything that goes to the DB is log:
    a high volume of data, long times... that clashes with fluxbee's dynamics, mainly in the
    routers, which is high speed").
- **The operator's decisions (2026-10-16, in the doc §0):**
  - NATS dies (*"quedó como algo inicial viejo"* ("it was left over as an old, early thing")).
  - The router gains a simple stream, one per router, `RT.<router>.stream@<hive>`. The pipe is
    the Fluxbee socket.
  - Best effort for everyone.
  - A small window: 2 minutes and 32 MB per router, in visible constants. When it piles up: an
    alarm and a reduced log.
  - The router never waits.
  - Order:
    1. the stream with SY.storage;
    2. cognition and policy, and NATS deleted;
    3. the tokens.
- **Review (one round, three views: the router, the data path, the tokens):** everything
  incorporated into the doc.
  - **Live bugs it found** (fixed in stage 1a, on today's NATS):
    - **the AI's responses are not stored in `turns`:** the `(ctx, seq)` key comes from the
      `trace_id` and the response reuses the request's, so `ON CONFLICT DO NOTHING` throws it away;
    - **SY.cognition processes each turn several times:** it only confirms when it finishes, and
      the broker redelivers every 2 s. A test turn cost 3 or 4 calls to the model;
    - **a frame over 128 KB kills a link forever** in the node, WAN and IRP writers;
    - **a message from another hive to a node on a router that is not the gateway is dropped;**
    - **SY.storage does not reconnect to postgres;**
    - **`storage_inbox` is never purged,** and its retry runs only at startup.
  - **From the design:**
    - `RT.` reserved, with verification of the router's PID;
    - the stream's disk in its own thread;
    - separate queues for turns and for nodes;
    - authenticated acks;
    - the cursor and the sequence made safe against trims and restarts;
    - an ack on receipt for cognition and policy, without reprocessing repeats;
    - a dedicated SY.storage connection for what arrives in batches.
  - **For the operator:** six questions (Q1 to Q6 of the doc). None of them holds up stage 1.
- **Stage 1a, the code (2026-10-16):** the live bugs fixed on today's NATS. A code review (two
  views) found the following, and it was fixed before the build:
  - **The `storage_inbox` purge would have deleted history:** Archi's messages page reads the
    inbox as the message log, and the responses that `turns` discarded exist only there. No purge;
    where the log lives is up to the operator (Q7 of the doc);
  - **the inbox retry did not count attempts** and always retried the same rows: now it adds one
    attempt per failure and moves forward with a cursor;
  - **SY.storage stayed without a database if postgres was not there at startup:** now it exits
    and systemd retries;
  - **the router's remote origin** comes only from the LSA, with the canonical name (EDGE-01), and
    not from the frame. The fallback through the reachability table was dead code outside the
    gateway;
  - **the damage from the big frame was smaller than written:** node and IRP reconnected, losing
    what was in between; only the WAN link stayed dead in one direction.
- **Stage 1a, validated on 8.x (0.1.75):**
  - a request and its response both end up in `turns`;
  - a turn costs one call to the labeler, versus 3 or 4 before;
  - SY.storage comes back on its own when its database is cut;
  - `RT.` stays reserved.

  Detail in `lab/DEPLOYMENTS.md`.
- **Q7, decided by the operator (2026-10-16):** Archi reads `turns` (*"que lea turn es integrado
  mas que lo otro, si es asi hacerlo"* ("having it read turn is more integrated than the other
  thing; if so, do it")). `turns` becomes the message log, and the inbox gets purged.
- **Stage 1b, the code:** the stream on each router, with SY.storage as the destination. It
  includes:
  - `turns` as the message log, with a one-time backfill and a purge of the inbox;
  - Archi on `turns`;
  - `list_streams` in the admin;
  - `streamlost` in `ops.py health`.

  A code review (one round) found no HIGH. Everything was fixed before the build:
  - **the loss counters lied in both directions:**
    - the trim removed what had expired without counting it;
    - each retry of a batch counted the skipped items again.

    Now each one is counted once: when the cursor passes it, or when it leaves the log without the
    destination having taken it. What is in an in-flight batch waits for its ack;
  - **the upgrade order revived the old consumer:** the new router starts before the old
    SY.storage, which subscribed again. It would have held on to `storage.turns` and stored every
    turn twice in the meantime. Now the broker rejects that consumer;
  - **minor:**
    - the disk is written outside the lock that the router's threads use;
    - a failed write opens a new segment instead of cutting the one that was there;
    - the queue has a cap in bytes;
    - the last segment is deleted when there is nothing left to deliver;
    - SY.storage's processing moves out of the ack path;
    - each record has to come from a node in its stream's hive;
    - Archi reads the tail cursor before the list.
- **Stage 1b, validated on 8.x (0.1.76):**
  - **the four hives reach the database:** worker1, the ingress and the egress for the first time;
  - **the backfill left the 582 rows of `turns` with their message,** and the inbox deleted 435 old
    rows;
  - **Archi shows `turns`,** with the live tail;
  - **SY.storage absent for 71 s:** the backlog was seen, and everything was delivered once;
  - **absent for more than 3 min:** 10 expired, counted once, with the alarm, and routing carried
    on;
  - **a router restarted with pending items** recovered them from disk;
  - **the motherbee restarted under traffic** lost nothing;
  - **clean health** apart from the induced expiry.

  To test it, `sy-storage` had to be masked: SY.orchestrator's watchdog brings it up 2 s after a
  `stop`. Detail in `lab/DEPLOYMENTS.md`.
- **Stage 2, discussed with the operator (2026-10-17, doc §6.1):**
  - **cognition belongs to the interlocutors** (*"la parte cognitiva es de los interlocutores no
    de que hive está o que nodo es... los nodos son mecanismos, igual que el router"* ("the
    cognitive part belongs to the interlocutors, not to which hive it is on or which node it is...
    the nodes are mechanisms, just like the router")): memory and references per communication
    between ILKs, never per hive or node;
  - **the owner of the turn is the owner of the communication:** the agent serving it;
  - **cognition stays next to the agents** (motherbee and workers), so they read their memory
    through local SHM (*"no quiero cambiar eso"* ("I don't want to change that")). The agent's
    router passes both directions to its local cognition;
  - SY.policy goes away (*"si no hace nada sacalo. molesta"* ("if it does nothing, take it out.
    it's in the way")); NATS is deleted entirely, with no legacy; the ingress's 597 turns are thrown
    away; test data can be deleted;
  - the LLM cost stays as it is; what cuts across hives is for later.

  A design review (one round) found one HIGH: removing SY.policy in a single version blocked the
  core-update of every worker. It goes in two versions. The MEDIUMs were incorporated: a
  communication is `(thread_id, agent ILK)`, the turn number replaces `thread_seq`, the stream's
  epoch, the packed envelopes, the region that survives restarts, the reload over the mesh, and
  equal weights for the two ILKs.
- **Stage 2a, the code:** cognition per communication, SY.policy retired (first version) and the
  router without NATS for turns. A code review (one round), no HIGH. Fixed before the build:
  - **MEDIUM:** a loaded communication went stale if its agent went to another hive and came back,
    and the other cognition had continued it. Now, after 5 min without turns, it is requested
    again from SY.storage, and the one that is further ahead wins;
  - **LOW:**
    - the agent's node is pinned as soon as the state is fetched;
    - the reload keeps what SY.storage does not have yet, and it runs outside the main loop;
    - without a thread, the memory region is not read;
    - the `hive.yaml` is written atomically;
    - scopes are also protected by the turn number;
    - SY.frontdesk.gov answers with its ILK;
    - old comments and doc passages.
- **Stage 2a, validated on 8.x (0.1.77):**
  - **two hives, one communication:** an interlocutor on worker1 and the agent on the motherbee.
    The motherbee's cognition received both directions, worker1's nothing; one single
    communication in the database, written by one cognition, with the agent's ILK;
  - **the responses go out with the agent's ILK;**
  - **the agent receives its communication's package,** and the one the sender sends is dropped;
  - **a cognition restart** reloads over the mesh and the memory stays; the numbering continues;
  - **the inactivity refetch** asks for the state once, after 5 min without turns;
  - **an agent on worker1:** worker1's cognition writes to postgres through the stream. Before, it
    had no database;
  - **SY.policy** runs on no hive; NATS received nothing; clean health.

  Detail in `lab/DEPLOYMENTS.md`.
- **Stage 2b, the code:** NATS deleted, some 9,000 lines: the broker, the SDK client, the diags,
  eight scripts, SY.policy and `thread_seq`. SY.storage's metrics and the orchestrator's wait for
  the database move to the mesh (`STORAGE_METRICS_GET`), and there are one-time transitions at
  startup. A code review with two views, no HIGH:
  - **MEDIUM:** on the motherbee, the old router wrote the broker's state again after the cleanup,
    when it stopped during startup. The cleanup now runs after the router runs the new binary;
  - **LOW, all fixed:**
    - retired nodes stay out for everyone who reads a roles section;
    - the admin's commands, which stayed queued during startup, get `ORCHESTRATOR_BOOTING`;
    - one metrics scan at a time;
    - SY.policy leftovers on a motherbee installed with `install.sh`;
    - old comments;
    - the admin catalog's parity check, broken since 1b and A-68.
- **Stage 2b, validated on 8.x (0.1.78):**
  - **nothing of NATS or SY.policy is left on any hive:** nothing on 4222, no directory, no
    section, no unit, no binary;
  - **the ingress recorded its 597 pending turns** before the broker was deleted;
  - **the metrics and the database wait work over the mesh;**
  - **with SY.storage held back,** the admin answered `ORCHESTRATOR_BOOTING` in 0.01 s during
    startup, instead of hanging;
  - **the cognition regression passes;**
  - **clean health.**

  Detail in `lab/DEPLOYMENTS.md`.
- **Stage 3, discussed with the operator (2026-10-17, doc §7.1):**
  - a tenant's node is paid for by its tenant; cognition, by the tenant of the communication's
    agent; the frontdesk, by the case's (*"OK todo"* ("OK, all of it"));
  - a call that its caller abandons stays `abandoned`, with no tokens (*"después vemos qué
    cambiamos si no sirve así"* ("later we'll see what we change if it doesn't work like this"));
  - Archi and the admin's executor pay with the root (*"cobranza será a raíz"* ("billing will go
    to root")). Each record stores which Archi work it belongs to: session, pipeline run and
    solution. That is the place for the customer's tenant, later on;
  - nothing is deleted for now; a general date-based policy for the database, later;
  - `ai_local_probe` is deleted;
  - the reads for Cloud, approved (*"en todo caso evolucionará después"* ("in any case it will
    evolve later")).
- **Stage 3, the code:**
  - **what it does:**
    - every call to a model leaves a record, charged to a tenant, in `llm_usage`;
    - the AI SDK writes it to the router's stream;
    - SY.storage checks it against identity;
    - the admin, Archi and Cloud read it.
  - **The code review,** with two views:
    - **HIGH:** the router compared the names of the `SY.*` nodes that charge other tenants
      case-sensitively, and SY.storage case-insensitively. A process with `sy.frontdesk.gov` from
      any hive could charge any tenant;
    - **MEDIUM:** a record written while the node's router was restarting was lost without being
      counted. Now the write waits until the node is connected;
    - **LOW, fixed:** Archi's failure classifier without a session, a finished pipeline named in
      the work of later turns, and the index of the per-tenant sync;
    - **LOW, stays with A-62:** the executor lets any node of the mesh read every tenant's usage,
      like any other action.
- **Stage 3, validated on 8.x (0.1.79),** with an agent in the test tenant `pruebas`:
  - **each node charges whoever it should:**
    - the agent, to `pruebas`;
    - cognition, to `pruebas` through the agent;
    - the frontdesk, to the case's tenant, or to the root as a fallback;
    - Archi, to the root with its session.
  - **The probes:**
    - a foreign tenant that was claimed ended up as a mismatch;
    - a foreign `SY.*` was rejected;
    - `sy.frontdesk.gov` in lowercase from worker1 was rejected by the router;
    - a node with no identity stayed unverified at 10 minutes.
  - **`abandoned`:** cognition with 50 ms left `abandoned` records with no tokens.
  - **Reads:** the operator, Archi and Cloud read them. Cloud sees only its tenant, and the root
    gives it `TENANT_ROOT_PROTECTED`.
  - **Health:** `usagelost` at 0. The only things rejected were the probes.
  - **Cleanup:** everything from the test was deleted, including the keys and the `pruebas`
    tenant.

  Detail in `lab/DEPLOYMENTS.md`.
- **Stages:**
  1. **1a:** the live bugs ✅;
  2. **1b:** the stream with SY.storage ✅;
  3. **2a:** cognition per communication, SY.policy retired ✅;
  4. **2b:** NATS deleted, and SY.policy gone completely ✅;
  5. **3:** LLM usage, recorded per call and charged to a tenant ✅.

### A-69 ✅ VALIDATED ON 8.x (0.1.73 and 0.1.74, 2026-10-16) — SY.cognition failed on every turn with both providers: it sent `temperature`

- **How it came up:** the operator asked to test their Anthropic key across the whole infra
  (2026-10-16).
  - Archi, the admin and the frontdesk worked with Claude: Archi answers and uses its tools.
  - SY.cognition failed when labeling a test turn, with Opus 5.5 and with Sonnet 5.5.
  - Back on OpenAI, it failed the same way.
- **What happened:**
  - **`temperature`:** the semantic labeler and the narrative summarizer sent it (0.1 and 0.2).
    The current models of both providers reject it:
    - Claude 5: "`temperature` is deprecated for this model";
    - gpt-5.5: "Unsupported parameter: 'temperature' is not supported with this model".

    Each turn was left without semantic derivation. Only a WARN and the
    `semantic_tagger_invalid_outputs_total` counter showed it, because the SDK reports the
    provider's 400 as a protocol error.
  - **The output caps** (256 and 220 tokens) were tight for a model that reasons before
    answering.
  - **The status lied after every startup:** it said `degraded_no_ai_provider` until its first
    turn or a key change, even though the key was there. It did not resolve it at startup, unlike
    Archi and the frontdesk. On worker1 it showed after every core-update.
- **Fix:**
  - no `temperature`, like its peers (Archi, the admin, ai.generic);
  - caps of 1024 tokens: the cap only bounds a runaway response, it does not change what is spent;
  - the key is resolved at startup, waiting for the vault like the rebuild from the database;
  - the secret-change log said "openai" hardcoded; now it names the key's type.
- **The 0.1.73 gate (with Claude) passed:**
  - cognition started ready on both hives, also on the freshly updated worker1;
  - a test turn came out labeled (`billing`, `duplicate charge`, `october invoice`…) and
    summarized.
- **The same turn with OpenAI found a second bug, in the SDK:**
  - Its tool-less path read the text from `output[0]`. A reasoning model (gpt-5.5) puts a
    `reasoning` item first, and the message comes after it: "missing text output".
  - Archi did not see it because its path with tools walks through all the items.
  - Fix for 0.1.74: one single reader for both paths. It takes `output_text` if the server sends
    it; if not, the first `message` item.
- **The 0.1.74 gate:** cognition worked with OpenAI (labeler and summarizer) and with Claude. With
  Opus 5.5, one summary out of three went past the 8 s timeout (`semantic_tagger.timeout_ms`): that
  turn is left without memory synthesis. Raising it or using Sonnet 5.5 is the operator's call.
- **What is left out (noted):**
  - the SDK reports a provider HTTP error as a protocol error, and that mixes two cognition
    counters. It goes with the AI SDK review;
  - `config-get` on SY.admin answers with its origin rejection (`CONFIG_GET_RESPONSE`) to a query
    from the admin itself. Not investigated. **Fixed in 0.1.81 (A-76):** the gate admits
    `SY.admin@motherbee` itself, as the system policy delivers it (`admin_origin_authorized`, with a
    test).
  - **SY.cognition on a worker does not reach the database:** each hive has its embedded NATS, and
    SY.storage runs only on the motherbee. What a worker1 node publishes (its turns, what its
    cognition writes) stays in worker1's NATS, and its rebuild fails ("error connecting to
    server"). It predates this; it matters for stage 5 (where a worker node's consumption gets
    recorded). **Obsolete since A-70:** every hive's router stream reaches SY.storage (0.1.76),
    NATS is gone (0.1.78), and since 0.1.103 SY.cognition keeps its state in its own file on its
    hive.

### A-68 ✅ VALIDATED ON 8.x (0.1.70 to 0.1.72, 2026-10-16) — WF nodes were outside the standard: SY.wf-rules handled them directly with the orchestrator, and the Go node had its own control plane

- **How it came up:** the operator asked whether the WF node starts like the others (2026-10-15).
  The orchestrator starts it the same way: `systemd-run`, `config.json` with `_system`, the 60 s
  reconcile. What was different was around it. Design: `docs/wf-standard-path-v1.md`.
- **What happened:**
  - **Who launches it.**
    - SY.wf-rules sent SPAWN, RESTART, KILL and NODE_CONFIG_GET/SET directly to
      `SY.orchestrator@<hive>`, and NODE_STATUS_GET to the node, with the authority of rule (4) of
      the system policy.
    - It broke "every action goes through the admin".
    - Every WF node ended up with `launched_by: system`.
  - **The Go node.**
    - It read `_system` strictly and rewrote it from its struct: a new key from the orchestrator
      brought it down (it happened with `launched_by` in stage 4).
    - With a broken config or definition it exited, systemd relaunched it every 5 s, and the
      reason stayed only in journald.
    - It always answered HEALTHY.
    - Its CONFIG_SET errors carried `detail`, which the admin does not read.
    - It wrote an extra `config_version` at the root, with no atomic write and with 0644.
  - **The deploy from Archi was broken.**
    - The admin served the mesh's requests one at a time, and Archi's `wf_rules_*` waited for
      SY.wf-rules.
    - SY.wf-rules called the admin to publish the package, and that request stayed in the queue:
      at 5 s `PACKAGE_PUBLISH_FAILED`, with `current` already rotated.
    - Deleting failed the same way, and the executor's plans also ran in that loop.
    - Only the operator's HTTP worked, which serves each connection in its own task.
  - **`WF.orch.diag`.**
    - Rule (4) gave almost-admin authority to that name from the E2E tests.
    - A workflow called `orch.diag` would have been deployed with that name, and a workflow can
      send system messages.
- **Fix:**
  - **The admin serves each mesh command in its own task,** like the HTTP. The whole class of
    deadlock goes away: the 0.1.69 one, Archi's and the plans'.
  - **SY.wf-rules handles its nodes through the admin.**
    - It uses `get_node_config`, `set_node_config`, `run_node`, `restart_node`, `kill_node` and
      `get_node_status`.
    - The admin lets it act only on the `WF.*` nodes of its hive.
    - The only thing it asks the node directly is its running instances (WF_LIST_INSTANCES),
      which is a read.
  - **`launched_by`:** the admin records who asked for the deploy (`operator` over HTTP, `archi`)
    and SY.wf-rules carries it to the `run_node`.
  - **The policy:**
    - rule (4) stays only for SY.admin, with both wasm recompiled;
    - config-routes' authority markers and the table in `docs/history/04-routing.md` are brought up to
      date.
  - **The Go SDK has the managed nodes' control plane,** a port of the Rust one:
    - `_system` untouchable;
    - FAILED_CONFIG with its reason;
    - an atomic write that keeps the file's mode;
    - health according to the config's state.
  - **The WF node uses it,** and it validates the workflow definition as part of the config
    (`invalid_definition`). With a config it cannot use, it stays up, answers the control messages
    and drops workflow traffic until a restart applies a good config.
  - **A `wf_rules_*` query over the mesh without a hive** answers with an error instead of nothing.
- **Review (one round): 1 blocker and 7 to fix, all fixed before the build.**
  - **Blocker: a half-synced workflow package.**
    - Syncthing can bring the manifest and the `package.json` before the `flow/definition.json`.
      Before, the WF node crash-looped until it arrived, and it fixed itself; with the change it
      waited in FAILED_CONFIG and nobody retried it.
    - Now the orchestrator answers `RUNTIME_NOT_PRESENT` for a workflow package without its
      definition: in `run_node`, on every start and restart (the reconcile included) and before
      `set_node_config` binds the version. SY.wf-rules retries while it syncs.
  - **The runtimes manifest.**
    - With the admin serving in parallel, publishing and deleting versions could overwrite each
      other; a lock serializes them.
    - SY.wf-rules no longer writes the motherbee's dist: it deletes the old versions through the
      admin on every hive.
  - **The timings.**
    - SY.wf-rules gives each request a 35 s deadline; the admin waits 45 s for it, and Archi 60 s.
    - Before, Archi and the admin waited the same, 30 s, and a response close to the limit got
      lost.
  - **Who asks SY.wf-rules.**
    - It accepts operations only from the admin; before, any node could ask it to deploy or
      delete.
    - The admin accepts only `operator` or `archi` as its `launched_by`. Never `cloud`, which would
      give Cloud the management of the node.
  - **The content:** the admin requires the node's own `wf.<x>` runtime, the `wf.engine` base and
    a `package_path` of that package.
  - **The re-binding sends only the binding** (the orchestrator merges), instead of copying the
    node's config, which dragged along keys written by old nodes.
  - **SY.wf-rules reads a WF node's refusal correctly** (`node_not_configured`) and stops taking
    it as a timeout.
  - **Minor:**
    - a mesh command that fails before responding answers `INTERNAL_ERROR`;
    - the WF node's CONFIG_SET response carries its real state, unchanged until the restart, and
      `pending_restart`;
    - the SDK does not write over a `config.json` it cannot read.
- **The 0.1.70 gate on 8.x found two things, fixed for 0.1.71:**
  - **An update on worker1 did not wait long enough.**
    - The new package arrived more than 20 s after it was published.
    - The cause: just before, another workflow had been deleted. worker1's Syncthing failed to
      delete its directories ("not empty", parent before children), marked the runtimes folder as
      failed and retried it.
    - Now SY.wf-rules waits for the package up to 45 s, with a 60 s deadline per request; the
      admin waits 75 s and Archi 90 s.
  - **A deletion on worker1 left its packages on the motherbee.**
    - SY.wf-rules deletes the current version last, according to its copy of the manifest, which
      after a rollback was out of date. The admin rejected it (`RUNTIME_CURRENT_CONFLICT`) and the
      purge stopped there.
    - Now a version rejected as current is retried once the others are gone.
    - The test's orphaned packages were deleted by hand.
- **The 0.1.71 gate found that waiting longer was not enough, fixed for 0.1.72.**
  - **A first deploy on worker1 waited 45 s, and the package did not arrive.**
    - From the motherbee to the worker: the manifest arrived at 12 s and the package's files at
      62 s.
    - The motherbee's watcher batches changes. With the runtimes folder churning (the core-update
      just done, the previous E2E's deletions), it takes a while to announce them.
    - Deleting a workflow (and the version cleanup of a core-update) also makes the worker's
      folder fail once ("not empty") and wait before retrying.
  - **Fix:** on a worker, after publishing, SY.wf-rules asks the admin for a `sync_hint` on the
    motherbee's `fluxbee-dist-runtimes`.
    - The motherbee scans right away and announces the package. The worker pulls it as soon as
      the announcement arrives, even if it is waiting to retry a failed pull.
    - While the package is not there, it asks for the same scan before each retry.
    - Scanning on the worker does not help: Syncthing 2.0 does a pull after a scan only if it
      found local changes. The first version of the fix (`a053102`) did that, and it also waited
      up to 20 s for the worker's folder to settle, which does not happen after a failed delete.
      It was corrected before it was published.
    - It is an explicit, bounded step of the deploy.
- **The 0.1.72 gate passed everything,** in the worst case: right after the core-update, and
  worker1's E2E right after the deletion of the motherbee's E2E.
  - **The motherbee and worker1:** 35/35 each.
    - On worker1 the first deploy took 4 s: the package arrived 1 s after it was published.
    - The v2 arrived in 2 s, and the deletion left no packages.
  - **Through Archi:** deploy and deletion in 1 s; `launched_by: archi`; an unknown `_system` key
    stays in the file, and the node stays `HEALTHY`.
  - **Nothing from the test was left,** and the test tenant was purged.
- **What is left out (noted):**
  - **The admin's surface over the mesh** is still open to any node for almost every action: only
    IO.cloud and SY.wf-rules have a gate. A workflow does not reach it: the engine rejects every
    `SY.*` or `RT.*` destination when sending. It is A-62, parked along with A-60.
  - **All node actions go through the motherbee orchestrator's serial worker.** A long rollback or
    a hive that does not answer delays the others, a WF deploy too.
  - **WF configs written before 0.1.70** with `config_version` at the root (a direct CONFIG_SET)
    stay in FAILED_CONFIG with the reason. They are fixed with `set_node_config?replace=true` and a
    restart. In PROD there are no WF nodes.
  - **The OPA version counter** has no lock either. Only operator actions touch it.
  - **The health of IO nodes** ignores the config's state, and io.slack does not answer
    NODE_STATUS_GET. It goes to the IO family review.
  - **In the WF node,** opening the database and the recovery are still fatal.
  - **The diagnostic tools that used `WF.orch.diag`'s authority** lose it and go to the cleanup
    catalog:
    - `orch_system_diag` and the `scripts/orchestrator_*_e2e.sh` that use it;
    - `blob_sync_diag`, whose SYSTEM_SYNC_HINT was already denied.

    **Gone:** `orch_system_diag` and the scripts that drove it in 0.1.80 (the cleanup of
    2026-10-17, `41decbd`), `blob_sync_diag` in 0.1.81.

### A-67 ✅ VALIDATED ON 8.x (0.1.68 and 0.1.69, 2026-10-15) — Cloud could not see or manage its nodes, nor suspend a tenant, and a stopped node came back by itself

- **The frame:** `docs/ilk-tenant-model-v1.md` §2.4 and D14, stage 4, decided with the operator on
  2026-10-15.
  - Cloud is the business layer: it associates companies with tenants and users with people's
    ILKs, and it manages credentials.
  - It launches IO nodes only to have somewhere to load the token: *"el nodo lo lanza porque sino el
    token no se puede cargar"* ("it launches the node because otherwise the token can't be
    loaded").
  - The infrastructure belongs to the operator, through Archi.
  - Everything goes through the admin.
- **What happened:**
  - **Nodes:** Cloud created nodes and could not see, stop or delete them. The linkedhelper runbook
    queried the status through the admin directly, which Cloud no longer has.
  - **Tenants:** Cloud could not suspend or reactivate a tenant, nor edit its sponsor or its
    settings. `get_tenant` only said whether it existed.
  - **Stopping did not last:** a kill without a purge stopped the process, and the relaunch every
    60 s brought it up again. It also happened to the operator.
  - **Who launched a node** was not recorded.
- **Fix:**
  - **Tenants:**
    - `update_tenant` accepts `status` (`active` or `suspended`), `settings` and
      `sponsor_tenant_id`;
    - `get_tenant` reads the full record through the admin, with `people_count` and `node_count`;
    - `list_tenants` brings all of them except the genesis, with the deleted ones marked.
  - **Nodes:**
    - the admin records who launched each node (`_system.launched_by`: `cloud`, `archi`,
      `operator`);
    - Cloud starts, stops and deletes only the IO nodes it launched in its tenant. The admin puts
      that condition on the request and the node's hive checks it; for anything else the answer is
      `NODE_NOT_FOUND`;
    - `list_nodes` gathers the tenant's nodes from every hive in parallel, and `get_node_status`
      gives the status of one;
    - Cloud sees the status, not how the node runs: no process, unit or paths.
  - **Stopping lasts, for everyone:** stopping removes the relaunch mark, and starting puts it back.
    A node that crashes on its own keeps being relaunched.
  - **The word:** Cloud's contract says tenant, never company (85 places).
  - **`a5f1b2a`** goes out in this release: the `set_ilk_tenant` rejection answers with the code
    only.
- **Review (one round): 3 major, 4 minor and details, fixed before the build.**
  - **Major 1:** every WF node launched with this version crash-looped. The Go WF node reads the
    `_system` block strictly and did not know `launched_by`. It learned it, and a Rust test pins
    the block's keys against the Go one. The build in progress was stopped.
  - **Major 2:** with a spoke on an old core, Cloud could stop or delete other people's nodes, and
    `list_nodes` handed it the whole router inventory of that spoke. Now the motherbee checks the
    condition before forwarding, reading the node's config there, and `list_nodes` filters by
    tenant and marks the hive as outdated.
  - **Major 3:** stopping a node in a crash loop (waiting for systemd's next restart) answered
    "stopped" without stopping it. Now that path also stops the unit.
  - **Minor:**
    - the relaunch re-reads the mark after taking the node's lock;
    - the start, stop and delete responses no longer show the unit or paths;
    - a node's status carries `launched_by` and `relaunch_on_boot`;
    - the actions that Cloud shares stay open to the other callers on the mesh (the lab's
      diagnostic nodes had been left out);
    - the lab's restart script brings the node down like a restart, not with a stop.
  - **Documented:** nodes launched before 0.1.68 are not managed by Cloud; the `launched_by` of a
    direct request to the orchestrator is taken as it comes (A-22).
- **Found in the 0.1.68 gate (8.x, 2026-10-16), fixed for 0.1.69:**
  - **`delete_node` from Cloud got stuck.** The admin serves the commands arriving over the mesh
    one at a time. The kill with purge that Cloud asks for waits for the orchestrator, and the
    orchestrator, to clean up, asks the admin to delete the node's identity and keys; that request
    stays queued behind it. Result: a timeout for Cloud (`ADMIN_UNAVAILABLE`), the identity marked
    without being purged (the name reserved) and the node's own key deleted only when the admin
    freed up. It did not happen to the operator: their requests arrive over HTTP, each in its own
    task. Now the admin runs the commands that call it back (node kill and remove) in their own
    task, and IO.cloud gives `delete_node` 25 s.
  - **An SY from another hive reading a genesis key while identity restarts** (the deploy) got
    `UNAUTHORIZED` once: the vault only knows the SYs of its own hive by heart, and the others
    through the SHM. Now it answers `IDENTITY_UNAVAILABLE`, which is retried, without giving away
    anything extra. It predates this; in this deploy it hit `SY.cognition@worker1`, which read
    fine 42 s later.
- **Validated on 8.x:**
  - **0.1.68:** tenants through Cloud (settings, sponsor, reads, `pending` and genesis rejected);
    its nodes (`list_nodes` with only the status, `NODE_NOT_FOUND` for a node of another tenant
    and for one the operator launched in the same tenant); stopping lasts longer than one relaunch
    pass; suspending through Cloud cuts off new nodes and credentials; `a5f1b2a` answers with the
    code only; a test workflow starts clean.
  - **0.1.69:** `delete_node` answers ok, the node's own key goes away, the assigned one stays and
    the name is freed; on the restart, the SY from another hive got `IDENTITY_UNAVAILABLE` instead
    of `UNAUTHORIZED`.

  Detail in `lab/DEPLOYMENTS.md`.

### A-66 ✅ VALIDATED ON 8.x (0.1.67, 2026-10-15) — A suspended tenant kept starting nodes, and deleting a tenant marked its people

- **The frame:** `docs/ilk-tenant-model-v1.md` §2.3, stage 3. The operator's decisions of
  2026-10-14: *"no quiero borrar ni desaparecer ilk de humanos"* ("I don't want to delete human
  ILKs or make them disappear"); *"sobre nodos y recursos sí vale la pena actuar"* ("on nodes and
  resources it is worth acting"); restoring means reactivating the tenant.
- **What happened:**
  - **Nodes:** the orchestrator started and relaunched nodes of a suspended or pending tenant.
    Since stage 2 they got no secrets, but they ran.
  - **Deleting a tenant** cascade-marked the people who had written to it through a channel
    (2026-09-28), and it was rejected if it had Cloud accounts (`TENANT_HAS_ACCOUNTS`, stage 1). A
    person deleted on their own did not come back while their tenant was deleted.
  - **Purging a tenant** deleted every person charged to it.
  - **`set_ilk_tenant`** moved only complete accounts: a company's contacts could not be taken out
    of there.
  - **io.api `by_ilk`:** a company's API acted only on people charged to that company. A person
    who wrote to it but was charged to another one could not be used.
- **Fix:**
  - **Orchestrator:**
    - `run_node`, start and restart answer `TENANT_NOT_ACTIVE` if the node's tenant is pending or
      suspended;
    - the relaunch at startup and the periodic one skip it, without relaunching it or moving it to
      another version, until the tenant is active again;
    - a node that is already running is not killed;
    - if the hive does not know the state (identity still starting), it carries on as before and
      says so in the log.
  - **SY.identity:**
    - deleting a tenant marks only the tenant itself, with no cascade, and answers
      `people_charged`; it is rejected if the tenant has nodes or sponsors tenants;
    - restoring it brings it back as it was;
    - purging it is rejected while it has people charged to it (`TENANT_HAS_PEOPLE`); the ones
      marked on their own go with it;
    - `set_ilk_tenant` also moves temporaries, but only out of a deleted tenant: the frontdesk
      registers a temporary in the case of the tenant it wrote to;
    - `restore_ilk` brings a person back even if their tenant is deleted;
    - the v4 migration unmarks, on every startup, whoever an old cascade marked (their id stays in
      the log).
  - **io.api `by_ilk` (D12):** people charged to the tenant or with a channel in its space.
  - **Admin and Cloud:** `TENANT_HAS_PEOPLE` and `TENANT_NOT_ACTIVE` are 409; the texts of the
    actions and Cloud's catalog.
- **Review (one round): 2 major, 2 minor, 4 details.**
  - **Major 1:** the people an old cascade had marked (up to 0.1.66) went away with the purge as if
    they had been deleted on their own, and restoring their tenant no longer brought them back.
    Fixed with the v4 migration. In PROD there were none.
  - **Major 2:** moving a temporary out of an active tenant cut off its registration: the frontdesk
    rejects the case if the person's tenant is not the one of the IO serving them
    (`tenant_mismatch`). Now a temporary is moved only out of a deleted tenant.
  - **Also fixed:**
    - the relaunch reads the tenant from the config if it is missing from the map, like start and
      restart;
    - `start_node` of a node that is already running answers `already_running`;
    - a temporary merged into another ILK does not count as a person of the tenant;
    - the HANDBOOK and io.cloud texts that described the cascade;
    - a test that pins the order: the check comes before registering, forwarding, stopping or
      starting.
- **No code, documented:**
  - the router does not cut off a suspended tenant's traffic: it would be a lookup on every
    message, on the busiest path, and it adds little to the above;
  - a workflow deployed on a suspended tenant: wf-rules writes the new version into the config,
    the restart is rejected and wf-rules reports `restart_failed`. On reactivation nothing restarts
    it, so it carries on with the old package until a restart;
  - a node running in a suspended tenant during an upgrade stays on the old version. If it
    crashes, systemd cannot relaunch it (its directory is gone), and the periodic relaunch brings
    it up on the new one once the tenant is active again.
- **Open (D13):** joining a contact to an account, for example a customer of A who becomes the
  owner of B, needs an explicit step (the person proves the channel from their account). It is not
  designed.
- **Validated on 8.x (0.1.67):**
  - **suspended:** the restart of its node gives `TENANT_NOT_ACTIVE` and the node keeps running
    without secrets; stopped, two passes do not relaunch it; reactivated, the next pass relaunches
    it;
  - **deleted:** nobody gets marked, people still resolve, a deleted person comes back with the
    tenant still deleted, and restoring it leaves it as it was;
  - **purge:** it is rejected first because of the key and then because of the people, and it
    goes through once they are moved (a temporary included, out of the deleted tenant);
  - **found:** the rejection of moving a temporary arrived as `CODE|detail` in `error_code`; fixed
    in `a5f1b2a`, it goes out with the next release.

  Detail in `lab/DEPLOYMENTS.md`.

### A-65 ✅ VALIDATED ON 8.x (0.1.67, 2026-10-15) · the residual is P-1, tracked in A-22 — The vault believed the ILK the node itself declared

- **What happened:** SY.vault authorized by `meta.src_ilk`, which the node itself sets. The router
  did not bind it to the sender. A compromised node could:
  - claim another node's ILK and read its secrets, own or assigned;
  - claim SY.admin's and write or delete any secret.

  It was noted as phase 0 of io.web (`docs/history/io-web-spec-beta-v1.md` §21).
- **The operator's decision (2026-10-14):** fix it now.
- **Fix:** the vault binds the ILK to the sender name that the router stamps
  (`routing.src_l2_name`). It compares them exactly: no lowercasing, no trimming of spaces.
  - **A system node** proves the ILK derived from its name, without the SHM, so the core reads its
    keys at startup.
  - **Any other node:** it has to be the ILK that identity registered for its name.
  - **Someone else's ILK, or a request with no stamped name:** `UNAUTHORIZED`.
  - **An ILK the SHM does not have:** an error that gets retried (`IDENTITY_UNAVAILABLE`), because
    a degraded identity startup writes only the system ones.

  The refusal is answered and audited, never dropped.
- **What it closes:**
  - putting another ILK in `meta.src_ilk` is no longer enough, and a subject that io.api forwards
    does not read secrets;
  - between hives, the name is bound to the hive's certificate (EDGE-01): a node from another hive
    cannot be `SY.admin@motherbee`.
- **What it does not close:** within the same hive, the name is declared by the process in its
  HELLO, and the router accepts it (P-1, see A-22). A local process can open its own connection
  with the name of another node of that hive and use its ILK; on the motherbee, SY.admin's. It is
  the hole P-1 already leaves for everything else, not a new one, and the nodes run as root
  (A-60).
- **Reviewed and not built:** two cheap checks in the router, which remain for P-1:
  - rejecting a local HELLO with a foreign `@hive`: it adds little while any name of one's own
    hive can be chosen;
  - rejecting a name that already has a live connection: it can block the restart of a node whose
    old socket has not closed yet.
- **The router does not need it:** in a user message, `src_ilk` is the person, not the node.
- **Before the change:** every (node, ILK) pair that the vault's audit had ever seen was reviewed
  in PROD. All the SYs pass with their derived ILK; the IO/AI ones that show up were test nodes
  already purged.
- **Validated on 8.x (0.1.67):** after the deploy, all the SYs read their keys without a single
  rejection, also those of other hives; the admin requesting with IO.cloud's ILK got
  `UNAUTHORIZED` (audited), and with its own it passed; an IO.slack node read with its own ILK.

### A-64 ✅ VALIDATED ON 8.x (0.1.66, 2026-10-14) — Secrets were shared only per tenant, a suspended tenant kept handing them out, and any node could be born in the genesis

- **The frame:** `docs/ilk-tenant-model-v1.md` §2.2, stage 2. The operator decided it on 2026-10-14:
  *"tenant no es identidad ni seguridad ... el secret se guarda con el tenant para filtrar el uso del
  que paga"* ("tenant is not identity or security ... the secret is stored with the tenant to filter
  the usage of whoever pays").
  - A secret's tenant is who pays, and the only thing the vault checks is that it is active.
  - Who uses it:
    - its owner (a node, or a person);
    - the assigned nodes;
    - or, if it has neither, every node of its tenant: the general secret.
  - A node is born and dies in its tenant.
  - Only the core is born in the genesis.
- **What happened:**
  - **Sharing:** a secret belonged to one node or to the whole tenant; sharing it among some was
    not possible.
  - **Tenant state:** it was not checked. A suspended or deleted tenant kept handing out its
    secrets.
  - **The root as a fallback:** A-35 promised that a company's AI node fell back to the root's key.
    The vault never allowed it (only the SYs read it). The promise lived in the SDK, in ai.generic,
    in Archi's handbook, in an admin note and in the firstboot banner.
  - **The genesis:** the orchestrator rejected only IO nodes there. An AI, a WF or an `RT.*` with
    any runtime could be launched in the genesis, and from there read the Postgres URL.
  - **A node's tenant:** a temporary could still take another tenant when registering, and a
    replica could push the change.
  - **Unfinished sign-ups:** one from Cloud could not be told apart from someone who wrote through a
    channel (D11, already resolved in `80b479c`).
- **Fix:**
  - **SY.vault:**
    - a list of assigned nodes in the secret (`assigned_ilks`);
    - `VAULT_ASSIGN`, which changes that list without touching the value;
    - the active-tenant check;
    - the change notice reaches whoever had the secret and whoever has it now (the previous owner
      too), and the whole tenant if it is or was general.
  - **SDK:** the resolution looks first for an own one, then an assigned one, then the tenant's
    general one; never the root's.
  - **SY.admin:**
    - `vault_assign` (over HTTP and over the mesh);
    - the nodes of `vault_put` and `vault_assign`, which have to be charged to the secret's tenant
      and never be a person.
  - **IO.cloud:** `nodes` in `put_token` and `assign_token`.
  - **Orchestrator:** the definition of the core (SY.\*, RT.\* with an `rt.*` runtime, the core's
    IO), with which it rejects everything else in the genesis.
  - **SY.identity:** a node's tenant does not change, neither through registration nor through a
    replica.
- **Review (one round):** 1 major and 9 minor.
  - **The major:** refreshing a secret's value without naming nodes made it general and gave it to
    the whole tenant. It happened with every token refresh from Cloud. Now a new value keeps who
    uses it.
  - **Fixed:**
    - a tenant the SHM does not have is answered with an error that gets retried, so an identity
      outage does not leave the nodes without credentials;
    - an invalid tenant id is rejected;
    - the previous owner gets the notice;
    - `RT.*` only passes in the genesis with an `rt.*` runtime;
    - the edge no longer restarts its public door because of an `assign` of its TLS key;
    - `nodes: null` no longer unassigns everything;
    - the texts that promised the root;
    - tests of the validation of assigned nodes and of the decision about the tenant.
  - **Documented, no code:**
    - a deleted node stays on the lists, because removing it could make a secret general on its
      own;
    - io.linkedhelper re-reads its credential only at startup and on CONFIG_SET;
    - the vault trusted the `src_ilk` that the node itself declared: A-65 (0.1.67) closes it
      between hives and for the header; within the same hive, P-1 remains.
- **Validated on 8.x (0.1.66):** the vault's audit shows what each node reads. With four IO.slack
  nodes from two companies:
  - with the key assigned to two nodes, only those two read it;
  - when a node is taken off it, that one is out at once;
  - as general, all of the company's nodes read it and none of the other's;
  - with the company suspended, nobody; reactivated, they come back;
  - a person's secret is read by its assigned node even after the person is moved to another
    company;
  - the genesis rejects an AI node;
  - the core read its keys after the restart.

  Detail in `lab/DEPLOYMENTS.md`.

### A-63 ✅ VALIDATED ON 8.x (0.1.65, 2026-10-14) — Identity depended on the tenant: duplicated people, keys that drifted and merges across tenants

- **The frame:** `docs/ilk-tenant-model-v1.md`. The operator, Claude and Astra decided it on
  2026-10-14: the ILK is the actor and is never duplicated, the tenant is a cost center and
  permissions are explicit.
- **What happened:**
  - A channel's key was `(channel, address, ILK's tenant)`, always recalculated from the ILK's
    current tenant. So:
    - the same person in two companies was two ILKs;
    - re-registering a temporary in another tenant left the primary's lookup out of date until a
      restart;
    - a merge stored the moved channels with one tenant in memory and another one in the database.
  - `merge_from_ilk_id` did not check the tenant.
  - Cloud's `delete_ilk` could mark node ILKs.
  - Deleting a tenant marked every person charged to it.
- **Fix (stage 1):**
  - **The channel's scope:**
    - each channel stores its `scope`, fixed when it is created;
    - every key uses it: memory, database, SHM and the fast path;
    - the SHM's `IlkEntry` carries the cost center;
    - there is no layout change.
  - **One Cloud account per email:**
    - the `cloud` channel lives in the installation's scope, and only IO.cloud writes it;
    - `register_human` returns the existing account (`existing: true`);
    - an abandoned temporary is recharged to the tenant that asks, with history;
    - `ACCOUNT_AMBIGUOUS` and `ACCOUNT_IN_ROOT_TENANT` cover the old data.
  - **`ILK_SET_TENANT`:**
    - only complete people, with history in the same transaction;
    - the admin stamps whom the change is for;
    - registration no longer changes a person's tenant.
  - **Merges** within the same tenant.
  - **Cloud** deletes and restores only people, and reads by id with no tenant filter.
  - **Deleting a tenant** with accounts charged to it gives `TENANT_HAS_ACCOUNTS`; the rest still
    cascades.
  - **v3 migration in the schema:** before loading the memory, it moves the unambiguous accounts and
    reports the rest in the log.
- **Design review (one round):** 7 major findings and 5 minor, all incorporated into the design
  before writing the code.
- **Code review (one round):** 1 major and 8 minor.
  - **The major:** purging a tenant that a person had left always failed. The FK from their
    channels to the tenant was still alive. Now the purge takes along the channels left in that
    scope (`dropped_channels`).
  - **Fixed:**
    - **A rejected replica push:** it is no longer taken as a removal.
    - **The `cloud` channel and the installation's scope**, also closed through the replica
      (snapshot and delta), through `ILK_UPDATE` and through the merge from an IO node of the
      tenant.
    - **Recharging a temporary:** only with IO.cloud and in the shared scope.
    - **A node's own channel** does not answer a person sign-up.
    - **A foreign `ich_id` is rejected** (`ICH_ID_CONFLICT`).
    - **An ambiguous email:** it is also ambiguous if another ILK has it in the installation's
      scope. It is checked before searching.
    - **The restore of an old account** goes back to the installation's scope if the key is free.
    - **An unfinished sign-up neither blocks nor gets marked** when its tenant is deleted
      (`pending_signups`).
    - **Tests:** the admin's `on_behalf_of` stamp, plus 9 in identity.
  - **Documented, no code:** the version skew between the core and io.cloud during the upgrade. An
    old core rejects `scope`, and a new admin rejects an old `delete_ilk` without `ilk_type`. They
    are the seconds of the A-39 rebind. The v3 SQL is validated on 8.x with data made in 0.1.64.
- **Validated on 8.x (0.1.65):** old data made with 0.1.64 through Cloud's door. The migration
  moved what was unambiguous and reported what was ambiguous. 30 checks went through the public
  door. On the operator's side:
  - the history;
  - worker1's SHM;
  - the purge of the tenant a person had left (`dropped_channels=1`);
  - a restart of sy-identity, which resolved the ambiguous email live.

  Detail in `lab/DEPLOYMENTS.md`.
- **Pending for the next stages, settled:** reassigning nodes was dropped, since a node is born and
  dies in its tenant (D6; stage 2, 0.1.66, A-64); the tenant's state as a gate and the rest of D8
  came with stage 3 (0.1.67, A-66).

### A-62 ✅ VALIDATED ON 8.x (0.1.83, 2026-10-22) — Any node on the mesh can ask the admin for any action

- **Resolved (0.1.83, [control-path-hardening-v1](../../docs/control-path-hardening-v1.md) §2):**
  SY.admin admits a mesh caller only by its allowlist (`mesh_access`, `authorize_mesh_caller`);
  a caller on an ingress or egress hive gets nothing, roles come from the registry and fail
  closed. Live: a node on ingress1 and a `WF.*` node on worker1 were refused; Archi, IO.cloud,
  SY.wf-rules (a deploy) and the orchestrator's `teardown_node` were admitted. What root on a
  spoke can still do is the design's §9.

- **What happens (seen in the code on 2026-10-07):** the admin's internal gateway (`ADMIN_COMMAND`)
  does not look at who is calling, except for a few actions with their own gate: Cloud's relay
  (`authorize_cloud_relay`), `externalize` and `publish_artifact`. The rest (`add_hive`,
  `vault_get`, `kill_node`, `opa_*`, `publish_cloud_endpoint`, which issues a new Cloud bearer and
  returns it) it runs for any node connected to the router. It has been like that by design since
  the internal gateway (`docs/history/02-protocolo.md` §7.10: AI/WF/IO callers).
- **Why it is not urgent:** the tenants' logic does not reach it. A WF cannot target `SY.*`
  (`go/nodes/wf/wf-generic/node/actions.go`), and ai.generic only sends `publish_artifact`, with
  the action fixed. It takes a compromised node process, and the nodes already run as root (A-60):
  whoever compromises one is already root on the host.
- **When to look at it:** together with A-60 (nodes without root). That is when who can ask for
  what starts to matter. Likely form: a list per role prefix, with the system actions only for
  `SY.*`.
- **Since 0.1.79 (A-70, stage 3):** over the mesh, `get_llm_usage` is requested only by Archi and
  IO.cloud, but the executor (`executor_execute_plan`) runs any plan as the operator for any node.
  A one-step plan reads the LLM usage of every tenant. The stage 3 code review found it, and it
  stays here: reading usage changes nothing about what the executor already allows.

### A-61 ✅ RESOLVED (0.1.64, validated on 8.x) — Cloud could overwrite another tenant's keys and read identities of any tenant

- **What happened (review of the Cloud team's cards, 2026-10-07):**
  - `put_token` overwrote an existing key of **any** tenant, infrastructure ones too (the AI
    provider's key): SY.vault did not look at the tenant when writing. With the root tenant it was
    even "its own".
  - `get_ilk_details` by id read the ILK of any tenant, and the fast reads took the tenant from
    `params`.
  - `edge_fanout_verify:<ich>` was not among the reserved keys.
- **Fix (approved by the operator, 2026-10-13: *"primero io.cloud"* ("io.cloud first")):**
  - **SY.vault, for every caller:** a put of a key that belongs to another tenant is `KEY_TAKEN`
    (even if the value is the same), and it never moves it. Delete, rotate and roll back require
    `tenant_id`, and another tenant's key answers `KEY_NOT_FOUND`. The admin sets the root if none
    comes, just like in the put; the orchestrator passes each key's tenant when it cleans up a
    node's secrets; Archi's plans have the field.
  - **SY.identity:** ILK_GET accepts `tenant_id` and answers `NOT_FOUND` for an ILK of another
    tenant. `update_tenant` rejects the root (`TENANT_ROOT_PROTECTED`; it is rewritten on every
    startup) and a name or domain of another tenant (`TENANT_NAME_TAKEN` /
    `TENANT_DOMAIN_TAKEN`), and it validates everything before changing anything (before, a bad
    `status` left the new name set).
  - **IO.cloud (rule 3):** the tenant goes only in the root of the envelope (one in `params` is
    rejected), it is never the root, and everything the op touches belongs to that tenant. New
    operations `list_tokens`, `delete_token` and `update_tenant`. Every error carries
    `error_code`.
  - **SY.admin:** it enforces all of that again on the server side for `IO.cloud`
    (`enforce_cloud_relay_content`).
- **What almost broke:** adding actions to Cloud's surface means that, over the mesh, only io.cloud
  can request them (it already happened with `delete_ilk` on 2026-09-28). `delete_ilk`,
  `vault_list` and `vault_delete` were specifically left open to the orchestrator (cleanup of a
  node), and the whole surface to `SY.architect@motherbee`. Over the mesh, Archi was already
  getting its `run_node`, `get_ilk` and `delete_ilk` commands rejected.
- **Adversarial review (one round, 2026-10-13):** one MAJOR: the .deb's factory reset deleted the
  secrets without naming the tenant, so with the new rule it left the tenants' secrets behind and
  then could not purge them (TENANT_HAS_SECRETS). Now it deletes each key in its tenant. Six minor
  ones fixed:
  - a key of another tenant reads the same as a nonexistent one (same code and same text; only the
    audit says "denied");
  - Cloud's tenant is accepted only in its canonical form (the other spellings of the root's UUID
    passed the exact check);
  - the Cloud emulator no longer uses the root by default, and its test deletes its key;
  - `KEY_TAKEN` and the tenant `*_TAKEN` codes answer 409, `TENANT_ROOT_PROTECTED` 403;
  - two doc blocks that were out of place;
  - a sentence in the doc.
- **Out of scope (the operator's decision):** a person in several companies and parent/child
  tenants go to the identity talk. Restricting Cloud's `delete_ilk`/`restore_ilk` to human ILKs
  was left as a question: the operator hesitated, because tokens are assigned to the node's ILK.
  That last part does not change the ILK, only the secret's metadata.
- **Contract:** `docs/io-cloud-api.md`, rewritten to the real state (it was at 3 operations and
  configuration through `.env`).
- **Validated on 8.x (0.1.64, 2026-10-13):** 53 checks through the real public door with Cloud's
  bearer, plus the ones on the operator's side (the node purge deleted its secret in its tenant,
  the HTTP `DELETE` with and without a tenant, Archi over the mesh), all PASS (ledger 0.1.64).
  Minor pending item, settled in 0.1.66: the admin's HTTP mapped `TENANT_NOT_FOUND` to 500; it
  answers 404 (`error_code_to_http_status`, src/bin/sy_admin.rs, with a test).

### A-60 [General/security] 🟡 PARKED (to evaluate, 2026-10-07; no design yet) — The managed nodes run as root

- **What happens:** the orchestrator launches the nodes (IO, AI, WF) with `systemd-run` without
  `User=`, so they run as root on every host. Seen in PROD on 2026-10-06 (read-only): `io-blob`,
  `io-cloud` and `sy-wf-rules` run as root. A compromised node is root on its host, and everything
  it writes ends up owned by root (hence A-56).
- **What 0.1.63 does (the minimal step, approved):** the nodes stay as root, but the SDK hands each
  blob over to the owner of the folder where it leaves it, and the postinst's `chown -R` goes away.
- **What has to be evaluated (operator: *"deja documentado el tema de evaluar correr los nodos como
  fluxbee de usuario"* ("leave the topic of evaluating running the nodes as the fluxbee user
  documented")):** running them as `fluxbee`, or with one user per node. First we have to see
  what they use today that only root can reach. On the motherbee, on 2026-10-06:
  - the SHM regions (`/dev/shm/jsr-*`: router, config, identity, lsa, memory, opa) are
    `root:root 0600`, and the SDK reads them for the ILK lookups, the config and the policy;
  - the router's socket is `0666`, but the `irp-*.sock` is `root:root 0600`;
  - `state/nodes/` is `root:root 0755`, with each node's `.uuid` owned by root;
  - `dist/runtimes` is `root:fluxbee 0755` (readable).
  We also have to measure what each base node (io.api, io.blob, io.cloud, io.slack, io.wapp,
  ai.generic, wf.engine, io.linkedhelper) does with the filesystem and the network.
- **Status:** to evaluate after 0.1.63; it has no design yet.

### A-59 ✅ RESOLVED (0.1.62, validated on 8.x) — The identity replica waited forever on a subscription cut without a FIN

- **What happened (DTAP panel round 7, P7-1, confirmed in the code):** a replica's delta
  subscription is a long-poll. The replica only writes to confirm a delta, it read with no timeout
  and the socket has no keepalive. If a stateful firewall stops tracking the connection, it drops
  the following deltas and also the FIN, and the replica never finds out. It happens with a table
  loaded after the connection is opened (the first apply of stage 4, or a join where sy-identity
  starts before the posture) or after 5 days without traffic (the conntrack timeout). Additions,
  removals and revocations stop reaching that hive until its sy-identity is restarted.
- **Fix:** the primary writes a heartbeat (`IDENTITY_SYNC_HEARTBEAT`, with no seq and no ack) when
  20 s go by without sending anything. The replica cuts the subscription after 60 s without
  hearing anything, says so in the log ("no frame from the primary for 60s (heartbeats stopped)")
  and subscribes again from a full snapshot. It is the same pattern as the peers: the WAN sends an
  LSA every 10 s and the identity publication reconciles every 30 s.
- **Tests:** three tests with real sockets: the primary beats and keeps delivering deltas; the
  replica cuts a silent stream; and the two together stay subscribed. Without the heartbeat two
  fail, and without the timeout one fails.
- **From the adversarial review (two more fixes; they predated this):**
  - a delta could be lost silently when a subscription was opened: the primary could build the
    snapshot before registering the subscriber, and a delta broadcast in between was in neither of
    the two. Now the handler queues the registration before responding, and the main loop
    registers what is queued before building each snapshot. With P7-1, re-subscriptions are more
    frequent, which is why it matters;
  - dead subscriptions were cleaned up only when a change was broadcast; now also when a new one is
    registered, so the cap of 256 counts only the live ones.
  - Rolling the motherbee back from 0.1.62 while leaving the spokes up makes the worker
    re-subscribe every 60 s: the HANDBOOK ("The rollback") asks to roll back the spokes first.
- **Seen in PROD before the fix (0.1.61):** worker1's subscription had gone 15,929 s without a
  packet in either direction; the publication channel, every 30 s.
- **Validated (2026-10-06, ledger 0.1.62):**
  - On worker1, the subscription went from 15,929 s without a packet to a `lastrcv` of 8, 3 and
    18.5 s.
  - A 100 s cut of what arrives from :9100: the replica reported it 60 s after the last heartbeat,
    and it re-subscribed 5 s after the cut was lifted.
  - A worker joined from scratch (worker2) also receives heartbeats.

### A-58 ✅ RESOLVED (0.1.63, validated on 8.x) — On the workers and the egress, Syncthing ran as root

- **What happens (seen on 2026-10-06 in PROD, read-only):** worker1 and egress1 do not have the
  `fluxbee` user, so `resolve_syncthing_service_user` falls back to root: Syncthing and its whole
  `dist/` belong to root. The ingress bootstrap creates the user (`useradd`); the worker and egress
  ones do not (`worker_remote_base_dirs_command`).
- **Impact:** a compromised Syncthing on those hosts is already root. Since stage 3 it listens only
  on loopback and its only peer is the motherbee, so the motherbee would have to be compromised
  first.
- **Fix (approved by the operator, 0.1.63):**
  - The orchestrator creates the `fluxbee` user at startup, in any role. Syncthing always runs as
    `fluxbee`: the root fallback and the `blob.sync.service_user` and
    `blob.sync.allow_root_fallback` options are gone.
  - On a spoke that runs it as root today, the change is a visible, one-time step: the log says
    "Syncthing moves to its own user (A-58)", Syncthing stops, its home, its blob folders and the
    spoke's `dist/` go to `fluxbee` (`chown -R` with the service stopped, so it does not keep
    creating root files), the new unit is written and it starts.
  - The home becomes `fluxbee`'s whenever Syncthing runs; before, the egress (which only syncs
    dist) never had it.
  - What the adversarial review added:
    - the binary is installed before anything is stopped: a vendor that cannot be installed leaves
      the Syncthing that delivers it running;
    - the change is decided by the unit, by the home's files and by the process's user, so a
      change left half done runs again;
    - it leaves a `syncthing_user_switched` alert, only once Syncthing is healthy as `fluxbee`;
    - `useradd` uses a `fluxbee` group that already exists;
    - second round:
      - the process's uid only counts when `/proc/<pid>/comm` is `syncthing`, because while
        systemd is starting it the process still runs as root;
      - `daemon-reload` on the change, so it does not bring up the old unit definition.
  - Rolling a box back to 0.1.62 leaves Syncthing as `fluxbee`, because the user already exists.
    But the 0.1.62 nodes write root blobs that this Syncthing cannot read. To bring it back to
    root: stop `fluxbee-syncthing`, `userdel fluxbee` and restart `sy-orchestrator` (HANDBOOK,
    "The rollback").
- **Remaining:** the ingress bootstrap still creates the user and does its `chown`. It is
  redundant, but it is not touched until an ingress join is validated (the ingress branch of the
  A-48 gate).
- **Validated (2026-10-06, ledger 0.1.63):**
  - worker1 and egress1 created `fluxbee` and switched once (`syncthing_user_switched` alert with
    the service healthy);
  - their folders stayed `idle` with no errors;
  - a blob written on worker1 reached the motherbee in ~12 s, and one from the motherbee reached
    worker1 in ~10 s;
  - the motherbee and the ingress were not touched.

### A-56 ✅ RESOLVED (0.1.63, validated on 8.x) — Root wrote by path in the Syncthing user's `blob/`

- **What happens (third adversarial review of A-51, 2026-10-06; verified step by step in the code,
  not run end to end):** on the motherbee, `blob/` and `blob/staging` belong to the Syncthing user,
  and root processes write there by path: the blob SDK (`fs::write`, chmod by path,
  `ensure_dir_mode_0750`) from io.blob, the IO and AI nodes and sy_architect.
  - The name in staging is predictable (`staging/<hash16[..2]>/<name>_<hash16>.<ext>`). That user
    can plant a symlink there, and root writes a third party's bytes (for example, an incoming
    attachment) wherever it points.
  - io.blob does a chmod 0750 of a planted `blob/public` on every startup.
- **What it also hid (seen on 2026-10-06):** the nodes run as root (A-60) and the SDK left each
  blob `root:root 0640`. On the motherbee, Syncthing runs as `fluxbee`, so a new blob could not be
  read or synced. Nobody noticed because the postinst did `chown -R fluxbee blob/` on every
  install.
- **Fix (approved by the operator, 0.1.63):**
  - Owners: `blob/` is `root:fluxbee 0750`; `staging/` belongs only to root, the whole tree, with
    no links inside; `active/` and `public/` belong to `fluxbee`. The orchestrator leaves it like
    that on every startup.
  - SDK (`blob::safe_fs`): staging is written with an exclusive temporary and a rename by
    descriptor. `promote` opens `active/<prefix>` without following links, hands the blob over to
    the owner of `active/` with `fchown` on the descriptor and does a `renameat`.
  - io.blob: the public copy is written the same way inside `blob/public`, with the folder's owner,
    an exclusive temporary and `linkat`. It no longer does a chmod by path, nor does it change the
    folder's mode at startup.
  - The postinst's `chown -R` and the `blob` ones in `install.sh` were deleted.
- **What the two adversarial reviews added (same step):**
  - **Reading, which predated this:** `resolve` handed out a path that io.slack, the AI SDK, the
    architect and `io_common` read with `fs::read`. Syncthing replicates symlinks, so a
    compromised worker could make the motherbee read a root file and send it to Slack or to an
    LLM. Now:
    - `open_blob` opens the whole chain without following links and only accepts a regular file of
      the `BlobRef`'s size;
    - `read_blob` also verifies that the SHA-256 starts with the name's hash;
    - the four readers and io.blob's curator use this;
    - `resolve` stays only to know where the blob lives.
  - **`promote`:**
    - it only considers a regular file promoted; a link planted with the name is replaced, never
      followed;
    - the blob in staging has to be a regular file with a single link, from this process;
    - opening does not block on a FIFO;
    - after the move, the inode that arrived is verified: a concurrent `put` could change the file
      between the handover and the `renameat`, and a root blob was left behind.
  - **Temporaries:**
    - the staging GC deletes the ones a crash left;
    - io.blob deletes at startup the `.curate-*` files left in `public/`.
  - **`blob/agent-assets`:** it becomes root-only. It had stayed owned by the Syncthing user
    because of the old `chown -R`, and the architect and ai.generic write and read there by path:
    that user could change an agent's system prompt.
  - **What root already left in `active/` and `public/`:**
    - on every startup, after the rebind, it is handed over to the Syncthing user (`chown -R`,
      which walks by descriptor), and an alert says how many entries there were. It only runs with
      `fs.protected_hardlinks=1`;
    - in addition, the watchdog alerts when Syncthing reports per-file errors in the blob folders.
      A file it cannot read never makes the folder "unhealthy".
  - **Markers:** the `.stfolder` files take their folder's owner.
- **Second round of the reviews (same step):**
  - **Concurrency on the same blob:** a concurrent `put` + `promote` of the same blob no longer
    gives a false integrity error. A staging file that another `put` replaced (with no links) or
    that another `promote` already handed over is accepted, and if another `promote` already moved
    it, it is Ok. There is a test with 4 threads.
  - **Bounded read:** `read_blob` reads at most the `BlobRef`'s size plus one byte, even if the
    file grows while it is being read.
  - **Async:** the reads run in the blocking pool (`read_blob_async`) for callers from async code.
  - **ai.generic:** it verifies that each asset in `agent-assets` has the SHA-256 of its name. If
    someone edited it before the first startup of 0.1.63, it is not loaded.
  - **The startup handover:** it tolerates races (`-ignore_readdir_race`, `-execdir chown -h`). A
    recount judges it, and it alerts per folder with what was left.
  - **`protected_hardlinks`:** the pass that returns staging and `agent-assets` to root also
    requires `fs.protected_hardlinks=1`.
- **Tests:**
  - no planted link is followed, not in staging, not in `active/<prefix>`, not in `public/`;
  - `read_blob` rejects a link, a different size, different bytes and a FIFO, without hanging;
  - the blob takes the group of `active/` (a secondary group of the test user, so it can be seen
    without root);
  - the GC deletes temporaries;
  - io.blob cleans up `.curate-*`.
- **Against the previous code:** according to the review, the link tests in staging and in the
  `active/` prefix failed, and so did the group one. The `public/` one also failed, but only
  because of the temporary it left.
- **Validated (2026-10-06, ledger 0.1.63):**
  - the new owners are in place on the 4 hosts, with 0 foreign entries;
  - the ingress handed over its `.stfolder`;
  - a blob written as root with the new SDK ended up `fluxbee:fluxbee 0640` and crossed between
    worker1 and the motherbee in both directions.
- **Residue:** the `active/` GC still lists, measures and deletes by path. If someone swaps a
  prefix folder for a link between the listing and the deletion, root could delete blob-shaped
  names outside. It ships off by default (`gc.enabled` and `gc.apply` at `false`); it moves to
  descriptors before it is turned on.

### A-57 ✅ RESOLVED (0.1.62, validated on 8.x) — `update category=vendor` installed the new Syncthing but did not restart it, and answered that it had

- **What happened (verified in the code on 2026-10-06):** the vendor update called
  `ensure_blob_sync_runtime`, which reinstalled the binary if the hash changed and then did
  `systemctl start`, which does nothing if the service is already running. The running Syncthing
  kept the old binary until the next restart, and the response said
  `restarted: [fluxbee-syncthing]` anyway.
- **Fix:** installing the binary and writing the unit now say whether they changed anything (the
  unit had the same problem: `daemon-reload` without a restart). If the service is running and
  either of the two changed, or if its process runs a deleted binary (`/proc/<pid>/exe` ends in
  `(deleted)`, the same check the core uses), it is restarted at that moment, before the health
  checks, which already use the new unit's port and home. Reading it from the process makes a
  restart that was lost because of an error in between happen on the next call (adversarial
  review, F1). The response says what happened: `updated: [syncthing]` only with a new binary,
  `restarted` only if there was a restart. Tests of the rule and of the response.
- **Validated (2026-10-06, ledger 0.1.62):**
  - With no new binary, the update answers `unchanged` and with no restart, and no host restarted
    Syncthing when the new orchestrator started.
  - With the binary replaced under the running process (an identical copy with another inode), the
    update restarted Syncthing and said so (`restarted: [fluxbee-syncthing]`). The next one did not
    restart.
  - Only the case of a vendor with new bytes is missing, and it follows the same path.

### A-54 ✅ RESOLVED (0.1.61, validated on 8.x) — The motherbee used SSH outside `add_hive`

- **What happens (verified in the code on 2026-10-06):** all the SSH lives inside the three
  `add_hive` flows, with one exception. On every motherbee startup, `reconcile_hive_tls_material`
  logs into each `connected` spoke over SSH to see whether it has its mesh certificate and, if it
  is missing, reissues it.
  - In PROD the four spokes joined in `revoke` mode: those attempts fail authentication and are
    discarded at debug level. It is noise on every startup.
  - With the optional `ssh_access=key_only_persist` mode (it leaves one SSH key per spoke in the
    vault after the join) it does get in. P6-2 of round 6 showed the risk: only an authentication
    failure cuts the attempt, so a machine that takes the spoke's IP and accepts any key receives a
    new leaf and its private key.
- **The operator's decision (D32, 2026-10-06):** *"no deberíamos tener comunicaciones NUNCA por ssh a
  no ser cuando se hace un add_hive y es totalmente temporal"* ("we should NEVER have
  communications over ssh, except when an add_hive is done, and it is completely temporary"). The
  startup reconciliation and the `key_only_persist` mode are deleted (no PROD spoke uses it). A
  spoke that loses its certificate joins again (`remove_hive` + `add_hive`), like a new machine.
- **Fix (0.1.61, one adversarial review):** the reconciliation and the mode were deleted; a request
  with `ssh_access` is rejected (`INVALID_REQUEST`). Sending the leaf became fatal for the join
  (`TLS_PUSH_FAILED`, retryable if it was an SSH cut): now the join is the only one that issues it.
  Each join's revoke also deletes a `fluxbee-spoke-recovery` key the old mode may have left, and
  the factory reset no longer preserves `ssh:*` secrets. A guard test walks the code from every use
  of ssh/scp/sftp/rsync/sshpass/ssh-keyscan up to `accept_add_hive` and fails with the chain if
  anything else reaches it (tested with probes: from startup, `sh -c "ssh …"`, `Command::new`
  without a literal, relaunching the join from `main`).
- **On the record:** a join that fails in a retryable way leaves the bootstrap key on purpose (so
  that the retry gets in by key); if the operator abandons it, the key and the sudoers entry stay
  on the box until they complete it or remove them through the console. A box joined at some point
  with `key_only_persist` (DEV only, July) loses its recovery key on its next join; the vault's
  `ssh:<hive>` secret is deleted with `vault_delete` or with a factory reset.
- **Validated (2026-10-06):** zero SSH attempts from the motherbee to the spokes since the deploy
  (before, one per startup); a new join completed with the fatal leaf push and the revoke (ledger
  0.1.61).
- **Follow-up (0.1.62, from the D32 review):**
  - each flow issues the leaf before its first SSH session: if the CA cannot, the join fails with
    `TLS_ISSUE_FAILED` before installing anything on the box (the failure path's revoke still
    tries to get in, as in any non-retryable failure);
  - the revoke deletes the `fluxbee-spoke-recovery` key only if that name is the line's comment
    (before, any line that mentioned it), with a test that runs the filter with bash;
  - the guard test also covers ssh-copy-id and autossh, looks for the program in command position
    inside a shell (after `sudo -n -u x`, `timeout 5`, `env A=b`, `bash -lc`, `su -c`,
    `runuser`, `systemd-run`, `if`, `{`, with the source's escaped quotes), rejects renaming
    `Command` and code after `mod tests`, and requires `accept_add_hive` to have a single caller,
    `handle_admin`. Tested with seven probes that make it fail and one control that does not;
  - along the way, a worker's join fails with `VENDOR_SYNC_FAILED` if the vendor does not reach
    it, as egress and ingress already did: without vendor it never gets Syncthing, and nothing
    repairs it.

### A-55 [General/security] 🟡 PARKED BY DECISION (D31) · a switch since 0.1.83 (`egress.ssh_from_wan: false`) — The egress's SSH door

- **What it is:** the operator keeps SSH (tcp 22) open on `egress.wan_iface` for operation and
  maintenance during this development. In PROD it is 192.168.8.240, an internal network behind a
  firewall (*"no es la NSA pero es red segura"* ("it's not the NSA, but it's a secure network")).
  Nothing else gets in through there.
- **Risk:** the code cannot tell a secure office network apart from the internet. If a customer
  connects the egress's WAN directly to the internet, that door is left open to the internet.
- **Status:** close it before a customer's first installation (a code change in a release; the
  design has it in the declaration of the egress role, §3.2).

### A-19 ✅ RESOLVED (0.1.43) — The published policy waited up to 60 s for the Syncthing watcher

- **What happened:** with 0.1.41 the apply reached the 4 hives in 16 s, but the clear took 63 s,
  and the admin answered with the three spokes pending. The motherbee's Syncthing events show it:
  the two applies were scanned 10 s after publishing (`fsWatcherDelayS`), the clear 60 s after.
  The watcher holds back the changes that are renames and deletions until its timeout (6 × 10 s),
  and the publication writes by rename. With 0.1.42 it also held back an apply: at 33 s only the
  motherbee was running v8.
- **Fix:** after applying on the motherbee, the admin sends the motherbee's orchestrator a sync
  hint for `fluxbee-dist-policy`, which scans the folder right then (the same step as a runtime's
  publish), and only then the notice (0.1.42). In 0.1.42 the orchestrator rejected it (invalid
  `folder_id` for the `dist` channel: it only accepted `fluxbee-dist`, even though the handler
  already knew how to honor a specific folder) → 0.1.43 accepts any `fluxbee-dist*`.

### A-20 ✅ VALIDATED ON 8.x (0.1.83, 2026-10-22: the direction) — With CONFIG_CHANGED crossing hives, its receivers applied any origin the router admitted

- **Resolved (0.1.83, design §3):** rule (3) admits orchestrator mutations only from
  `SY.orchestrator@motherbee` and reads also from the same hive; rule (4) never admits
  `ADD_HIVE_FINALIZE`/`REMOVE_HIVE_CLEANUP` on the motherbee; the orchestrator answers admin
  messages only from its own hive's SY.admin. Live: a SPAWN from ingress1 was dropped and an
  admin read to the motherbee's orchestrator answered FORBIDDEN; spawns on worker1 still work.

- **What happened:** the router's gate for CONFIG_CHANGED admits the primary admin **and the
  orchestrators** (rule 3 of the system policy). SY.opa.rules applied a compile/apply/clear from
  any of them, and what is applied on the motherbee is now published to every hive.
  SY.config.routes applied the routes/VPN/taps list of a global CONFIG_CHANGED, whose only sender
  was `PUT /config/*` (removed in 0.1.41): now it would reach every hive and overwrite their
  config.
- **It is not a new capability:** rule 3 already lets an orchestrator write the policy or the
  routes of any hive through `CONFIG_SET`. The orchestrator does not use it: `CONFIG_SET`/
  `CONFIG_GET` are sent only by SY.admin; the orchestrator forwards between hives
  SPAWN/KILL/NODE_CONFIG_*/NODE_STATUS/LIST_NODES/GET_*/ROLLBACK/ADD_HIVE_FINALIZE/
  REMOVE_HIVE_CLEANUP and sends CONFIG_CHANGED `node_config` only to nodes of its hive.
  **Decided (operator, 2026-10-01): shrink it** → 0.1.45: rule 3 is the list of the 16 actions
  the orchestrator forwards, and the config signals that nobody used were deleted (`node_config` +
  `notify`, storage's `CONFIG_RESPONSE`, sy-wf-rules' handler, io-slack's branch). Validated: a WF
  cycle on worker1 and reads across hives without a single gate rejection.
- **What it does NOT close (DTAP panel, D-2):** the list has no direction. The orchestrator of any
  hive, the ingress's included, can still send another hive the list's mutations
  (`NODE_CONFIG_SET`, `SPAWN`/`KILL`/`RESTART`/`REMOVE`, `SYSTEM_CORE_ROLLBACK`, finalize/cleanup).
  And as long as the HELLO name is not authenticated (A-22), any local process can present itself
  as an orchestrator. Deferred with the security package (A-22).
- **Fix:** SY.opa.rules takes CONFIG_CHANGED only from `SY.admin@motherbee`, just like its command
  path and the protocol; SY.config.routes no longer applies lists that arrive through
  CONFIG_CHANGED.

### A-21 ✅ RESOLVED (0.1.50) — Router status without a user policy: `error` at startup, `ok` after a clear

- **What is seen:** in `/hives/{h}/opa/status`, a router that started without a policy reports
  `load_status=1, error`; after a clear, `load_status=0, ok`. In both cases there is no policy and
  the routing is the same (a message with no destination that does not go to the frontdesk comes
  out as `OPA_ERROR`).
- **Cause:** the resolver is born in `OPA_STATUS_ERROR`, and `unload()` leaves it in
  `OPA_STATUS_OK`. It is only informational.
- **Fix (0.1.50):** the resolver is born in the same state a clear leaves (`ok`, no policy).

### A-17 ✅ RESOLVED (0.1.39) — An identity replica that starts without a primary deletes its hive's ILKs

- **What happened:** the replica re-publishes its set of own ILKs every 30 s, and the primary takes
  it as authoritative (it deletes the hive's ILKs that do not come in it). If the worker starts
  with the primary unreachable, in memory it only has its system ILKs: when the connection came
  back it published that, and the primary **deleted the ILKs of the hive's nodes**. Seen on 8.x:
  the primary went from 25 to 24 ILKs, and `AI.vaultprobe@worker1` was left without an identity
  (`ILK_NOT_FOUND` when killing it). It is the case of a full reboot in which a worker comes up
  before the motherbee.
- **Fix:** the own set is not published until it has a base from the primary (the startup full
  sync or the subscription's first snapshot). Loose local deltas still go out.
- **Validated:** the same degraded startup → the replica logs `holding its self-owned snapshot`;
  the primary stays at 25 ILKs, with the probe's.

### A-18 ✅ CLOSED (0.1.50) — config-routes: static routes, VPN and taps

- **The need (operator, 2026-09-30):** static routes are rarely used and can stay local. The VPN
  is going to be used globally (separating tenants, security: a worker1 node talks to a worker2
  one). Taps are the most used tool (echo to other nodes) and have to be global. Decided not to put
  them in OPA: they are fixed in config-routes.
- **How it is today (verified in code, 2026-10-01):**

  | Tool | Loaded | Reaches the rest | Global? |
  |---|---|---|---|
  | Static routes | on one hive | LSA → FIB of every router | yes |
  | Taps | on one hive | LSA; the origin router applies the local ones and the LSA ones, only once | yes |
  | VPN | on one hive | LSA carries them, but `assign_vpn` uses only the local config | **no** |

- **The operator's decision (2026-10-01): the VPN stays per hive.** Each router assigns it with its
  hive's rules; for a VPN between hives, the same rule is loaded on each one (documented in
  `06-regiones.md` §5.1). With that, nothing is left open; *"resolvé los detalles por
  costo/beneficio... prefiero empezar lo próximo con estos temas cerrados"* ("resolve the details
  by cost/benefit... I'd rather start the next thing with these topics closed").
- **Closed in 0.1.50 (`aa206ac`):**
  - One single surface: `/hives/{h}/routes|vpns|taps`. `/routes`, `/vpns` and `/taps` with
    `?hive=` were deleted (nobody used them).
  - Rule 5 of `system.rego` no longer names `SY.config-routes`.
  - The router re-reads the routing config on every heartbeat, with version control (panel D-12):
    a router with no local nodes is no longer left with an old config. Today it did not apply (one
    router per hive).
- **Accepted:**
  - If the hive where a route or a tap was loaded gets cut off, it stops being applied on the
    others during the cut.
  - Taps match by exact name (v1).
  - On a host with several routers, the peer router does not do the taps fanout (same case as
    D-12).
- **Already resolved:**
  - `PUT /config/*`: 0.1.40–0.1.41.
  - The broadcast after each add, and the router's special treatment of CONFIG_CHANGED: 0.1.44.
  - Storage's broadcast: 0.1.41; its CONFIG_RESPONSE: 0.1.45.
  - Hive-scoped config and origin control: 0.1.36 (A-8, A-9).
  - Cross-hive taps with a destination by UUID: July (`18c4dd8`).
- **Accepted:** if a hive is deleted or rebuilt, its routes, taps and VPN are lost (nobody keeps a
  copy).

### A-16 ✅ RESOLVED (0.1.56) — The receive-only mirrors pile up local changes silently

- **What happens:** in a receive-only folder, Syncthing neither propagates nor reverts local
  changes. If something touches the mirror on a spoke, it diverges from the motherbee forever and
  nobody finds out. The orchestrator manages Syncthing but does not look at or revert these
  changes.
- **To discuss:** have the orchestrator revert (`/rest/db/revert`) the local changes of the
  receive-only folders and log it, so that the mirror converges to the origin by itself.
- **Fix (`10c97b1`):**
  - The watchdog, which already read each folder's state on every round, also reads
    `receiveOnlyTotalItems`.
  - If a receive-only folder has local changes, it leaves a WARN and reverts it.
  - It covers the spokes' `dist/` mirrors and the ingress's blob `public/`.
  - On 2026-10-02, the 12 receive-only folders in PROD were at 0.
- **Validated live (0.1.56, 2026-10-03, with the operator's OK):**
  - A test file in worker1's `dist/vendor` raised `receiveOnlyTotalItems` to 1 at 11 s.
  - The watchdog left the WARN and reverted the folder.
  - At 16 s the file was gone, and the counter was back at 0.

---

## B. Infrastructure and tooling — no product code change required

### B-1 🟢 LAB NOTE — The Proxmox API does not allow writing cloud-init *snippets*

- Neither `POST /nodes/{n}/storage/{s}/upload` nor `download-url` accepts `content=snippets`
  (enum: `iso, vztmpl, import`), even though the storage **does** support the type.
- **Impact:** **a template cannot be built 100% through the API**; the operator has to drop the
  file in place once. It is the Level-3 limit of `METHOD.md` §2 in action.
- **Mitigation applied:** the operator wrote it just once; the template ended up with the agent
  baked in, and **the clones no longer depend on the snippet**.

### B-2 🟢 LAB NOTE — `cicustom` replaces Proxmox's user-data (it overrides `ciuser`/`cipassword`)

- When `cicustom=user=...` is used, Proxmox uses that file **instead of** the user-data it
  generates, which is the one that creates the user. Result: the user **is not created**, and
  `add_hive`'s bootstrap would have no one to log in as.
- **Rule for the handbook:** *either* `cicustom` *or* `ciuser`/`cipassword`; **they do not
  coexist**.

### B-3 🟢 The Ubuntu cloud image is qcow2 even though it is published as `.img`

- `download-url` rejects `filename=*.img` (`invalid filename or wrong extension`). It is downloaded
  by renaming it to `.qcow2`.

### B-4 🟢 Ubuntu 24.04 uses **socket activation** for sshd

- `ssh.socket` active, listening on `:22`, `ssh.service` inactive. **It is normal** and does not
  affect `add_hive` (which only needs port 22 answering). Noted so it is not misdiagnosed.

### B-5 🟢 VM operations in Proxmox are serialized by a *lock*

- Launching `resize`/`start` right after a `qmcreate` that imports a large image fails with
  `can't lock file … got timeout`. **Our own mistake, made and corrected.**
- **Rule:** chain operations by **task state** (`/tasks/<UPID>/status` → `stopped OK`), never by
  `sleep`.

### B-7 🟢 The Proxmox guest-agent runs **without `HOME`** defined

- **What happens:** `agent/exec` runs the command without `HOME` in the environment. With
  `bash -lc`, that makes `/root/.profile` evaluate `. "$HOME/.cargo/env"` as `. "/.cargo/env"` →
  an error on **every** command.
- **Real impact (not cosmetic):** that noise on `stderr` **polluted the output of every command**
  and **broke two of our own pollers** that extracted numbers from the output (the message contains
  `line 10`). Two false positives: the problem **was not the VM, it was the helper**.
- **Solution:** invoke `/usr/bin/env HOME=/root /bin/bash -lc '<cmd>'` in the guest-agent helper.
- **Rule for the handbook:** the guest-agent channel **is not a normal login shell** — define
  `HOME` explicitly, and **never parse numbers from an output that can carry stderr**.

### B-8 🟢 Cloning as one user and building as another breaks the Go build (`buildvcs`)

- **What happened:** the repo was cloned as `fluxops` (to use the deploy key), but `build-deb.sh`
  runs as `root`. Git rejects the foreign repo (`fatal: detected dubious ownership in repository`)
  and the Go step fails when stamping the VCS information:
  ```
  error obtaining VCS status: exit status 128
      Use -buildvcs=false to disable VCS stamping.
  ```
  The build got as far as `[2/5] build go` and exited with `rc=1` **without producing a `.deb`** —
  after ~55 min of an already successful Rust compilation.
- **Cause:** our own setup mistake (two different users to clone and to build), **not** a fluxbee
  problem.
- **Solution applied (git's standard one, not a shortcut):**
  `git config --global --add safe.directory /opt/fluxbee` for root.
  *(The `-buildvcs=false` alternative would have hidden the problem and lost the version stamping
  in the Go binaries: it was discarded.)*
- **Rule for the handbook:** **the same user who clones must build**, or declare the repo as
  `safe.directory`. Check it **before** launching a long build.

### B-9 🟢 Proxmox on VMware: **the portgroup security breaks ALL bridged networking of the nested VMs**

- **Symptom:** the VMs with a card on `vmbr0`/`vmbr1` **could not even ARP** their gateway
  (`ARP INCOMPLETE`, 100% packet loss), even though the Proxmox **host** worked perfectly on the
  **same bridge**, with correct IP/routing/carrier on the VM's side.
- **Cause:** the Proxmox is a VMware VM. The portgroups come by default with
  **`Forged transmits: Reject`** (+ `MAC address changes: Reject`, `Promiscuous mode: Reject`),
  which **drops every frame whose source MAC is not the one assigned to the Proxmox's vNIC**. The
  nested VMs transmit with **their own** MAC (`BC:24:11:…`) → VMware throws them away.
- **What made the diagnosis conclusive** (and ruled out fluxbee, the firewall and the
  configuration):

  | Observation | Explanation |
  |---|---|
  | ✅ the Proxmox host works | it uses **its** assigned MAC |
  | ✅ the worker goes out to the internet through `fbint`+SNAT | **routed** traffic → goes out with the **host's MAC** |
  | ❌ a VM on `vmbr0` cannot even ARP | **bridged** traffic → **the VM's MAC** → dropped |

  That is: **the internal SDN network worked precisely because it is routing with NAT, not
  bridging.** That asymmetry was the clue that closed the case.
- **Solution (operator, in ESXi, live and per portgroup):**
  `Promiscuous mode: Accept` · `MAC address changes: Accept` · **`Forged transmits: Accept`** ← the
  one that enables **sending**.
- **Costly operational detail:** there is **one portgroup per physical card**. When it was applied
  only to `nic0`'s, `vmbr0` started working and `vmbr1` **was still broken** — same symptom,
  another card. **It has to be applied to ALL the portgroups** of the nested hypervisor's NICs.
- **Verified after the change:** ingress with **its own public IP `71.182.182.80`, visible from
  the internet** + an internal leg to the mesh, at the same time. Egress going out through its WAN
  leg.
- **For the handbook:** on any prod on VMware, **this is configured BEFORE deploying**; otherwise
  hours are lost diagnosing a network that is flawless on the guest's side.

### B-6 🟢 `build-deb.sh` could produce a truncated `.deb` without failing

- With the disk full, `dpkg-deb` exited with code 0 but wrote a ~1.8 KB package without
  `data.tar`. **Already fixed** (commit `01db2cc`): a space preflight + an integrity check of the
  `.deb` with a loud failure.

### B-10 🟢 The apt repo died with every reboot and looked empty during every publish

- **What happened (two defects in `scripts/apt-repo-publish.sh`):**
  1. `--serve` brought up the server with `systemd-run` → a **transient** unit: it died with every
     reboot of the build box, and its errors were swallowed (`>/dev/null 2>&1 || true`), so a
     re-run could leave the repo down **without warning** (it happened in August, after a VM110
     reboot).
  2. The index was regenerated *in place* (`dpkg-scanpackages > Packages`): during the re-hash of
     every `.deb` (~5.5 min with 33 × ~240 MB) the repo served **0 packages** — a client that ran
     `apt-get update` in that window saw the repo empty.
- **Evidence:** `FragmentPath=/run/systemd/transient/fluxbee-apt.service`; polls during a publish:
  0 packages from 14:47 to 14:52 (logbook 2026-09-25).
- **Solution:** `--serve` writes a **persistent, enabled** unit (idempotent; it replaces an old
  transient one and fails loudly if it does not start); the index is built in `.new` and goes in
  through atomic renames (`Release` last); the `.deb` is copied as `.partial` + `mv`. The
  one-liner prints all the box's IPs.
- **Validated in prod:** VM110 reboot → repo up on its own; a real publish with 32 samples from a
  client → minimum 33 packages.

### B-11 🟢 A character above U+00FF in `agent/exec` jams the guest-agent — it was the "flakiness under load"

- **What happens:** the Proxmox API (Perl) does not encode the `guest-exec` arguments with wide
  characters: a single `—` (U+2014), `→` or `✓` in the command leaves the **qemu-guest-agent
  jammed** — even `guest-ping` times out ("QEMU guest agent is not running") until the agent is
  restarted. Latin-1 characters (`é`, `ñ`, `¿`) go through. `agent/file-write` with `encode=1`
  fails for the same reason (`Wide character in subroutine entry`, without jamming anything). In
  addition, the output comes back with mojibake (UTF-8 bytes read as Latin-1: `—` → `â€”`).
- **Evidence (controlled A/B, VM110):** with the agent healthy, `exec echo acento-é` → OK and the
  agent stays alive; `exec echo guion—largo` → `guest-exec failed - got timeout`, and the next
  `ping` dies. Load (host cpu 0%), disk (34%) and kernel (6.8.0-142 runs fine) were ruled out.
- **Impact:** it explains the "flaky qga" episodes of the whole August campaign (the agent's
  commands carried `—`, `→`, `✓` in the `echo`s) — **the cause was not the load**. Each one cost
  reboots/resets of prod VMs.
- **Solution (in `lab/pve.py`):** a non-ASCII argv travels in base64, and an ASCII bootstrap
  decodes it and runs the exact original argv; `push` encodes the bytes locally (`encode=0`, which
  also works for binaries); the output is de-mojibaked. Along the way, the **B-7** solution
  (`env HOME=/root`), which was not in the helper, was applied. Validated live: accents/`—`/`✓`,
  exit codes propagated, the agent survives.

### B-12 🟢 All the PROD VMs had a pending reboot for kernel + libc6 — applied

- **What was observed:** `unattended-upgrades` installed kernels 6.8.0-137…142 and libc6 on the
  VMs; until they reboot, they stay on the old kernel (fb-egress: runs 6.8.0-136,
  `reboot-required`). Today's VM110 reboot activated **6.8.0-142** without problems (guest-agent,
  network and repo OK).
- **Resolution (2026-09-25; the operator delegated the infra: "hacé lo que creas conveniente" ("do
  whatever you think is best")):**
  - Reboot one VM at a time, with a prior snapshot `pre-kernel-reboot-20260925`, in the order
    egress → worker1 → ingress → mb.
  - All 4 ended up on **6.8.0-142**, without `reboot-required`, with identical IPs and routes and
    0 `failed` units. Mesh 4/4, 9/9 runtimes, public 200.
  - The spokes withstood the hub going down without restarting anything.
  - The boot-time load of the egress's NAT (F17) was validated in PROD: the ruleset came back
    identical.
  - Detail in the 2026-09-25 logbook.

### B-13 ✅ RESOLVED (2026-10-02) — The PROD VMs did not start on their own after a host reboot (`onboot` not defined)

- **What happens:** `onboot` is not defined on fb-mb, fb-worker1, fb-ingress, fb-egress or
  fb-build. If the host reboots (power cut, update), **PROD stays powered off** until someone
  starts the VMs by hand.
- **Evidence:** it happened on 2026-07-30 ("the 4 VMs were left powered off. I started the 4 VMs").
  The HANDBOOK recorded it as "hypervisor reboot: validated", but what was validated was that the
  mesh re-forms, not that the VMs start on their own.
- **Proposed solution:** `onboot=1` on 100, 101, 102, 103 and 110 (fb-build, because of the
  decision to keep it always on). No start order, because the simultaneous start of the 4 is what
  was validated on 30/07. `PUT /nodes/pve/qemu/<id>/config onboot=1`, or in the GUI: VM → Options
  → *Start at boot*.
- **Status:** resolved. The operator set `onboot=1` on 100, 101, 102, 103 and 110; verified
  through the API on 2026-10-02. It remains to be seen live on the next host reboot.

### B-14 🟡 LAB ONLY, OPEN — Slow boot of the PROD VMs (userspace from 53 s to 1 min 48 s)

- **What was observed (reboots of 2026-09-25):** userspace of ingress 53 s, egress 60 s, worker1
  89 s, mb 108 s (fb-build: 31 s).
- **What it is not:**
  - It is not the journal flush, even though `systemd-analyze blame` puts
    `systemd-journal-flush.service` at the top (mb 74 s, worker1 58 s): journald reports that the
    flush itself took ~1 s (mb: 1.4 s for 752 entries).
  - Nor is it the size of the journal: mb 1.8 G; egress 109 M and it took 27 s; fb-build 112 M and
    0.6 s.
- **Clue:** in mb's log, unrelated jobs (flush, apparmor, binfmt) finish at the same instant
  (94.6 s). Something common holds them back.
- **Impact:** it adds 1 to 1.5 min to every recovery. With mb it is ~3 min 20 s from the reboot to
  the 9 runtimes; do not diagnose before that.
- **Untested candidate:** the serial console (`console=ttyS0` with `serial0=socket` and
  `vga=serial0`).
- **Pending:** a diagnosis (`systemd-analyze plot`, what holds those jobs back) before touching
  anything.

### B-15 ✅ RESOLVED (2026-10-02) — Memory headroom of the PROD host with fb-build always on

- **What was observed:** 26.0 G configured in the powered-on VMs over 27.4 G physical, with no
  ballooning. Real usage stable at ~22 G (a peak of 81% during the week, 0 swap). mb reaches 5.8 G
  of its 10 G; fb-build, 7.6 G of 8 G.
- **Risk:** if mb were to use its 10 G while fb-build compiles, the host runs out of headroom, and
  the first thing to go down is a QEMU process.
- **Options (the operator's decision):**
  - Ballooning with a minimum on fb-build (e.g. `balloon=2048`): Proxmox reclaims memory from it
    when the host goes over 80%.
  - Lowering its RAM outside of builds.
- **Status:** resolved. The operator enabled ballooning on fb-build with a 2 GiB minimum
  (`balloon=2048`); verified through the API on 2026-10-02. To give the host more RAM it has to be
  shut down, because it is a VMware VM.

### B-16 ✅ RESOLVED (2026-10-03) — `ops.py deploy` reported spokes as updated when they were not

- **What happened (0.1.56 deploy):**
  - The core update fired a few seconds after the motherbee's admin came back, before its router
    had the spokes back. The three requests failed with `TRANSPORT_ERROR` and none arrived: no
    spoke service was restarted.
  - Even so, the deploy said "worker1 on 0.1.56". `ops.py` judged by `core.manifest_hash` and the
    component versions in `/versions`, which on a spoke are what dist brought. Syncthing updates
    them before any update. What is installed is in `core.installed`, and it was still on the
    0.1.55 hash.
- **How it was seen:** the binaries check after the deploy showed `not_restarted` on every unit of
  the spokes.
- **Fix (`lab/ops.py`):**
  - `core_of` uses the installed hash when the hive reports it, just like the `/versions`
    comparison (X1). `ops versions` says where each hash comes from.
  - `deploy` waits for each spoke's orchestrator to answer before the update, and retries a
    request that did not arrive up to 3 times.
  - The update was sent again by hand, and the three spokes finished fine (U-8b validated).
- **To consider:** in a spoke's `/versions`, `core.manifest_hash` and the component versions
  describe what is available, not what is running. The fleet comparison already solves it, but
  read on its own it is confusing.

### B-17 ✅ RESOLVED (2026-10-29, 94% → 50%) — The PROD host's disk pool reached 94%, and the VM disks never gave space back

- **What was observed (API reads and `df` in the VMs, 2026-10-29):**
  - `local-lvm` (LVM-thin, 186.4 GiB) holds every VM disk: 175.1 GiB used (93.9%), 11.3 GiB
    free. The volume group `pve` has 16 GiB more, not allocated to the pool.
  - Its history (the storage's RRD): 56% on 2026-09-29, 80% on 10-10, 94% on 10-25. Since 10-22
    it moves between 92.6% and 94.5%: each deploy drops the oldest snapshots and takes new ones.
  - What the guests use: fb-mb 7.5 G, worker1 3.1 G, ingress 2.9 G, egress 2.7 G, fb-build 65 G.
    The pool holds about twice that.
- **Why:**
  - **No VM disk has `discard=on`** (100–104, 110 and the template 9000). The blocks a guest
    frees never go back to the pool, so each volume grows toward its full size:
    60 + 30 + 20 + 20 + 20 + 80 + 20 = 250 GiB, on a 186 GiB pool. Every clone of the template
    inherits it.
  - **fb-build keeps the snapshot `pre-lan-nic-20260925`,** taken before its second NIC was added.
    Every block a build has rewritten since then is held by it.
  - **fb-build stores each `.deb` twice:** `/opt/fluxbee/dist` (21 G) and the apt repo
    `/var/lib/fluxbee-apt` (21 G, all 90 versions).
- **Risk:** when an LVM-thin pool fills, every VM on it fails its writes at once: the four hives
  and the build box.
- **What was done (2026-10-29, 15:05–15:28; the operator handed the host to the agent that day,
  METHOD §2):**
  1. **`discard=on` on every VM disk** (100–103, 110) and on the template 9000, so every clone is
     born with it. A running VM takes it at its next start: each one was rebooted through
     Proxmox, one at a time (fb-build, egress1, worker1, ingress1, motherbee), and `fstrim -av`
     ran inside. The guests' weekly `fstrim.timer` was already on: until now QEMU dropped the
     discards.
  2. **The apt repo keeps the newest 10 versions, and `dist/` only the `.deb` just published**
     (`ops publish`, commit `71620322`): 80 versions and 90 `.deb`s gone, fb-build from 65 G to
     24 G used.
  3. **One snapshot per VM** (the operator's rule, lowered from 3): the hives kept
     `pre-wfcase0-0-1-90`, the backup of 0.1.90, and their 8 older snapshots were deleted.
  4. **Memory: a balloon floor on the hives** (motherbee 4 GiB, worker1 2 GiB, ingress1 and
     egress1 1 GiB; fb-build already had 2 GiB, B-15). The guests' processes use 0.4–0.8 GiB
     each, and the rest of their RAM is page cache that the host could not take back. Now
     Proxmox reclaims it when the host goes over 80%.
  5. **With the operator's explicit OK** (Claude Code's permission classifier had refused both):
     fb-build got a fresh snapshot, `backup-20261029`, and its `pre-lan-nic-20260925` was
     deleted: it held 35.6 GiB. VM 104 `fb-worker2` was destroyed with its volumes; a gate that
     needs a throwaway worker clones one from the template.
- **The pool:** 93.9% → 87.7% (fb-build trimmed) → 81.8% (the retention) → 71.2% (the hives'
  snapshots) → 52.1% (fb-build's old snapshot) → 50.1% (VM 104): 93.5 of 186.4 GiB used, 93.0
  GiB free. Each reboot was checked with `ops health` and `ops versions` (0.1.90 everywhere, no
  failed unit, the edge certificate and the web door fine).
- **What comes back by itself:** each hive's one snapshot, `pre-wfcase0-0-1-90`, still holds
  most of what its guest trimmed (motherbee 49.6 GiB issued, worker1 25, ingress1 15.5, egress1
  15.7). It comes back when the next deploy replaces that snapshot.
- **A second environment fits:** half the pool is free, and the balloon floors leave room in
  memory without new hardware.

### B-18 ✅ RESOLVED (2026-11-12, the template 9000 rebuilt: a fresh clone's cloud-init 661 s → 60 s) — The template 9000 was stale: a fresh clone upgraded its packages for about 11 min at its first boot

- **What was observed (0.1.137's gate, 2026-11-12):** VM 104, cloned from the template 9000 for a
  throwaway worker, spent about 674 s of its first boot in cloud-init's package upgrade, which
  `add_hive`'s settle wait (15 min at most, A-5) sat out: the join of `gate0237i` took 773 s in
  all, where 0.1.118's gate joined a fresh clone in 127 s. The next fresh clone joined as
  `gate0237j` in 829 s.
- **Impact:** every gate that joins a throwaway worker waits about 11 minutes more, and its join's
  timings measure the template, not the join.
- **To do:** refresh the template (`lab/template-prep.sh`), so that a clone does not start with a
  backlog of package upgrades.
- **Resolved (2026-11-12, the operator: *"rehace el template 9000, no podemos esperar 11 min por cada
  instalación cuando se resuelve corriendo todo en 11 min una vez"*):** the template 9000 (`fb-template`,
  8.x, Proxmox 9.2.2) rebuilt by `lab/template-rebuild.py`, a valid template at every moment: a clone
  (9001) ran its first-boot upgrade (661 s, 17:16–17:28Z), `dist-upgrade` found nothing more (2
  packages phased), a reboot onto kernel 6.8.0-149, `lab/template-prep.sh`, then 9001 a template, the
  old 9000 destroyed, 9001 cloned onto 9000 and made a template, 9001 destroyed (17:33Z). A check
  clone with Proxmox's default upgrade on: cloud-init done at +60 s, 2 packages left (phased), its
  own machine-id and SSH host keys. `lab/pve.py create` keeps Proxmox's default (`--ciupgrade 1`;
  `0` skips the upgrade). Rebuild the template again when a clone's first boot grows long:
  `python3 lab/template-rebuild.py --env scratchpad/pve.env --pve-host 192.168.8.207 all`.

---

## How this document is used

1. During the deployment: **findings are added, not fixed**.
2. At the end: section **A** is reviewed with the operator, and the **code-change plan** comes out
   of it (what changes, why, in what order, and what stays as a design decision). Since
   2026-11-04 that plan is the board at the top, by track, with the sprint; each open item names
   its track after its id. [`PENDING-BUGS.md`](PENDING-BUGS.md) keeps the plan of August.
3. Section **B** feeds `HANDBOOK.md` (recipes) and, where it applies, the infra scripts.
