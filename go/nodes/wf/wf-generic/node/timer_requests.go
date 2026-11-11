package node

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"sort"
	"strings"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// The requests a WF node sends SY.timer about its instances' timers (FINDINGS A-254). A
// TIMER_SCHEDULE or a TIMER_RESCHEDULE goes without waiting for SY.timer's answer: the node's only
// receive loop never waits on SY.timer. The node keeps each one by its trace_id until the answer
// comes (timerRequests), and takes as refused one that:
//   - the router answers UNREACHABLE (SY.timer not connected, NODE_NOT_FOUND, while it restarts; its
//     link full, LINK_BUSY) or TTL_EXCEEDED (takeTimerRequestNotice);
//   - SY.timer answers with ok false (its store failing, its limit of pending timers reached,
//     takeTimerAnswer), or a TIMER_NOT_FOUND to a reschedule of a timer it never got;
//   - gets no answer in timerAnswerTimeout (SY.timer dropped it under load, or its answer was lost);
//   - did not go at all (the send queue full during a reconnect).
//
// A refusal marks the timer in the index (refuseTimer), and the node's log says which timer, which
// request and why. A refused timer is sent again, a TIMER_SCHEDULE at its fire_at, after 5, 10, 20
// and 40 s, then each minute, as SY.timer sends again a fire it could not deliver, until SY.timer
// takes it, or the instance cancels it or ends. One less than 60 s from its time, SY.timer's floor,
// is not sent again, since SY.timer would take it for 60 s from then and fire it late: the node fires
// it itself at its time, as the reconcile fires a timer lost while the node was down
// (sendRefusedTimersAgain). While a timer is refused the node is DEGRADED, timers_not_reconciled,
// naming it (refusedTimersDegraded).
//
// The order with the instance's later requests about the same timer holds. What goes again is the
// index row, so a timer the instance cancelled since is not sent (the cancel dropped its row), and one
// it moved goes at its new time (MoveTimer keeps the refusal). A later request about the same timer
// supersedes an earlier one, whose answer then decides nothing (timerRequests.sent): a schedule
// refused while SY.timer restarted and then moved is settled by the reschedule's own answer, a
// TIMER_NOT_FOUND from SY.timer, which never got the timer. A move SY.timer did not take of a timer
// it holds is settled by the time SY.timer says it holds: the TIMER_SCHEDULE that goes again is
// answered already_existed at the old time, and a time later than the index's is moved there, a
// TIMER_RESCHEDULE (timerHeld), as is one the reconcile's TIMER_LIST lists.

const (
	verbTimerSchedule   = "TIMER_SCHEDULE"
	verbTimerReschedule = "TIMER_RESCHEDULE"
	verbTimerCancel     = "TIMER_CANCEL"

	// timerAnswerTimeout is how long the node waits for SY.timer's answer to a timer request before
	// it takes the request as refused. SY.timer answers in milliseconds; one that dropped the request
	// under load (its 64 messages at once, GO-2) never does.
	timerAnswerTimeout = 30 * time.Second
	// timerResendMaxDelay bounds the wait of a refused timer before it goes again.
	timerResendMaxDelay = time.Minute
	// timerRequestsInterval is how often the receive loop looks at the requests SY.timer has not
	// answered and at the refused timers, while there are any.
	timerRequestsInterval = time.Second
	// maxTimerRequestsPerTick bounds the unanswered requests one tick takes as refused, and the
	// refused timers it sends again: SY.timer takes 64 messages at once, and the timers refused
	// during a long outage go paced.
	maxTimerRequestsPerTick = 32
)

// timerResendDelays are how long a refused timer waits before it goes again, by the times it went
// again already, then timerResendMaxDelay each time: SY.timer's own schedule for a fire it could not
// deliver (docs/sy-timer.md §7.8).
var timerResendDelays = []time.Duration{5 * time.Second, 10 * time.Second, 20 * time.Second, 40 * time.Second}

func timerResendDelay(resends int) time.Duration {
	if resends < len(timerResendDelays) {
		return timerResendDelays[resends]
	}
	return timerResendMaxDelay
}

