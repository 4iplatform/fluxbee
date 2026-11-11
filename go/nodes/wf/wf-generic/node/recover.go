package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"strings"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// timerReconcileRetryInterval is how often a node whose timers are not reconciled tries again: SY.timer
// did not answer, or the timer index could not be read.
const timerReconcileRetryInterval = 30 * time.Second

// RecoverOptions controls boot-time reconciliation behaviour.
type RecoverOptions struct {
	// OwnerL2Name is this WF node's own L2 name, used to filter SY.timer results.
	OwnerL2Name string
}

// Recover brings the engine back at start, before the receive loop:
//
//  1. every running instance, on the definition it was born with (by its hash; one that is not
//     kept, or no longer loads, fails the instance: causeDefinitionUnavailable; so does a current
//     state the definition does not have: causeStateNotInDefinition), on its thread, with the
//     messages it kept;
//  2. the internal events a crash left queued;
//  3. the inbound events a crash left unfinished, run again in the order they came: an instance's
//     event, or a creation, under the instance_id it had (at least once), when the current
//     definition still starts from its sender; a creation whose thread a running instance holds
//     (a later message started it while the creation's commit had failed) is that instance's event;
//     a creation that starts nothing (it has no thread, the current definition does not start from
//     its sender, or its input_schema refuses it) is dropped and answered as live dispatch answers
//     it, NOT_TAKEN;
//  4. the timer index against SY.timer. When SY.timer does not answer, or the index cannot be read,
//     the node starts anyway, DEGRADED (timersPending), and the receive loop reconciles again until
//     it can: a fire dropped while the node could not take it is given then;
//  5. the definitions no running instance uses are dropped.
//
// Each event a start runs again (2 and 3) counts its attempt, in its own commit, before it runs: one
// already run at maxReplayAttempts starts is a poison event that takes the node down, and is not run
// again; its instance fails (causeReplayExhausted).
//
// Only a failing store is an error, in 1 and 3: one in 2, the drain of an instance's internal events,
// is logged and stops nothing (drainRecovered), the events left queued for the instance's next event
// or the next start; one in 4 leaves the timers pending (timersPending).
func (rt *NodeRuntime) Recover(ctx context.Context, opts RecoverOptions) error {
	if err := rt.loadRunningInstances(ctx); err != nil {
		return err
	}
	for _, inst := range rt.Registry.All() {
		rt.drainRecovered(ctx, inst)
	}
	if err := rt.replayInbound(ctx); err != nil {
		return err
	}
	if err := rt.ReconcileTimers(ctx, opts); err != nil {
		rt.timersPending = err
		log.Printf("recover: timers not reconciled, the node runs DEGRADED until they are: %v", err)
	}
	rt.pruneDefinitions(ctx)
	log.Printf("recover: complete, %d running instances", rt.Registry.Count())
	return nil
}

