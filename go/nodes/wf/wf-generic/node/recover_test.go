package node

import (
	"context"
	"fmt"
	"strings"
	"testing"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// mockTimerLister extends mockTimerSender with a configurable List response.
type mockTimerLister struct {
	mockTimerSender
	timerList  []sdk.TimerInfo
	listErr    error
	lastFilter sdk.ListFilter
}

func (m *mockTimerLister) List(_ context.Context, filter sdk.ListFilter) ([]sdk.TimerInfo, error) {
	m.lastFilter = filter
	return m.timerList, m.listErr
}

// recoverRuntime is a node runtime on definition whose SY.timer answers TIMER_LIST with timers.
func recoverRuntime(t *testing.T, definition string, timer *mockTimerLister) *NodeRuntime {
	t.Helper()
	rt, _ := newTestRuntime(t, "", definition, timer)
	return rt
}

var recoverOpts = RecoverOptions{OwnerL2Name: "WF.invoice@motherbee"}

func timerInfo(clientRef string, fireAtMS int64) sdk.TimerInfo {
	ref := clientRef
	return sdk.TimerInfo{
		UUID:         "uuid-" + clientRef,
		OwnerL2Name:  "WF.invoice@motherbee",
		ClientRef:    &ref,
		Kind:         "oneshot",
		FireAtUTCMS:  fireAtMS,
		MissedPolicy: sdk.MissedPolicyFire,
		Status:       "pending",
	}
}

func TestRecoverLoadsRunningInstances(t *testing.T) {
	timer := &mockTimerLister{}
	rt := recoverRuntime(t, validWorkflowJSON(), timer)
	putRunningInstance(t, rt, "wfi:recover-1", "collecting_data")

	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	if rt.Registry.Count() != 1 {
		t.Errorf("expected 1 instance in registry, got %d", rt.Registry.Count())
	}
	if _, ok := rt.Registry.Get("wfi:recover-1"); !ok {
		t.Error("wfi:recover-1 not in registry")
	}
	if timer.lastFilter.Limit != sdk.MaxTimerListLimit {
		t.Fatalf("expected recover TIMER_LIST limit=%d, got %d", sdk.MaxTimerListLimit, timer.lastFilter.Limit)
	}
}

func TestRecoverCaseA_FutureTimer_NoAction(t *testing.T) {
	// fixedClock = 1776100000000 ms; future = 1776100000000 + 60000 ms
	futureMS := int64(1776100060000)
	ref := timerClientRef("wfi:a1", "invoice_timeout")

	timer := &mockTimerLister{
		timerList: []sdk.TimerInfo{timerInfo(ref, futureMS)},
	}
	rt := recoverRuntime(t, validWorkflowJSON(), timer)
	putRunningInstance(t, rt, "wfi:a1", "collecting_data")
	_ = rt.Store.RegisterTimer(context.Background(), "wfi:a1", "invoice_timeout", 1000, futureMS)

	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	// Case A: future timer — should NOT be cancelled
	if len(timer.cancelled) != 0 {
		t.Errorf("Case A: no timers should be cancelled, got %v", timer.cancelled)
	}

	// Timer row should still exist in local store
	timers, _ := rt.Store.ListTimersForInstance(context.Background(), "wfi:a1")
	if len(timers) != 1 {
		t.Errorf("Case A: timer row should still exist, got %d", len(timers))
	}
}

func TestRecoverCaseB_PastTimer_SyntheticFireAndCancel(t *testing.T) {
	// fixedClock = 1776100000000; past = 1776099000000
	pastMS := int64(1776099000000)
	ref := timerClientRef("wfi:b1", "invoice_timeout")

	timer := &mockTimerLister{
		timerList: []sdk.TimerInfo{timerInfo(ref, pastMS)},
	}
	rt := recoverRuntime(t, validWorkflowJSON(), timer)
	putRunningInstance(t, rt, "wfi:b1", "collecting_data")
	_ = rt.Store.RegisterTimer(context.Background(), "wfi:b1", "invoice_timeout", 1000, pastMS)

	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	// Case B: past timer — should be cancelled in SY.timer
	found := false
	for _, c := range timer.cancelled {
		if c == ref {
			found = true
		}
	}
	if !found {
		t.Errorf("Case B: expected timer %q to be cancelled, cancelled=%v", ref, timer.cancelled)
	}

	// Local timer row should be deleted after synthetic fire
	timers, _ := rt.Store.ListTimersForInstance(context.Background(), "wfi:b1")
	if len(timers) != 0 {
		t.Errorf("Case B: timer row should be deleted after synthetic fire, got %d", len(timers))
	}
}

func TestRecoverCaseB_DoesNotCancelNewTimerWhenSyntheticFireReschedulesSameKey(t *testing.T) {
	pastMS := int64(1776099000000)
	ref := timerClientRef("wfi:b2", "invoice_timeout")

	timer := &mockTimerLister{
		timerList: []sdk.TimerInfo{timerInfo(ref, pastMS)},
	}
	rt := recoverRuntime(t, workflowJSONWithTimerRefire(), timer)
	putRunningInstance(t, rt, "wfi:b2", "waiting_timeout")
	_ = rt.Store.RegisterTimer(context.Background(), "wfi:b2", "invoice_timeout", 1000, pastMS)

	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	if len(timer.cancelled) != 0 {
		t.Fatalf("re-scheduled timer should not be cancelled during recovery, got %v", timer.cancelled)
	}
	rows, err := rt.Store.ListTimersForInstance(context.Background(), "wfi:b2")
	if err != nil {
		t.Fatalf("ListTimersForInstance: %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("expected re-scheduled timer row to remain, got %d", len(rows))
	}
	if rows[0].FireAtMS == pastMS {
		t.Fatalf("expected timer row to be updated after re-schedule")
	}
}

func workflowJSONWithTimerRefire() string {
	return `{
  "wf_schema_version": "1",
  "workflow_type": "invoice",
  "description": "re-schedules timeout on timer fire",
  "start_from": ["IO.api.billing@motherbee"],
  "input_schema": {
    "type": "object",
    "required": ["customer_id"],
    "properties": {
      "customer_id": { "type": "string" }
    }
  },
  "initial_state": "waiting_timeout",
  "terminal_states": ["completed", "failed", "cancelled"],
  "states": [
    {
      "name": "waiting_timeout",
      "description": "waits",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.timer_key == 'invoice_timeout'",
          "target_state": "waiting_timeout",
          "actions": [
            {
              "type": "schedule_timer",
              "timer_key": "invoice_timeout",
              "fire_in": "5m",
              "missed_policy": "fire"
            }
          ]
        }
      ]
    },
    {
      "name": "completed",
      "description": "done",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "failed",
      "description": "failed",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "cancelled",
      "description": "cancelled",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    }
  ]
}`
}

func TestRecoverCaseC_MissingInSYTimer_DeletesLocalRow(t *testing.T) {
	timer := &mockTimerLister{timerList: []sdk.TimerInfo{}} // SY.timer has nothing

	rt := recoverRuntime(t, validWorkflowJSON(), timer)
	putRunningInstance(t, rt, "wfi:c1", "collecting_data")
	_ = rt.Store.RegisterTimer(context.Background(), "wfi:c1", "invoice_timeout", 1000, 99999)

	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	// Case C: local row should be deleted
	timers, _ := rt.Store.ListTimersForInstance(context.Background(), "wfi:c1")
	if len(timers) != 0 {
		t.Errorf("Case C: local timer row should be deleted, got %d", len(timers))
	}
}

func TestRecoverOrphanedSYTimerCancelled(t *testing.T) {
	// SY.timer has a timer the WF doesn't expect
	orphanRef := timerClientRef("wfi:gone", "some_timer")
	timer := &mockTimerLister{
		timerList: []sdk.TimerInfo{timerInfo(orphanRef, 1776100060000)},
	}
	rt := recoverRuntime(t, validWorkflowJSON(), timer)
	// No local timer rows registered

	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	found := false
	for _, c := range timer.cancelled {
		if c == orphanRef {
			found = true
		}
	}
	if !found {
		t.Errorf("orphaned timer %q should be cancelled, cancelled=%v", orphanRef, timer.cancelled)
	}
}

func TestRecoverCancellingInstanceSkipsTimerReconcile(t *testing.T) {
	futureMS := int64(1776100060000)
	ref := timerClientRef("wfi:cancelling", "invoice_timeout")

	timer := &mockTimerLister{
		timerList: []sdk.TimerInfo{timerInfo(ref, futureMS)},
	}
	rt := recoverRuntime(t, validWorkflowJSON(), timer)

	// Instance is in cancelling status
	putRunningInstance(t, rt, "wfi:cancelling", "collecting_data")
	if _, err := rt.Store.db.Exec(`UPDATE wf_instances SET status = 'cancelling' WHERE instance_id = 'wfi:cancelling';`); err != nil {
		t.Fatal(err)
	}
	_ = rt.Store.RegisterTimer(context.Background(), "wfi:cancelling", "invoice_timeout", 1000, futureMS)

	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	// Cancelling instance should still be in registry
	if _, ok := rt.Registry.Get("wfi:cancelling"); !ok {
		t.Error("cancelling instance should be in registry")
	}
	// Timer should NOT be cancelled (we skip reconciliation for cancelling instances)
	if len(timer.cancelled) != 0 {
		t.Errorf("cancelling instance: no timers should be touched, cancelled=%v", timer.cancelled)
	}
}

func TestRecoverIdempotent(t *testing.T) {
	futureMS := int64(1776100060000)
	ref := timerClientRef("wfi:idem", "invoice_timeout")

	timer := &mockTimerLister{
		timerList: []sdk.TimerInfo{timerInfo(ref, futureMS)},
	}
	rt := recoverRuntime(t, validWorkflowJSON(), timer)
	putRunningInstance(t, rt, "wfi:idem", "collecting_data")
	_ = rt.Store.RegisterTimer(context.Background(), "wfi:idem", "invoice_timeout", 1000, futureMS)

	// Run recovery twice
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("first Recover: %v", err)
	}
	first := rt.Registry.Count()
	rt.Registry = NewInstanceRegistry()
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("second Recover: %v", err)
	}

	// Both runs should have loaded the same instance
	if first != 1 || rt.Registry.Count() != 1 {
		t.Errorf("both registries should have 1 instance: first=%d second=%d", first, rt.Registry.Count())
	}
	// No timers cancelled (Case A in both runs)
	if len(timer.cancelled) != 0 {
		t.Errorf("no timers should be cancelled on idempotent run, got %v", timer.cancelled)
	}
}

