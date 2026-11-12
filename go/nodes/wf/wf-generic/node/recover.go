package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"math"
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

// maxTimerListPages bounds the pages of SY.timer's TIMER_LIST a reconcile reads (listTimers). A page
// holds the timers that fit 96 KiB: about 210 to 240 of a WF's (410 to 460 bytes each, listed, by
// the lengths of its name and its timer keys), so the 10,000 pending timers SY.timer keeps for an
// owner at the most take 42 to 48 pages, and about 90 at the longest client_ref (255 characters):
// an SY.timer that lists them right never brings a reconcile to the bound, 200 pages.
const maxTimerListPages = 200

// timerListing is what SY.timer listed of the node's pending timers (listTimers), by fire time, then
// uuid. It is complete when its last page said no timer follows. Otherwise the list stopped before
// its end (the bound, or an answer it could not go on from), and it holds the timers up to
// throughMS, the fire time of the last one listed (math.MinInt64 when none was).
type timerListing struct {
	timers    []sdk.TimerInfo
	complete  bool
	throughMS int64
	pages     int
}

// covers reports whether the listing would hold a timer the index has at fireAtMS, were SY.timer to
// hold it pending at that time: always when the listing is complete; otherwise when the timer fires
// before the last one listed by more than timerFireTolerance. SY.timer normally holds a timer at the
// index's time, or up to that much later (it counts a fire_in from when it gets the request), so one
// the listing does not cover may be pending past the last one listed: it is never taken as missing
// (FINDINGS A-278). One SY.timer holds later still, a move it did not take or a schedule it took
// more than 2 s late, can be past the last one listed while the listing covers it: it reads as
// missing and goes again once, and SY.timer's already_existed settles it (timerHeld), its fire
// neither lost nor doubled.
func (l timerListing) covers(fireAtMS int64) bool {
	return l.complete || fireAtMS+timerFireTolerance.Milliseconds() < l.throughMS
}

// distinctTimers is how many timers listed holds, each once: a timer can come on two pages (its fire
// time moved between them), and a page can come again (listTimers).
func distinctTimers(listed []sdk.TimerInfo) int {
	uuids := make(map[string]bool, len(listed))
	for _, timer := range listed {
		uuids[timer.UUID] = true
	}
	return len(uuids)
}

// listTimers asks SY.timer for the timers this node has pending, all of them, in pages: the first,
// then each from the cursor the page before gave, until a page gives none (FINDINGS A-278: until
// 0.1.137 a reconcile read 1000 at the most, and took the index's timers past the 1000th as missing).
// It stops before the end of the list, the listing incomplete, at an answer it cannot go on from (a
// page that holds no timer and gives a cursor, or that gives a cursor it gave before; or one that
// holds the request's limit and gives none, as SY.timer before 1.6 answers a page its limit cut,
// FINDINGS A-266), or at maxTimerListPages pages. An error says SY.timer did not answer a page, and
// nothing is listed. It is safe to call off the receive loop: it only asks.
func (rt *NodeRuntime) listTimers(ctx context.Context, opts RecoverOptions) (timerListing, error) {
	listing := timerListing{throughMS: math.MinInt64}
	if rt.ActCtx.Timer == nil {
		listing.complete = true
		return listing, nil
	}
	authority := timerAuthority(rt.NodeName)
	started := time.Now()
	filter := sdk.ListFilter{OwnerL2Name: opts.OwnerL2Name, StatusFilter: "pending", Limit: sdk.MaxTimerListLimit}
	given := map[string]bool{}
	for {
		page, err := rt.ActCtx.Timer.ListPage(ctx, filter)
		if err != nil {
			return timerListing{}, fmt.Errorf("%s did not answer TIMER_LIST (page %d): %w", authority, listing.pages+1, err)
		}
		listing.pages++
		listing.timers = append(listing.timers, page.Timers...)
		if n := len(page.Timers); n > 0 {
			listing.throughMS = page.Timers[n-1].FireAtUTCMS
		}
		stopped := ""
		switch {
		case page.NextCursor == "" && len(page.Timers) < filter.Limit:
			listing.complete = true
		case page.NextCursor == "":
			stopped = fmt.Sprintf("page %d holds %d timers, the limit asked, and gives no next_cursor, as SY.timer before 1.6 answers a page its limit cut (FINDINGS A-266)",
				listing.pages, len(page.Timers))
		case len(page.Timers) == 0:
			stopped = fmt.Sprintf("page %d holds no timer, and gives a next_cursor", listing.pages)
		case given[page.NextCursor]:
			stopped = fmt.Sprintf("page %d gives a next_cursor a page before it gave", listing.pages)
		case listing.pages == maxTimerListPages:
			stopped = fmt.Sprintf("it read %d pages, the bound", maxTimerListPages)
		default:
			given[page.NextCursor] = true
			filter.Cursor = page.NextCursor
			continue
		}
		elapsed := time.Since(started).Round(time.Millisecond)
		if listing.complete {
			log.Printf("recover: %s listed %d pending timer(s) of %s in %d page(s), in %s",
				authority, distinctTimers(listing.timers), opts.OwnerL2Name, listing.pages, elapsed)
		} else {
			log.Printf("recover: %s listed %d pending timer(s) of %s in %d page(s), in %s, and the list stops there, before its end: %s",
				authority, distinctTimers(listing.timers), opts.OwnerL2Name, listing.pages, elapsed, stopped)
		}
		return listing, nil
	}
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
	listing, err := rt.listTimers(ctx, opts)
	if err != nil {
		return err
	}
	return rt.reconcileTimers(ctx, listing, opts, askedAtMS)
}

