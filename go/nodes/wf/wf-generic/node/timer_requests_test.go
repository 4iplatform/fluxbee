package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// deadlineWorkflowJSON waits for DONE under a deadline two hours away: MOVE moves the deadline to
// three hours from then, DROP cancels it, and its fire ends the instance expired.
const deadlineWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "deadline",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["done", "expired"],
  "states": [
    {
      "name": "waiting",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "deadline", "fire_in": "2h"}],
      "transitions": [
        {"event_match": {"msg": "MOVE"}, "guard": "true", "actions": [{"type": "reschedule_timer", "timer_key": "deadline", "fire_in": "3h"}]},
        {"event_match": {"msg": "DROP"}, "guard": "true", "actions": [{"type": "cancel_timer", "timer_key": "deadline"}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'deadline'", "target_state": "expired"},
        {"event_match": {"msg": "DONE"}, "guard": "true", "target_state": "done"}
      ]
    },
    {"name": "done"},
    {"name": "expired"}
  ]
}`

// refusalTest is a node on deadlineWorkflowJSON whose timer requests wait for SY.timer's answer
// (FINDINGS A-254), on a clock the test moves.
type refusalTest struct {
	t     *testing.T
	rt    *NodeRuntime
	disp  *mockDispatcher
	timer *mockTimerSender
	now   time.Time
}

func newRefusalTest(t *testing.T) *refusalTest {
	t.Helper()
	return newRefusalTestOn(t, deadlineWorkflowJSON)
}

// newRefusalTestOn is a refusalTest on definition.
func newRefusalTestOn(t *testing.T, definition string) *refusalTest {
	t.Helper()
	timer := &mockTimerSender{}
	rt, disp := newTestRuntime(t, "", definition, timer)
	rt.Plane = bootstrap(t, writeNodeConfig(t, spawnConfig(writeWorkflowPackage(t, definition))))
	rt.ActCtx.Requests = newTimerRequests()
	tc := &refusalTest{t: t, rt: rt, disp: disp, timer: timer, now: fixedClock()}
	rt.ActCtx.Clock = func() time.Time { return tc.now }
	return tc
}

func (tc *refusalTest) later(d time.Duration) { tc.now = tc.now.Add(d) }

// start starts an instance, which schedules its deadline, and returns it with the trace_id its
// schedule went under.
func (tc *refusalTest) start() (string, string) {
	tc.t.Helper()
	instanceID := startInstance(tc.t, tc.rt, from("IO.in@motherbee", msgWithName("START")))
	return instanceID, tc.timer.lastTrace(tc.t)
}

// tell gives the instance a message named name from its IO node, on its thread.
func (tc *refusalTest) tell(instanceID, name string) {
	tc.t.Helper()
	msg := from("IO.in@motherbee", msgWithName(name))
	msg.Meta.ThreadID = threadOf(tc.t, tc.rt, instanceID)
	tc.dispatch(msg)
}

func (tc *refusalTest) dispatch(msg sdk.Message) {
	tc.t.Helper()
	if err := Dispatch(context.Background(), msg, tc.rt); err != nil {
		tc.t.Fatalf("Dispatch %s: %v", rawMsgName(msg), err)
	}
}

func (tc *refusalTest) tick() { tc.rt.tickTimerRequests(context.Background()) }

// deadline is the instance's deadline as the index keeps it.
func (tc *refusalTest) deadline(instanceID string) (TimerRow, bool) {
	tc.t.Helper()
	return tc.timerRow(instanceID, "deadline")
}

// timerRow is the instance's timer timerKey as the index keeps it.
func (tc *refusalTest) timerRow(instanceID, timerKey string) (TimerRow, bool) {
	tc.t.Helper()
	row, ok, err := tc.rt.Store.GetTimerForInstance(context.Background(), instanceID, timerKey)
	if err != nil {
		tc.t.Fatal(err)
	}
	return row, ok
}

// health is the node's NODE_STATUS_GET: its health_state, and its last_error's code and message.
func (tc *refusalTest) health() (string, string, string) {
	tc.t.Helper()
	statusGet := sdk.MSGNodeStatusGet
	tc.dispatch(from("SY.orchestrator@motherbee", sdk.Message{Routing: sdk.Routing{Src: "orchestrator-uuid", TraceID: "t-status"}, Meta: sdk.Meta{MsgType: "system", Msg: &statusGet}}))
	var reply struct {
		Health    string                 `json:"health_state"`
		LastError *sdk.ControlPlaneError `json:"last_error"`
	}
	if err := json.Unmarshal(tc.disp.sent[len(tc.disp.sent)-1].Payload, &reply); err != nil {
		tc.t.Fatal(err)
	}
	if reply.LastError == nil {
		return reply.Health, "", ""
	}
	return reply.Health, reply.LastError.Code, reply.LastError.Message
}

// routerRefusal is the router's notice name of a message on traceID, as the router makes it: with
// no routing.src_l2_name.
func routerRefusal(name, traceID string, payload map[string]any) sdk.Message {
	raw, _ := json.Marshal(payload)
	msgName := name
	return sdk.Message{
		Routing: sdk.Routing{Src: "router-uuid", Dst: sdk.UnicastDestination("wf-uuid"), TTL: 16, TraceID: traceID},
		Meta:    sdk.Meta{MsgType: sdk.SYSTEMKind, Msg: &msgName},
		Payload: raw,
	}
}

// timerAnswer is SY.timer's TIMER_RESPONSE on traceID, as SY.timer of the node's hive sends it.
func timerAnswer(traceID string, payload map[string]any) sdk.Message {
	raw, _ := json.Marshal(payload)
	msgName := sdk.MsgTimerResponse
	return from("SY.timer@motherbee", sdk.Message{
		Routing: sdk.Routing{Src: "timer-uuid", Dst: sdk.UnicastDestination("wf-uuid"), TTL: 16, TraceID: traceID},
		Meta:    sdk.Meta{MsgType: sdk.SYSTEMKind, Msg: &msgName},
		Payload: raw,
	})
}

func timerTook(traceID string) sdk.Message {
	return timerAnswer(traceID, map[string]any{"ok": true, "verb": verbTimerSchedule, "timer_uuid": "timer-1", "already_existed": false})
}

// timerHeldAt is SY.timer's answer to a schedule under a client_ref it holds a pending timer for
// already, at fireAt: it keeps one per client_ref, and answers already_existed with the time it holds.
func timerHeldAt(traceID string, fireAt time.Time) sdk.Message {
	return timerAnswer(traceID, map[string]any{"ok": true, "verb": verbTimerSchedule, "timer_uuid": "timer-1",
		"fire_at_utc_ms": fireAt.UnixMilli(), "already_existed": true})
}

// timerMoved is SY.timer's answer to a TIMER_RESCHEDULE it took, the timer moved to fireAt.
func timerMoved(traceID string, fireAt time.Time) sdk.Message {
	return timerAnswer(traceID, map[string]any{"ok": true, "verb": verbTimerReschedule, "timer_uuid": "timer-1",
		"fire_at_utc_ms": fireAt.UnixMilli(), "found": true})
}

func timerRefused(traceID, verb, code, message string) sdk.Message {
	return timerAnswer(traceID, map[string]any{"ok": false, "verb": verb, "error": map[string]any{"code": code, "message": message}})
}

func unreachable(traceID, reason string) sdk.Message {
	return routerRefusal(sdk.MSGUnreachable, traceID, map[string]any{"original_dst": "SY.timer@motherbee", "reason": reason})
}

// notTakenBy is sender's NOT_TAKEN for a message on traceID: its Go SDK's queue full (FINDINGS
// A-329).
func notTakenBy(sender, traceID string) sdk.Message {
	request := sdk.Message{Routing: sdk.Routing{Src: "wf-uuid", TraceID: traceID}}
	msg, _ := sdk.BuildNotTaken("sender-uuid", request, sdk.NotTakenReasonNodeBusy,
		sender+" holds 1024 system messages, all it can take now; send it again later", true)
	return from(sender, msg)
}

// FINDINGS A-254: a schedule SY.timer does not take is no longer lost. The router's UNREACHABLE
// (SY.timer restarting, NODE_NOT_FOUND; its link full, LINK_BUSY), its TTL_EXCEEDED, or SY.timer's
// own refusal marks the timer in the index, never answered; the node is DEGRADED, naming it; the timer
// goes again 5 s later, at its time; and once SY.timer takes it the node is healthy again. Before, the
// index kept a timer SY.timer did not have, and the instance waited until the node's next start.
func TestAScheduleSYTimerDoesNotTakeGoesAgainUntilItDoes(t *testing.T) {
	for _, c := range []struct {
		name    string
		refusal func(traceID string) sdk.Message
		says    string
	}{
		{"SY.timer restarting", func(traceID string) sdk.Message { return unreachable(traceID, "NODE_NOT_FOUND") },
			"the router answered UNREACHABLE reason=NODE_NOT_FOUND original_dst=SY.timer@motherbee"},
		{"SY.timer's link full", func(traceID string) sdk.Message { return unreachable(traceID, "LINK_BUSY") },
			"the router answered UNREACHABLE reason=LINK_BUSY original_dst=SY.timer@motherbee"},
		{"the ttl run out", func(traceID string) sdk.Message {
			return routerRefusal(sdk.MSGTTLExceeded, traceID, map[string]any{"original_dst": "SY.timer@motherbee", "last_hop": "router:r1"})
		}, "the router answered TTL_EXCEEDED original_dst=SY.timer@motherbee last_hop=router:r1"},
		{"SY.timer's own refusal", func(traceID string) sdk.Message {
			return timerRefused(traceID, verbTimerSchedule, "TIMER_STORAGE_ERROR", "disk I/O error")
		}, "SY.timer@motherbee answered TIMER_STORAGE_ERROR: disk I/O error"},
		// FINDINGS A-329: SY.timer's queue full, its Go SDK's NOT_TAKEN; before, the WF dropped it and
		// took the request as refused only once its 30 s passed.
		{"SY.timer's queue full", func(traceID string) sdk.Message { return notTakenBy("SY.timer@motherbee", traceID) },
			"SY.timer@motherbee answered NOT_TAKEN node_busy"},
	} {
		t.Run(c.name, func(t *testing.T) {
			tc := newRefusalTest(t)
			instanceID, traceID := tc.start()
			fireAt := tc.now.Add(2 * time.Hour).UnixMilli()
			if health, _, _ := tc.health(); health != "HEALTHY" {
				t.Fatalf("a schedule waiting for its answer is no trouble: %s", health)
			}

			sent := len(tc.disp.sent)
			tc.dispatch(c.refusal(traceID))
			if len(tc.disp.sent) != sent {
				t.Fatalf("a refusal is never answered: %+v", tc.disp.sent[sent:])
			}
			row, ok := tc.deadline(instanceID)
			if !ok || row.FireAtMS != fireAt || row.Refusal != "TIMER_SCHEDULE: "+c.says+" (trace_id="+traceID+")" ||
				row.ResendAtMS != tc.now.Add(5*time.Second).UnixMilli() || row.Resends != 0 {
				t.Fatalf("the index marks the timer refused, to go again in 5 s: %+v", row)
			}
			health, code, message := tc.health()
			if health != "DEGRADED" || code != errCodeTimersNotReconciled ||
				!strings.Contains(message, "1 timer(s) not taken by SY.timer@motherbee") || !strings.Contains(message, instanceID+"/deadline") ||
				!strings.Contains(message, c.says) {
				t.Fatalf("NODE_STATUS_GET names the refused timer: %s %s %q", health, code, message)
			}

			scheduled := len(tc.timer.scheduled)
			tc.later(4 * time.Second)
			tc.tick()
			if len(tc.timer.scheduled) != scheduled {
				t.Fatalf("the timer waits its 5 s: %v", tc.timer.scheduled[scheduled:])
			}
			tc.later(time.Second)
			tc.tick()
			ref := timerClientRef(instanceID, "deadline")
			if len(tc.timer.scheduled) != scheduled+1 || tc.timer.scheduled[scheduled] != ref ||
				tc.timer.in[len(tc.timer.in)-1] != time.Duration(fireAt-tc.now.UnixMilli())*time.Millisecond {
				t.Fatalf("the timer goes again as %s, at its time: %v %v", ref, tc.timer.scheduled[scheduled:], tc.timer.in)
			}
			if row, _ := tc.deadline(instanceID); row.Refusal == "" || row.Resends != 1 || row.FireAtMS != fireAt {
				t.Fatalf("its refusal stays until SY.timer takes it: %+v", row)
			}

			tc.dispatch(timerTook(tc.timer.lastTrace(t)))
			if row, _ := tc.deadline(instanceID); row.Refusal != "" || row.Resends != 0 || row.ResendAtMS != 0 || row.FireAtMS != fireAt {
				t.Fatalf("SY.timer took it: %+v", row)
			}
			if health, code, _ := tc.health(); health != "HEALTHY" {
				t.Fatalf("the node is healthy again: %s %s", health, code)
			}
			tc.tick()
			if tc.rt.ActCtx.Requests.waiting() {
				t.Fatalf("nothing waits for SY.timer any more: %+v", tc.rt.ActCtx.Requests)
			}
			if len(tc.disp.sent) != sent+2 {
				t.Fatalf("the node sent only its two status replies: %d messages", len(tc.disp.sent)-sent)
			}
		})
	}
}

// A refused timer waits longer each time it is refused again: 5, 10, 20 and 40 s, then each minute,
// as SY.timer sends again a fire it could not deliver; never more.
func TestARefusedTimerWaitsLongerEachTimeUpToAMinute(t *testing.T) {
	tc := newRefusalTest(t)
	instanceID, traceID := tc.start()
	tc.dispatch(unreachable(traceID, "NODE_NOT_FOUND"))
	for i, wait := range []time.Duration{5 * time.Second, 10 * time.Second, 20 * time.Second, 40 * time.Second, time.Minute, time.Minute} {
		row, _ := tc.deadline(instanceID)
		if row.ResendAtMS != tc.now.Add(wait).UnixMilli() || row.Resends != i {
			t.Fatalf("refusal %d: the timer goes again in %s: %+v", i+1, wait, row)
		}
		sent := len(tc.timer.traces)
		tc.later(wait)
		tc.tick()
		if len(tc.timer.traces) != sent+1 {
			t.Fatalf("refusal %d: the timer went again", i+1)
		}
		tc.dispatch(unreachable(tc.timer.lastTrace(t), "NODE_NOT_FOUND"))
	}
}

// A request SY.timer does not answer in 30 s is taken as refused (SY.timer dropped it under load, or
// its answer was lost): the timer goes again. A late answer to it decides nothing: the timer stays
// refused until SY.timer takes the request that went again.
func TestARequestSYTimerDoesNotAnswerIn30sIsRefused(t *testing.T) {
	tc := newRefusalTest(t)
	instanceID, traceID := tc.start()
	tc.later(29 * time.Second)
	tc.tick()
	if row, _ := tc.deadline(instanceID); row.Refusal != "" {
		t.Fatalf("29 s without an answer is no refusal: %+v", row)
	}
	tc.later(time.Second)
	tc.tick()
	row, _ := tc.deadline(instanceID)
	if row.Refusal != "TIMER_SCHEDULE: no answer from SY.timer@motherbee in 30s (trace_id="+traceID+")" ||
		row.ResendAtMS != tc.now.Add(5*time.Second).UnixMilli() {
		t.Fatalf("30 s without an answer is a refusal: %+v", row)
	}
	tc.dispatch(timerTook(traceID))
	if row, _ := tc.deadline(instanceID); row.Refusal == "" {
		t.Fatalf("a late answer decides nothing: %+v", row)
	}
	tc.later(5 * time.Second)
	tc.tick()
	tc.dispatch(timerTook(tc.timer.lastTrace(t)))
	if row, _ := tc.deadline(instanceID); row.Refusal != "" {
		t.Fatalf("SY.timer took the timer that went again: %+v", row)
	}
}

// A schedule that does not go at all (the send queue full during a reconnect) no longer fails the
// action, which left the instance going on without its timer: the timer is the instance's, marked to
// go again, and goes.
func TestAScheduleThatDoesNotGoIsSentAgain(t *testing.T) {
	tc := newRefusalTest(t)
	tc.timer.scheduleErr = errors.New("sender queue full")
	instanceID := startInstance(t, tc.rt, from("IO.in@motherbee", msgWithName("START")))
	if row := mustGetInstance(t, tc.rt, instanceID); row.Status != "running" || row.CurrentState != "waiting" {
		t.Fatalf("the instance runs: %+v", row)
	}
	logs, _ := tc.rt.Store.GetRecentLog(context.Background(), instanceID, 10)
	for _, entry := range logs {
		if entry.ActionType == "schedule_timer" && !entry.OK {
			t.Fatalf("the action did not fail: %+v", entry)
		}
	}
	if row, ok := tc.deadline(instanceID); !ok || row.Refusal != "TIMER_SCHEDULE: not sent: sender queue full" {
		t.Fatalf("the timer is the instance's, marked to go again: %+v %v", row, ok)
	}
	tc.timer.scheduleErr = nil
	tc.later(5 * time.Second)
	tc.tick()
	tc.dispatch(timerTook(tc.timer.lastTrace(t)))
	if row, _ := tc.deadline(instanceID); row.Refusal != "" {
		t.Fatalf("the timer went, and SY.timer took it: %+v", row)
	}
}

// A refusal keeps the order with the instance's later cancel of the same timer: the index row is what
// goes again, and the cancel dropped it. Whether the refusal comes before the cancel or after, the
// timer never goes again, and the node stays healthy.
func TestARefusedTimerTheInstanceCancelsDoesNotGoAgain(t *testing.T) {
	for _, refusedFirst := range []bool{true, false} {
		t.Run(fmt.Sprintf("refused first: %v", refusedFirst), func(t *testing.T) {
			tc := newRefusalTest(t)
			instanceID, traceID := tc.start()
			if refusedFirst {
				tc.dispatch(unreachable(traceID, "NODE_NOT_FOUND"))
			}
			tc.tell(instanceID, "DROP")
			if !refusedFirst {
				tc.dispatch(unreachable(traceID, "NODE_NOT_FOUND"))
			}
			if _, ok := tc.deadline(instanceID); ok {
				t.Fatal("the cancel dropped the timer")
			}
			scheduled := len(tc.timer.scheduled)
			tc.later(time.Minute)
			tc.tick()
			if len(tc.timer.scheduled) != scheduled {
				t.Fatalf("a cancelled timer never goes again: %v", tc.timer.scheduled[scheduled:])
			}
			if health, code, message := tc.health(); health != "HEALTHY" {
				t.Fatalf("nothing is refused: %s %s %q", health, code, message)
			}
		})
	}
}

// FINDINGS A-254's review: a cancel_timer drops its timer from the index whatever its cancel's send
// returns. During a reconnect a schedule that did not go is marked, and the cancel that follows does
// not go either: the action failed, the row stayed with its mark, the loop sent the timer again once
// the link was back, SY.timer took it, and the instance took the fire of a timer it had cancelled.
// Now the action does not fail, nothing goes again, and a fire SY.timer still sends is dropped.
func TestACancelThatDoesNotGoStillDropsTheTimer(t *testing.T) {
	tc := newRefusalTest(t)
	tc.timer.scheduleErr = errors.New("sender queue full")
	instanceID := startInstance(t, tc.rt, from("IO.in@motherbee", msgWithName("START")))
	if row, ok := tc.deadline(instanceID); !ok || row.Refusal == "" {
		t.Fatalf("the schedule did not go: its timer is marked: %+v %v", row, ok)
	}
	tc.timer.cancelErr = errors.New("sender queue full")
	tc.tell(instanceID, "DROP")
	if _, ok := tc.deadline(instanceID); ok {
		t.Fatal("the cancel drops the timer, its send failing")
	}
	logs, err := tc.rt.Store.GetRecentLog(context.Background(), instanceID, 10)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range logs {
		if entry.ActionType == "cancel_timer" && !entry.OK {
			t.Fatalf("the cancel does not fail: %+v", entry)
		}
	}

	tc.timer.scheduleErr, tc.timer.cancelErr = nil, nil
	scheduled := len(tc.timer.scheduled)
	tc.later(time.Minute)
	tc.tick()
	if len(tc.timer.scheduled) != scheduled {
		t.Fatalf("a cancelled timer never goes again: %v", tc.timer.scheduled[scheduled:])
	}
	if health, code, message := tc.health(); health != "HEALTHY" {
		t.Fatalf("nothing is refused: %s %s %q", health, code, message)
	}
	tc.dispatch(timerFiredFor(t, testNodeName, instanceID, "deadline", fixedClock().Add(2*time.Hour)))
	if row := mustGetInstance(t, tc.rt, instanceID); row.CurrentState != "waiting" || row.Status != "running" {
		t.Fatalf("a fire SY.timer still sends of it is dropped: %+v", row)
	}
}

// A refusal keeps the order with the instance's later move of the same timer: the timer goes again at
// its new time. A schedule refused and then moved keeps its refusal (SY.timer never got the timer,
// and answers the reschedule TIMER_NOT_FOUND); a refusal of a schedule a move has superseded decides
// nothing, and the move's own answer does.
func TestARefusedTimerTheInstanceMovesGoesAgainAtItsNewTime(t *testing.T) {
	for _, refusedFirst := range []bool{true, false} {
		t.Run(fmt.Sprintf("refused first: %v", refusedFirst), func(t *testing.T) {
			tc := newRefusalTest(t)
			instanceID, scheduleTrace := tc.start()
			if refusedFirst {
				tc.dispatch(unreachable(scheduleTrace, "NODE_NOT_FOUND"))
			}
			tc.later(time.Second)
			tc.tell(instanceID, "MOVE")
			movedTo := tc.now.Add(3 * time.Hour).UnixMilli()
			moveTrace := tc.timer.lastTrace(t)
			row, _ := tc.deadline(instanceID)
			if row.FireAtMS != movedTo || (row.Refusal != "") != refusedFirst {
				t.Fatalf("the move keeps the timer's refusal: %+v", row)
			}
			if !refusedFirst {
				tc.dispatch(unreachable(scheduleTrace, "NODE_NOT_FOUND"))
				if row, _ := tc.deadline(instanceID); row.Refusal != "" {
					t.Fatalf("the refusal of a superseded schedule decides nothing: %+v", row)
				}
			}
			tc.dispatch(timerRefused(moveTrace, verbTimerReschedule, "TIMER_NOT_FOUND", "timer not found"))
			if row, _ := tc.deadline(instanceID); !strings.HasPrefix(row.Refusal, "TIMER_RESCHEDULE: SY.timer@motherbee answered TIMER_NOT_FOUND") {
				t.Fatalf("SY.timer never got the timer it was asked to move: %+v", row)
			}
			scheduled := len(tc.timer.scheduled)
			tc.later(5 * time.Second)
			tc.tick()
			if len(tc.timer.scheduled) != scheduled+1 || tc.timer.in[len(tc.timer.in)-1] != time.Duration(movedTo-tc.now.UnixMilli())*time.Millisecond {
				t.Fatalf("the timer goes again as a schedule at its new time: %v %v", tc.timer.scheduled[scheduled:], tc.timer.in)
			}
			tc.dispatch(timerTook(tc.timer.lastTrace(t)))
			if row, _ := tc.deadline(instanceID); row.Refusal != "" || row.FireAtMS != movedTo {
				t.Fatalf("SY.timer took the timer at its new time: %+v", row)
			}
		})
	}
}

// escalateWorkflowJSON escalates in 7 days: URGENT moves the escalation to an hour from then, and
// AGAIN schedules it again for an hour from then, a schedule of a key SY.timer holds already.
const escalateWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "escalate",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["escalated"],
  "states": [
    {
      "name": "waiting",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "escalate", "fire_in": "7d"}],
      "transitions": [
        {"event_match": {"msg": "URGENT"}, "guard": "true", "actions": [{"type": "reschedule_timer", "timer_key": "escalate", "fire_in": "1h"}]},
        {"event_match": {"msg": "AGAIN"}, "guard": "true", "actions": [{"type": "schedule_timer", "timer_key": "escalate", "fire_in": "1h"}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'escalate'", "target_state": "escalated"}
      ]
    },
    {"name": "escalated"}
  ]
}`