// FINDINGS A-118: a WF whose SY.timer does not answer starts anyway, DEGRADED, with its instances
// loaded; its timers are reconciled when SY.timer answers (the receive loop asks again).
func TestRecoverStartsDegradedWhenSYTimerDoesNotAnswer(t *testing.T) {
	timer := &mockTimerLister{listErr: errSendFailed}
	rt := recoverRuntime(t, validWorkflowJSON(), timer)
	rt.Plane = bootstrap(t, writeNodeConfig(t, spawnConfig(writeWorkflowPackage(t, validWorkflowJSON()))))
	putRunningInstance(t, rt, "wfi:degraded", "collecting_data")
	pastMS := int64(1776099000000)
	_ = rt.Store.RegisterTimer(context.Background(), "wfi:degraded", "invoice_timeout", 1000, pastMS)

	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("SY.timer not answering must not stop the node: %v", err)
	}
	if rt.timersPending == nil || rt.Registry.Count() != 1 {
		t.Fatalf("expected a degraded start with the instance loaded: pending=%v instances=%d", rt.timersPending, rt.Registry.Count())
	}
	health := func() map[string]any {
		t.Helper()
		disp := rt.ActCtx.Dispatcher.(*mockDispatcher)
		statusGet := sdk.MSGNodeStatusGet
		probe := from("SY.orchestrator@motherbee", sdk.Message{Routing: sdk.Routing{Src: "orchestrator-uuid", TraceID: "t-status"}, Meta: sdk.Meta{MsgType: "system", Msg: &statusGet}})
		if err := Dispatch(context.Background(), probe, rt); err != nil {
			t.Fatal(err)
		}
		return lastReply(t, disp)
	}
	reply := health()
	lastError, _ := reply["last_error"].(map[string]any)
	if reply["health_state"] != "DEGRADED" || lastError["code"] != errCodeTimersNotReconciled {
		t.Fatalf("NODE_STATUS_GET while the timers wait = %v", reply)
	}
	if timers, _ := rt.Store.ListTimersForInstance(context.Background(), "wfi:degraded"); len(timers) != 1 {
		t.Fatalf("nothing is reconciled before SY.timer answers, got %v", timers)
	}

	// SY.timer answers: the past timer is fired, and the node is healthy.
	timer.listErr = nil
	timers, err := rt.listTimers(context.Background(), recoverOpts)
	rt.takeTimerList(context.Background(), timerListResult{timers: timers, err: err, askedAtMS: nowMS(rt.ActCtx.Clock)}, recoverOpts)
	if rt.timersPending != nil {
		t.Fatalf("still pending: %v", rt.timersPending)
	}
	if reply := health(); reply["health_state"] != "HEALTHY" || reply["last_error"] != nil {
		t.Fatalf("NODE_STATUS_GET after the reconciliation = %v", reply)
	}
	logs, _ := rt.Store.GetRecentLog(context.Background(), "wfi:degraded", 5)
	if len(logs) == 0 || logs[0].Summary != "unhandled event TIMER_FIRED in state collecting_data" {
		t.Fatalf("the missed fire reaches the instance once SY.timer answers: %+v", logs)
	}
}