// loadRunningInstances puts every running instance in the registry, on the definition it was born
// with.
func (rt *NodeRuntime) loadRunningInstances(ctx context.Context) error {
	rows, err := rt.Store.LoadRunningInstances(ctx)
	if err != nil {
		return fmt.Errorf("recover: load running instances: %w", err)
	}
	definitions := map[string]*WorkflowDefinition{rt.DefHash: rt.Def}
	for _, row := range rows {
		def, err := rt.definitionFor(ctx, row.DefinitionHash, definitions)
		if err != nil {
			cause := fmt.Errorf("%s: %v", causeDefinitionUnavailable, err)
			if ferr := failStoredInstance(ctx, rt.ActCtx, row.InstanceID, row.CurrentState, cause); ferr != nil {
				return fmt.Errorf("recover: %w", ferr)
			}
			continue
		}
		if !definitionHasState(def, row.CurrentState) {
			cause := fmt.Errorf("%s: state %q is not in the instance's definition", causeStateNotInDefinition, row.CurrentState)
			if ferr := failStoredInstance(ctx, rt.ActCtx, row.InstanceID, row.CurrentState, cause); ferr != nil {
				return fmt.Errorf("recover: %w", ferr)
			}
			continue
		}
		kept, err := rt.Store.ListKeptMessages(ctx, row.InstanceID)
		if err != nil {
			return fmt.Errorf("recover: read the messages instance %s kept: %w", row.InstanceID, err)
		}
		inst, err := FromRow(row, def, kept)
		if err != nil {
			log.Printf("recover: skip instance %s: %v", row.InstanceID, err)
			continue
		}
		// The store gives the instances in the order they started: the first one on a thread keeps
		// it, and a later one on the same thread fails, loudly.
		if err := rt.Registry.Add(inst); err != nil {
			log.Printf("recover: REFUSED instance %s: %v", row.InstanceID, err)
			cause := fmt.Errorf("%s: %v", causeThreadTaken, err)
			if ferr := failStoredInstance(ctx, rt.ActCtx, row.InstanceID, row.CurrentState, cause); ferr != nil {
				return fmt.Errorf("recover: %w", ferr)
			}
		}
	}
	log.Printf("recover: loaded %d running instances on %d definitions", rt.Registry.Count(), len(definitions))
	return nil
}

// definitionFor is the definition kept under hash, loaded (and validated) once per start.
func (rt *NodeRuntime) definitionFor(ctx context.Context, hash string, loaded map[string]*WorkflowDefinition) (*WorkflowDefinition, error) {
	if def, ok := loaded[hash]; ok {
		return def, nil
	}
	if hash == "" {
		return nil, fmt.Errorf("the instance records no definition")
	}
	row, ok, err := rt.Store.GetDefinition(ctx, hash)
	if err != nil {
		return nil, fmt.Errorf("read definition %s: %w", hash, err)
	}
	if !ok {
		return nil, fmt.Errorf("definition %s is not kept", hash)
	}
	def, err := LoadDefinitionBytes([]byte(row.DefinitionJSON), "", rt.ActCtx.Clock)
	if err != nil {
		return nil, fmt.Errorf("definition %s no longer loads: %w", hash, err)
	}
	loaded[hash] = def
	return def, nil
}

func definitionHasState(def *WorkflowDefinition, name string) bool {
	for i := range def.States {
		if def.States[i].Name == name {
			return true
		}
	}
	return false
}

func (rt *NodeRuntime) drainRecovered(ctx context.Context, inst *WFInstance) {
	inst.Lock()
	defer inst.Unlock()
	err := rt.protect(ctx, inst, sdk.Message{}, takenEvent{}, func() error {
		return inst.drainLeftoverInternalEvents(ctx, rt.ActCtx)
	})
	if err != nil {
		log.Printf("recover: drain internal events for %s: %v", inst.InstanceID, err)
	}
	rt.settle(ctx, inst)
}