// FINDINGS A-254's review: a move SY.timer did not take of a timer it holds goes to its new time. A
// day after SY.timer took an escalation for 7 days, the instance moves it to an hour from then while
// SY.timer restarts, and the router refuses the TIMER_RESCHEDULE, or while the link reconnects, and
// the TIMER_RESCHEDULE does not go: the TIMER_SCHEDULE that goes again 5 s later is answered
// already_existed at the old time, which SY.timer's store kept. Taken as SY.timer's take, the mark
// was cleared, the node said HEALTHY, and the escalation fired 6 days late (the stale-fire rule
// rescues only a timer SY.timer holds earlier). Now the timer keeps its mark and goes to its time, a
// TIMER_RESCHEDULE followed as any request, until SY.timer takes it.
func TestAMoveSYTimerDidNotTakeOfATimerItHoldsGoesToItsNewTime(t *testing.T) {
	for _, c := range []struct {
		name   string
		refuse func(tc *refusalTest, instanceID string)
	}{
		{"the router refuses the move", func(tc *refusalTest, instanceID string) {
			tc.tell(instanceID, "URGENT")
			tc.dispatch(unreachable(tc.timer.lastTrace(tc.t), "NODE_NOT_FOUND"))
		}},
		{"the move does not go", func(tc *refusalTest, instanceID string) {
			tc.timer.rescheduleErr = errors.New("sender queue full")
			tc.tell(instanceID, "URGENT")
			tc.timer.rescheduleErr = nil
		}},
	} {
		t.Run(c.name, func(t *testing.T) {
			tc := newRefusalTestOn(t, escalateWorkflowJSON)
			instanceID, scheduleTrace := tc.start()
			heldAt := tc.now.Add(7 * 24 * time.Hour)
			tc.dispatch(timerTook(scheduleTrace))
			tc.later(24 * time.Hour)
			c.refuse(tc, instanceID)
			movedTo := tc.now.Add(time.Hour)
			if row, _ := tc.timerRow(instanceID, "escalate"); row.Refusal == "" || row.FireAtMS != movedTo.UnixMilli() {
				t.Fatalf("the move is marked, at its new time: %+v", row)
			}
			tc.later(5 * time.Second)
			tc.tick()
			resendTrace, rescheduled := tc.timer.lastTrace(t), len(tc.timer.rescheduled)
			logged := nodeLog(t, func() { tc.dispatch(timerHeldAt(resendTrace, heldAt)) })
			if row, _ := tc.timerRow(instanceID, "escalate"); row.Refusal == "" || row.FireAtMS != movedTo.UnixMilli() {
				t.Fatalf("SY.timer holds it at its old time: the timer keeps its mark, at its new time: %+v", row)
			}
			if len(tc.timer.rescheduled) != rescheduled+1 || tc.timer.rescheduled[rescheduled] != timerClientRef(instanceID, "escalate") ||
				tc.timer.rescheduledIn[rescheduled] != movedTo.Sub(tc.now) {
				t.Fatalf("it goes to its time, a TIMER_RESCHEDULE: %v %v", tc.timer.rescheduled, tc.timer.rescheduledIn)
			}
			if !strings.Contains(logged, "SY.timer@motherbee holds it at "+msTime(heldAt.UnixMilli())+", later than its time") ||
				strings.Contains(logged, "taken by SY.timer") {
				t.Fatalf("the log says where SY.timer holds it, and no take: %s", logged)
			}
			if health, code, _ := tc.health(); health != "DEGRADED" || code != errCodeTimersNotReconciled {
				t.Fatalf("the timer is not reconciled yet: %s %s", health, code)
			}

			tc.dispatch(timerMoved(tc.timer.lastTrace(t), movedTo))
			if row, _ := tc.timerRow(instanceID, "escalate"); row.Refusal != "" || row.FireAtMS != movedTo.UnixMilli() {
				t.Fatalf("SY.timer took the move: %+v", row)
			}
			if health, code, message := tc.health(); health != "HEALTHY" {
				t.Fatalf("the node is healthy again: %s %s %q", health, code, message)
			}
		})
	}
}