func TestRecoverDrainsPendingInternalEvents(t *testing.T) {
	timer := &mockTimerLister{}
	rt := recoverRuntime(t, workflowJSONWithInternalTransition(), timer)
	putRunningInstance(t, rt, "wfi:recover-internal", "waiting_internal")
	if err := rt.Store.EnqueueInternalEvents(context.Background(), []InternalEventRow{{
		InstanceID:  "wfi:recover-internal",
		MsgType:     "user",
		MsgName:     "INTERNAL_READY",
		PayloadJSON: `{"value":"ready"}`,
		TraceID:     "trace-recover-internal",
		CreatedAtMS: 1000,
	}}); err != nil {
		t.Fatalf("EnqueueInternalEvents: %v", err)
	}

	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	if rt.Registry.Count() != 0 {
		t.Fatalf("expected completed instance to be removed from registry, got %d", rt.Registry.Count())
	}
	row := mustGetInstance(t, rt, "wfi:recover-internal")
	if row.Status != "completed" || row.CurrentState != "completed" {
		t.Fatalf("expected completed row after recover, got status=%q state=%q", row.Status, row.CurrentState)
	}
	events, err := rt.Store.ListInternalEvents(context.Background(), "wfi:recover-internal")
	if err != nil {
		t.Fatalf("ListInternalEvents: %v", err)
	}
	if len(events) != 0 {
		t.Fatalf("expected internal queue to be drained on recover, got %d", len(events))
	}
}