// replayInbound runs again every inbound event a crash left unfinished, in the order they came.
func (rt *NodeRuntime) replayInbound(ctx context.Context) error {
	rows, err := rt.Store.ListInboundEvents(ctx)
	if err != nil {
		return fmt.Errorf("recover: list inbound events: %w", err)
	}
	for _, row := range rows {
		// drop reports whether the event left the journal.
		drop := func(why string) bool {
			log.Printf("recover: inbound event %d (%s for %s) dropped: %s", row.EventID, row.Kind, row.InstanceID, why)
			if err := rt.Store.DeleteInboundEvent(ctx, row.EventID); err != nil {
				log.Printf("recover: drop inbound event %d: %v", row.EventID, err)
				return false
			}
			return true
		}
		var msg sdk.Message
		if err := json.Unmarshal([]byte(row.MessageJSON), &msg); err != nil {
			drop(fmt.Sprintf("not a message: %v", err))
			continue
		}
		// refuse drops a creation that starts nothing, and then answers it as live dispatch does: a
		// NOT_TAKEN to whoever answerNotTaken says, never to a sender the current definition does not
		// involve (FINDINGS A-123). The answer goes once the drop went, so a later start does not send
		// it again.
		refuse := func(reason, why, detail string) {
			if !drop(why) || !rt.Def.Involves(msg.SourceL2Name()) {
				return
			}
			if err := answerNotTaken(rt.ActCtx, msg, reason, detail); err != nil {
				log.Printf("recover: inbound event %d: %v", row.EventID, err)
			}
		}
		if row.Attempts >= maxReplayAttempts {
			rt.giveUpInbound(ctx, row, msg)
			continue
		}
		if err := rt.Store.CountInboundAttempt(ctx, row.EventID); err != nil {
			return fmt.Errorf("recover: count the attempt of inbound event %d: %w", row.EventID, err)
		}
		log.Printf("recover: running again inbound event %d (attempt %d): %s msg=%q for instance %s trace_id=%s",
			row.EventID, row.Attempts+1, row.Kind, rawMsgName(msg), row.InstanceID, msg.Routing.TraceID)
		switch row.Kind {
		case inboundCreate:
			if _, err := rt.Store.GetInstance(ctx, row.InstanceID); err == nil {
				drop("its instance exists")
				continue
			} else if !errors.Is(err, errInstanceNotFound) {
				return fmt.Errorf("recover: read instance %s: %w", row.InstanceID, err)
			}
			// A creation journaled by an engine of before FINDINGS A-135 may have no thread: a WF
			// never makes one up, so it starts nothing.
			threadID := derefStringPtr(msg.Meta.ThreadID)
			if strings.TrimSpace(threadID) == "" {
				refuse(notTakenNoThread, "it has no thread, and a WF never makes one up", noThreadDetail)
				continue
			}
			// A running instance holds the message's thread: one a later message started while this
			// creation's commit had failed. The message is that instance's event, as live dispatch
			// takes it, and that instance's commit consumes it.
			if inst, held := rt.Registry.GetByThread(threadID); held {
				if !inst.def.Involves(msg.SourceL2Name()) {
					drop(fmt.Sprintf("instance %s holds its thread, and its definition does not involve %q", inst.InstanceID, msg.SourceL2Name()))
					continue
				}
				if err := rt.Store.RetargetInboundEvent(ctx, row.EventID, inst.InstanceID); err != nil {
					return fmt.Errorf("recover: give inbound event %d to instance %s: %w", row.EventID, inst.InstanceID, err)
				}
				log.Printf("recover: inbound event %d is an event of instance %s, which holds its thread", row.EventID, inst.InstanceID)
				if err := rt.runEvent(ctx, inst, msg, row.EventID); err != nil {
					log.Printf("recover: run inbound event %d on instance %s: %v", row.EventID, inst.InstanceID, err)
				}
				continue
			}
			if !rt.Def.StartsFrom(msg.SourceL2Name()) {
				refuse(notTakenNoConversation, fmt.Sprintf("the current definition does not start from %q", msg.SourceL2Name()),
					noConversationDetail(threadID, msg.SourceL2Name()))
				continue
			}
			input, err := creationInput(rt.Def, msg)
			if err != nil {
				refuse(notTakenInvalidInput, fmt.Sprintf("the current definition takes no such input: %v", err), err.Error())
				continue
			}
			if err := rt.runCreation(ctx, msg, row.InstanceID, input, row.EventID); err != nil {
				log.Printf("recover: create %s again: %v", row.InstanceID, err)
			}
		default:
			inst, ok := rt.Registry.Get(row.InstanceID)
			if !ok {
				drop("its instance is not running")
				continue
			}
			if err := rt.runEvent(ctx, inst, msg, row.EventID); err != nil {
				log.Printf("recover: run inbound event %d again: %v", row.EventID, err)
			}
		}
	}
	return nil
}