// A schedule of a key SY.timer holds already is answered already_existed at the time SY.timer holds:
// a time later than the index's goes to the index's, a TIMER_RESCHEDULE (SY.timer kept its old time,
// wf-orchestration-v1 §4.4); an earlier one is taken, its fire stale when it comes, and the timer
// scheduled again then (takeTimerFired). Neither marks the timer.
func TestAScheduleOfAKeySYTimerHoldsLaterGoesToItsTime(t *testing.T) {
	tc := newRefusalTestOn(t, escalateWorkflowJSON)
	instanceID, scheduleTrace := tc.start()
	tc.dispatch(timerTook(scheduleTrace))
	tc.tell(instanceID, "AGAIN")
	tc.dispatch(timerHeldAt(tc.timer.lastTrace(t), tc.now.Add(7*24*time.Hour)))
	if len(tc.timer.rescheduled) != 1 || tc.timer.rescheduledIn[0] != time.Hour {
		t.Fatalf("SY.timer holds it later: it goes to its time: %v %v", tc.timer.rescheduled, tc.timer.rescheduledIn)
	}
	tc.dispatch(timerMoved(tc.timer.lastTrace(t), tc.now.Add(time.Hour)))
	tc.tell(instanceID, "AGAIN")
	tc.dispatch(timerHeldAt(tc.timer.lastTrace(t), tc.now.Add(30*time.Minute)))
	if len(tc.timer.rescheduled) != 1 {
		t.Fatalf("SY.timer holds it earlier: taken: %v", tc.timer.rescheduled)
	}
	if row, _ := tc.timerRow(instanceID, "escalate"); row.Refusal != "" || row.FireAtMS != tc.now.Add(time.Hour).UnixMilli() {
		t.Fatalf("the timer is not marked: %+v", row)
	}
	if tc.rt.ActCtx.Requests.waiting() {
		t.Fatalf("nothing waits for SY.timer: %+v", tc.rt.ActCtx.Requests)
	}
}

