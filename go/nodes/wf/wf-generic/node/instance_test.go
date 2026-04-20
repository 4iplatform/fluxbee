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
	scheduled   []string // clientRefs scheduled
	cancelled   []string // clientRefs cancelled
	rescheduled []string // clientRefs rescheduled
	scheduleErr error
	cancelErr   error
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
func (m *mockTimerSender) CancelByClientRefConfirmed(_ context.Context, ref string) error {
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
	var emitted []InternalEventRow
	inst.executeActions(context.Background(), initialState.EntryActions, msgWithName("START"), actx, &emitted)

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
	err = inst.RunTransition(context.Background(), event, actx, nil)
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

func TestWFCancelInstanceTransitionsImmediatelyToCancelled(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-cancel-now"}
	timer := &mockTimerSender{}
	actx := makeActx(t, disp, timer)
	reg := NewInstanceRegistry()

	inst := NewInstance(def, "wfi:cancel-now", def.WorkflowType, map[string]any{"customer_id": "cust-1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	if err := actx.Store.CreateInstance(context.Background(), row); err != nil {
		t.Fatalf("CreateInstance: %v", err)
	}
	if err := actx.Store.RegisterTimer(context.Background(), inst.InstanceID, "invoice_timeout", nowMS(fixedClock), nowMS(fixedClock)+60000); err != nil {
		t.Fatalf("RegisterTimer: %v", err)
	}
	reg.Add(inst)

	rt := &NodeRuntime{
		Def:      def,
		DefJSON:  validWorkflowJSON(),
		Registry: reg,
		Store:    actx.Store,
		ActCtx:   actx,
		NodeUUID: "wf-uuid-cancel-now",
		NodeName: "WF.invoice@motherbee",
	}

	msgName := MsgWFCancelInstance
	msg := sdk.Message{
		Routing: sdk.Routing{
			Src:     "caller-uuid-cancel",
			Dst:     sdk.UnicastDestination("WF.invoice@motherbee"),
			TTL:     16,
			TraceID: "trace-cancel-now-1",
		},
		Meta: sdk.Meta{
			MsgType: "system",
			Msg:     &msgName,
		},
		Payload: json.RawMessage(`{"instance_id":"wfi:cancel-now","reason":"operator request"}`),
	}

	if err := Dispatch(context.Background(), msg, rt); err != nil {
		t.Fatalf("Dispatch cancel: %v", err)
	}
	rowAfter, err := actx.Store.GetInstance(context.Background(), inst.InstanceID)
	if err != nil {
		t.Fatalf("GetInstance: %v", err)
	}
	if rowAfter.Status != "cancelled" {
		t.Fatalf("expected cancelled status, got %q", rowAfter.Status)
	}
	if rowAfter.CurrentState != "cancelled" {
		t.Fatalf("expected cancelled state, got %q", rowAfter.CurrentState)
	}
	if _, ok := reg.Get(inst.InstanceID); ok {
		t.Fatalf("expected instance to be removed from active registry")
	}
	expectedRef := timerClientRef(inst.InstanceID, "invoice_timeout")
	foundCancel := false
	for _, ref := range timer.cancelled {
		if ref == expectedRef {
			foundCancel = true
			break
		}
	}
	if !foundCancel {
		t.Fatalf("expected timer %q to be cancelled, got %v", expectedRef, timer.cancelled)
	}
	reply := disp.sent[len(disp.sent)-1]
	if got := derefString(reply.Meta.Msg); got != MsgWFCancelInstanceResponse {
		t.Fatalf("expected cancel response verb, got %q", got)
	}
	var payload map[string]any
	if err := json.Unmarshal(reply.Payload, &payload); err != nil {
		t.Fatalf("unmarshal reply payload: %v", err)
	}
	if payload["status"] != "cancelled" {
		t.Fatalf("expected reply status cancelled, got %#v", payload["status"])
	}
}

func TestDispatchIgnoresInfrastructureSystemMessages(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-ignore"}
	timer := &mockTimerSender{}
	actx := makeActx(t, disp, timer)
	rt := &NodeRuntime{
		Def:      def,
		DefJSON:  validWorkflowJSON(),
		Registry: NewInstanceRegistry(),
		Store:    actx.Store,
		ActCtx:   actx,
		NodeUUID: "wf-uuid-ignore",
		NodeName: "WF.invoice@motherbee",
	}

	msgName := sdk.MSGUnreachable
	msg := sdk.Message{
		Routing: sdk.Routing{
			Src:     "router-uuid",
			Dst:     sdk.UnicastDestination("WF.invoice@motherbee"),
			TTL:     16,
			TraceID: "trace-ignore-unreachable-1",
		},
		Meta: sdk.Meta{
			MsgType: sdk.SYSTEMKind,
			Msg:     &msgName,
		},
		Payload: json.RawMessage(`{"original_dst":"router-uuid","reason":"NODE_NOT_FOUND"}`),
	}

	if err := Dispatch(context.Background(), msg, rt); err != nil {
		t.Fatalf("Dispatch UNREACHABLE: %v", err)
	}
	if len(disp.sent) != 0 {
		t.Fatalf("expected no replies for ignored infrastructure message, got %d", len(disp.sent))
	}
	rows, err := actx.Store.ListInstances(context.Background(), "", 10, 0)
	if err != nil {
		t.Fatalf("ListInstances: %v", err)
	}
	if len(rows) != 0 {
		t.Fatalf("expected no instances created, got %d", len(rows))
	}
}

func TestDispatchIgnoresTimerResponseSystemMessages(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-ignore-timer"}
	timer := &mockTimerSender{}
	actx := makeActx(t, disp, timer)
	rt := &NodeRuntime{
		Def:      def,
		DefJSON:  validWorkflowJSON(),
		Registry: NewInstanceRegistry(),
		Store:    actx.Store,
		ActCtx:   actx,
		NodeUUID: "wf-uuid-ignore-timer",
		NodeName: "WF.invoice@motherbee",
	}

	msgName := sdk.MsgTimerResponse
	msg := sdk.Message{
		Routing: sdk.Routing{
			Src:     "SY.timer@motherbee",
			Dst:     sdk.UnicastDestination("WF.invoice@motherbee"),
			TTL:     16,
			TraceID: "trace-ignore-timer-response-1",
		},
		Meta: sdk.Meta{
			MsgType: sdk.SYSTEMKind,
			Msg:     &msgName,
		},
		Payload: json.RawMessage(`{"ok":false,"verb":"TIMER_LIST","error":{"code":"TIMER_INTERNAL","message":"boom"}}`),
	}

	if err := Dispatch(context.Background(), msg, rt); err != nil {
		t.Fatalf("Dispatch TIMER_RESPONSE: %v", err)
	}
	if len(disp.sent) != 0 {
		t.Fatalf("expected no replies for ignored TIMER_RESPONSE, got %d", len(disp.sent))
	}
	rows, err := actx.Store.ListInstances(context.Background(), "", 10, 0)
	if err != nil {
		t.Fatalf("ListInstances: %v", err)
	}
	if len(rows) != 0 {
		t.Fatalf("expected no instances created, got %d", len(rows))
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
	err = inst.RunTransition(context.Background(), event, actx, nil)
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
	err = inst.RunTransition(context.Background(), event, actx, nil)
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
	err = inst.RunTransition(context.Background(), msgWithName("ANYTHING"), actx, nil)
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
	err = inst.RunTransition(context.Background(), event, actx, nil)
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
	err = inst.RunTransition(context.Background(), event, actx, nil)
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
	err = inst.RunTransition(context.Background(), event, actx, nil)
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
	err = inst.RunTransition(context.Background(), event, actx, nil)
	inst.Unlock()

	if err != nil {
		t.Fatalf("RunTransition: %v", err)
	}
	// After completion, current_trace_id should be cleared
	if inst.CurrentTraceID != "" {
		t.Errorf("CurrentTraceID should be cleared after transition, got %q", inst.CurrentTraceID)
	}
}

func TestRunTransitionEmitInternalEventAdvancesSameInstance(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(workflowJSONWithInternalTransition()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	actx := makeActx(t, &mockDispatcher{l2name: "WF.internal@motherbee"}, &mockTimerSender{})

	inst := NewInstance(def, "wfi:int-1", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)

	inst.Lock()
	err = inst.RunTransition(context.Background(), msgWithName("START"), actx, nil)
	if err == nil {
		err = inst.drainInternalEvents(context.Background(), actx)
	}
	inst.Unlock()

	if err != nil {
		t.Fatalf("internal event transition: %v", err)
	}
	if inst.CurrentState != "completed" {
		t.Fatalf("expected completed, got %s", inst.CurrentState)
	}
	if got := inst.StateVars["seen"]; got != "ready" {
		t.Fatalf("expected seen=ready, got %#v", got)
	}
	events, err := actx.Store.ListInternalEvents(context.Background(), inst.InstanceID)
	if err != nil {
		t.Fatalf("ListInternalEvents: %v", err)
	}
	if len(events) != 0 {
		t.Fatalf("expected empty internal queue, got %d", len(events))
	}
}

func TestCreateAndDispatchDrainsInitialInternalEvent(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(workflowJSONWithInitialInternalEvent()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	disp := &mockDispatcher{l2name: "WF.initial@motherbee", uuid: "wf-uuid-initial"}
	actx := makeActx(t, disp, &mockTimerSender{})
	reg := NewInstanceRegistry()

	msg := msgWithNameAndPayload("INITIAL_TRIGGER", map[string]any{"customer_id": "c1"})
	if err := CorrelateAndDispatch(context.Background(), msg, reg, def, actx.Store, actx); err != nil {
		t.Fatalf("CorrelateAndDispatch: %v", err)
	}
	if reg.Count() != 0 {
		t.Fatalf("completed instance should not remain in active registry")
	}
	rows, err := actx.Store.ListInstances(context.Background(), "completed", 10, 0)
	if err != nil {
		t.Fatalf("ListInstances: %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("expected one completed instance, got %d", len(rows))
	}
}

func TestInternalEventsPreserveFIFOOrder(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(workflowJSONWithTwoInternalEvents()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	actx := makeActx(t, &mockDispatcher{l2name: "WF.fifo@motherbee"}, &mockTimerSender{})

	inst := NewInstance(def, "wfi:int-fifo", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)

	inst.Lock()
	err = inst.RunTransition(context.Background(), msgWithName("START"), actx, nil)
	if err == nil {
		err = inst.drainInternalEvents(context.Background(), actx)
	}
	inst.Unlock()

	if err != nil {
		t.Fatalf("FIFO transition: %v", err)
	}
	if inst.CurrentState != "completed" {
		t.Fatalf("expected completed, got %s", inst.CurrentState)
	}
	if got := inst.StateVars["first_seen"]; got != "first" {
		t.Fatalf("expected first_seen=first, got %#v", got)
	}
	if got := inst.StateVars["second_seen"]; got != "second" {
		t.Fatalf("expected second_seen=second, got %#v", got)
	}
}

func TestEmitInternalEventFromTargetEntryActionsIsDrainedAfterCommit(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(workflowJSONWithTargetEntryInternalEvent()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	actx := makeActx(t, &mockDispatcher{l2name: "WF.entry@motherbee"}, &mockTimerSender{})

	inst := NewInstance(def, "wfi:int-entry", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)

	inst.Lock()
	err = inst.RunTransition(context.Background(), msgWithName("START"), actx, nil)
	if err == nil {
		err = inst.drainInternalEvents(context.Background(), actx)
	}
	inst.Unlock()

	if err != nil {
		t.Fatalf("target entry internal event: %v", err)
	}
	if inst.CurrentState != "completed" {
		t.Fatalf("expected completed, got %s", inst.CurrentState)
	}
	if got := inst.StateVars["entry_seen"]; got != "entered" {
		t.Fatalf("expected entry_seen=entered, got %#v", got)
	}
}

func TestTerminalTransitionClearsQueuedInternalEvents(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("load def: %v", err)
	}
	actx := makeActx(t, &mockDispatcher{l2name: "WF.invoice@motherbee"}, &mockTimerSender{})

	inst := NewInstance(def, "wfi:clear-queue", def.WorkflowType, map[string]any{"customer_id": "c1"}, fixedClock)
	row, _ := inst.ToRow(fixedClock)
	_ = actx.Store.CreateInstance(context.Background(), row)
	if err := actx.Store.EnqueueInternalEvents(context.Background(), []InternalEventRow{{
		InstanceID:  inst.InstanceID,
		MsgType:     "system",
		MsgName:     "SHOULD_BE_DROPPED",
		PayloadJSON: `{}`,
		TraceID:     "trace-drop",
		CreatedAtMS: nowMS(fixedClock),
	}}); err != nil {
		t.Fatalf("EnqueueInternalEvents: %v", err)
	}

	inst.Lock()
	err = inst.RunTransition(context.Background(), msgWithNameAndPayload("DATA_VALIDATION_RESPONSE", map[string]any{"complete": true}), actx, nil)
	inst.Unlock()

	if err != nil {
		t.Fatalf("RunTransition: %v", err)
	}
	events, err := actx.Store.ListInternalEvents(context.Background(), inst.InstanceID)
	if err != nil {
		t.Fatalf("ListInternalEvents: %v", err)
	}
	if len(events) != 0 {
		t.Fatalf("expected terminal cleanup to clear internal events, got %d", len(events))
	}
}

// errSendFailed is a sentinel error for mock dispatcher failures.
var errSendFailed = fmt.Errorf("mock send failure")

func strPtr(s string) *string { return &s }

func workflowJSONWithInternalTransition() string {
	return `{
  "wf_schema_version": "1",
  "workflow_type": "internal-transition",
  "description": "emit internal event from transition",
  "input_schema": {
    "type": "object",
    "required": ["customer_id"],
    "properties": {
      "customer_id": { "type": "string" }
    }
  },
  "initial_state": "waiting_start",
  "terminal_states": ["completed"],
  "states": [
    {
      "name": "waiting_start",
      "description": "waits for start",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": { "msg": "START" },
          "guard": "true",
          "target_state": "waiting_internal",
          "actions": [
            {
              "type": "emit_internal_event",
              "meta": { "msg": "INTERNAL_READY" },
              "payload": { "value": "ready" }
            }
          ]
        }
      ]
    },
    {
      "name": "waiting_internal",
      "description": "waits for internal event",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": { "msg": "INTERNAL_READY" },
          "guard": "event.payload.value == 'ready'",
          "target_state": "completed",
          "actions": [
            { "type": "set_variable", "name": "seen", "value": "event.payload.value" }
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
    }
  ]
}`
}

func workflowJSONWithInitialInternalEvent() string {
	return `{
  "wf_schema_version": "1",
  "workflow_type": "initial-internal",
  "description": "emit internal event from initial entry actions",
  "input_schema": {
    "type": "object",
    "required": ["customer_id"],
    "properties": {
      "customer_id": { "type": "string" }
    }
  },
  "initial_state": "booting",
  "terminal_states": ["completed"],
  "states": [
    {
      "name": "booting",
      "description": "boots",
      "entry_actions": [
        {
          "type": "emit_internal_event",
          "meta": { "msg": "AUTO_CONTINUE" },
          "payload": { "value": "booted" }
        }
      ],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": { "msg": "AUTO_CONTINUE" },
          "guard": "event.payload.value == 'booted'",
          "target_state": "completed",
          "actions": []
        }
      ]
    },
    {
      "name": "completed",
      "description": "done",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    }
  ]
}`
}

func workflowJSONWithTwoInternalEvents() string {
	return `{
  "wf_schema_version": "1",
  "workflow_type": "fifo-internal",
  "description": "multiple internal events preserve order",
  "input_schema": {
    "type": "object",
    "required": ["customer_id"],
    "properties": {
      "customer_id": { "type": "string" }
    }
  },
  "initial_state": "waiting_start",
  "terminal_states": ["completed"],
  "states": [
    {
      "name": "waiting_start",
      "description": "wait start",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": { "msg": "START" },
          "guard": "true",
          "target_state": "waiting_internal",
          "actions": [
            {
              "type": "emit_internal_event",
              "meta": { "msg": "FIRST" },
              "payload": { "order": "first" }
            },
            {
              "type": "emit_internal_event",
              "meta": { "msg": "SECOND" },
              "payload": { "order": "second" }
            }
          ]
        }
      ]
    },
    {
      "name": "waiting_internal",
      "description": "wait internal",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": { "msg": "FIRST" },
          "guard": "event.payload.order == 'first'",
          "target_state": "after_first",
          "actions": [
            { "type": "set_variable", "name": "first_seen", "value": "event.payload.order" }
          ]
        }
      ]
    },
    {
      "name": "after_first",
      "description": "after first",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": { "msg": "SECOND" },
          "guard": "state.first_seen == 'first' && event.payload.order == 'second'",
          "target_state": "completed",
          "actions": [
            { "type": "set_variable", "name": "second_seen", "value": "event.payload.order" }
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
    }
  ]
}`
}

func workflowJSONWithTargetEntryInternalEvent() string {
	return `{
  "wf_schema_version": "1",
  "workflow_type": "entry-internal",
  "description": "target entry emits internal event",
  "input_schema": {
    "type": "object",
    "required": ["customer_id"],
    "properties": {
      "customer_id": { "type": "string" }
    }
  },
  "initial_state": "waiting_start",
  "terminal_states": ["completed"],
  "states": [
    {
      "name": "waiting_start",
      "description": "wait start",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": { "msg": "START" },
          "guard": "true",
          "target_state": "entered",
          "actions": []
        }
      ]
    },
    {
      "name": "entered",
      "description": "entered",
      "entry_actions": [
        {
          "type": "emit_internal_event",
          "meta": { "msg": "ENTRY_READY" },
          "payload": { "value": "entered" }
        }
      ],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": { "msg": "ENTRY_READY" },
          "guard": "event.payload.value == 'entered'",
          "target_state": "completed",
          "actions": [
            { "type": "set_variable", "name": "entry_seen", "value": "event.payload.value" }
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
    }
  ]
}`
}