// giveUpInbound handles a journaled event already run at maxReplayAttempts starts without finishing:
// it is not run again. Its instance fails (causeReplayExhausted), with no action run; a creation whose
// instance never committed is dropped.
func (rt *NodeRuntime) giveUpInbound(ctx context.Context, row InboundEventRow, msg sdk.Message) {
	cause := fmt.Errorf("%s: inbound event %d (%s msg=%q trace_id=%s) ran at %d starts without finishing",
		causeReplayExhausted, row.EventID, row.Kind, rawMsgName(msg), msg.Routing.TraceID, row.Attempts)
	inst, running := rt.Registry.Get(row.InstanceID)
	if !running {
		log.Printf("recover: %v; instance %s is not running: the event is dropped", cause, row.InstanceID)
		if err := rt.Store.DeleteInboundEvent(ctx, row.EventID); err != nil {
			log.Printf("recover: drop inbound event %d: %v", row.EventID, err)
		}
		return
	}
	inst.Lock()
	defer inst.Unlock()
	if err := rt.failInStore(ctx, inst, cause); err != nil {
		log.Printf("recover: %v", err)
	}
	rt.settle(ctx, inst)
}

// listTimers asks SY.timer for the timers this node owns. It is safe to call off the receive loop:
// it only asks.
func (rt *NodeRuntime) listTimers(ctx context.Context, opts RecoverOptions) ([]sdk.TimerInfo, error) {
	if rt.ActCtx.Timer == nil {
		return nil, nil
	}
	timers, err := rt.ActCtx.Timer.List(ctx, sdk.ListFilter{
		OwnerL2Name:  opts.OwnerL2Name,
		StatusFilter: "pending",
		Limit:        sdk.MaxTimerListLimit,
	})
	if err != nil {
		return nil, fmt.Errorf("%s did not answer TIMER_LIST: %w", timerAuthority(rt.NodeName), err)
	}
	return timers, nil
}

// ReconcileTimers reconciles the timer index with what SY.timer has pending; an error says the timers
// are not reconciled: SY.timer did not answer, and nothing changed, or the index could not be read
// (reconcileTimers).
func (rt *NodeRuntime) ReconcileTimers(ctx context.Context, opts RecoverOptions) error {
	if rt.ActCtx.Timer == nil {
		log.Printf("recover: no timer sender configured, skipping timer reconciliation")
		return nil
	}
	askedAtMS := nowMS(rt.ActCtx.Clock)
	syTimers, err := rt.listTimers(ctx, opts)
	if err != nil {
		return err
	}
	return rt.reconcileTimers(ctx, syTimers, opts, askedAtMS)
}

// timerListResult is SY.timer's answer to a TIMER_LIST asked off the receive loop at askedAtMS.
type timerListResult struct {
	timers    []sdk.TimerInfo
	err       error
	askedAtMS int64
}

// takeTimerList reconciles with a TIMER_LIST asked off the receive loop: once SY.timer answered and
// the index was read, the node is no longer DEGRADED; otherwise the loop asks again on its tick.
func (rt *NodeRuntime) takeTimerList(ctx context.Context, result timerListResult, opts RecoverOptions) {
	err := result.err
	if err == nil {
		err = rt.reconcileTimers(ctx, result.timers, opts, result.askedAtMS)
	}
	if err != nil {
		rt.timersPending = err
		log.Printf("wf: timers still not reconciled: %v", err)
		return
	}
	rt.timersPending = nil
	// The node stays DEGRADED while a timer waits for SY.timer to take it (degradedBy), and the log
	// says so (FINDINGS A-254's review).
	switch count, _, err := rt.Store.RefusedTimers(ctx); {
	case err != nil:
		log.Printf("wf: timers reconciled with SY.timer; the timers it has not taken could not be counted: %v", err)
	case count > 0:
		log.Printf("wf: timers reconciled with SY.timer; %d timer(s) wait for it to take them, the node DEGRADED until it does", count)
	default:
		log.Printf("wf: timers reconciled with SY.timer; the node is no longer degraded")
	}
}