// resendTime is when a refused timer that went again resends times goes again, at nowMS: after
// timerResendDelay, while SY.timer could still take it at its fireAtMS, 60 s ahead at least (its
// floor). Past that the timer is not sent again: the node fires it itself at its time, never before
// nowMS (FINDINGS A-254's review).
func resendTime(nowMS int64, resends int, fireAtMS int64) int64 {
	at := nowMS + timerResendDelay(resends).Milliseconds()
	if at > fireAtMS-minTimerDuration.Milliseconds() {
		at = max(fireAtMS, nowMS)
	}
	return at
}

// timerRef names a timer of the index: its instance and its key.
type timerRef struct {
	instanceID, timerKey string
}

func (ref timerRef) String() string { return ref.instanceID + "/" + ref.timerKey }

// timerRequest is a request about an instance's timer that SY.timer has not answered yet.
type timerRequest struct {
	timer    timerRef
	verb     string // verbTimerSchedule or verbTimerReschedule
	traceID  string
	sentAtMS int64
}

// timerRequests are what the node waits for from SY.timer: the requests SY.timer has not answered,
// by trace_id and the latest about each timer, and whether a timer of the index may be refused. Only
// the receive loop's goroutine touches them (Recover runs on it, before the loop). A nil one keeps
// nothing.
type timerRequests struct {
	byTrace map[string]timerRequest
	latest  map[timerRef]string
	// refused says a timer of the index may be refused: set when one is, and counted again from the
	// index on the loop's tick (countRefusedTimers).
	refused bool
}

func newTimerRequests() *timerRequests {
	return &timerRequests{byTrace: map[string]timerRequest{}, latest: map[timerRef]string{}}
}

// sent keeps req until SY.timer answers it. An earlier request about the same timer is superseded:
// its answer, or its refusal, decides nothing, since the index row goes with the latest.
func (r *timerRequests) sent(req timerRequest) {
	if r == nil || req.traceID == "" {
		return
	}
	if earlier, ok := r.latest[req.timer]; ok {
		delete(r.byTrace, earlier)
	}
	r.byTrace[req.traceID] = req
	r.latest[req.timer] = req.traceID
}

// answered takes the request an answer on traceID is for, if the node waits for it.
func (r *timerRequests) answered(traceID string) (timerRequest, bool) {
	if r == nil {
		return timerRequest{}, false
	}
	req, ok := r.byTrace[traceID]
	if !ok {
		return timerRequest{}, false
	}
	delete(r.byTrace, traceID)
	if r.latest[req.timer] == traceID {
		delete(r.latest, req.timer)
	}
	return req, true
}

// unanswered takes, the oldest first, up to limit requests sent at cutoffMS or before: SY.timer did
// not answer them in time.
func (r *timerRequests) unanswered(cutoffMS int64, limit int) []timerRequest {
	if r == nil {
		return nil
	}
	var late []timerRequest
	for _, req := range r.byTrace {
		if req.sentAtMS <= cutoffMS {
			late = append(late, req)
		}
	}
	sort.Slice(late, func(i, j int) bool {
		if late[i].sentAtMS != late[j].sentAtMS {
			return late[i].sentAtMS < late[j].sentAtMS
		}
		return late[i].traceID < late[j].traceID
	})
	if len(late) > limit {
		late = late[:limit]
	}
	for _, req := range late {
		r.answered(req.traceID)
	}
	return late
}

// noteRefused says a timer of the index may be refused: the loop's tick looks at it.
func (r *timerRequests) noteRefused() {
	if r != nil {
		r.refused = true
	}
}

// waiting reports whether the node waits for anything from SY.timer: an answer, or a refused timer
// to go again.
func (r *timerRequests) waiting() bool {
	return r != nil && (len(r.byTrace) > 0 || r.refused)
}

// clear forgets every request: the engine stopped, and its next start reconciles the timers.
func (r *timerRequests) clear() {
	if r == nil {
		return
	}
	r.byTrace, r.latest, r.refused = map[string]timerRequest{}, map[timerRef]string{}, false
}

// timerRequestSent keeps req for SY.timer's answer or, when it did not go (sendErr: the send queue
// full during a reconnect, say), refuses its timer at once: it goes again. The timer is in the index
// already. An error says the refusal could not be kept either.
func (actx ActionContext) timerRequestSent(ctx context.Context, req timerRequest, sendErr error) error {
	if sendErr == nil {
		actx.Requests.sent(req)
		return nil
	}
	if err := refuseTimer(ctx, actx, req.timer, req.verb+": not sent: "+sendErr.Error()); err != nil {
		return fmt.Errorf("%s not sent (%v), and its timer could not be marked to go again: %w", req.verb, sendErr, err)
	}
	return nil
}