// A timer an instance schedules while the node is DEGRADED goes to SY.timer fire-and-forget, and
// SY.timer, not answering, may never get it. When SY.timer answers again without it, the timer is
// scheduled again, under the same client_ref (SY.timer keeps one pending timer per client_ref),
// instead of leaving the index, which left its instance waiting forever.
func TestATimerScheduledWhileDegradedIsScheduledAgain(t *testing.T) {
	timer := &mockTimerLister{listErr: errSendFailed}
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, timer)
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	if rt.timersPending == nil {
		t.Fatal("expected a DEGRADED start")
	}
	instanceID := startAsking(t, rt, disp, "trace-start")
	scheduled := len(timer.scheduled)

	// A minute later SY.timer answers, without the timer.
	rt.ActCtx.Clock = func() time.Time { return fixedClock().Add(time.Minute) }
	rt.takeTimerList(context.Background(), timerListResult{timers: []sdk.TimerInfo{}, askedAtMS: nowMS(rt.ActCtx.Clock)}, recoverOpts)
	if rt.timersPending != nil {
		t.Fatalf("still DEGRADED: %v", rt.timersPending)
	}
	if ref := timerClientRef(instanceID, "ask_timeout"); len(timer.scheduled) != scheduled+1 || timer.scheduled[scheduled] != ref {
		t.Fatalf("the timer is scheduled again as %s: %v", ref, timer.scheduled[scheduled:])
	}
	row, ok, err := rt.Store.GetTimerForInstance(context.Background(), instanceID, "ask_timeout")
	if err != nil || !ok || row.FireAtMS != fixedClock().Add(2*time.Minute).UnixMilli() {
		t.Fatalf("the index keeps the timer, at its time: %+v %v %v", row, ok, err)
	}
}

