package node

import (
	"context"
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

func makeRecoverActx(t *testing.T, timer TimerSender) ActionContext {
	t.Helper()
	s := openTestStore(t)
	return ActionContext{
		Store:      s,
		Dispatcher: &mockDispatcher{l2name: "WF.invoice@motherbee"},
		Timer:      timer,
		Clock:      fixedClock,
	}
}

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
	def, _ := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	timer := &mockTimerLister{}
	actx := makeRecoverActx(t, timer)
	reg := NewInstanceRegistry()

	// Insert a running instance
	inst := WFInstanceRow{
		InstanceID: "wfi:recover-1", WorkflowType: "invoice",
		Status: "running", CurrentState: "collecting_data",
		InputJSON: `{"customer_id":"c1"}`, StateJSON: `{}`,
		CreatedAtMS: 1000, UpdatedAtMS: 1000,
	}
	if err := actx.Store.CreateInstance(context.Background(), inst); err != nil {
		t.Fatalf("create: %v", err)
	}

	if err := Recover(context.Background(), def, actx.Store, reg, actx, RecoverOptions{OwnerL2Name: "WF.invoice@motherbee"}); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	if reg.Count() != 1 {
		t.Errorf("expected 1 instance in registry, got %d", reg.Count())
	}
	if _, ok := reg.Get("wfi:recover-1"); !ok {
		t.Error("wfi:recover-1 not in registry")
	}
	if timer.lastFilter.Limit != sdk.MaxTimerListLimit {
		t.Fatalf("expected recover TIMER_LIST limit=%d, got %d", sdk.MaxTimerListLimit, timer.lastFilter.Limit)
	}
}

func TestRecoverCaseA_FutureTimer_NoAction(t *testing.T) {
	def, _ := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	// fixedClock = 1776100000000 ms; future = 1776100000000 + 60000 ms
	futureMS := int64(1776100060000)
	ref := timerClientRef("wfi:a1", "invoice_timeout")

	timer := &mockTimerLister{
		timerList: []sdk.TimerInfo{timerInfo(ref, futureMS)},
	}
	actx := makeRecoverActx(t, timer)
	reg := NewInstanceRegistry()

	inst := WFInstanceRow{
		InstanceID: "wfi:a1", WorkflowType: "invoice",
		Status: "running", CurrentState: "collecting_data",
		InputJSON: `{"customer_id":"c1"}`, StateJSON: `{}`,
		CreatedAtMS: 1000, UpdatedAtMS: 1000,
	}
	_ = actx.Store.CreateInstance(context.Background(), inst)
	_ = actx.Store.RegisterTimer(context.Background(), "wfi:a1", "invoice_timeout", 1000, futureMS)

	if err := Recover(context.Background(), def, actx.Store, reg, actx, RecoverOptions{OwnerL2Name: "WF.invoice@motherbee"}); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	// Case A: future timer — should NOT be cancelled
	if len(timer.cancelled) != 0 {
		t.Errorf("Case A: no timers should be cancelled, got %v", timer.cancelled)
	}

	// Timer row should still exist in local store
	timers, _ := actx.Store.ListTimersForInstance(context.Background(), "wfi:a1")
	if len(timers) != 1 {
		t.Errorf("Case A: timer row should still exist, got %d", len(timers))
	}
}