// refuseTimer marks timer ref of the index refused, saying why, to go again after timerResendDelay
// of the times it went again already, or at its fire_at when that comes first. A timer no longer in
// the index (the instance cancelled it, took its fire, or ended) is left alone.
func refuseTimer(ctx context.Context, actx ActionContext, ref timerRef, why string) error {
	row, ok, err := actx.Store.GetTimerForInstance(ctx, ref.instanceID, ref.timerKey)
	if err != nil {
		return fmt.Errorf("read timer %s: %w", ref, err)
	}
	if !ok {
		log.Printf("wf: timer %s not taken by SY.timer (%s), and the instance no longer has it", ref, why)
		return nil
	}
	now := nowMS(actx.Clock)
	resendAt := resendTime(now, row.Resends, row.FireAtMS)
	if err := actx.Store.RefuseTimer(ctx, ref.instanceID, ref.timerKey, why, resendAt); err != nil {
		return fmt.Errorf("mark timer %s refused: %w", ref, err)
	}
	actx.Requests.noteRefused()
	if resendAt >= row.FireAtMS {
		log.Printf("wf: timer %s not taken by SY.timer: %s; too near its time for SY.timer, the node fires it itself at %s", ref, why, msTime(row.FireAtMS))
		return nil
	}
	log.Printf("wf: timer %s not taken by SY.timer: %s; it goes again at %s", ref, why, msTime(resendAt))
	return nil
}

// timerRequestRefused marks the timer of req refused: SY.timer did not take the request, for why.
func (rt *NodeRuntime) timerRequestRefused(ctx context.Context, req timerRequest, why string) {
	if rt.Store == nil {
		return
	}
	if req.traceID != "" {
		why = fmt.Sprintf("%s (trace_id=%s)", why, req.traceID)
	}
	if err := refuseTimer(ctx, rt.ActCtx, req.timer, req.verb+": "+why); err != nil {
		log.Printf("wf: timer %s not taken by SY.timer (%s): %v", req.timer, why, err)
	}
}

// timerTaken clears the refusal of timer ref, if it has one: SY.timer took a request about it.
func (rt *NodeRuntime) timerTaken(ctx context.Context, ref timerRef) {
	if row, ok := rt.timerRow(ctx, ref, "taken by SY.timer"); ok {
		rt.clearRefusal(ctx, row)
	}
}

// timerRow reads timer ref of the index, false when the index no longer has it, or when it cannot be
// read: the log says so, with what SY.timer did (what).
func (rt *NodeRuntime) timerRow(ctx context.Context, ref timerRef, what string) (TimerRow, bool) {
	if rt.Store == nil {
		return TimerRow{}, false
	}
	row, ok, err := rt.Store.GetTimerForInstance(ctx, ref.instanceID, ref.timerKey)
	if err != nil {
		log.Printf("wf: timer %s %s, and its row could not be read: %v", ref, what, err)
		return TimerRow{}, false
	}
	return row, ok
}

// clearRefusal clears the refusal of row, if it has one: SY.timer has its timer.
func (rt *NodeRuntime) clearRefusal(ctx context.Context, row TimerRow) {
	if row.Refusal == "" {
		return
	}
	ref := timerRef{row.InstanceID, row.TimerKey}
	if err := rt.Store.TimerTaken(ctx, row.InstanceID, row.TimerKey); err != nil {
		log.Printf("wf: timer %s taken by SY.timer, and its refusal could not be cleared: %v", ref, err)
		return
	}
	log.Printf("wf: timer %s taken by SY.timer, sent again %d time(s) after %s", ref, row.Resends, row.Refusal)
}