// FINDINGS A-254's review: the reconcile settles a timer SY.timer lists at a later time than the
// index's, a move it did not take, where it cleared the timer's mark and SY.timer fired it late. An
// hour from its time, it goes to its time, a TIMER_RESCHEDULE, its mark kept until SY.timer takes
// it; 30 s from its time, the node fires it itself then; its time past, the instance takes its fire
// at once, in the order of the index, and SY.timer's pending timer is cancelled.
func TestTheReconcileSettlesATimerSYTimerHoldsLate(t *testing.T) {
	for _, c := range []struct {
		name  string
		after time.Duration // from the move to the reconcile
	}{
		{"an hour from its time", time.Second},
		{"30 s from its time", time.Hour - 30*time.Second},
		{"its time past", 2 * time.Hour},
	} {
		t.Run(c.name, func(t *testing.T) {
			tc := newRefusalTestOn(t, escalateWorkflowJSON)
			instanceID, scheduleTrace := tc.start()
			heldAt := tc.now.Add(7 * 24 * time.Hour)
			ref := timerClientRef(instanceID, "escalate")
			tc.dispatch(timerTook(scheduleTrace))
			tc.tell(instanceID, "URGENT")
			movedTo := tc.now.Add(time.Hour)
			tc.dispatch(unreachable(tc.timer.lastTrace(t), "NODE_NOT_FOUND"))
			tc.later(c.after)
			rescheduled := len(tc.timer.rescheduled)
			tc.rt.takeTimerList(context.Background(), timerListResult{listing: wholeList(timerInfo(ref, heldAt.UnixMilli())),
				askedAtMS: nowMS(tc.rt.ActCtx.Clock)}, recoverOpts)
			switch {
			case c.after < time.Hour-time.Minute:
				if len(tc.timer.rescheduled) != rescheduled+1 || tc.timer.rescheduledIn[rescheduled] != movedTo.Sub(tc.now) {
					t.Fatalf("it goes to its time: %v %v", tc.timer.rescheduled, tc.timer.rescheduledIn)
				}
				if row, _ := tc.timerRow(instanceID, "escalate"); row.Refusal == "" {
					t.Fatalf("its mark stays until SY.timer takes the move: %+v", row)
				}
				tc.dispatch(timerMoved(tc.timer.lastTrace(t), movedTo))
				if row, _ := tc.timerRow(instanceID, "escalate"); row.Refusal != "" {
					t.Fatalf("SY.timer took the move: %+v", row)
				}
			case c.after < time.Hour:
				if row, _ := tc.timerRow(instanceID, "escalate"); row.Refusal == "" || row.ResendAtMS != movedTo.UnixMilli() {
					t.Fatalf("the node fires it itself at its time: %+v", row)
				}
				for tc.now.Before(movedTo) {
					tc.later(time.Second)
					tc.tick()
				}
				if len(tc.timer.rescheduled) != rescheduled || len(tc.timer.scheduled) != 1 {
					t.Fatalf("nothing goes to SY.timer: %v %v", tc.timer.rescheduled, tc.timer.scheduled)
				}
				if row := mustGetInstance(t, tc.rt, instanceID); row.CurrentState != "escalated" {
					t.Fatalf("the instance took its fire at its time: %+v", row)
				}
			default:
				if row := mustGetInstance(t, tc.rt, instanceID); row.CurrentState != "escalated" {
					t.Fatalf("the instance took its fire: %+v", row)
				}
				if len(tc.timer.cancelled) == 0 || tc.timer.cancelled[len(tc.timer.cancelled)-1] != ref {
					t.Fatalf("SY.timer's pending timer is cancelled: %v", tc.timer.cancelled)
				}
			}
		})
	}
}