// reconcileTimers reconciles the timer index with syTimers, what SY.timer had pending for this node
// when it was asked, at askedAtMS. It goes through the index in the order the timers fire, and reads
// each row again before it acts: a fire it gives an instance may cancel or move a later timer.
//
//   - pending, fire_at ahead: nothing to do, but a refusal the index keeps of it is cleared: SY.timer
//     has it (FINDINGS A-254). SY.timer holding it later than the index's time (a move it did not
//     take) would fire it late: it is moved to its time, or, less than 60 s from it, fired by the
//     node itself then (timerHeld, A-254's review);
//   - pending, fire_at past, SY.timer's or the index's (SY.timer holding it later): the instance takes
//     a fire now (as SY.timer's would, through the journal), in the order of the index, and the
//     pending one is cancelled unless the fire scheduled the key again;
//   - not in SY.timer, fire_at past: it fired while the WF was down and the fire was lost; the
//     instance takes a fire now;
//   - not in SY.timer, fire_at ahead: SY.timer never got it (it was scheduled while SY.timer did not
//     answer) or lost it; it goes again from the loop's tick, paced (timerMissing);
//   - scheduled after SY.timer was asked: left alone (the list could not hold it);
//   - in SY.timer but not in the index: an orphan, cancelled.
//
// An error says the timers are not reconciled, and the node stays DEGRADED until a later reconcile
// is (timersPending), since a fire dropped while it could not take one has no other way back (SY.timer
// sends a fire once, FINDINGS A-217): the index could not be read, and nothing changed (every timer
// of SY.timer would have read as an orphan), or a row could not be read again, and stays as it was
// while the others are reconciled.
func (rt *NodeRuntime) reconcileTimers(ctx context.Context, syTimers []sdk.TimerInfo, opts RecoverOptions, askedAtMS int64) error {
	actx := rt.ActCtx
	expectedTimers, err := rt.Store.ListAllExpectedTimers(ctx)
	if err != nil {
		return fmt.Errorf("read the timer index: %w", err)
	}
	if len(syTimers) == sdk.MaxTimerListLimit {
		log.Printf("recover: TIMER_LIST reached max limit=%d for owner=%s; reconciliation may be truncated", sdk.MaxTimerListLimit, opts.OwnerL2Name)
	}
	syView := make(map[string]sdk.TimerInfo, len(syTimers))
	for _, t := range syTimers {
		if t.ClientRef != nil && *t.ClientRef != "" {
			syView[*t.ClientRef] = t
		}
	}

	now := actx.Clock().UnixMilli()
	var unread error
	for _, listed := range expectedTimers {
		ref := timerClientRef(listed.InstanceID, listed.TimerKey)
		syTimer, existsInSY := syView[ref]
		delete(syView, ref)
		row, current, err := rt.Store.GetTimerForInstance(ctx, listed.InstanceID, listed.TimerKey)
		if err != nil {
			log.Printf("recover: reread timer %s/%s: %v", listed.InstanceID, listed.TimerKey, err)
			if unread == nil {
				unread = fmt.Errorf("read timer %s/%s of the index again: %w", listed.InstanceID, listed.TimerKey, err)
			}
			continue
		}
		if !current || row.FireAtMS != listed.FireAtMS {
			// An earlier fire's transition cancelled it or scheduled it again.
			continue
		}
		if row.ScheduledAtMS > askedAtMS {
			continue
		}
		inst, running := rt.Registry.Get(row.InstanceID)

		switch {
		case existsInSY && syTimer.FireAtUTCMS > now && row.FireAtMS > now:
			// Pending, fire_at ahead: SY.timer has it. A refusal of it is settled (FINDINGS A-254),
			// unless SY.timer holds it later than its time, a move it did not take: it goes to its
			// time (timerHeld, A-254's review).
			rt.timerHeld(ctx, timerRef{row.InstanceID, row.TimerKey}, syTimer.FireAtUTCMS)

		case existsInSY:
			// Pending, and its time has come: SY.timer's, or the index's when SY.timer holds it later
			// (a move to an earlier time it did not take), whose fire would come late.
			if running {
				log.Printf("recover: timer %s/%s past fire_at, injecting synthetic TIMER_FIRED", row.InstanceID, row.TimerKey)
				rt.fireMissedTimer(ctx, inst, row, syTimer.UUID)
				if _, rearmed, _ := rt.Store.GetTimerForInstance(ctx, row.InstanceID, row.TimerKey); rearmed {
					log.Printf("recover: timer %s/%s was scheduled again by the fire, keeping it", row.InstanceID, row.TimerKey)
					continue
				}
			} else {
				_ = rt.Store.DeleteTimer(ctx, row.InstanceID, row.TimerKey)
			}
			// Cancel the pending one in SY.timer so it does not fire again.
			if err := actx.Timer.CancelByClientRefConfirmed(ctx, ref); err != nil {
				log.Printf("recover: cancel past timer %s: %v", ref, err)
			}

		case row.FireAtMS <= now:
			// G3: it fired while wf.engine was down, and SY.timer (which delivers TIMER_FIRED
			// best-effort) dropped the fire: without one, the instance (a self-rescheduling ticker,
			// say) would wait forever.
			if running {
				log.Printf("recover: expected timer %s/%s past fire_at but missing from SY.timer — fired while down; injecting synthetic TIMER_FIRED", row.InstanceID, row.TimerKey)
				rt.fireMissedTimer(ctx, inst, row, "")
			} else {
				_ = rt.Store.DeleteTimer(ctx, row.InstanceID, row.TimerKey)
			}

		case running:
			rt.timerMissing(ctx, row, now)

		default:
			_ = rt.Store.DeleteTimer(ctx, row.InstanceID, row.TimerKey)
		}
	}

	for ref := range syView {
		instanceID, _, err := parseTimerClientRef(ref)
		if err != nil {
			// client_ref doesn't belong to this WF format, skip.
			continue
		}
		log.Printf("recover: orphaned timer %s (instance %s), cancelling in SY.timer", ref, instanceID)
		if err := actx.Timer.CancelByClientRefConfirmed(ctx, ref); err != nil {
			log.Printf("recover: cancel orphan %s: %v", ref, err)
		}
	}
	if unread != nil {
		return unread
	}
	log.Printf("recover: timer reconciliation complete")
	return nil
}