// timerListResult is SY.timer's answer to the TIMER_LIST asked off the receive loop at askedAtMS:
// its listing, or the error of a page it did not answer.
type timerListResult struct {
	listing   timerListing
	err       error
	askedAtMS int64
}

// takeTimerList reconciles with a TIMER_LIST asked off the receive loop: once SY.timer answered and
// the index was read, the node is no longer DEGRADED, a list that stopped before its end included
// (reconcileTimers); otherwise the loop asks again on its tick.
func (rt *NodeRuntime) takeTimerList(ctx context.Context, result timerListResult, opts RecoverOptions) {
	err := result.err
	if err == nil {
		err = rt.reconcileTimers(ctx, result.listing, opts, result.askedAtMS)
	}
	if err != nil {
		rt.timersPending = err
		log.Printf("wf: timers still not reconciled: %v", err)
		return
	}
	rt.timersPending = nil
	reconciled := "timers reconciled with SY.timer"
	if !result.listing.complete {
		reconciled += " as far as its list went, which stopped before its end (the node's next start lists the rest)"
	}
	// The node stays DEGRADED while a timer waits for SY.timer to take it (degradedBy), and the log
	// says so (FINDINGS A-254's review).
	switch count, _, err := rt.Store.RefusedTimers(ctx); {
	case err != nil:
		log.Printf("wf: %s; the timers it has not taken could not be counted: %v", reconciled, err)
	case count > 0:
		log.Printf("wf: %s; %d timer(s) wait for it to take them, the node DEGRADED until it does", reconciled, count)
	default:
		log.Printf("wf: %s; the node is no longer degraded", reconciled)
	}
}