// A refused timer whose time comes is fired by the node itself, as the reconcile fires a timer lost
// while the node was down: SY.timer, which never took it, has no fire to give. A refused timer goes
// again at its time at the latest, never later.
func TestARefusedTimerWhoseTimeComesIsFiredByTheNode(t *testing.T) {
	tc := newRefusalTest(t)
	instanceID, traceID := tc.start()
	fireAt := tc.now.Add(2 * time.Hour)
	tc.later(2*time.Hour - 3*time.Second)
	tc.dispatch(unreachable(traceID, "NODE_NOT_FOUND"))
	if row, _ := tc.deadline(instanceID); row.ResendAtMS != fireAt.UnixMilli() {
		t.Fatalf("a timer due in 3 s goes again at its time, not 5 s later: %+v", row)
	}
	scheduled := len(tc.timer.scheduled)
	tc.later(3 * time.Second)
	tc.tick()
	if len(tc.timer.scheduled) != scheduled {
		t.Fatalf("a timer whose time came is not sent again: %v", tc.timer.scheduled[scheduled:])
	}
	if row := mustGetInstance(t, tc.rt, instanceID); row.CurrentState != "expired" || row.Status != "completed" {
		t.Fatalf("the instance took its deadline's fire: %+v", row)
	}
	if _, ok := tc.deadline(instanceID); ok {
		t.Fatal("the fired timer left the index")
	}
	if health, code, message := tc.health(); health != "HEALTHY" {
		t.Fatalf("nothing is refused: %s %s %q", health, code, message)
	}
}

// FINDINGS A-254's review: a refused timer less than 60 s from its time, SY.timer's floor, is not
// sent again. SY.timer would have taken it for 60 s from then, and once it had, the node no longer
// fired it: the timer fired up to 60 s late. The node fires it itself at its time: a move to a minute
// from now that SY.timer never answers is refused at 30 s, and one SY.timer refuses at once is not
// sent again 5 s later; a timer marked to go now (as the reconcile marks one) 30 s from its time
// waits for it too.
func TestARefusedTimerTooNearItsTimeIsFiredByTheNodeOnTime(t *testing.T) {
	for _, c := range []struct {
		name   string
		refuse func(tc *refusalTest, instanceID, traceID string)
	}{
		{"no answer in 30 s", func(tc *refusalTest, _, _ string) {
			tc.later(30 * time.Second)
			tc.tick()
		}},
		{"refused at once", func(tc *refusalTest, _, traceID string) {
			tc.dispatch(unreachable(traceID, "NODE_NOT_FOUND"))
		}},
		{"marked to go now, 30 s from its time", func(tc *refusalTest, instanceID, traceID string) {
			tc.dispatch(timerTook(traceID))
			tc.later(30 * time.Second)
			if err := tc.rt.Store.RefuseTimer(context.Background(), instanceID, "reminder", "TIMER_LIST: SY.timer@motherbee does not have it", nowMS(tc.rt.ActCtx.Clock)); err != nil {
				tc.t.Fatal(err)
			}
			tc.tick()
		}},
	} {
		t.Run(c.name, func(t *testing.T) {
			tc := newRefusalTestOn(t, reminderWorkflowJSON)
			instanceID, _ := tc.start()
			tc.tell(instanceID, "SOON")
			fireAt := tc.now.Add(time.Minute)
			scheduled, rescheduled := len(tc.timer.scheduled), len(tc.timer.rescheduled)
			c.refuse(tc, instanceID, tc.timer.lastTrace(t))
			if row, _ := tc.timerRow(instanceID, "reminder"); row.Refusal == "" || row.ResendAtMS != fireAt.UnixMilli() {
				t.Fatalf("the timer waits for its time: %+v", row)
			}
			for tc.now.Before(fireAt.Add(-time.Second)) {
				tc.later(time.Second)
				tc.tick()
			}
			if len(tc.timer.scheduled) != scheduled || len(tc.timer.rescheduled) != rescheduled {
				t.Fatalf("a timer too near its time is not sent again: %v %v", tc.timer.scheduled[scheduled:], tc.timer.rescheduled[rescheduled:])
			}
			if row := mustGetInstance(t, tc.rt, instanceID); row.Status != "running" {
				t.Fatalf("the timer does not fire before its time: %+v", row)
			}
			tc.later(time.Second)
			tc.tick()
			if row := mustGetInstance(t, tc.rt, instanceID); row.CurrentState != "done" || row.Status != "completed" {
				t.Fatalf("the node fires it at its time: %+v", row)
			}
		})
	}
}

