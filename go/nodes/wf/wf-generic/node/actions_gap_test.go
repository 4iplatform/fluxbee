package node

import (
	"context"
	"errors"
	"fmt"
	"testing"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// gap-2: a failing send_message (default on_error=fail) is fatal — executeActions returns the error
// so the caller can route to failed; on_error=continue suppresses it; best-effort never fails.
func TestExecuteActionsFailFastPolicy(t *testing.T) {
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "u", sendErr: fmt.Errorf("router down")}
	actx := makeActx(t, disp, &mockTimerSender{})
	inst := &WFInstance{InstanceID: "wfi:f1", Input: map[string]any{}, StateVars: map[string]any{}}
	actions := []ActionDefinition{
		{Type: "send_message", Target: "AI.host@motherbee", Meta: &ActionMeta{Msg: "ask"}, Payload: map[string]any{}},
	}
	var emitted []InternalEventRow

	if err := inst.executeActions(context.Background(), actions, msgWithName("GO"), actx, &emitted); err == nil {
		t.Fatal("default on_error for send_message must be fatal when the send fails")
	}
	actions[0].OnError = OnErrorContinue
	if err := inst.executeActions(context.Background(), actions, msgWithName("GO"), actx, &emitted); err != nil {
		t.Fatalf("on_error=continue must not be fatal, got %v", err)
	}
	actions[0].OnError = ""                                                                         // back to default fail
	inst.executeActionsBestEffort(context.Background(), actions, msgWithName("GO"), actx, &emitted) // must not be fatal
}

// flakyDispatcher fails its first failFirst SendMsg calls (transient backpressure, or err when set)
// then succeeds.
type flakyDispatcher struct {
	failFirst int
	err       error
	calls     int
	sent      []sdk.Message
}

func (m *flakyDispatcher) SendMsg(msg sdk.Message) error {
	m.calls++
	if m.calls <= m.failFirst {
		if m.err != nil {
			return m.err
		}
		return fmt.Errorf("sender queue full")
	}
	m.sent = append(m.sent, msg)
	return nil
}
func (m *flakyDispatcher) NodeL2Name() string { return "WF.invoice@motherbee" }
func (m *flakyDispatcher) NodeUUID() string   { return "u" }

// FIX-R2: a transient "sender queue full" is retried, so a momentary blip does NOT fail the instance.
func TestSendMessageRetriesTransientQueueFull(t *testing.T) {
	disp := &flakyDispatcher{failFirst: sendMaxAttempts - 1} // fail all but the last attempt
	s := openTestStore(t)
	actx := ActionContext{Store: s, Dispatcher: disp, Timer: &mockTimerSender{}, Clock: fixedClock}
	inst := &WFInstance{InstanceID: "wfi:r2", Input: map[string]any{}, StateVars: map[string]any{}}
	action := ActionDefinition{Type: "send_message", Target: "AI.host@motherbee", Meta: &ActionMeta{Msg: "ask"}, Payload: map[string]any{}}
	if err := execSendMessage(context.Background(), action, inst, msgWithName("GO"), actx); err != nil {
		t.Fatalf("transient queue-full should be retried, not fatal: %v", err)
	}
	if len(disp.sent) != 1 {
		t.Fatalf("expected the message to eventually send, got %d", len(disp.sent))
	}
	if disp.calls != sendMaxAttempts {
		t.Fatalf("expected %d attempts, got %d", sendMaxAttempts, disp.calls)
	}
}

// A-75: a message over the frame limit is refused the same way every time, so send_message fails
// at once instead of retrying it.
func TestSendMessageDoesNotRetryAnOversizeFrame(t *testing.T) {
	disp := &flakyDispatcher{failFirst: sendMaxAttempts, err: &sdk.MessageTooLargeError{Size: 200 * 1024, Max: 128 * 1024}}
	s := openTestStore(t)
	actx := ActionContext{Store: s, Dispatcher: disp, Timer: &mockTimerSender{}, Clock: fixedClock}
	inst := &WFInstance{InstanceID: "wfi:r3", Input: map[string]any{}, StateVars: map[string]any{}}
	action := ActionDefinition{Type: "send_message", Target: "AI.host@motherbee", Meta: &ActionMeta{Msg: "ask"}, Payload: map[string]any{}}
	err := execSendMessage(context.Background(), action, inst, msgWithName("GO"), actx)
	var tooLarge *sdk.MessageTooLargeError
	if !errors.As(err, &tooLarge) {
		t.Fatalf("expected the oversize error, got %v", err)
	}
	if disp.calls != 1 {
		t.Fatalf("expected 1 attempt, got %d", disp.calls)
	}
}