// timerMissing marks a timer of the index the reconcile found SY.timer does not have (scheduled while
// SY.timer did not answer, or lost by SY.timer) to go again from the receive loop's tick at once, as
// a timer SY.timer has not taken: maxTimerRequestsPerTick a tick at most, at its fire_at, a request
// followed as any other, and one less than 60 s from its time is fired by the node itself then
// (sendRefusedTimersAgain). The reconcile sent each at once, past SY.timer's 64 messages and the
// node's 64 answers after a long outage, and the answers the SDK dropped read as refusals 30 s later
// (FINDINGS A-254's review).
func (rt *NodeRuntime) timerMissing(ctx context.Context, row TimerRow, nowMS int64) {
	ref := timerRef{row.InstanceID, row.TimerKey}
	why := "TIMER_LIST: " + timerAuthority(rt.NodeName) + " does not have it"
	if err := rt.Store.RefuseTimer(ctx, row.InstanceID, row.TimerKey, why, nowMS); err != nil {
		log.Printf("recover: expected timer %s missing from SY.timer, and it could not be marked to go again: %v", ref, err)
		return
	}
	rt.ActCtx.Requests.noteRefused()
	log.Printf("recover: expected timer %s missing from SY.timer (fire_at still future): it goes again from the loop", ref)
}