// Only the router's notice and SY.timer of the node's hive refuse a request: an UNREACHABLE a node
// sent (it carries its routing.src_l2_name), a notice on a trace the node waits on nothing for, and a
// TIMER_RESPONSE from any other node change nothing, and none is answered.
func TestOnlyTheRouterAndSYTimerOfTheHiveRefuseARequest(t *testing.T) {
	tc := newRefusalTest(t)
	instanceID, traceID := tc.start()
	sent := len(tc.disp.sent)
	forged := unreachable(traceID, "NODE_NOT_FOUND")
	stranger := "IO.in@motherbee"
	forged.Routing.SrcL2Name = &stranger
	otherTimer := timerRefused(traceID, verbTimerSchedule, "TIMER_STORAGE_ERROR", "x")
	otherHive := "SY.timer@worker1"
	otherTimer.Routing.SrcL2Name = &otherHive
	for _, msg := range []sdk.Message{
		unreachable("trace-of-something-else", "NODE_NOT_FOUND"),
		forged,
		otherTimer,
		from("IO.in@motherbee", timerRefused(traceID, verbTimerSchedule, "TIMER_STORAGE_ERROR", "x")),
		notTakenBy("IO.in@motherbee", traceID),
		notTakenBy("SY.timer@worker1", traceID),
		notTakenBy("SY.timer@motherbee", "trace-of-something-else"),
	} {
		tc.dispatch(msg)
	}
	if row, _ := tc.deadline(instanceID); row.Refusal != "" {
		t.Fatalf("nothing refused the schedule: %+v", row)
	}
	if len(tc.disp.sent) != sent {
		t.Fatalf("none is answered: %+v", tc.disp.sent[sent:])
	}
	tc.dispatch(timerTook(traceID))
	if tc.rt.ActCtx.Requests.waiting() {
		t.Fatalf("SY.timer's answer settled the schedule: %+v", tc.rt.ActCtx.Requests)
	}
}

// The reconcile with SY.timer settles the refused timers: one SY.timer lists pending is no longer
// refused, and one it does not have goes again from the loop's tick, at once, a request like any
// other (FINDINGS A-254's review). The node is DEGRADED until SY.timer takes it, and the reconcile's
// log says no longer degraded only once no timer waits for SY.timer.
func TestTheReconcileSettlesTheRefusedTimers(t *testing.T) {
	tc := newRefusalTest(t)
	listed, listedTrace := tc.start()
	missing, missingTrace := tc.start()
	tc.dispatch(unreachable(listedTrace, "NODE_NOT_FOUND"))
	tc.dispatch(unreachable(missingTrace, "NODE_NOT_FOUND"))
	tc.later(time.Second)
	scheduled := len(tc.timer.scheduled)
	pending := timerInfo(timerClientRef(listed, "deadline"), fixedClock().Add(2*time.Hour).UnixMilli())
	logged := nodeLog(t, func() {
		tc.rt.takeTimerList(context.Background(), timerListResult{listing: wholeList(pending), askedAtMS: nowMS(tc.rt.ActCtx.Clock)}, recoverOpts)
	})
	if row, _ := tc.deadline(listed); row.Refusal != "" {
		t.Fatalf("SY.timer has the listed timer: %+v", row)
	}
	if row, _ := tc.deadline(missing); row.Refusal != "TIMER_LIST: SY.timer@motherbee does not have it" || row.ResendAtMS != tc.now.UnixMilli() ||
		len(tc.timer.scheduled) != scheduled {
		t.Fatalf("the missing timer goes from the loop's tick, at once: %+v %v", row, tc.timer.scheduled[scheduled:])
	}
	if !strings.Contains(logged, "timers reconciled with SY.timer; 1 timer(s) wait for it to take them") || strings.Contains(logged, "no longer degraded") {
		t.Fatalf("the log says a timer waits for SY.timer: %s", logged)
	}
	if health, code, _ := tc.health(); health != "DEGRADED" || code != errCodeTimersNotReconciled {
		t.Fatalf("a timer waits for SY.timer: %s %s", health, code)
	}

	tc.tick()
	if len(tc.timer.scheduled) != scheduled+1 || tc.timer.scheduled[scheduled] != timerClientRef(missing, "deadline") {
		t.Fatalf("the missing timer is scheduled again: %v", tc.timer.scheduled[scheduled:])
	}
	tc.dispatch(unreachable(tc.timer.lastTrace(t), "NODE_NOT_FOUND"))
	if row, _ := tc.deadline(missing); !strings.Contains(row.Refusal, "UNREACHABLE reason=NODE_NOT_FOUND") {
		t.Fatalf("the reconcile's schedule is a request like any other: %+v", row)
	}
	tc.later(10 * time.Second)
	tc.tick()
	tc.dispatch(timerTook(tc.timer.lastTrace(t)))
	if health, code, message := tc.health(); health != "HEALTHY" {
		t.Fatalf("SY.timer took it: %s %s %q", health, code, message)
	}
	both := []sdk.TimerInfo{pending, timerInfo(timerClientRef(missing, "deadline"), fixedClock().Add(2*time.Hour).UnixMilli())}
	logged = nodeLog(t, func() {
		tc.rt.takeTimerList(context.Background(), timerListResult{listing: wholeList(both...), askedAtMS: nowMS(tc.rt.ActCtx.Clock)}, recoverOpts)
	})
	if !strings.Contains(logged, "timers reconciled with SY.timer; the node is no longer degraded") {
		t.Fatalf("nothing waits for SY.timer: %s", logged)
	}
}

// FINDINGS A-254's review: the timers the reconcile finds SY.timer lost go paced, 32 a tick at most,
// where the reconcile sent every one at once, past SY.timer's 64 messages and the node's 64 answers
// after a long outage: the answers the SDK dropped read as refusals 30 s later.
func TestTheTimersSYTimerLostGoAgainPaced(t *testing.T) {
	tc := newRefusalTest(t)
	for range 40 {
		_, traceID := tc.start()
		tc.dispatch(timerTook(traceID))
	}
	tc.later(time.Second)
	scheduled := len(tc.timer.scheduled)
	tc.rt.takeTimerList(context.Background(), timerListResult{listing: wholeList(), askedAtMS: nowMS(tc.rt.ActCtx.Clock)}, recoverOpts)
	if len(tc.timer.scheduled) != scheduled {
		t.Fatalf("the reconcile sends none at once: %d", len(tc.timer.scheduled)-scheduled)
	}
	for tick, want := range []int{32, 40} {
		tc.tick()
		if len(tc.timer.scheduled)-scheduled != want {
			t.Fatalf("tick %d: %d timers went again, want %d", tick+1, len(tc.timer.scheduled)-scheduled, want)
		}
		tc.later(time.Second)
	}
}

// FINDINGS A-254's review: the answers of SY.timer the SDK dropped, the node's channel for them
// full, are named in the node's log on the loop's tick, once each, so the gate tells the refusals
// their requests read as 30 s later from SY.timer's own.
func TestTheAnswersTheSDKDroppedAreLogged(t *testing.T) {
	tc := newRefusalTest(t)
	tc.start()
	var dropped uint64
	tc.rt.timerAnswerDrops = func() uint64 { return dropped }
	if logged := nodeLog(t, tc.tick); strings.Contains(logged, "dropped") {
		t.Fatalf("nothing dropped: %s", logged)
	}
	dropped = 3
	if logged := nodeLog(t, tc.tick); !strings.Contains(logged, "the SDK dropped 3 answer(s) of SY.timer@motherbee, the node's channel for them full") {
		t.Fatalf("the drops are logged: %s", logged)
	}
	if logged := nodeLog(t, tc.tick); strings.Contains(logged, "dropped") {
		t.Fatalf("once: %s", logged)
	}
	dropped = 5
	if logged := nodeLog(t, tc.tick); !strings.Contains(logged, "the SDK dropped 2 answer(s)") {
		t.Fatalf("the new drops are logged: %s", logged)
	}
}

// loopTimer is SY.timer as the receive loop's goroutine reaches it, for a test on another goroutine:
// it lists no timer, and hands the trace_id of each schedule to schedules.
type loopTimer struct {
	schedules chan string
	count     atomic.Int64
}

