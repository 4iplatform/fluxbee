# Auditoría — SY.timer + WF (engine wf-generic + sy-wf-rules) — 2026-07-23

Objetivo: (1) ¿SY.timer dispara bien lo recurrente? (2) ¿al WF le faltan funciones para workflows básicos? Meta: un WF que cada 10 min llama a un nodo AI alternando A/B, corriendo en la infra. Método: 6 lentes de auditoría → verificación adversarial → síntesis (45 agentes; 65 findings sobrevivieron, 0 refutados). Nota: las últimas ~14 verificaciones no corrieron por el límite de gasto de la cuenta vieja; la síntesis igual incorporó lo verificado.

## Veredicto de objetivo: **buildable-as-is**

## SY.timer — salud
SY.timer is a solid, drift-free foundation for a 10-minute cadence. Recurring timers are 5-field cron specs (robfig/cron); both the initial arm and every re-arm call schedule.Next() against absolute wall-clock (scheduler.go:394-420, 208-235), so per-fire jitter never accumulates — '*/10 * * * *' fires on the :00/:10/:20 grid indefinitely. Firing is at-most-once (emit-then-persist; a crash between emit and persist re-advances the row to the next future slot on replay, not a re-emit), restart-safe (replayPending recomputes the next future fire, so no catch-up storm), and self-healing against stale heap entries via a per-UUID lock + fire_at>now re-check. Ownership is strict and router-stamped (create/cancel/reschedule owner-gated; PURGE_OWNER reserved to local SY.orchestrator). The real caveats are minor and by-design: (1) TIMER_FIRED is a best-effort fire-and-forget unicast with no ack/retry — a tick is silently lost if the target WF node is down at fire time, but the next tick 10 min later is unaffected (self-correcting for an exerciser); (2) missed_policy is accepted/stored on recurring timers but is a no-op on replay (no downtime catch-up); (3) recurring timers are not editable in place (cancel+recreate, use a stable client_ref for idempotency); (4) a reschedule-time cron/tz error would silently halt the timer until node restart, but this path is effectively unreachable for a plain UTC */10. Net: the timer layer meets the cadence, persistence, and ownership requirements cleanly.

