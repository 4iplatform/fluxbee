package node

import (
	"context"
	"encoding/json"
	"fmt"
	"testing"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// --- Mock implementations ---

type mockDispatcher struct {
	sent    []sdk.Message
	l2name  string
	uuid    string
	sendErr error
}

func (m *mockDispatcher) SendMsg(msg sdk.Message) error {
	if m.sendErr != nil {
		return m.sendErr
	}
	m.sent = append(m.sent, msg)
	return nil
}
func (m *mockDispatcher) NodeL2Name() string { return m.l2name }
func (m *mockDispatcher) NodeUUID() string {
	if m.uuid == "" {
		return "wf-uuid-test"
	}
	return m.uuid
}

type mockTimerSender struct {
	scheduled    []string // clientRefs scheduled
	cancelled    []string // clientRefs cancelled
	rescheduled  []string // clientRefs rescheduled
	scheduleErr  error
	cancelErr    error
}

func (m *mockTimerSender) ScheduleIn(_ context.Context, d time.Duration, opts sdk.ScheduleOptions) (sdk.TimerID, error) {
	m.scheduled = append(m.scheduled, opts.ClientRef)
	return sdk.TimerID("mock-uuid"), m.scheduleErr
}
func (m *mockTimerSender) Schedule(_ context.Context, _ time.Time, opts sdk.ScheduleOptions) (sdk.TimerID, error) {
	m.scheduled = append(m.scheduled, opts.ClientRef)
	return sdk.TimerID("mock-uuid"), m.scheduleErr
}
func (m *mockTimerSender) CancelByClientRef(_ context.Context, ref string) error {
	m.cancelled = append(m.cancelled, ref)
	return m.cancelErr
}
func (m *mockTimerSender) RescheduleByClientRef(_ context.Context, ref string, _ time.Time) error {
	m.rescheduled = append(m.rescheduled, ref)
	return nil
}
func (m *mockTimerSender) List(_ context.Context, _ sdk.ListFilter) ([]sdk.TimerInfo, error) {
	return nil, nil
}

// --- Helpers ---

func makeActx(t *testing.T, disp *mockDispatcher, timer *mockTimerSender) ActionContext {
	t.Helper()
	s := openTestStore(t)
	return ActionContext{
		Store:      s,
		Dispatcher: disp,
		Timer:      timer,
		Clock:      fixedClock,
	}
}

func msgWithName(name string) sdk.Message {
	nameCopy := name
	return sdk.Message{
		Routing: sdk.Routing{TraceID: "trace-1"},
		Meta:    sdk.Meta{MsgType: "system", Msg: &nameCopy},
		Payload: json.RawMessage(`{}`),
	}
}

func msgWithNameAndPayload(name string, payload map[string]any) sdk.Message {
	nameCopy := name
	raw, _ := json.Marshal(payload)
	return sdk.Message{
		Routing: sdk.Routing{TraceID: "trace-1"},
		Meta:    sdk.Meta{MsgType: "system", Msg: &nameCopy},
		Payload: raw,
	}
}

// --- Tests ---

func TestInstanceCreationRunsEntryActions(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-1"}
	timer := &mockTimerSender{}
	actx := makeActx(t, disp, timer)

	inst := NewInstance(def, "wfi:test-001", def.WorkflowType, map[string]any{"customer_id": "cust-1"}, fixedClock)

	// Persist before running entry_actions (as createAndDispatch does)
	row, _ := inst.ToRow(fixedClock)
	if err := actx.Store.CreateInstance(context.Background(), row); err != nil {
		t.Fatalf("CreateInstance: %v", err)
	}

	inst.Lock()
	defer inst.Unlock()

	initialState := inst.findState(def.InitialState)
	if initialState == nil {
		t.Fatalf("initial state not found")
	}
	inst.executeActions(context.Background(), initialState.EntryActions, msgWithName("START"), actx)

	// entry_actions has: send_message + schedule_timer
	if len(disp.sent) != 1 {
		t.Fatalf("expected 1 send_message, got %d", len(disp.sent))
	}
	if got := disp.sent[0].Routing.Src; got != "wf-uuid-1" {
		t.Fatalf("send_message should use node uuid as routing.src, got %q", got)
	}
	if len(timer.scheduled) != 1 {
		t.Fatalf("expected 1 schedule_timer, got %d", len(timer.scheduled))
	}
	expectedRef := timerClientRef("wfi:test-001", "invoice_timeout")
	if timer.scheduled[0] != expectedRef {
		t.Errorf("expected client_ref=%q, got %q", expectedRef, timer.scheduled[0])
	}
}

func TestRunTransitionTakesFirstMatchingGuard(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	actx := makeActx(t, &mockDispatcher{l2name: "WF.invoice@motherbee"}, &mockTimerSender{})

	inst := NewInstance(def, "wfi:t2", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)

	// Event that matches the transition guard (complete == true)
	event := msgWithNameAndPayload("DATA_VALIDATION_RESPONSE", map[string]any{
		"complete": true,
	})

	inst.Lock()
	err = inst.RunTransition(context.Background(), event, actx)
	inst.Unlock()

	if err != nil {
		t.Fatalf("RunTransition: %v", err)
	}
	if inst.CurrentState != "completed" {
		t.Errorf("expected state=completed, got %s", inst.CurrentState)
	}
	if inst.Status != "completed" {
		t.Errorf("expected status=completed, got %s", inst.Status)
	}
	if inst.TerminatedAtMS == nil {
		t.Error("TerminatedAtMS should be set after terminal state")
	}
}

func TestCreateAndDispatchRejectsPayloadThatViolatesInputSchema(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-2"}
	actx := makeActx(t, disp, &mockTimerSender{})
	reg := NewInstanceRegistry()

	msg := msgWithNameAndPayload("INVOICE_START", map[string]any{
		"wrong_field": "missing required customer_id",
	})
	msg.Routing.Src = "src-uuid-1"

	err = CorrelateAndDispatch(context.Background(), msg, reg, def, actx.Store, actx)
	if err != nil {
		t.Fatalf("CorrelateAndDispatch: %v", err)
	}
	if reg.Count() != 0 {
		t.Fatalf("invalid payload should not create an instance")
	}
	if len(disp.sent) != 1 {
		t.Fatalf("expected one error reply, got %d", len(disp.sent))
	}
	if got := derefString(disp.sent[0].Meta.Msg); got != "INVALID_INPUT" {
		t.Fatalf("expected INVALID_INPUT error msg, got %q", got)
	}
}

func TestWFGetInstanceInvalidRequestUsesResponseVerbAndUUIDSrc(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-3"}
	actx := makeActx(t, disp, &mockTimerSender{})
	rt := &NodeRuntime{
		Def:      def,
		DefJSON:  validWorkflowJSON(),
		Registry: NewInstanceRegistry(),
		Store:    actx.Store,
		ActCtx:   actx,
		NodeUUID: "wf-uuid-3",
	}

	msgName := MsgWFGetInstance
	msg := sdk.Message{
		Routing: sdk.Routing{
			Src:     "caller-uuid-1",
			Dst:     sdk.UnicastDestination("WF.invoice@motherbee"),
			TTL:     16,
			TraceID: "trace-get-1",
		},
		Meta: sdk.Meta{
			MsgType: "system",
			Msg:     &msgName,
		},
		Payload: json.RawMessage(`{}`),
	}

	if err := Dispatch(context.Background(), msg, rt); err != nil {
		t.Fatalf("Dispatch: %v", err)
	}
	if len(disp.sent) != 1 {
		t.Fatalf("expected one error reply, got %d", len(disp.sent))
	}
	reply := disp.sent[0]
	if got := derefString(reply.Meta.Msg); got != MsgWFGetInstanceResponse {
		t.Fatalf("expected response verb %q, got %q", MsgWFGetInstanceResponse, got)
	}
	if reply.Routing.Src != "wf-uuid-3" {
		t.Fatalf("expected routing.src to use node uuid, got %q", reply.Routing.Src)
	}
}

func derefString(value *string) string {
	if value == nil {
		return ""
	}
	return *value
}

func TestRunTransitionNoMatchLogsUnhandled(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	actx := makeActx(t, &mockDispatcher{l2name: "WF.invoice@motherbee"}, &mockTimerSender{})

	inst := NewInstance(def, "wfi:t3", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)

	// Event that doesn't match any transition
	event := msgWithName("UNKNOWN_EVENT")

	inst.Lock()
	err = inst.RunTransition(context.Background(), event, actx)
	inst.Unlock()

	if err != nil {
		t.Fatalf("RunTransition: %v", err)
	}
	// State should be unchanged
	if inst.CurrentState != "collecting_data" {
		t.Errorf("expected state unchanged, got %s", inst.CurrentState)
	}

	logs, _ := actx.Store.GetRecentLog(context.Background(), inst.InstanceID, 5)
	if len(logs) == 0 {
		t.Fatal("expected unhandled event to be logged")
	}
	found := false
	for _, l := range logs {
		if l.ActionType == "event" && !l.OK {
			found = true
			break
		}
	}
	if !found {
		t.Error("expected an unhandled-event log entry")
	}
}

func TestRunTransitionGuardFalseNoTransition(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	actx := makeActx(t, &mockDispatcher{l2name: "WF.invoice@motherbee"}, &mockTimerSender{})

	inst := NewInstance(def, "wfi:t4", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)

	// Correct msg but guard evaluates to false (complete=false)
	event := msgWithNameAndPayload("DATA_VALIDATION_RESPONSE", map[string]any{"complete": false})

	inst.Lock()
	err = inst.RunTransition(context.Background(), event, actx)
	inst.Unlock()

	if err != nil {
		t.Fatalf("RunTransition: %v", err)
	}
	if inst.CurrentState != "collecting_data" {
		t.Errorf("expected state unchanged, got %s", inst.CurrentState)
	}
}

func TestRunTransitionCancellingGoesToCancelled(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	actx := makeActx(t, &mockDispatcher{l2name: "WF.invoice@motherbee"}, &mockTimerSender{})

	inst := NewInstance(def, "wfi:t5", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	inst.Status = "cancelling"
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)

	inst.Lock()
	err = inst.RunTransition(context.Background(), msgWithName("ANYTHING"), actx)
	inst.Unlock()

	if err != nil {
		t.Fatalf("RunTransition: %v", err)
	}
	if inst.Status != "cancelled" {
		t.Errorf("expected status=cancelled, got %s", inst.Status)
	}
	if inst.TerminatedAtMS == nil {
		t.Error("TerminatedAtMS should be set")
	}
}

func TestTerminalStateCleansCancelledTimers(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	timer := &mockTimerSender{}
	actx := makeActx(t, &mockDispatcher{l2name: "WF.invoice@motherbee"}, timer)

	inst := NewInstance(def, "wfi:t6", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)

	// Register a timer manually
	_ = actx.Store.RegisterTimer(context.Background(), inst.InstanceID, "invoice_timeout", 1000, 2000)

	// Transition to terminal state
	event := msgWithNameAndPayload("DATA_VALIDATION_RESPONSE", map[string]any{"complete": true})

	inst.Lock()
	err = inst.RunTransition(context.Background(), event, actx)
	inst.Unlock()

	if err != nil {
		t.Fatalf("RunTransition: %v", err)
	}
	if inst.Status != "completed" {
		t.Fatalf("expected completed, got %s", inst.Status)
	}
	expectedRef := timerClientRef(inst.InstanceID, "invoice_timeout")
	found := false
	for _, ref := range timer.cancelled {
		if ref == expectedRef {
			found = true
		}
	}
	if !found {
		t.Errorf("expected timer %q to be cancelled on terminal state, cancelled=%v", expectedRef, timer.cancelled)
	}
}

func TestActionFailureDoesNotHaltTransition(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	// Dispatcher that always fails
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", sendErr: errSendFailed}
	actx := makeActx(t, disp, &mockTimerSender{})

	inst := NewInstance(def, "wfi:t7", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)

	// Transition still completes even though send_message fails
	event := msgWithNameAndPayload("DATA_VALIDATION_RESPONSE", map[string]any{"complete": true})

	inst.Lock()
	err = inst.RunTransition(context.Background(), event, actx)
	inst.Unlock()

	if err != nil {
		t.Fatalf("RunTransition should succeed even with action failure: %v", err)
	}
	if inst.CurrentState != "completed" {
		t.Errorf("expected completed, got %s", inst.CurrentState)
	}
}

func TestSetVariableUpdatesStateVars(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	actx := makeActx(t, &mockDispatcher{l2name: "WF.invoice@motherbee"}, &mockTimerSender{})

	inst := NewInstance(def, "wfi:t8", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)

	// The transition in validWorkflowJSON has set_variable validated_at = now()
	event := msgWithNameAndPayload("DATA_VALIDATION_RESPONSE", map[string]any{"complete": true})

	inst.Lock()
	err = inst.RunTransition(context.Background(), event, actx)
	inst.Unlock()

	if err != nil {
		t.Fatalf("RunTransition: %v", err)
	}
	if _, ok := inst.StateVars["validated_at"]; !ok {
		t.Error("expected validated_at to be set in StateVars")
	}
}

func TestTimerFiredCorrelatesByClientRef(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	actx := makeActx(t, &mockDispatcher{l2name: "WF.invoice@motherbee"}, &mockTimerSender{})

	reg := NewInstanceRegistry()
	inst := NewInstance(def, "wfi:timer-test", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)
	reg.Add(inst)

	// Build a TIMER_FIRED message for this instance
	clientRef := timerClientRef("wfi:timer-test", "invoice_timeout")
	clientRefCopy := clientRef
	fired := sdk.FiredEvent{
		TimerUUID:            "some-uuid",
		OwnerL2Name:          "WF.invoice@motherbee",
		ClientRef:            &clientRefCopy,
		Kind:                 "oneshot",
		ScheduledFireAtUTCMS: 1776100000000,
		ActualFireAtUTCMS:    1776100000000,
		FireCount:            1,
		IsLastFire:           true,
		UserPayload:          json.RawMessage(`{"instance_id":"wfi:timer-test","timer_key":"invoice_timeout"}`),
	}
	firedJSON, _ := json.Marshal(fired)
	msgNameStr := sdk.MsgTimerFired
	timerMsg := sdk.Message{
		Routing: sdk.Routing{TraceID: "trace-timer"},
		Meta:    sdk.Meta{MsgType: "system", Msg: &msgNameStr},
		Payload: firedJSON,
	}

	err = CorrelateAndDispatch(context.Background(), timerMsg, reg, def, actx.Store, actx)
	if err != nil {
		t.Fatalf("CorrelateAndDispatch: %v", err)
	}
	// State should not have changed (TIMER_FIRED doesn't match DATA_VALIDATION_RESPONSE)
	// but the dispatch should have reached the correct instance
	if inst.CurrentState != "collecting_data" {
		t.Errorf("unexpected state change: %s", inst.CurrentState)
	}
}

func TestThreadIDCorrelation(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	actx := makeActx(t, &mockDispatcher{l2name: "WF.invoice@motherbee"}, &mockTimerSender{})

	reg := NewInstanceRegistry()
	inst := NewInstance(def, "wfi:thread-test", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)
	reg.Add(inst)

	// Message with thread_id = instance_id
	threadID := "wfi:thread-test"
	event := sdk.Message{
		Routing: sdk.Routing{TraceID: "trace-2"},
		Meta: sdk.Meta{
			MsgType:  "system",
			Msg:      strPtr("DATA_VALIDATION_RESPONSE"),
			ThreadID: &threadID,
		},
		Payload: json.RawMessage(`{"complete":true}`),
	}

	err = CorrelateAndDispatch(context.Background(), event, reg, def, actx.Store, actx)
	if err != nil {
		t.Fatalf("CorrelateAndDispatch: %v", err)
	}
	if inst.CurrentState != "completed" {
		t.Errorf("expected completed via thread_id correlation, got %s", inst.CurrentState)
	}
}

func TestTraceIDPreservedAcrossTransition(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee"}
	actx := makeActx(t, disp, &mockTimerSender{})

	inst := NewInstance(def, "wfi:trace-test", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)

	event := sdk.Message{
		Routing: sdk.Routing{TraceID: "original-trace-id"},
		Meta:    sdk.Meta{MsgType: "system", Msg: strPtr("DATA_VALIDATION_RESPONSE")},
		Payload: json.RawMessage(`{"complete":true}`),
	}

	inst.Lock()
	err = inst.RunTransition(context.Background(), event, actx)
	inst.Unlock()

	if err != nil {
		t.Fatalf("RunTransition: %v", err)
	}
	// After completion, current_trace_id should be cleared
	if inst.CurrentTraceID != "" {
		t.Errorf("CurrentTraceID should be cleared after transition, got %q", inst.CurrentTraceID)
	}
}

// errSendFailed is a sentinel error for mock dispatcher failures.
var errSendFailed = fmt.Errorf("mock send failure")

func strPtr(s string) *string { return &s }