func (l *loopTimer) schedule() (string, error) {
	traceID := fmt.Sprintf("loop-trace-%d", l.count.Add(1))
	l.schedules <- traceID
	return traceID, nil
}
func (l *loopTimer) ScheduleIn(context.Context, time.Duration, sdk.ScheduleOptions) (string, error) {
	return l.schedule()
}
func (l *loopTimer) Schedule(context.Context, time.Time, sdk.ScheduleOptions) (string, error) {
	return l.schedule()
}
func (l *loopTimer) CancelByClientRef(context.Context, string) error          { return nil }
func (l *loopTimer) CancelByClientRefConfirmed(context.Context, string) error { return nil }
func (l *loopTimer) RescheduleInByClientRef(context.Context, string, time.Duration) (string, error) {
	return l.schedule()
}
func (l *loopTimer) RescheduleByClientRef(context.Context, string, time.Time) (string, error) {
	return l.schedule()
}
func (l *loopTimer) ListPage(context.Context, sdk.ListFilter) (sdk.TimerListPage, error) {
	return sdk.TimerListPage{}, nil
}

// FINDINGS A-254, the receive loop's wiring (serve), driven without a router: the router's
// UNREACHABLE of a schedule comes on the loop's messages and makes the node DEGRADED; the loop's tick
// sends the timer again once its 5 s are past; SY.timer's answer comes on the timer answers' channel,
// and the node is healthy again.
func TestTheReceiveLoopSendsARefusedTimerAgainUntilSYTimerTakesIt(t *testing.T) {
	document := spawnConfig(writeWorkflowPackage(t, deadlineWorkflowJSON))
	document["db_path"] = filepath.Join(t.TempDir(), "wf.db")
	plane := bootstrap(t, writeNodeConfig(t, document))
	disp := &chanDispatcher{sent: make(chan sdk.Message, 64)}
	timer := &loopTimer{schedules: make(chan string, 16)}
	var later atomic.Int64
	clock := func() time.Time { return fixedClock().Add(time.Duration(later.Load())) }
	rt := &NodeRuntime{ActCtx: ActionContext{Dispatcher: disp, Timer: timer, Clock: clock, Requests: newTimerRequests()},
		NodeUUID: "wf-uuid", NodeName: testNodeName, Plane: plane}

	ctx, cancel := context.WithCancel(context.Background())
	incoming, answers := make(chan sdk.Message), make(chan sdk.Message)
	served := make(chan struct{})
	go func() {
		defer close(served)
		rt.serve(ctx, incoming, answers, plane.Configured(), loopIntervals{storeRetry: 10 * time.Millisecond, timerRetry: 10 * time.Millisecond, timerRequests: 10 * time.Millisecond})
	}()
	defer func() {
		cancel()
		<-served
		if rt.Store != nil {
			rt.Store.Close()
		}
	}()

	give := func(to chan sdk.Message, msg sdk.Message) {
		t.Helper()
		select {
		case to <- msg:
		case <-time.After(5 * time.Second):
			t.Fatal("the loop takes no message")
		}
	}
	nextSchedule := func() string {
		t.Helper()
		select {
		case traceID := <-timer.schedules:
			return traceID
		case <-time.After(5 * time.Second):
			t.Fatal("no schedule went to SY.timer")
		}
		return ""
	}
	statusName := sdk.MSGNodeStatusGet
	status := func() (string, string) {
		t.Helper()
		give(incoming, sdk.Message{Routing: sdk.Routing{Src: "orchestrator-uuid", TraceID: "t-status"}, Meta: sdk.Meta{MsgType: "system", Msg: &statusName}})
		for {
			select {
			case msg := <-disp.sent:
				if derefString(msg.Meta.Msg) != sdk.MSGNodeStatusGetResponse {
					continue
				}
				var reply struct {
					Health    string                 `json:"health_state"`
					LastError *sdk.ControlPlaneError `json:"last_error"`
				}
				if err := json.Unmarshal(msg.Payload, &reply); err != nil {
					t.Fatal(err)
				}
				if reply.LastError == nil {
					return reply.Health, ""
				}
				return reply.Health, reply.LastError.Code
			case <-time.After(5 * time.Second):
				t.Fatal("no answer to NODE_STATUS_GET")
			}
		}
	}
	until := func(health, code string) {
		t.Helper()
		for deadline := time.Now().Add(10 * time.Second); ; time.Sleep(5 * time.Millisecond) {
			if got, gotCode := status(); got == health && gotCode == code {
				return
			}
			if time.Now().After(deadline) {
				t.Fatalf("the loop never reached %s %q", health, code)
			}
		}
	}

	give(incoming, onThread("conv-loop", from("IO.in@motherbee", msgWithName("START"))))
	scheduleTrace := nextSchedule()
	give(incoming, unreachable(scheduleTrace, "NODE_NOT_FOUND"))
	until("DEGRADED", errCodeTimersNotReconciled)

	later.Store(int64(5 * time.Second))
	resendTrace := nextSchedule()
	give(answers, timerTook(resendTrace))
	until("HEALTHY", "")
}

// FINDINGS A-254, the SDK's routing, through a router of the test's on a unix socket: once a call
// waited for a TIMER_RESPONSE (the start's TIMER_LIST), SY.timer's answer to a schedule the node sent
// without waiting still reaches the node, on the timer answers' channel. With the profile of before,
// the SDK dropped it as an orphan, and SY.timer's refusals went unseen.
func TestSYTimersAnswerToAScheduleReachesTheNode(t *testing.T) {
	before, err := sdk.NewOperationalRouteProfile().
		CommandChannel(incomingChannel).
		PostPendingRule(sdk.RouteAny{}, sdk.RouteCommand{Channel: incomingChannel}).
		Build()
	if err != nil {
		t.Fatal(err)
	}
	now, err := wfRouteProfile()
	if err != nil {
		t.Fatal(err)
	}
	for _, c := range []struct {
		name    string
		profile sdk.OperationalRouteProfile
		reaches bool
	}{
		{"the node's profile", now, true},
		{"the profile of before", before, false},
	} {
		t.Run(c.name, func(t *testing.T) {
			dispatcher := connectToTestRouter(t, c.profile)
			var answers <-chan sdk.Message
			if c.reaches {
				if answers, err = dispatcher.Subscribe(timerAnswersChannel); err != nil {
					t.Fatal(err)
				}
			}
			timer, err := NewSDKTimerSender(dispatcher, "")
			if err != nil {
				t.Fatal(err)
			}
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			if page, err := timer.ListPage(ctx, sdk.ListFilter{OwnerL2Name: testNodeName, StatusFilter: "pending", Limit: 10}); err != nil || len(page.Timers) != 0 || page.NextCursor != "" {
				t.Fatalf("TIMER_LIST: %+v %v", page, err)
			}
			traceID, err := timer.ScheduleIn(ctx, 2*time.Hour, timerScheduleOptions(testNodeName, "wfi:1", "deadline"))
			if err != nil || traceID == "" {
				t.Fatalf("the schedule goes, under its trace_id: %q %v", traceID, err)
			}
			if !c.reaches {
				for deadline := time.Now().Add(5 * time.Second); dispatcher.UnknownResponseDrops() == 0; time.Sleep(5 * time.Millisecond) {
					if time.Now().After(deadline) {
						t.Fatal("the SDK of before drops SY.timer's answer as an orphan")
					}
				}
				return
			}
			select {
			case msg := <-answers:
				answer, err := sdk.ParseTimerResponse(&msg)
				if err != nil || msg.Routing.TraceID != traceID || answer.OK || answer.Error == nil || answer.Error.Code != "TIMER_STORAGE_ERROR" ||
					msg.SourceL2Name() != "SY.timer@motherbee" {
					t.Fatalf("SY.timer's refusal of the schedule: %+v %+v %v", msg, answer, err)
				}
			case <-time.After(5 * time.Second):
				t.Fatal("SY.timer's answer never reached the node")
			}
			if drops := dispatcher.UnknownResponseDrops(); drops != 0 {
				t.Fatalf("the SDK dropped %d answers", drops)
			}
		})
	}
}