// scheduleAgain schedules again, at its fire_at (SY.timer's minimum ahead at least), a timer SY.timer
// fired at a time the instance had moved it from (takeTimerFired): SY.timer fires a timer once.
// SY.timer keeps a pending timer with the same client_ref, so it never fires twice. The index does
// not keep the timer's missed_policy: the new schedule has SY.timer's default, fire. It is a new
// request, as an action's: the index first, with no refusal, and a request SY.timer does not take,
// or that does not go, marks the timer to go again (FINDINGS A-254).
func (rt *NodeRuntime) scheduleAgain(ctx context.Context, row TimerRow, nowMS int64) {
	in := time.Duration(row.FireAtMS-nowMS) * time.Millisecond
	if in < minTimerDuration {
		in = minTimerDuration
	}
	if err := rt.Store.RegisterTimer(ctx, row.InstanceID, row.TimerKey, nowMS, nowMS+in.Milliseconds()); err != nil {
		log.Printf("recover: register timer %s/%s: %v", row.InstanceID, row.TimerKey, err)
		return
	}
	opts := timerScheduleOptions(rt.ActCtx.Dispatcher.NodeL2Name(), row.InstanceID, row.TimerKey)
	traceID, err := rt.ActCtx.Timer.ScheduleIn(ctx, in, opts)
	req := timerRequest{timer: timerRef{row.InstanceID, row.TimerKey}, verb: verbTimerSchedule, traceID: traceID, sentAtMS: nowMS}
	if err := rt.ActCtx.timerRequestSent(ctx, req, err); err != nil {
		log.Printf("recover: schedule timer %s/%s again: %v", row.InstanceID, row.TimerKey, err)
	}
}

// fireMissedTimer gives inst the fire of a timer recovery found past its time, as SY.timer's fire
// goes: journaled, with the timer's index row gone in the same commit. It names this node as the
// timer's owner, as SY.timer's fire of the node's own timer does.
func (rt *NodeRuntime) fireMissedTimer(ctx context.Context, inst *WFInstance, row TimerRow, timerUUID string) {
	info := sdk.TimerInfo{UUID: timerUUID, OwnerL2Name: rt.NodeName, FireAtUTCMS: row.FireAtMS}
	synthMsg := buildSyntheticTimerFired(row.InstanceID, row.TimerKey, timerClientRef(row.InstanceID, row.TimerKey), info, rt.ActCtx.Clock)
	if err := rt.deliver(ctx, inst, synthMsg, row.TimerKey); err != nil {
		log.Printf("recover: synthetic TIMER_FIRED for %s/%s: %v", row.InstanceID, row.TimerKey, err)
	}
}

// buildSyntheticTimerFired constructs a fake TIMER_FIRED message for use during
// boot recovery when a timer should have fired while the WF was down.
func buildSyntheticTimerFired(instanceID, timerKey, clientRef string, info sdk.TimerInfo, clock ClockFunc) sdk.Message {
	userPayload := map[string]any{
		"instance_id": instanceID,
		"timer_key":   timerKey,
	}
	clientRefCopy := clientRef
	fired := sdk.FiredEvent{
		TimerUUID:            info.UUID,
		OwnerL2Name:          info.OwnerL2Name,
		ClientRef:            &clientRefCopy,
		Kind:                 "oneshot",
		ScheduledFireAtUTCMS: info.FireAtUTCMS,
		ActualFireAtUTCMS:    clock().UnixMilli(),
		FireCount:            1,
		IsLastFire:           true,
	}
	raw, _ := json.Marshal(userPayload)
	fired.UserPayload = raw

	firedJSON, _ := json.Marshal(fired)
	msgName := sdk.MsgTimerFired
	return sdk.Message{
		Routing: sdk.Routing{
			Src:     info.OwnerL2Name,
			Dst:     sdk.UnicastDestination(info.OwnerL2Name),
			TTL:     1,
			TraceID: fmt.Sprintf("recover-%s-%s", instanceID, timerKey),
		},
		Meta: sdk.Meta{
			MsgType: sdk.SYSTEMKind,
			Msg:     &msgName,
		},
		Payload: firedJSON,
	}
}