// timerHeld settles timer ref, which SY.timer holds pending at heldAtMS: it answered a schedule
// already_existed with that time (it keeps one pending timer per client_ref), or the reconcile's
// TIMER_LIST lists it. The timer is taken, its refusal cleared, unless SY.timer holds it later than
// the index's time, past a fire's tolerance: a move SY.timer did not take (its TIMER_RESCHEDULE
// refused or lost, and the TIMER_SCHEDULE that went again answered already_existed at the old time),
// or a schedule of a key it held already. SY.timer would fire it late, and the stale-fire rule
// (takeTimerFired) rescues only a timer it holds earlier. The timer is moved to its time, a
// TIMER_RESCHEDULE followed as any request, its refusal, if it has one, kept until SY.timer takes
// it; one less than 60 s from its time, SY.timer's floor, is marked, and the node fires it itself at
// its time (FINDINGS A-254's review).
func (rt *NodeRuntime) timerHeld(ctx context.Context, ref timerRef, heldAtMS int64) {
	row, ok := rt.timerRow(ctx, ref, "held by SY.timer")
	if !ok {
		return
	}
	if heldAtMS <= row.FireAtMS+timerFireTolerance.Milliseconds() || rt.ActCtx.Timer == nil {
		rt.clearRefusal(ctx, row)
		return
	}
	why := fmt.Sprintf("%s holds it at %s, later than its time, %s", timerAuthority(rt.NodeName), msTime(heldAtMS), msTime(row.FireAtMS))
	now := nowMS(rt.ActCtx.Clock)
	if row.FireAtMS-now < minTimerDuration.Milliseconds() {
		if err := refuseTimer(ctx, rt.ActCtx, ref, why); err != nil {
			log.Printf("wf: timer %s: %v", ref, err)
		}
		return
	}
	in := time.Duration(row.FireAtMS-now) * time.Millisecond
	traceID, err := rt.ActCtx.Timer.RescheduleInByClientRef(ctx, timerClientRef(row.InstanceID, row.TimerKey), in)
	if err == nil {
		log.Printf("wf: timer %s: %s; moved to its time (trace_id=%s)", ref, why, traceID)
	}
	req := timerRequest{timer: ref, verb: verbTimerReschedule, traceID: traceID, sentAtMS: now}
	if err := rt.ActCtx.timerRequestSent(ctx, req, err); err != nil {
		log.Printf("wf: timer %s: %v", ref, err)
	}
}

// takeTimerRequestNotice takes the router's UNREACHABLE or TTL_EXCEEDED of a timer request the node
// waits for SY.timer to answer: the request never reached SY.timer, and its timer goes again. It
// reports whether msg was one. Like any notice it is never answered (FINDINGS A-112); only one the
// router made counts, with no routing.src_l2_name: a node's message always carries its own.
func (rt *NodeRuntime) takeTimerRequestNotice(ctx context.Context, msg sdk.Message) bool {
	name := rawMsgName(msg)
	if strings.TrimSpace(msg.SourceL2Name()) != "" || (name != sdk.MSGUnreachable && name != sdk.MSGTTLExceeded) {
		return false
	}
	req, waited := rt.ActCtx.Requests.answered(msg.Routing.TraceID)
	if !waited {
		return false
	}
	rt.timerRequestRefused(ctx, req, "the router answered "+routerNotice(name, msg.Payload))
	return true
}

// routerNotice is the router's UNREACHABLE or TTL_EXCEEDED as a log line names it, with what it says,
// as the Go SDK's RPC error does: "UNREACHABLE reason=LINK_BUSY original_dst=SY.timer@motherbee".
func routerNotice(name string, payload json.RawMessage) string {
	var notice struct {
		Reason      string `json:"reason"`
		OriginalDst string `json:"original_dst"`
		LastHop     string `json:"last_hop"`
	}
	_ = json.Unmarshal(payload, &notice)
	text := name
	for _, field := range []struct{ key, value string }{
		{"reason", notice.Reason},
		{"original_dst", notice.OriginalDst},
		{"last_hop", notice.LastHop},
	} {
		if field.value != "" {
			text += " " + field.key + "=" + field.value
		}
	}
	return text
}