// reminderWorkflowJSON reminds in 30 minutes: SOON moves the reminder to a minute from then, the
// least a timer waits, and AT to the time its payload names.
const reminderWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "reminder",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "waiting",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "reminder", "fire_in": "30m"}],
      "transitions": [
        {"event_match": {"msg": "SOON"}, "guard": "true", "actions": [{"type": "reschedule_timer", "timer_key": "reminder", "fire_in": "1m"}]},
        {"event_match": {"msg": "AT"}, "guard": "true", "actions": [{"type": "reschedule_timer", "timer_key": "reminder", "fire_at": {"$ref": "event.payload.at"}}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'reminder'", "target_state": "done"}
      ]
    },
    {"name": "done"}
  ]
}`

// FINDINGS A-254's review: a reschedule_timer's fire_in goes to SY.timer as the delay it is
// (new_fire_in_ms), as a schedule's does (fire_in_ms): SY.timer counts it from when it gets the
// request, its 60 s floor included. It went as a time counted on the node's clock before the move's
// commit, so a move of "1m", the least a timer waits, reached SY.timer less than 60 s ahead, and
// SY.timer refused it (TIMER_BELOW_MINIMUM) practically every time. A fire_at goes as the time it is.
func TestARescheduleSendsItsFireInAsADelay(t *testing.T) {
	timer := &mockTimerSender{}
	rt, _ := newTestRuntime(t, "", reminderWorkflowJSON, timer)
	instanceID := startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
	tell := func(msg sdk.Message) {
		t.Helper()
		msg = from("IO.in@motherbee", msg)
		msg.Meta.ThreadID = threadOf(t, rt, instanceID)
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}
	tell(msgWithName("SOON"))
	if len(timer.rescheduledIn) != 1 || timer.rescheduledIn[0] != time.Minute || !timer.rescheduledAt[0].IsZero() {
		t.Fatalf("a fire_in goes as a delay: %v %v", timer.rescheduledIn, timer.rescheduledAt)
	}
	if row, _, _ := rt.Store.GetTimerForInstance(context.Background(), instanceID, "reminder"); row.FireAtMS != fixedClock().Add(time.Minute).UnixMilli() {
		t.Fatalf("the index keeps the timer's time: %+v", row)
	}
	at := fixedClock().Add(2 * time.Hour)
	tell(msgWithNameAndPayload("AT", map[string]any{"at": at.Format(time.RFC3339)}))
	if len(timer.rescheduledAt) != 2 || !timer.rescheduledAt[1].Equal(at) || timer.rescheduledIn[1] != 0 {
		t.Fatalf("a fire_at goes as a time: %v %v", timer.rescheduledIn, timer.rescheduledAt)
	}
	if row, _, _ := rt.Store.GetTimerForInstance(context.Background(), instanceID, "reminder"); row.FireAtMS != at.UnixMilli() {
		t.Fatalf("the index keeps the timer's time: %+v", row)
	}
}

// What a reschedule sends SY.timer, through the SDK and a router of the test's: a delay as
// new_fire_in_ms, a time as new_fire_at_utc_ms, each under the timer's client_ref, and nothing else.
// A delay under SY.timer's floor does not go.
func TestAReschedulesRequestOnTheWire(t *testing.T) {
	profile, err := wfRouteProfile()
	if err != nil {
		t.Fatal(err)
	}
	seen := make(chan sdk.Message, 4)
	timer, err := NewSDKTimerSender(connectToTestRouterSeeing(t, profile, seen), "")
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	ref := timerClientRef("wfi:1", "reminder")
	at := fixedClock().Add(2 * time.Hour)
	if _, err := timer.RescheduleInByClientRef(ctx, ref, time.Minute); err != nil {
		t.Fatal(err)
	}
	if _, err := timer.RescheduleByClientRef(ctx, ref, at); err != nil {
		t.Fatal(err)
	}
	for _, want := range []map[string]any{
		{"client_ref": ref, "new_fire_in_ms": float64(time.Minute.Milliseconds())},
		{"client_ref": ref, "new_fire_at_utc_ms": float64(at.UnixMilli())},
	} {
		select {
		case request := <-seen:
			var payload map[string]any
			if err := json.Unmarshal(request.Payload, &payload); err != nil || derefString(request.Meta.Msg) != verbTimerReschedule ||
				fmt.Sprint(payload) != fmt.Sprint(want) {
				t.Fatalf("%s %s, want %v (%v)", derefString(request.Meta.Msg), request.Payload, want, err)
			}
		case <-time.After(5 * time.Second):
			t.Fatal("the reschedule never reached SY.timer")
		}
	}
	if _, err := timer.RescheduleInByClientRef(ctx, ref, time.Minute-time.Millisecond); err == nil {
		t.Fatal("a delay under SY.timer's floor does not go")
	}
}

// connectToTestRouter connects a node named testNodeName, with profile, to a router of the test's: it
// announces the node and answers as SY.timer of the hive, TIMER_LIST with no timer and any other
// request refused, TIMER_STORAGE_ERROR, each on its trace_id with SY.timer's name stamped.
func connectToTestRouter(t *testing.T, profile sdk.OperationalRouteProfile) *sdk.RouterDispatcher {
	t.Helper()
	return connectToTestRouterSeeing(t, profile, nil)
}

// connectToTestRouterSeeing is connectToTestRouter whose router hands each timer request it gets to
// seen, when it is not nil, before it answers it.
func connectToTestRouterSeeing(t *testing.T, profile sdk.OperationalRouteProfile, seen chan<- sdk.Message) *sdk.RouterDispatcher {
	t.Helper()
	// A socket's path holds about 100 bytes, which a test's own temporary directory may pass.
	dir, err := os.MkdirTemp("", "wfr")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	socket := filepath.Join(dir, "router.sock")
	listener, err := (&net.ListenConfig{}).Listen(context.Background(), "unix", socket)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	go serveAsTestRouter(listener, seen)

	configDir := t.TempDir()
	if err := os.WriteFile(filepath.Join(configDir, "hive.yaml"), []byte("hive_id: motherbee\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	dispatcher, err := sdk.ConnectWithRetry(sdk.NodeConfig{
		Name: "WF.invoice", RouterSocket: socket, UUIDPersistenceDir: t.TempDir(), UUIDMode: sdk.NodeUuidEphemeral,
		ConfigDir: configDir, Version: "test",
	}, 0, profile)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = dispatcher.Close() })
	return dispatcher
}

func serveAsTestRouter(listener net.Listener, seen chan<- sdk.Message) {
	conn, err := listener.Accept()
	if err != nil {
		return
	}
	defer conn.Close()
	if _, err := sdk.ReadFrame(conn); err != nil { // the node's HELLO
		return
	}
	announce, err := sdk.BuildAnnounce("router-uuid", "node-uuid", "trace-announce", sdk.NodeAnnouncePayload{
		UUID: "node-uuid", Name: testNodeName, Status: "ok", VpnID: 1, RouterName: "RT.test@motherbee",
	})
	if err != nil {
		return
	}
	write := func(msg sdk.Message) bool {
		raw, err := sdk.MarshalMessage(msg)
		return err == nil && sdk.WriteFrame(conn, raw) == nil
	}
	if !write(announce) {
		return
	}
	timerName := "SY.timer@motherbee"
	for {
		frame, err := sdk.ReadFrame(conn)
		if err != nil {
			return
		}
		var request sdk.Message
		if err := json.Unmarshal(frame, &request); err != nil {
			return
		}
		verb := derefString(request.Meta.Msg)
		if !strings.HasPrefix(verb, "TIMER_") { // the node's WITHDRAW as it closes
			continue
		}
		if seen != nil {
			seen <- request
		}
		payload := map[string]any{"ok": true, "verb": verb, "timers": []any{}}
		if verb != "TIMER_LIST" {
			payload = map[string]any{"ok": false, "verb": verb, "error": map[string]any{"code": "TIMER_STORAGE_ERROR", "message": "disk I/O error"}}
		}
		answer, err := sdk.BuildSystemResponse(&request, "timer-uuid", sdk.MsgTimerResponse, payload, sdk.SystemEnvelopeOptions{})
		if err != nil {
			return
		}
		answer.Routing.SrcL2Name = &timerName
		if !write(answer) {
			return
		}
	}
}