// reconcileTimers reconciles the timer index with listing, what SY.timer had pending for this node
// when it was asked, at askedAtMS. It goes through the index in the order the timers fire, and reads
// each row again before it acts: a fire it gives an instance may cancel or move a later timer.
//
//   - pending, fire_at ahead: nothing to do, but a refusal the index keeps of it is cleared: SY.timer
//     has it (FINDINGS A-254). SY.timer holding it later than the index's time (a move it did not
//     take) would fire it late: it is moved to its time, or, less than 60 s from it, fired by the
//     node itself then (timerHeld, A-254's review);
//   - pending, fire_at past, SY.timer's or the index's (SY.timer holding it later): the instance takes
//     a fire now (as SY.timer's would, through the journal), in the order of the index, and the
//     pending one is cancelled unless the fire scheduled the key again (firePastTimer);
//   - not listed, from the last timer listed on, when the list stopped before its end (covers):
//     SY.timer may have it pending, or may have lost it. One whose fire_at is past is taken as a
//     pending one past its time: the instance takes a fire now, and the one SY.timer may hold is
//     cancelled unless the fire scheduled the key again (A-278's review). One whose fire_at is ahead
//     is left as it is, neither taken as missing nor fired, until the node's next start: no reconcile
//     comes before it, since a list that stopped before its end is no error (the node is not DEGRADED
//     for it, and the loop asks no list again). Until then one SY.timer lost does not go again, and
//     the log line that counts them is the only sign (FINDINGS A-278);
//   - not in SY.timer, fire_at past: it fired while the WF was down and the fire was lost; the
//     instance takes a fire now;
//   - not in SY.timer, fire_at ahead: SY.timer never got it (it was scheduled while SY.timer did not
//     answer) or lost it; it goes again from the loop's tick, paced (timerMissing);
//   - scheduled, or moved, after SY.timer was asked: left alone; the list may not hold it, or hold
//     it at its old time (its pages are read one after another, no snapshot). The node's own
//     requests while the pages of a reconcile off the loop are read, a refused timer sent again from
//     the loop's tick or one SY.timer holds late moved to its time (timerHeld), keep the row's
//     scheduled_at_ms: SY.timer may take such a timer behind a page already read, and it then reads
//     as missing and goes again once, which SY.timer answers already_existed (timerHeld);
//   - in SY.timer but not in the index: an orphan, cancelled.
//
// An error says the timers are not reconciled, and the node stays DEGRADED until a later reconcile
// is (timersPending), since a fire dropped while it could not take one has no other way back (SY.timer
// sends a fire once, FINDINGS A-217): the index could not be read, and nothing changed (every timer
// of SY.timer would have read as an orphan), or a row could not be read again, and stays as it was
// while the others are reconciled. A listing that stopped before its end is no error: what it lists
// is reconciled, its listTimers' log says why it stopped, and the node's next start lists the rest.
func (rt *NodeRuntime) reconcileTimers(ctx context.Context, listing timerListing, opts RecoverOptions, askedAtMS int64) error {
	actx := rt.ActCtx
	expectedTimers, err := rt.Store.ListAllExpectedTimers(ctx)
	if err != nil {
		return fmt.Errorf("read the timer index: %w", err)
	}
	// A timer can come on two pages (its fire time moved between them): the later page has it as it
	// is since.
	syView := make(map[string]sdk.TimerInfo, len(listing.timers))
	for _, t := range listing.timers {
		if t.ClientRef != nil && *t.ClientRef != "" {
			syView[*t.ClientRef] = t
		}
	}

	now := actx.Clock().UnixMilli()
	var unread error
	unlisted := 0
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
			// time (timerHeld, A-254's review). The timer of an instance that no longer runs is left
			// alone: its fire is dropped as an orphan's, and a mark of it goes on the loop's tick.
			if running {
				rt.timerHeld(ctx, timerRef{row.InstanceID, row.TimerKey}, syTimer.FireAtUTCMS)
			}

		case existsInSY:
			// Pending, and its time has come: SY.timer's, or the index's when SY.timer holds it later
			// (a move to an earlier time it did not take), whose fire would come late.
			if running {
				log.Printf("recover: timer %s/%s past fire_at, injecting synthetic TIMER_FIRED", row.InstanceID, row.TimerKey)
			}
			rt.firePastTimer(ctx, inst, running, row, syTimer.UUID)

		case running && !listing.covers(row.FireAtMS) && row.FireAtMS <= now:
			// Past its time, and past the last timer SY.timer listed, the list having stopped before
			// its end: SY.timer may hold it pending, or may have lost it, and no reconcile comes
			// before the node's next start. It is fired now, as a pending one past its time, and the
			// one SY.timer may hold is cancelled (FINDINGS A-278's review).
			log.Printf("recover: timer %s/%s past fire_at, and past the last timer %s listed, its list stopped before its end: injecting synthetic TIMER_FIRED, and cancelling the one it may hold",
				row.InstanceID, row.TimerKey, timerAuthority(rt.NodeName))
			rt.firePastTimer(ctx, inst, running, row, "")

		case running && !listing.covers(row.FireAtMS):
			// Past the last timer SY.timer listed, the list having stopped before its end, and ahead
			// of its time: SY.timer may have it pending, so it is neither taken as missing nor fired
			// (FINDINGS A-278).
			unlisted++

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
	if unlisted > 0 {
		last := "it listed none"
		if listing.throughMS != math.MinInt64 {
			last = "at " + msTime(listing.throughMS)
		}
		log.Printf("recover: %d timer(s) of %s ahead of their time, from the last one %s listed on (%s), are left as they are until the node's next start, none taken as missing: its list stopped before its end",
			unlisted, opts.OwnerL2Name, timerAuthority(rt.NodeName), last)
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
	if listing.complete {
		log.Printf("recover: timer reconciliation complete")
	} else {
		log.Printf("recover: timer reconciliation done for the timers %s listed: its list stopped before its end, and the node's next start lists the rest",
			timerAuthority(rt.NodeName))
	}
	return nil
}

// firePastTimer settles a timer of the index whose time has come while SY.timer holds it pending,
// or may (reconcileTimers): the instance takes its fire now (fireMissedTimer), and the one SY.timer
// holds is cancelled so it does not fire again, unless the fire scheduled the key again. SY.timer
// answers the cancel of a client_ref it holds no pending timer for found false, and a fire of it
// that comes all the same is dropped, the index no longer having the timer (takeTimerFired). The
// timer of an instance that no longer runs leaves the index, and SY.timer's is cancelled.
func (rt *NodeRuntime) firePastTimer(ctx context.Context, inst *WFInstance, running bool, row TimerRow, timerUUID string) {
	ref := timerClientRef(row.InstanceID, row.TimerKey)
	if running {
		rt.fireMissedTimer(ctx, inst, row, timerUUID)
		if _, rearmed, _ := rt.Store.GetTimerForInstance(ctx, row.InstanceID, row.TimerKey); rearmed {
			log.Printf("recover: timer %s/%s was scheduled again by the fire, keeping it", row.InstanceID, row.TimerKey)
			return
		}
	} else {
		_ = rt.Store.DeleteTimer(ctx, row.InstanceID, row.TimerKey)
	}
	if err := rt.ActCtx.Timer.CancelByClientRefConfirmed(ctx, ref); err != nil {
		log.Printf("recover: cancel past timer %s: %v", ref, err)
	}
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
