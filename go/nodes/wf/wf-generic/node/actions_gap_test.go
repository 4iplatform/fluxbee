package node

import (
	"context"
	"encoding/json"
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

// 0.1.91 (wf-orchestration-v1 §5.2.1, items 1 and 7) + gap-3 + Case 3 (§5.3.6): a send carries the
// instance's thread, the conversation's (thread_id_ref left the language), and its meta.context
// (e.g. the person's context.io) with the WF's block: a trail that starts at this node, what the
// action's vars writes, $ref-resolved, and the action's menu, as written.
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
				"io": map[string]any{"reply_target": map[string]any{"kind": "api", "address": "conv-9"}},
			},
		},
		Vars: map[string]any{"order": map[string]any{"$ref": "state.order"}, "note": "<a & b>"},
		Menu: &Menu{Help: "Invoice the <order> & answer.", Options: []MenuOption{
			{Name: "issued", Description: "You issued it.", Params: map[string]MenuParam{"due_in": {Type: "string", Enum: []string{"1h", "30d"}}}},
			{Name: "answer", Description: "You answer."},
		}},
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
	want := `{"io":{"reply_target":{"address":"conv-9","kind":"api"}},"wf":{"menu":{"help":"Invoice the <order> & answer.","options":[{"name":"issued","description":"You issued it.","params":{"due_in":{"type":"string","enum":["1h","30d"]}}},{"name":"answer","description":"You answer."}]},"trail":["WF.invoice@motherbee"],"vars":{"note":"<a & b>","order":{"quantity":10,"unit_price":1.5}}}}`
	if got := string(sent.Meta.Context); got != want {
		t.Fatalf("meta.context:\n got %s\nwant %s", got, want)
	}
}

// Without a meta.context or vars, a send still carries the WF's block, with its trail, and the
// instance's thread; without a person (the message that started the instance had no src_ilk nor
// ich), it carries none.
func TestSendMessageWithoutContextCarriesTheTrail(t *testing.T) {
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-gap2"}
	actx := makeActx(t, disp, &mockTimerSender{})
	inst := NewInstance(mustLoadValidDefinition(t), "wfi:inst-2", "conv-2", "invoice", map[string]any{}, fixedClock)
	action := ActionDefinition{
		Type:    "send_message",
		Target:  "AI.host@motherbee",
		Meta:    &ActionMeta{Msg: "ask"},
		Payload: map[string]any{},
	}
	if err := execSendMessage(context.Background(), action, inst, msgWithName("GO"), actx); err != nil {
		t.Fatalf("execSendMessage: %v", err)
	}
	if got := derefStringPtr(disp.sent[0].Meta.ThreadID); got != "conv-2" {
		t.Fatalf("a send carries the instance's thread, got %q", got)
	}
	if got := string(disp.sent[0].Meta.Context); got != `{"wf":{"trail":["WF.invoice@motherbee"]}}` {
		t.Fatalf("meta.context: %s", got)
	}
	if meta := disp.sent[0].Meta; meta.SrcIlk != nil || meta.Ich != nil {
		t.Fatalf("no person: %+v", meta)
	}
}

// Case 4 (wf-orchestration-v1 §5.4.10): a send carries the instance's person, the src_ilk and ich of
// the message that started it, so an agent files the WF's own message under the person (the rest of
// FINDINGS A-116); and a send to an IO node carries the action's subject in the WF's block, which
// the IO node places it by.
func TestASendCarriesThePersonAndTheSubject(t *testing.T) {
	disp := &mockDispatcher{l2name: "WF.sales@motherbee", uuid: "wf-uuid-person"}
	actx := makeActx(t, disp, &mockTimerSender{})
	inst := NewInstance(mustLoadValidDefinition(t), "wfi:inst-3", "conv-3", "invoice", map[string]any{}, fixedClock)
	inst.PersonILK, inst.PersonICH = "ilk:person", "ich:api:conv-3"
	toAgent := ActionDefinition{Type: "send_message", Target: "AI.payments@motherbee", Meta: &ActionMeta{Msg: "PAYMENT_DUE"}, Payload: map[string]any{"type": "text", "content": "due"}}
	toTeam := ActionDefinition{Type: "send_message", Target: "IO.slack.team@motherbee", Subject: "end", Meta: &ActionMeta{Msg: "CLOSE"}, Payload: map[string]any{"type": "text", "content": "closed"}}
	for _, action := range []ActionDefinition{toAgent, toTeam} {
		if err := execSendMessage(context.Background(), action, inst, msgWithName("GO"), actx); err != nil {
			t.Fatalf("execSendMessage: %v", err)
		}
	}
	for _, sent := range disp.sent {
		if derefStringPtr(sent.Meta.SrcIlk) != "ilk:person" || derefStringPtr(sent.Meta.Ich) != "ich:api:conv-3" || derefStringPtr(sent.Meta.ThreadID) != "conv-3" {
			t.Fatalf("a send carries the person, on the instance's thread: %+v", sent.Meta)
		}
	}
	if got := string(disp.sent[0].Meta.Context); got != `{"wf":{"trail":["WF.sales@motherbee"]}}` {
		t.Fatalf("a send without a subject has none: %s", got)
	}
	if got := string(disp.sent[1].Meta.Context); got != `{"wf":{"subject":"end","trail":["WF.sales@motherbee"]}}` {
		t.Fatalf("the subject goes in the WF's block: %s", got)
	}

	// A forward carries the subject too, and the message's own person, never the instance's.
	forwarded := from("IO.api.sales@motherbee", sdk.Message{
		Routing: sdk.Routing{TraceID: "trace-person"},
		Meta:    sdk.Meta{MsgType: "user", SrcIlk: strPtr("ilk:other"), ThreadID: strPtr("conv-3")},
		Payload: json.RawMessage(`{"type":"text","content":"hi"}`),
	})
	if err := execForward(context.Background(), ActionDefinition{Type: "forward", Target: "IO.slack.team@motherbee", Subject: "same"}, inst, forwarded, actx); err != nil {
		t.Fatal(err)
	}
	sent := disp.sent[2]
	if derefStringPtr(sent.Meta.SrcIlk) != "ilk:other" || sent.Meta.Ich != nil || string(sent.Meta.Context) != `{"wf":{"subject":"same","trail":["IO.api.sales@motherbee","WF.sales@motherbee"]}}` {
		t.Fatalf("the forward: %+v %s", sent.Meta, sent.Meta.Context)
	}
}