// gap-2: routeToFailed terminates the instance loudly in the failed status.
func TestRouteToFailedTerminatesInstance(t *testing.T) {
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "u"}
	actx := makeActx(t, disp, &mockTimerSender{})
	inst := &WFInstance{
		InstanceID:   "wfi:f2",
		Input:        map[string]any{},
		StateVars:    map[string]any{},
		Status:       "running",
		CurrentState: "processing",
	}
	row, _ := inst.ToRow(fixedClock)
	if err := actx.Store.CreateInstance(context.Background(), row); err != nil {
		t.Fatalf("CreateInstance: %v", err)
	}
	if err := inst.routeToFailed(context.Background(), msgWithName("X"), actx, fmt.Errorf("boom"), takenEvent{}); err != nil {
		t.Fatalf("routeToFailed: %v", err)
	}
	if inst.Status != "failed" {
		t.Fatalf("expected Status=failed, got %q", inst.Status)
	}
	if inst.TerminatedAtMS == nil {
		t.Fatal("expected TerminatedAtMS to be set on failure")
	}
}

// 0.1.91 (wf-orchestration-v1 §5.2.1, items 1 and 7) + gap-3: a send carries the instance's
// thread, the conversation's (thread_id_ref left the language), and its meta.context (e.g.
// response_envelope for AI structured output) with the WF's block: a trail that starts at this
// node, and what the action's vars writes, $ref-resolved.
func TestSendMessageCarriesTheInstancesThreadAndTheWFBlock(t *testing.T) {
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-gap"}
	actx := makeActx(t, disp, &mockTimerSender{})
	inst := &WFInstance{
		InstanceID: "wfi:inst-1",
		ThreadID:   "conv-9",
		Input:      map[string]any{},
		StateVars:  map[string]any{"order": map[string]any{"quantity": int64(10), "unit_price": 1.5}},
	}
	action := ActionDefinition{
		Type:   "send_message",
		Target: "AI.invoice@motherbee",
		Meta: &ActionMeta{
			Msg: "answer",
			Context: map[string]any{
				"response_envelope": map[string]any{"schema": "invoice_result"},
			},
		},
		Vars:    map[string]any{"order": map[string]any{"$ref": "state.order"}, "note": "<a & b>"},
		Payload: map[string]any{"text": "done"},
	}
	if err := execSendMessage(context.Background(), action, inst, msgWithName("REPLY"), actx); err != nil {
		t.Fatalf("execSendMessage: %v", err)
	}
	if len(disp.sent) != 1 {
		t.Fatalf("expected 1 sent message, got %d", len(disp.sent))
	}
	sent := disp.sent[0]
	if got := derefStringPtr(sent.Meta.ThreadID); got != "conv-9" {
		t.Fatalf("a send carries the instance's thread, got %q", got)
	}
	want := `{"response_envelope":{"schema":"invoice_result"},"wf":{"trail":["WF.invoice@motherbee"],"vars":{"note":"<a & b>","order":{"quantity":10,"unit_price":1.5}}}}`
	if got := string(sent.Meta.Context); got != want {
		t.Fatalf("meta.context:\n got %s\nwant %s", got, want)
	}
}

// Without a meta.context or vars, a send still carries the WF's block, with its trail; the thread of
// an instance started by a message without one is the instance's own id.
func TestSendMessageWithoutContextCarriesTheTrail(t *testing.T) {
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-gap2"}
	actx := makeActx(t, disp, &mockTimerSender{})
	inst := NewInstance(mustLoadValidDefinition(t), "wfi:inst-2", "invoice", map[string]any{}, fixedClock)
	action := ActionDefinition{
		Type:    "send_message",
		Target:  "AI.host@motherbee",
		Meta:    &ActionMeta{Msg: "ask"},
		Payload: map[string]any{},
	}
	if err := execSendMessage(context.Background(), action, inst, msgWithName("GO"), actx); err != nil {
		t.Fatalf("execSendMessage: %v", err)
	}
	if got := derefStringPtr(disp.sent[0].Meta.ThreadID); got != "wfi:inst-2" {
		t.Fatalf("the thread of an instance started without one is its id, got %q", got)
	}
	if got := string(disp.sent[0].Meta.Context); got != `{"wf":{"trail":["WF.invoice@motherbee"]}}` {
		t.Fatalf("meta.context: %s", got)
	}
}