// raceWorkflowJSON waits for an answer under a timeout; the answer cancels the timeout, and a fire of
// it would end the instance.
const raceWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "race",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "waiting",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "timeout", "fire_in": "5m"}],
      "transitions": [
        {"event_match": {"msg": "ANSWER"}, "guard": "true", "target_state": "working",
         "actions": [{"type": "cancel_timer", "timer_key": "timeout"}]}
      ]
    },
    {
      "name": "working",
      "transitions": [
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'timeout'", "target_state": "done"},
        {"event_match": {"msg": "FINISH"}, "guard": "true", "target_state": "done"}
      ]
    },
    {"name": "done"}
  ]
}`

// The receive loop asks SY.timer for its list off the loop and takes the answer later: a timer an
// instance schedules in between cannot be in that list. Its row stays (and is not scheduled again),
// so a later cancel_timer still cancels it in SY.timer.
func TestATimerScheduledAfterTheListWasAskedKeepsItsRow(t *testing.T) {
	timer := &mockTimerLister{listErr: errSendFailed}
	rt, _ := newTestRuntime(t, "", raceWorkflowJSON, timer)
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	// The list is asked; a second later, before its answer is taken, an instance schedules a timer.
	askedAtMS := nowMS(rt.ActCtx.Clock)
	rt.ActCtx.Clock = func() time.Time { return fixedClock().Add(time.Second) }
	instanceID := startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
	scheduled := len(timer.scheduled)
	rt.takeTimerList(context.Background(), timerListResult{timers: []sdk.TimerInfo{}, askedAtMS: askedAtMS}, recoverOpts)
	if _, ok, _ := rt.Store.GetTimerForInstance(context.Background(), instanceID, "timeout"); !ok || len(timer.scheduled) != scheduled {
		t.Fatalf("the row stays as it is: kept=%v, scheduled again %d times", ok, len(timer.scheduled)-scheduled)
	}

	answer := from("IO.in@motherbee", msgWithName("ANSWER"))
	answer.Meta.ThreadID = &instanceID
	if err := Dispatch(context.Background(), answer, rt); err != nil {
		t.Fatal(err)
	}
	ref := timerClientRef(instanceID, "timeout")
	if len(timer.cancelled) != 1 || timer.cancelled[0] != ref {
		t.Fatalf("cancel_timer cancels %s in SY.timer: %v", ref, timer.cancelled)
	}
}

// remindWorkflowJSON schedules a reminder (2m), which cancels the deadline (10m), named
// deadlineKey; the entry actions schedule them in the order they fire, or the other way round.
func remindWorkflowJSON(deadlineKey string, deadlineFirst bool) string {
	remind := `{"type": "schedule_timer", "timer_key": "remind", "fire_in": "2m"}`
	deadline := fmt.Sprintf(`{"type": "schedule_timer", "timer_key": %q, "fire_in": "10m"}`, deadlineKey)
	entry := remind + ", " + deadline
	if deadlineFirst {
		entry = deadline + ", " + remind
	}
	return fmt.Sprintf(`{
  "wf_schema_version": "1",
  "workflow_type": "remind",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["timed_out", "done"],
  "states": [
    {
      "name": "waiting",
      "entry_actions": [%[1]s],
      "transitions": [
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'remind'", "target_state": "reminded",
         "actions": [{"type": "cancel_timer", "timer_key": %[2]q}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == '%[2]s'", "target_state": "timed_out"}
      ]
    },
    {
      "name": "reminded",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "final", "fire_in": "1h"}],
      "transitions": [
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == '%[2]s'", "target_state": "timed_out"},
        {"event_match": {"msg": "DONE"}, "guard": "true", "target_state": "done"}
      ]
    },
    {"name": "timed_out"},
    {"name": "done"}
  ]
}`, entry, deadlineKey)
}

// Two timers fired while the WF was down and SY.timer lost both fires: recovery fires them in the
// order they fire, and reads each row again before its fire. The reminder (2m) cancels the deadline
// (10m), so the deadline never fires: the instance ends in reminded, whatever the timers are called
// or the order they were scheduled in.
func TestRecoveryFiresLostTimersInTheOrderTheyFire(t *testing.T) {
	for _, tc := range []struct {
		name          string
		deadlineKey   string
		deadlineFirst bool
	}{
		{"remind and timeout", "timeout", false},
		{"a deadline scheduled first, whose key sorts first", "deadline", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			definition := remindWorkflowJSON(tc.deadlineKey, tc.deadlineFirst)
			rt, _ := newTestRuntime(t, "", definition, nil)
			instanceID := startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
			// 30 minutes later the WF starts again; SY.timer has nothing pending.
			rt.Registry = NewInstanceRegistry()
			rt.ActCtx.Timer = &mockTimerLister{}
			rt.ActCtx.Clock = func() time.Time { return fixedClock().Add(30 * time.Minute) }
			if err := rt.Recover(context.Background(), recoverOpts); err != nil {
				t.Fatal(err)
			}
			if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "reminded" || row.Status != "running" {
				t.Fatalf("the deadline fired after the reminder cancelled it: state %s, status %s", row.CurrentState, row.Status)
			}
		})
	}
}

// An instance whose current state its definition does not have (one a v1/v2 store's migration put
// on the definition the node had then, whatever it was born with) cannot take an event: recovery
// fails it at load (state_not_in_definition), loudly, and loads the others.
func TestAnInstanceInAStateItsDefinitionLacksFailsAtRecovery(t *testing.T) {
	rt := recoverRuntime(t, validWorkflowJSON(), &mockTimerLister{})
	putRunningInstance(t, rt, "wfi:lost", "a_state_no_longer_there")
	putRunningInstance(t, rt, "wfi:fine", "collecting_data")
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	if row := mustGetInstance(t, rt, "wfi:lost"); row.Status != "failed" {
		t.Fatalf("the instance: %+v", row)
	}
	if entry := lastLog(t, rt, "wfi:lost"); !strings.HasPrefix(entry.ErrorDetail, causeStateNotInDefinition) {
		t.Fatalf("its log names the cause: %+v", entry)
	}
	if _, ok := rt.Registry.Get("wfi:fine"); !ok || rt.Registry.Count() != 1 {
		t.Fatalf("the other instance is loaded: %d running", rt.Registry.Count())
	}
}