// takeTimerAnswer takes SY.timer's TIMER_RESPONSE to a request the node sent without waiting, never
// answered: taken only from SY.timer of the node's hive, the one its timers are on. An answer the node
// waits for settles its timer: one SY.timer took is no longer refused, and one it refused goes again.
// A schedule SY.timer answers already_existed, holding a timer under the client_ref already, is
// taken at the time SY.timer holds it (timerHeld): one later than the index's is moved to its time.
// Any other answer (to a cancel, or to a request a later one about the same timer superseded) decides
// nothing; a refusal among them is logged.
func (rt *NodeRuntime) takeTimerAnswer(ctx context.Context, msg sdk.Message) {
	authority := timerAuthority(rt.NodeName)
	if sender := msg.SourceL2Name(); sender != authority {
		log.Printf("wf: TIMER_RESPONSE from %q dropped (trace_id=%s): taken only from %s", sender, msg.Routing.TraceID, authority)
		return
	}
	answer, err := sdk.ParseTimerResponse(&msg)
	if err != nil {
		log.Printf("wf: TIMER_RESPONSE dropped (trace_id=%s): %v", msg.Routing.TraceID, err)
		return
	}
	refusal := answer.ResponseError()
	req, waited := rt.ActCtx.Requests.answered(msg.Routing.TraceID)
	switch {
	case !waited:
		if refusal != nil {
			log.Printf("wf: %s refused %s (trace_id=%s): %v", authority, answer.Verb, msg.Routing.TraceID, refusal)
		}
	case refusal == nil && answer.AlreadyExisted != nil && *answer.AlreadyExisted && answer.FireAtUTCMS != nil:
		rt.timerHeld(ctx, req.timer, *answer.FireAtUTCMS)
	case refusal == nil:
		rt.timerTaken(ctx, req.timer)
	default:
		rt.timerRequestRefused(ctx, req, fmt.Sprintf("%s answered %v", authority, refusal))
	}
}

// tickTimerRequests is the receive loop's tick while the node waits for something from SY.timer: the
// requests SY.timer did not answer in timerAnswerTimeout are refused, and the refused timers due go
// again (sendRefusedTimersAgain). Each tick takes maxTimerRequestsPerTick of each at most.
func (rt *NodeRuntime) tickTimerRequests(ctx context.Context) {
	if rt.Store == nil || rt.Registry == nil || rt.ActCtx.Timer == nil {
		return
	}
	rt.noteDroppedTimerAnswers()
	now := nowMS(rt.ActCtx.Clock)
	for _, req := range rt.ActCtx.Requests.unanswered(now-timerAnswerTimeout.Milliseconds(), maxTimerRequestsPerTick) {
		rt.timerRequestRefused(ctx, req, fmt.Sprintf("no answer from %s in %s", timerAuthority(rt.NodeName), timerAnswerTimeout))
	}
	rt.sendRefusedTimersAgain(ctx, now)
	rt.countRefusedTimers(ctx)
}

// noteDroppedTimerAnswers logs the answers of SY.timer the SDK dropped since the tick before, the
// node's channel for them full (64 at once): the requests they answered are taken as refused once
// timerAnswerTimeout passes, and their timers go again, so the log tells those from SY.timer's own
// refusals (FINDINGS A-254's review).
func (rt *NodeRuntime) noteDroppedTimerAnswers() {
	if rt.timerAnswerDrops == nil {
		return
	}
	if dropped := rt.timerAnswerDrops(); dropped > rt.timerAnswersDropped {
		log.Printf("wf: the SDK dropped %d answer(s) of %s, the node's channel for them full: the requests they answered are taken as refused %s after they went, and their timers go again",
			dropped-rt.timerAnswersDropped, timerAuthority(rt.NodeName), timerAnswerTimeout)
		rt.timerAnswersDropped = dropped
	}
}

// sendRefusedTimersAgain sends again the refused timers due at nowMS, the earliest first: a timer
// whose time has come is fired by the node itself, as the reconcile fires one lost while the node was
// down (SY.timer, which never took it, has no fire to give); one less than 60 s from its time waits
// for it, since SY.timer would fire it late (FINDINGS A-254's review); a timer of an instance that no
// longer runs leaves the index.
func (rt *NodeRuntime) sendRefusedTimersAgain(ctx context.Context, nowMS int64) {
	due, err := rt.Store.RefusedTimersDue(ctx, nowMS, maxTimerRequestsPerTick)
	if err != nil {
		log.Printf("wf: read the refused timers: %v", err)
		return
	}
	for _, row := range due {
		ref := timerRef{row.InstanceID, row.TimerKey}
		inst, running := rt.Registry.Get(row.InstanceID)
		switch {
		case !running:
			_ = rt.Store.DeleteTimer(ctx, row.InstanceID, row.TimerKey)
		case row.FireAtMS <= nowMS:
			// The timer waits its delay first: a fire that cannot be given (its journal failing) is
			// tried again then, not on every tick. One given takes the row with it.
			if err := rt.Store.TimerSentAgain(ctx, row.InstanceID, row.TimerKey, row.Resends+1, nowMS+timerResendDelay(row.Resends).Milliseconds()); err != nil {
				log.Printf("wf: timer %s: %v", ref, err)
			}
			log.Printf("wf: timer %s, which SY.timer has not taken (%s), is due: the instance takes its fire now", ref, row.Refusal)
			rt.fireMissedTimer(ctx, inst, row, "")
		case row.FireAtMS-nowMS < minTimerDuration.Milliseconds():
			// SY.timer would take it for 60 s from now, its floor, and fire it late: the node fires it
			// itself at its time, as it would once that came (FINDINGS A-254's review).
			if err := rt.Store.RefuseTimer(ctx, row.InstanceID, row.TimerKey, row.Refusal, row.FireAtMS); err != nil {
				log.Printf("wf: timer %s: %v", ref, err)
				continue
			}
			log.Printf("wf: timer %s, which SY.timer has not taken (%s), is less than %s from its time: the node fires it itself at %s",
				ref, row.Refusal, minTimerDuration, msTime(row.FireAtMS))
		default:
			rt.sendTimerAgain(ctx, row, nowMS)
		}
	}
}