func TestRecoverCaseB_PastTimer_SyntheticFireAndCancel(t *testing.T) {
	def, _ := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	// fixedClock = 1776100000000; past = 1776099000000
	pastMS := int64(1776099000000)
	ref := timerClientRef("wfi:b1", "invoice_timeout")

	timer := &mockTimerLister{
		timerList: []sdk.TimerInfo{timerInfo(ref, pastMS)},
	}
	actx := makeRecoverActx(t, timer)
	reg := NewInstanceRegistry()

	inst := WFInstanceRow{
		InstanceID: "wfi:b1", WorkflowType: "invoice",
		Status: "running", CurrentState: "collecting_data",
		InputJSON: `{"customer_id":"c1"}`, StateJSON: `{}`,
		CreatedAtMS: 1000, UpdatedAtMS: 1000,
	}
	_ = actx.Store.CreateInstance(context.Background(), inst)
	_ = actx.Store.RegisterTimer(context.Background(), "wfi:b1", "invoice_timeout", 1000, pastMS)

	if err := Recover(context.Background(), def, actx.Store, reg, actx, RecoverOptions{OwnerL2Name: "WF.invoice@motherbee"}); err != nil {
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
	timers, _ := actx.Store.ListTimersForInstance(context.Background(), "wfi:b1")
	if len(timers) != 0 {
		t.Errorf("Case B: timer row should be deleted after synthetic fire, got %d", len(timers))
	}
}

func TestRecoverCaseB_DoesNotCancelNewTimerWhenSyntheticFireReschedulesSameKey(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(workflowJSONWithTimerRefire()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	pastMS := int64(1776099000000)
	ref := timerClientRef("wfi:b2", "invoice_timeout")

	timer := &mockTimerLister{
		timerList: []sdk.TimerInfo{timerInfo(ref, pastMS)},
	}
	actx := makeRecoverActx(t, timer)
	reg := NewInstanceRegistry()

	inst := WFInstanceRow{
		InstanceID: "wfi:b2", WorkflowType: "invoice",
		Status: "running", CurrentState: "waiting_timeout",
		InputJSON: `{"customer_id":"c1"}`, StateJSON: `{}`,
		CreatedAtMS: 1000, UpdatedAtMS: 1000,
	}
	_ = actx.Store.CreateInstance(context.Background(), inst)
	_ = actx.Store.RegisterTimer(context.Background(), "wfi:b2", "invoice_timeout", 1000, pastMS)

	if err := Recover(context.Background(), def, actx.Store, reg, actx, RecoverOptions{OwnerL2Name: "WF.invoice@motherbee"}); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	if len(timer.cancelled) != 0 {
		t.Fatalf("re-scheduled timer should not be cancelled during recovery, got %v", timer.cancelled)
	}
	rows, err := actx.Store.ListTimersForInstance(context.Background(), "wfi:b2")
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
          "guard": "true",
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
	def, _ := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	timer := &mockTimerLister{timerList: []sdk.TimerInfo{}} // SY.timer has nothing

	actx := makeRecoverActx(t, timer)
	reg := NewInstanceRegistry()

	inst := WFInstanceRow{
		InstanceID: "wfi:c1", WorkflowType: "invoice",
		Status: "running", CurrentState: "collecting_data",
		InputJSON: `{"customer_id":"c1"}`, StateJSON: `{}`,
		CreatedAtMS: 1000, UpdatedAtMS: 1000,
	}
	_ = actx.Store.CreateInstance(context.Background(), inst)
	_ = actx.Store.RegisterTimer(context.Background(), "wfi:c1", "invoice_timeout", 1000, 99999)

	if err := Recover(context.Background(), def, actx.Store, reg, actx, RecoverOptions{OwnerL2Name: "WF.invoice@motherbee"}); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	// Case C: local row should be deleted
	timers, _ := actx.Store.ListTimersForInstance(context.Background(), "wfi:c1")
	if len(timers) != 0 {
		t.Errorf("Case C: local timer row should be deleted, got %d", len(timers))
	}
}

func TestRecoverOrphanedSYTimerCancelled(t *testing.T) {
	def, _ := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	// SY.timer has a timer the WF doesn't expect
	orphanRef := timerClientRef("wfi:gone", "some_timer")
	timer := &mockTimerLister{
		timerList: []sdk.TimerInfo{timerInfo(orphanRef, 1776100060000)},
	}
	actx := makeRecoverActx(t, timer)
	reg := NewInstanceRegistry()
	// No local timer rows registered

	if err := Recover(context.Background(), def, actx.Store, reg, actx, RecoverOptions{OwnerL2Name: "WF.invoice@motherbee"}); err != nil {
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
	def, _ := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	futureMS := int64(1776100060000)
	ref := timerClientRef("wfi:cancelling", "invoice_timeout")

	timer := &mockTimerLister{
		timerList: []sdk.TimerInfo{timerInfo(ref, futureMS)},
	}
	actx := makeRecoverActx(t, timer)
	reg := NewInstanceRegistry()

	// Instance is in cancelling status
	inst := WFInstanceRow{
		InstanceID: "wfi:cancelling", WorkflowType: "invoice",
		Status: "cancelling", CurrentState: "collecting_data",
		InputJSON: `{"customer_id":"c1"}`, StateJSON: `{}`,
		CreatedAtMS: 1000, UpdatedAtMS: 1000,
	}
	_ = actx.Store.CreateInstance(context.Background(), inst)
	_ = actx.Store.RegisterTimer(context.Background(), "wfi:cancelling", "invoice_timeout", 1000, futureMS)

	if err := Recover(context.Background(), def, actx.Store, reg, actx, RecoverOptions{OwnerL2Name: "WF.invoice@motherbee"}); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	// Cancelling instance should still be in registry
	if _, ok := reg.Get("wfi:cancelling"); !ok {
		t.Error("cancelling instance should be in registry")
	}
	// Timer should NOT be cancelled (we skip reconciliation for cancelling instances)
	if len(timer.cancelled) != 0 {
		t.Errorf("cancelling instance: no timers should be touched, cancelled=%v", timer.cancelled)
	}
}

func TestRecoverIdempotent(t *testing.T) {
	def, _ := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	futureMS := int64(1776100060000)
	ref := timerClientRef("wfi:idem", "invoice_timeout")

	timer := &mockTimerLister{
		timerList: []sdk.TimerInfo{timerInfo(ref, futureMS)},
	}
	actx := makeRecoverActx(t, timer)

	inst := WFInstanceRow{
		InstanceID: "wfi:idem", WorkflowType: "invoice",
		Status: "running", CurrentState: "collecting_data",
		InputJSON: `{"customer_id":"c1"}`, StateJSON: `{}`,
		CreatedAtMS: 1000, UpdatedAtMS: 1000,
	}
	_ = actx.Store.CreateInstance(context.Background(), inst)
	_ = actx.Store.RegisterTimer(context.Background(), "wfi:idem", "invoice_timeout", 1000, futureMS)

	// Run recovery twice
	reg1 := NewInstanceRegistry()
	if err := Recover(context.Background(), def, actx.Store, reg1, actx, RecoverOptions{OwnerL2Name: "WF.invoice@motherbee"}); err != nil {
		t.Fatalf("first Recover: %v", err)
	}
	reg2 := NewInstanceRegistry()
	if err := Recover(context.Background(), def, actx.Store, reg2, actx, RecoverOptions{OwnerL2Name: "WF.invoice@motherbee"}); err != nil {
		t.Fatalf("second Recover: %v", err)
	}

	// Both runs should have loaded the same instance
	if reg1.Count() != 1 || reg2.Count() != 1 {
		t.Errorf("both registries should have 1 instance: reg1=%d reg2=%d", reg1.Count(), reg2.Count())
	}
	// No timers cancelled (Case A in both runs)
	if len(timer.cancelled) != 0 {
		t.Errorf("no timers should be cancelled on idempotent run, got %v", timer.cancelled)
	}
}

func TestRecoverTimerListFailureReturnsError(t *testing.T) {
	def, _ := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	timer := &mockTimerLister{listErr: errSendFailed}
	actx := makeRecoverActx(t, timer)
	// Override clock so retries are fast
	actx.Clock = func() time.Time { return time.UnixMilli(1776100000000).UTC() }

	reg := NewInstanceRegistry()
	err := Recover(context.Background(), def, actx.Store, reg, actx, RecoverOptions{
		OwnerL2Name: "WF.invoice@motherbee",
		MaxRetries:  1,
	})
	if err == nil {
		t.Fatal("expected error when TIMER_LIST fails")
	}
}

func TestRecoverDrainsPendingInternalEvents(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(workflowJSONWithInternalTransition()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	timer := &mockTimerLister{}
	actx := makeRecoverActx(t, timer)
	reg := NewInstanceRegistry()

	inst := WFInstanceRow{
		InstanceID:   "wfi:recover-internal",
		WorkflowType: "internal-transition",
		Status:       "running",
		CurrentState: "waiting_internal",
		InputJSON:    `{"customer_id":"c1"}`,
		StateJSON:    `{}`,
		CreatedAtMS:  1000,
		UpdatedAtMS:  1000,
	}
	if err := actx.Store.CreateInstance(context.Background(), inst); err != nil {
		t.Fatalf("CreateInstance: %v", err)
	}
	if err := actx.Store.EnqueueInternalEvents(context.Background(), []InternalEventRow{{
		InstanceID:  inst.InstanceID,
		MsgType:     "system",
		MsgName:     "INTERNAL_READY",
		PayloadJSON: `{"value":"ready"}`,
		TraceID:     "trace-recover-internal",
		CreatedAtMS: 1000,
	}}); err != nil {
		t.Fatalf("EnqueueInternalEvents: %v", err)
	}

	if err := Recover(context.Background(), def, actx.Store, reg, actx, RecoverOptions{OwnerL2Name: "WF.invoice@motherbee"}); err != nil {
		t.Fatalf("Recover: %v", err)
	}

	if reg.Count() != 0 {
		t.Fatalf("expected completed instance to be removed from registry, got %d", reg.Count())
	}
	row, err := actx.Store.GetInstance(context.Background(), inst.InstanceID)
	if err != nil {
		t.Fatalf("GetInstance: %v", err)
	}
	if row.Status != "completed" || row.CurrentState != "completed" {
		t.Fatalf("expected completed row after recover, got status=%q state=%q", row.Status, row.CurrentState)
	}
	events, err := actx.Store.ListInternalEvents(context.Background(), inst.InstanceID)
	if err != nil {
		t.Fatalf("ListInternalEvents: %v", err)
	}
	if len(events) != 0 {
		t.Fatalf("expected internal queue to be drained on recover, got %d", len(events))
	}
}
