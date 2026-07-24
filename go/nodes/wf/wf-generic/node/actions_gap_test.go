package node

import (
	"context"
	"encoding/json"
	"testing"
)

// gap-5 + gap-3: a terminal reply can answer an external ingress caller on the caller's own
// thread_id (thread_id_ref override) and carry a meta.context (e.g. response_envelope for AI
// structured output).
func TestSendMessageThreadIDRefOverrideAndContext(t *testing.T) {
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-gap"}
	actx := makeActx(t, disp, &mockTimerSender{})
	inst := &WFInstance{
		InstanceID: "wfi:inst-1",
		Input:      map[string]any{},
		StateVars:  map[string]any{},
	}
	// The ingress event carries the caller's own thread_id.
	event := msgWithNameAndPayload("REPLY", map[string]any{"thread_id": "caller-thread-9"})
	action := ActionDefinition{
		Type:        "send_message",
		Target:      "IO.api@motherbee",
		ThreadIDRef: "event.payload.thread_id",
		Meta: &ActionMeta{
			Msg:  "answer",
			Type: "user",
			Context: map[string]any{
				"response_envelope": map[string]any{"schema": "invoice_result"},
			},
		},
		Payload: map[string]any{"text": "done"},
	}
	if err := execSendMessage(context.Background(), action, inst, event, actx); err != nil {
		t.Fatalf("execSendMessage: %v", err)
	}
	if len(disp.sent) != 1 {
		t.Fatalf("expected 1 sent message, got %d", len(disp.sent))
	}
	sent := disp.sent[0]
	if got := derefStringPtr(sent.Meta.ThreadID); got != "caller-thread-9" {
		t.Fatalf("gap-5: thread_id_ref should override instance_id, got %q", got)
	}
	if sent.Meta.Context == nil {
		t.Fatal("gap-3: expected meta.context to be set")
	}
	var ctx map[string]any
	if err := json.Unmarshal(sent.Meta.Context, &ctx); err != nil {
		t.Fatalf("gap-3: context is not valid JSON: %v", err)
	}
	if _, ok := ctx["response_envelope"]; !ok {
		t.Fatalf("gap-3: expected response_envelope in meta.context, got %v", ctx)
	}
}

// Default (no thread_id_ref): thread_id stays the instance_id so AI round-trip replies correlate.
func TestSendMessageDefaultThreadIDIsInstanceID(t *testing.T) {
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-gap2"}
	actx := makeActx(t, disp, &mockTimerSender{})
	inst := &WFInstance{
		InstanceID: "wfi:inst-2",
		Input:      map[string]any{},
		StateVars:  map[string]any{},
	}
	action := ActionDefinition{
		Type:    "send_message",
		Target:  "AI.host@motherbee",
		Meta:    &ActionMeta{Msg: "ask", Type: "system"},
		Payload: map[string]any{},
	}
	if err := execSendMessage(context.Background(), action, inst, msgWithName("GO"), actx); err != nil {
		t.Fatalf("execSendMessage: %v", err)
	}
	if got := derefStringPtr(disp.sent[0].Meta.ThreadID); got != "wfi:inst-2" {
		t.Fatalf("default thread_id must be instance_id, got %q", got)
	}
	if disp.sent[0].Meta.Context != nil {
		t.Fatalf("no meta.context expected when unset, got %s", string(disp.sent[0].Meta.Context))
	}
}