// sendTimerAgain sends a refused timer again, 60 s from its time at least (sendRefusedTimersAgain): a
// TIMER_SCHEDULE at its fire_at, under its client_ref, with SY.timer's default missed_policy, fire,
// as scheduleAgain does. SY.timer keeps one pending timer per client_ref, so a timer it holds already
// is not doubled. Its refusal stays until SY.timer takes it.
func (rt *NodeRuntime) sendTimerAgain(ctx context.Context, row TimerRow, nowMS int64) {
	ref := timerRef{row.InstanceID, row.TimerKey}
	resends := row.Resends + 1
	if err := rt.Store.TimerSentAgain(ctx, row.InstanceID, row.TimerKey, resends, resendTime(nowMS, resends, row.FireAtMS)); err != nil {
		log.Printf("wf: timer %s not sent again: %v", ref, err)
		return
	}
	in := time.Duration(row.FireAtMS-nowMS) * time.Millisecond
	traceID, err := rt.ActCtx.Timer.ScheduleIn(ctx, in, timerScheduleOptions(rt.ActCtx.Dispatcher.NodeL2Name(), row.InstanceID, row.TimerKey))
	if err == nil {
		log.Printf("wf: timer %s sent again to SY.timer (resend %d, trace_id=%s) after %s", ref, resends, traceID, row.Refusal)
	}
	req := timerRequest{timer: ref, verb: verbTimerSchedule, traceID: traceID, sentAtMS: nowMS}
	if err := rt.ActCtx.timerRequestSent(ctx, req, err); err != nil {
		log.Printf("wf: timer %s: %v", ref, err)
	}
}

// countRefusedTimers counts the refused timers of the index again: the loop's tick goes on while
// there are any.
func (rt *NodeRuntime) countRefusedTimers(ctx context.Context) {
	if rt.ActCtx.Requests == nil || rt.Store == nil {
		return
	}
	count, _, err := rt.Store.RefusedTimers(ctx)
	if err != nil {
		log.Printf("wf: count the refused timers: %v", err)
		return
	}
	rt.ActCtx.Requests.refused = count > 0
}

// refusedTimersDegraded is why a node with refused timers is DEGRADED, nil when it has none: its
// timers are not reconciled with SY.timer, and the message says how many, and the first to go again.
// The index says, whatever the loop counted last.
func (rt *NodeRuntime) refusedTimersDegraded(ctx context.Context) *sdk.ControlPlaneError {
	if rt.Store == nil {
		return nil
	}
	count, first, err := rt.Store.RefusedTimers(ctx)
	switch {
	case err != nil:
		return &sdk.ControlPlaneError{Code: errCodeTimersNotReconciled, Message: fmt.Sprintf("the refused timers could not be read: %v", err)}
	case count == 0:
		return nil
	}
	return &sdk.ControlPlaneError{Code: errCodeTimersNotReconciled, Message: fmt.Sprintf(
		"%d timer(s) not taken by %s yet, sent again until it takes them; the next, %s/%s at %s: %s",
		count, timerAuthority(rt.NodeName), first.InstanceID, first.TimerKey, msTime(first.ResendAtMS), first.Refusal)}
}