## WF — salud
The WF engine's core machinery is sound — atomic act-then-persist state commits (CommitInstanceMutation, WAL), boot recovery that rehydrates running instances and reconciles/synthetically re-fires past-due timers, terminal-only GC (7-day retention), CEL guards that are safe-by-default (compiled once, 10ms timeout, fail-to-false), $ref payload templating, and async request/reply correlation by meta.thread_id=instance_id. All six primitives the goal needs exist and are exercised by the wf.invoice example. But the every-10-min cadence cannot be expressed declaratively: WF's schedule_timer is one-shot only (no cron field in ActionDefinition; TimerSender omits the SDK's ScheduleRecurring), and a TIMER_FIRED that doesn't resolve to an already-running instance is dropped as an 'orphaned timer' — never spawns one (correlate.go:83-95), diverging from spec §11.2. So the only viable shape is ONE long-lived self-rescheduling instance, seeded by a non-timer kickoff message (there is no WF_START verb — an operability gap between 'package deployed' and 'instance running'), that re-arms a one-shot each cycle and alternates A/B via a state counter + two guarded transitions (send_message target is a static literal, not $ref-resolvable). Several authoring footguns must be gotten right or the loop silently hangs: the AI send MUST set meta.type='user' (default 'system' is diverted to the AI control plane and never answered) with a text/v1 body, and the reply transition MUST match the SAME msg name it sent (the AI chat reply echoes the request msg name — there is no *_RESPONSE rename). One genuine recovery defect: if a one-shot fires while wf.engine is down and SY.timer stays up, recover.go Case C deletes the local index with no synthetic fire, permanently stranding the ticker (medium). Lesser risks: integer state counters degrade to float64 on restart (breaks % guards — use a bool toggle), no built-in AI-reply timeout (author must add a guard timer), per-instance audit log capped at 100 FIFO, and unbounded AI-side thread memory for a constant thread_id. No code changes are strictly required to build the goal, but the definition must be authored with all of the above in mind.

## Checklist de capacidades para la meta

| # | Capacidad | Estado |
|---|-----------|--------|
| | [1] Fire on a ~10-minute cadence, durable across restarts | **present** |
| | [2] Send a request to an AI node and receive/branch on its async reply | **present** |
| | [3] Alternate/intermittently pick AI node A vs B | **present** |
| | [4] Long-lived instance survives wf.engine / SY.timer restarts | **partial** |
| | [5] Observe that it fired / that A/B replied | **partial** |
| | [6] Deploy the workflow onto the infra and start it | **partial** |

_Evidencia por capacidad:_
- **[1] Fire on a ~10-minute cadence, durable across restarts** — SY.timer native recurring cron is drift-free and restart-safe (scheduler.go:208-235,394-420,80-107). Inside WF, cadence is emulated by a self-rearming one-shot each cycle (actions.go:155-205, parseWorkflowDuration '10m'=600s>=60s min); recover.go re-injects a past-due tick on restart. WF has no native recurring action but does not need one for the goal.
- **[2] Send a request to an AI node and receive/branch on its async reply** — execSendMessage sets meta.thread_id=instance_id (actions.go:126); ai-generic replies via build_reply_message_runtime_src which clones meta and preserves top-level thread_id+msg (message.rs:63-72, VERIFIED); correlate.go:98-104 matches the reply back to the instance. Contract: send action must set meta.type='user' + text/v1 body, and the reply transition must event_match the SAME msg name it sent. src_ilk is NOT required on the chat path (warn-only, ai_node_runner.rs:1087-1092, VERIFIED — refutes the prior blocker claim).
- **[3] Alternate/intermittently pick AI node A vs B** — set_variable writes a CEL result into persisted state_json (actions.go:283-306); two guarded transitions (guard e.g. state.use_b) each with a hard-coded target implement A/B. Caveat: send_message target is a static literal validated at publish (validate.go:200) — NOT $ref/state-resolvable — so A/B is a two-branch state-machine shape, not a computed target.
- **[4] Long-lived instance survives wf.engine / SY.timer restarts** — recover.go:25-187 reloads running instances and reconciles timers; GC never touches running instances (gc.go:30-39). REAL gap: recover Case C (recover.go:149-153) permanently strands the instance if a one-shot fired while wf.engine was down and SY.timer already dropped it from 'pending' — no synthetic fire, loop dies silently. Mitigated by an author-added reply/watchdog timer, but a real medium-severity strand.
- **[5] Observe that it fired / that A/B replied** — WF_GET_INSTANCE/WF_LIST_INSTANCES + wf-rules status expose the row and recent transitions; SY.timer emits timer_fired structured logs. But the per-instance WF audit log is FIFO-capped at 100 entries (store.go:17,384-403), so a perpetual ticker only shows ~the last 15-20 cycles; long-horizon run history must come from a state counter + SY.timer/AI logs.
- **[6] Deploy the workflow onto the infra and start it** — sy-wf-rules apply(auto_spawn=true, tenant_id) compiles+validates CEL, publishes the package, and RunNode-spawns WF.<name>@hive on the wf.engine runtime (deploy.go:59-162); bare run_node is refused with WF_RUNTIME_PACKAGE_REQUIRED (sy_orchestrator.rs:10955). GAP: no first-class instance kickoff — after spawn the operator must manually send one non-control, schema-matching message to birth the looping instance (no WF_START verb). Prereqs: wf.engine published, tenant_id on first deploy, AI.A/AI.B Configured with provider+key.

## Gaps rankeados

### G1 — [medium] (integration)
A recurring/external SY.timer cannot spawn or trigger a NEW WF instance: CorrelateAndDispatch drops any TIMER_FIRED that doesn't resolve to an already-running instance as an 'orphaned timer' (correlate.go:83-95), diverging from spec §11.2. Rules out the intuitive 'cron pokes WF every 10 min -> fresh run' design; forces one perpetual self-looping instance.

**Fix mínimo:** In correlate.go, when a TIMER_FIRED has no matching instance, fall through to createAndDispatch (validate payload against input_schema) gated by a definition flag such as spawn_on_timer=true. Or simply document/accept the self-loop pattern for now (goal is achievable without the fix).

### G2 — [medium] (integration)
Silent-hang footgun: WF send_message defaults msg_type='system' (diverted to the AI control plane, never answered) and the AI chat reply echoes the REQUEST msg name (no *_RESPONSE rename). If the author copies the wf.invoice REQUEST->RESPONSE convention or forgets meta.type='user', the loop stalls every cycle with no error.

**Fix mínimo:** No code change needed — author the send action with meta.type='user' + a text/v1 payload and set the reply transition's event_match.msg to the exact msg name sent (e.g. both 'AI_TICK'). Provide a canonical AI-call example in wf-rules-quickstart to prevent the mistake.

### G3 — [medium] (wf)
Recovery strand: if a one-shot WF timer fires while wf.engine is down and SY.timer stays up, SY.timer delivers best-effort (lost) then drops the fired row from 'pending'; on WF reboot recover.go Case C (recover.go:149-153) deletes the local index with NO synthetic fire and NO reschedule, permanently and silently stalling the ticker. Small window, permanent when hit.

**Fix mínimo:** In recover Case C, consult the locally-stored expected fire_at: if it is <= now, inject a synthetic TIMER_FIRED (as Case B does) instead of just deleting the index; belt-and-suspenders is an author-added reply/heartbeat timer.

### G4 — [medium] (integration)
No built-in AI-reply timeout: send_message is fire-and-forget with no response deadline; a lost/never-arriving AI reply leaves the instance in its wait state indefinitely (no watchdog reaps running instances).

**Fix mínimo:** Author must schedule_timer a guard timeout alongside every AI send and add a TIMER_FIRED transition in the wait state (the §11 pattern); on non-ok reply (payload.ok==false) branch to retry/error. No engine change required, but must be built.

### G5 — [low] (wf)
No first-class instance kickoff / no WF_START verb: after publish+spawn the operator must manually emit one non-control, input_schema-matching message to WF.<name>@hive to birth the looping instance. Undocumented last hop.

**Fix mínimo:** Document the kickoff message (permissive/empty input_schema so any trigger works), or add a bootstrap-on-spawn flag / WF_START verb to sy-wf-rules to remove the manual step.

### G6 — [low] (wf)
WF has no native recurring-timer action (schedule_timer is one-shot only; TimerSender omits the SDK's ScheduleRecurring; ActionDefinition has no cron field), so cadence is emulated by re-arming a one-shot each cycle, which drifts by AI round-trip + processing latency and requires the instance to never terminate.

**Fix mínimo:** Optional: add cron_spec/cron_tz to ActionDefinition, add ScheduleRecurring to TimerSender+SDKTimerSender, branch in execScheduleTimer. Workaround: re-arm the next tick in the SAME transition that fires the AI request (before the send), keeping each period ~10m regardless of AI latency; or pass an absolute fire_at for grid alignment.

### G7 — [low] (wf)
Integer state counters degrade to float64 after a wf.engine restart (store.go:653-662 json.Unmarshal without UseNumber), and cel-go has no double overload for %, so a 'state.count % 2 == 0' A/B guard silently returns false after the first restart and alternation wedges.

**Fix mínimo:** Alternate with a BOOLEAN toggle (set_variable value '!state.use_b', seeded false) which round-trips JSON losslessly, instead of an int counter + modulo. Engine-side fix: use json.Decoder.UseNumber() in unmarshalJSON.

### G8 — [low] (wf)
Static send_message target: action.Target is a literal L2 name validated at publish (validate.go:200) and used verbatim; only payload is $ref-resolved. A/B cannot be a computed/dynamic target — it must be two guarded branches with hard-coded targets (more verbose than expected).

**Fix mínimo:** Accept the two-branch shape (works today), or run action.Target through Resolve() to support a $ref/CEL target and collapse A/B to one send_message + one set_variable.

### G9 — [low] (integration)
No per-request correlation id — correlation granularity is exactly thread_id=instance_id, and matchesEvent keys only on msg name+type. A single instance fanning out to A and B simultaneously cannot tell the two replies apart (both share thread_id + echoed msg name); the second is dropped as 'unhandled event'. Only affects the elaborate parallel fan-out/compare design.

**Fix mínimo:** For the simple alternate-A-then-B design, no fix needed. For a true parallel join, serialize the two calls across two states, or add a request_id threaded through meta and matched on the reply.

### G10 — [low] (wf)
Observability limits: per-instance audit log is FIFO-capped at 100 entries (store.go:17), and the CEL 'event' map omits the sender identity (messageToMap, instance.go:378-396) so a guard cannot verify which AI node actually replied. Also a constant thread_id makes AI-side conversation memory grow unbounded for a perpetual ticker.

**Fix mínimo:** Track cycle count / last-A-or-B in state_json for long-horizon visibility; for fresh cheap AI calls override the outgoing payload thread_id per tick (instance_id + cycle counter) instead of the constant default.

## Preguntas de diseño (charlar antes de construir)

- Architecture: confirm the ONLY buildable shape is ONE long-lived self-rescheduling WF instance (seeded once by a kickoff message), not a recurring SY.timer that spawns a fresh run each tick (that pattern silently drops — G1). Are you OK with a perpetual instance that must never reach a terminal state?
- A/B selection: strict alternation (A,B,A,B via a bool toggle — restart-safe, avoids the float64 modulo bug G7) vs intermittent/random vs weighted? And do you want to record which node was targeted in state (you cannot verify reply origin in a guard — G10)?
- Cadence tightness: is ~10 min (drifts by AI round-trip if re-armed after the reply) acceptable, or do you want each period held to ~10m by re-arming the tick in the same transition that fires the AI request, before the send (G6)? Do we want to invest in a native WF recurring action, or accept the self-loop?
- Reply handling: how should a lost/failed AI reply be handled — add a per-tick timeout timer + retry (N attempts) then continue, or just skip to the next tick? There is no built-in deadline; without a guard timer a dropped reply freezes the ticker (G4).
- Conversation memory: should each tick be a fresh, cheap AI call (override thread_id per cycle) or accumulate one growing conversation per node (constant thread_id -> unbounded context/cost — G10)?
- Elaboration scope: do you want the richer fan-out-to-both-A-and-B-and-compare flow? If so we must serialize the two calls across states because there is no per-request correlation id (G9). The branch+retry+timeout elaboration is fully supported.
- Deployment/kickoff: who sends the one-time kickoff message after apply(auto_spawn,tenant_id), and on which hive? Should we document a manual kickoff or add a bootstrap-on-spawn/WF_START path (G5)? Confirm AI.A and AI.B are Configured with provider+API key before starting.
- Do you want the medium recovery strand (G3, ticker permanently stalls if wf.engine restart straddles a tick) fixed in recover.go before running this long-term, or is an operator/heartbeat re-trigger acceptable for now?
