//go:build linux

package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// The delivery of a fire (A-115): saved before it goes, sent again while the router answers that
// its target is not connected, and only while it is on time.

// fastRetries makes every retry wait a millisecond.
func fastRetries(t *testing.T) {
	t.Helper()
	saved := deliveryRetryDelays
	deliveryRetryDelays = []time.Duration{time.Millisecond}
	t.Cleanup(func() { deliveryRetryDelays = saved })
}

// insertDueTimer writes a one-shot of WF.demo that is due, under policy.
func insertDueTimer(t *testing.T, service *Service, timerUUID, policy string) {
	t.Helper()
	row := timerRow{
		UUID:         timerUUID,
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(-time.Second).UnixMilli(),
		MissedPolicy: policy,
		Payload:      `{"ticket_id":1234}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().Add(-time.Minute).UnixMilli(),
	}
	if policy == "fire_if_within" {
		row.MissedWithinMS = sql.NullInt64{Int64: time.Hour.Milliseconds(), Valid: true}
	}
	insertOwnedTimer(t, service.db, row)
}

// mustReadFire reads the next TIMER_FIRED the service sent: its trace_id and its payload.
func mustReadFire(t *testing.T, tx chan []byte) (string, fluxbeesdk.FiredEvent) {
	t.Helper()
	var msg fluxbeesdk.Message
	if err := json.Unmarshal(mustReadSentFrame(t, tx), &msg); err != nil {
		t.Fatalf("decode sent message: %v", err)
	}
	if msg.Meta.Msg == nil || *msg.Meta.Msg != fluxbeesdk.MsgTimerFired {
		t.Fatalf("sent %+v, not a TIMER_FIRED", msg.Meta)
	}
	var fired fluxbeesdk.FiredEvent
	if err := json.Unmarshal(msg.Payload, &fired); err != nil {
		t.Fatalf("decode TIMER_FIRED: %v", err)
	}
	return msg.Routing.TraceID, fired
}

// unreachable is the router's answer to a message of traceID whose target it could not reach.
func unreachable(t *testing.T, traceID, reason string) fluxbeesdk.Message {
	t.Helper()
	msg, err := fluxbeesdk.BuildSystemMessage(
		"33333333-3333-3333-3333-333333333333",
		fluxbeesdk.UnicastDestination("timer-node-uuid"),
		16,
		traceID,
		fluxbeesdk.MSGUnreachable,
		map[string]any{"original_dst": "WF.demo@motherbee", "reason": reason},
	)
	if err != nil {
		t.Fatalf("build UNREACHABLE: %v", err)
	}
	return msg
}

func queuedRetries(s *timerScheduler) int {
	s.mu.Lock()
	defer s.mu.Unlock()
	n := 0
	for _, entry := range s.items {
		if entry.retry != nil {
			n++
		}
	}
	return n
}

// fireOnce fires the due timer timerUUID and returns the trace_id of its TIMER_FIRED.
func fireOnce(t *testing.T, service *Service, tx chan []byte, timerUUID string) string {
	t.Helper()
	service.scheduler.push(timerUUID, time.Now().UTC().UnixMilli())
	service.scheduler.processDue(context.Background(), time.Now().UTC())
	traceID, fired := mustReadFire(t, tx)
	if fired.TimerUUID != timerUUID || fired.FireCount != 1 {
		t.Fatalf("unexpected fire: %+v", fired)
	}
	return traceID
}

func TestAOneShotIsSavedFiredBeforeItsFireGoes(t *testing.T) {
	fastRetries(t)
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	insertDueTimer(t, service, "timer-1", "fire")
	sender := service.sender.(*stubSender)
	sender.refuse, sender.err = 1, errors.New("router link down")

	service.scheduler.push("timer-1", time.Now().UTC().UnixMilli())
	service.scheduler.processDue(context.Background(), time.Now().UTC())

	row, err := getTimer(context.Background(), service.db, "timer-1")
	if err != nil {
		t.Fatalf("load timer: %v", err)
	}
	if row.Status != "fired" || row.FireCount != 1 {
		t.Fatalf("the one-shot must be saved fired before its fire goes: %+v", row)
	}
	if queuedRetries(service.scheduler) != 1 {
		t.Fatalf("a fire the link did not take must be sent again")
	}
	service.scheduler.processDue(context.Background(), time.Now().UTC().Add(time.Second))
	if _, fired := mustReadFire(t, tx); fired.TimerUUID != "timer-1" || fired.FireCount != 1 {
		t.Fatalf("unexpected retry: %+v", fired)
	}
}

func TestAFireItsTargetDidNotTakeIsSentAgain(t *testing.T) {
	fastRetries(t)
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	insertDueTimer(t, service, "timer-1", "fire")
	first := fireOnce(t, service, tx, "timer-1")

	if err := service.handleMessage(unreachable(t, first, "NODE_NOT_FOUND")); err != nil {
		t.Fatalf("handle UNREACHABLE: %v", err)
	}
	if len(tx) != 0 {
		t.Fatalf("answered the router's UNREACHABLE")
	}
	service.scheduler.processDue(context.Background(), time.Now().UTC().Add(time.Second))
	again, fired := mustReadFire(t, tx)
	if again == first || fired.TimerUUID != "timer-1" || fired.FireCount != 1 {
		t.Fatalf("the retry must be the same fire under a new trace_id: %s %+v", again, fired)
	}
	row, err := getTimer(context.Background(), service.db, "timer-1")
	if err != nil {
		t.Fatalf("load timer: %v", err)
	}
	if row.Status != "fired" || row.FireCount != 1 {
		t.Fatalf("a retry saves nothing: %+v", row)
	}

	// The first attempt's UNREACHABLE was taken: a second one is only a notice.
	if err := service.handleMessage(unreachable(t, first, "NODE_NOT_FOUND")); err != nil {
		t.Fatalf("handle UNREACHABLE: %v", err)
	}
	if queuedRetries(service.scheduler) != 0 {
		t.Fatalf("an UNREACHABLE of no fire in flight queued a retry")
	}
}

// A fire the router answers UNREACHABLE LINK_BUSY, its target's link queue full, is sent again as
// one behind a link that is down (WAN_UNAVAILABLE) is: the refusal clears as the target reads (the
// router from 0.1.129, FINDINGS A-222). Before, it was refused for good, and the fire lost.
func TestAFireRefusedLinkBusyIsSentAgain(t *testing.T) {
	for _, reason := range []string{"LINK_BUSY", "WAN_UNAVAILABLE"} {
		t.Run(reason, func(t *testing.T) {
			fastRetries(t)
			service, tx := newTestService(t)
			service.scheduler = newTimerScheduler(service)
			insertDueTimer(t, service, "timer-1", "fire")
			first := fireOnce(t, service, tx, "timer-1")
			if err := service.handleMessage(unreachable(t, first, reason)); err != nil {
				t.Fatalf("handle UNREACHABLE: %v", err)
			}
			if queuedRetries(service.scheduler) != 1 {
				t.Fatalf("a fire refused %s must be sent again", reason)
			}
			service.scheduler.processDue(context.Background(), time.Now().UTC().Add(time.Second))
			if again, fired := mustReadFire(t, tx); again == first || fired.TimerUUID != "timer-1" || fired.FireCount != 1 {
				t.Fatalf("the retry must be the same fire under a new trace_id: %s %+v", again, fired)
			}
		})
	}
}

// notTaken is the NOT_TAKEN that from answers a fire of traceID with: its Go SDK's queue full
// (FINDINGS A-329), retryable or not.
func notTaken(t *testing.T, traceID, from string, retryable bool) fluxbeesdk.Message {
	t.Helper()
	fire := fluxbeesdk.Message{Routing: fluxbeesdk.Routing{Src: "timer-node-uuid", TraceID: traceID}}
	msg, err := fluxbeesdk.BuildNotTaken("wf-node-uuid", fire, fluxbeesdk.NotTakenReasonNodeBusy,
		"WF.demo@motherbee holds 1024 system messages, all it can take now; send it again later", retryable)
	if err != nil {
		t.Fatalf("build NOT_TAKEN: %v", err)
	}
	msg.Routing.SrcL2Name = &from
	return msg
}

// FINDINGS A-329: a fire its target answers NOT_TAKEN, retryable (its Go SDK's queue full), is sent
// again while it is on time, as one whose target was not connected is; one not retryable is not.
// Only the fire's target decides it, and a NOT_TAKEN is never answered. Before, a NOT_TAKEN got a
// TIMER_UNKNOWN_VERB, and the fire was lost. The log says what happened to the fire: "sent again"
// only for a fire queued to go again, never for one no longer on time (the review: a drop
// one-shot's line said "sent again", the next one "lost").
func TestAFireItsTargetAnsweredNotTakenIsSentAgain(t *testing.T) {
	const resent = "fire of timer-1 not taken by WF.demo@motherbee (node_busy): sent again at "
	for _, tc := range []struct {
		name, policy, from string
		retryable, retried bool
		// logged is a line SY.timer's log must hold, and never what it must not.
		logged, never string
	}{
		{name: "retryable", policy: "fire", from: "WF.demo@motherbee", retryable: true, retried: true, logged: resent},
		{name: "retryable, inside its window", policy: "fire_if_within", from: "WF.demo@motherbee", retryable: true, retried: true, logged: resent},
		{name: "retryable, a drop one-shot", policy: "drop", from: "WF.demo@motherbee", retryable: true,
			logged: "fire of timer-1 lost: WF.demo@motherbee did not take it in 1 attempts (NOT_TAKEN node_busy)", never: "sent again"},
		{name: "not retryable", policy: "fire", from: "WF.demo@motherbee",
			logged: "fire of timer-1 not taken by WF.demo@motherbee (node_busy, not retryable): not sent again", never: "sent again at"},
		{name: "from another node", policy: "fire", from: "WF.other@motherbee", retryable: true, never: "not taken by"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			fastRetries(t)
			logged := captureLog(t)
			service, tx := newTestService(t)
			service.scheduler = newTimerScheduler(service)
			insertDueTimer(t, service, "timer-1", tc.policy)
			first := fireOnce(t, service, tx, "timer-1")
			if err := service.handleMessage(notTaken(t, first, tc.from, tc.retryable)); err != nil {
				t.Fatalf("handle NOT_TAKEN: %v", err)
			}
			if len(tx) != 0 {
				t.Fatalf("answered a NOT_TAKEN: %s", <-tx)
			}
			if retried := queuedRetries(service.scheduler) == 1; retried != tc.retried {
				t.Fatalf("retried=%v, want %v", retried, tc.retried)
			}
			if text := logged.String(); !strings.Contains(text, tc.logged) || (tc.never != "" && strings.Contains(text, tc.never)) {
				t.Fatalf("want a line with %q and none with %q, got:\n%s", tc.logged, tc.never, text)
			}
			if !tc.retried {
				return
			}
			service.scheduler.processDue(context.Background(), time.Now().UTC().Add(time.Second))
			again, fired := mustReadFire(t, tx)
			if again == first || fired.TimerUUID != "timer-1" || fired.FireCount != 1 {
				t.Fatalf("the retry must be the same fire under a new trace_id: %s %+v", again, fired)
			}
			// The second attempt is answered NOT_TAKEN too: sent a third time.
			if err := service.handleMessage(notTaken(t, again, tc.from, true)); err != nil {
				t.Fatalf("handle NOT_TAKEN: %v", err)
			}
			service.scheduler.processDue(context.Background(), time.Now().UTC().Add(time.Second))
			if third, fired := mustReadFire(t, tx); third == again || fired.TimerUUID != "timer-1" {
				t.Fatalf("the third attempt: %s %+v", third, fired)
			}
		})
	}
}

// FINDINGS A-329: a NOT_TAKEN another node sends on a fire's trace leaves the fire waiting for its
// target's own answer, and one for no fire in flight is dropped, never answered.
func TestANotTakenOfAnotherNodeLeavesTheFireWaiting(t *testing.T) {
	fastRetries(t)
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	insertDueTimer(t, service, "timer-1", "fire")
	first := fireOnce(t, service, tx, "timer-1")
	if err := service.handleMessage(notTaken(t, first, "WF.other@motherbee", true)); err != nil {
		t.Fatalf("handle NOT_TAKEN: %v", err)
	}
	if err := service.handleMessage(notTaken(t, first, "WF.demo@motherbee", true)); err != nil {
		t.Fatalf("handle NOT_TAKEN: %v", err)
	}
	if queuedRetries(service.scheduler) != 1 || len(tx) != 0 {
		t.Fatalf("the target's NOT_TAKEN sends the fire again, and nothing answers either: %d retries, %d sent", queuedRetries(service.scheduler), len(tx))
	}
	if err := service.handleMessage(notTaken(t, "no-fire-trace", "WF.demo@motherbee", true)); err != nil {
		t.Fatalf("handle NOT_TAKEN: %v", err)
	}
	if queuedRetries(service.scheduler) != 1 || len(tx) != 0 {
		t.Fatalf("a NOT_TAKEN of no fire changed nothing, and is not answered")
	}
}

func TestAFireIsNotSentAgainWhenItWouldNotBeOnTime(t *testing.T) {
	for _, tc := range []struct {
		name, policy, reason string
		retried              bool
	}{
		{name: "fire, target not connected", policy: "fire", reason: "NODE_NOT_FOUND", retried: true},
		{name: "fire_if_within, inside its window", policy: "fire_if_within", reason: "PEER_UNAVAILABLE", retried: true},
		{name: "drop", policy: "drop", reason: "NODE_NOT_FOUND"},
		{name: "refused by the router", policy: "fire", reason: "VPN_BLOCKED"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			fastRetries(t)
			service, tx := newTestService(t)
			service.scheduler = newTimerScheduler(service)
			insertDueTimer(t, service, "timer-1", tc.policy)
			traceID := fireOnce(t, service, tx, "timer-1")
			if err := service.handleMessage(unreachable(t, traceID, tc.reason)); err != nil {
				t.Fatalf("handle UNREACHABLE: %v", err)
			}
			if retried := queuedRetries(service.scheduler) == 1; retried != tc.retried {
				t.Fatalf("retried=%v, want %v", retried, tc.retried)
			}
		})
	}
}

func TestARetryStopsWhenItsTimerIsPurged(t *testing.T) {
	fastRetries(t)
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	insertDueTimer(t, service, "timer-1", "fire")
	traceID := fireOnce(t, service, tx, "timer-1")
	if err := service.handleMessage(unreachable(t, traceID, "NODE_NOT_FOUND")); err != nil {
		t.Fatalf("handle UNREACHABLE: %v", err)
	}
	if _, _, err := deleteTimersByOwner(context.Background(), service.db, "WF.demo@motherbee"); err != nil {
		t.Fatalf("purge: %v", err)
	}
	service.scheduler.processDue(context.Background(), time.Now().UTC().Add(time.Second))
	if len(tx) != 0 {
		t.Fatalf("sent the fire of a purged timer")
	}
}

func TestDeliveryDeadline(t *testing.T) {
	first := time.Now().UTC()
	scheduled := first.Add(-2 * time.Minute)
	oneShot := func(policy string, withinMS int64) timerRow {
		row := timerRow{Kind: "oneshot", MissedPolicy: policy, FireAtUTC: scheduled.UnixMilli()}
		if withinMS > 0 {
			row.MissedWithinMS = sql.NullInt64{Int64: withinMS, Valid: true}
		}
		return row
	}
	for _, tc := range []struct {
		name string
		row  timerRow
		want time.Time
	}{
		{name: "fire", row: oneShot("fire", 0), want: first.Add(deliveryRetryWindow)},
		{name: "drop", row: oneShot("drop", 0), want: first},
		{name: "fire_if_within, a short window", row: oneShot("fire_if_within", (10 * time.Minute).Milliseconds()), want: scheduled.Add(10 * time.Minute)},
		{name: "fire_if_within, a long window", row: oneShot("fire_if_within", (3 * time.Hour).Milliseconds()), want: first.Add(deliveryRetryWindow)},
		{name: "recurring", row: timerRow{Kind: "recurring", MissedPolicy: "drop"}, want: first.Add(deliveryRetryWindow)},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := deliveryDeadline(tc.row, first); !got.Equal(tc.want.Truncate(time.Millisecond)) && !got.Equal(tc.want) {
				t.Fatalf("got %v, want %v", got, tc.want)
			}
		})
	}
}
