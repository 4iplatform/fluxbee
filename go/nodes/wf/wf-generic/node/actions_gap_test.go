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

func (m *flakyDispatcher) SendMsg(msg sdk.Message, _ sdk.TraceNote) error {
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
// (e.g. the person's context.io) with its trace, the WF's entry naming its action
// (docs/message-trace-v1.md), and the WF's block: what the action's vars writes, $ref-resolved, and
// the action's menu, as written.
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
	want := `{"io":{"reply_target":{"address":"conv-9","kind":"api"}},"wf":{"menu":{"help":"Invoice the <order> & answer.","options":[{"name":"issued","description":"You issued it.","params":{"due_in":{"type":"string","enum":["1h","30d"]}}},{"name":"answer","description":"You answer."}]},"vars":{"note":"<a & b>","order":{"quantity":10,"unit_price":1.5}}}}`
	if got := contextWithoutTrace(t, sent); got != want {
		t.Fatalf("meta.context:\n got %s\nwant %s", got, want)
	}
	if tracePath(sent) != "[WF.invoice@motherbee]" || traceAct(sent) != "send_message" {
		t.Fatalf("the trace: %s", sent.Meta.Context)
	}
}

// Without a meta.context or vars, a send still carries its trace and the instance's thread, and no
// WF block, which would say nothing; without a person (the message that started the instance had no
// src_ilk nor ich), it carries none.
func TestSendMessageWithoutContextCarriesItsTrace(t *testing.T) {
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
	if got := contextWithoutTrace(t, disp.sent[0]); got != `{}` || tracePath(disp.sent[0]) != "[WF.invoice@motherbee]" {
		t.Fatalf("meta.context: %s", disp.sent[0].Meta.Context)
	}
	if meta := disp.sent[0].Meta; meta.SrcIlk != nil || meta.Ich != nil {
		t.Fatalf("no person: %+v", meta)
	}
}

// What the WF writes names the WF as its writer, its ILK and its node name; a forward passes the
// text on with the writer it came with, whatever the action's meta.context brings, and a message
// that named none goes on naming none (docs/cognition-hands-v1.md §5). A send to an agent still
// carries the person as its src_ilk: whose conversation it is.
func TestTheWFNamesTheWriterOfWhatItSends(t *testing.T) {
	disp := &mockDispatcher{l2name: "WF.soporte@worker1", uuid: "wf-uuid-writer"}
	actx := makeActx(t, disp, &mockTimerSender{})
	actx.SelfILK = "ilk:wf"
	inst := NewInstance(mustLoadValidDefinition(t), "wfi:writer", "conv-w", "invoice", map[string]any{}, fixedClock)
	inst.PersonILK = "ilk:ana"
	writerOf := func(msg sdk.Message) sdk.Writer {
		t.Helper()
		writer, _ := sdk.WriterOf(msg.Meta.Context)
		return writer
	}

	send := ActionDefinition{Type: "send_message", Target: "AI.sales@worker1", Meta: &ActionMeta{Msg: "remind"}, Payload: map[string]any{"text": "Ana has not answered"}}
	if err := execSendMessage(context.Background(), send, inst, msgWithName("TIMER_FIRED"), actx); err != nil {
		t.Fatal(err)
	}
	sent := disp.sent[len(disp.sent)-1]
	if got := writerOf(sent); got != (sdk.Writer{ILK: "ilk:wf", Name: "WF.soporte@worker1"}) {
		t.Fatalf("a send's writer: %+v", got)
	}
	if derefStringPtr(sent.Meta.SrcIlk) != "ilk:ana" {
		t.Fatalf("a send to an agent carries the person: %+v", sent.Meta)
	}

	answer := from("AI.sales@worker1", sdk.Message{
		Routing: sdk.Routing{TraceID: "trace-answer"},
		Meta:    sdk.Meta{MsgType: "user", ThreadID: strPtr("conv-w"), SrcIlk: strPtr("ilk:sales"), Context: json.RawMessage(`{"writer":{"ilk":"ilk:sales","name":"AI.sales@worker1"}}`)},
		Payload: json.RawMessage(`{"type":"text","content":"credit note issued"}`),
	})
	inst.takeCycleTrace(answer)
	// The kept answer is forwarded on a later event whose context names another writer, and the
	// action's meta.context brings one too: from the event by a $ref, or written.
	event := answer
	event.Meta.Context = json.RawMessage(`{"writer":{"ilk":"ilk:forged"}}`)
	for _, actionContext := range []any{
		map[string]any{"$ref": "event.context"},
		map[string]any{"writer": map[string]any{"ilk": "ilk:forged"}},
	} {
		for _, c := range []struct {
			context string
			want    sdk.Writer
		}{
			{`{"writer":{"ilk":"ilk:sales","name":"AI.sales@worker1"}}`, sdk.Writer{ILK: "ilk:sales", Name: "AI.sales@worker1"}},
			{`{"io":{"channel":"slack"}}`, sdk.Writer{}},
		} {
			answer.Meta.Context = json.RawMessage(c.context)
			if err := inst.keep("answer", answer, actx); err != nil {
				t.Fatal(err)
			}
			forward := ActionDefinition{Type: "forward", Target: "AI.invoice@worker1", Message: "answer", Meta: &ActionMeta{Context: actionContext}}
			if err := execForward(context.Background(), forward, inst, event, actx); err != nil {
				t.Fatal(err)
			}
			if got := writerOf(disp.sent[len(disp.sent)-1]); got != c.want {
				t.Fatalf("a forward of %s, its action's context %v: writer %+v, want %+v", c.context, actionContext, got, c.want)
			}
		}
	}
}

// A forward of another WF's message carries this WF's block only: none when its action has no vars,
// menu or subject, and never the block the message brought.
func TestAForwardCarriesNoOtherWFsBlock(t *testing.T) {
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-gap2b"}
	actx := makeActx(t, disp, &mockTimerSender{})
	inst := NewInstance(mustLoadValidDefinition(t), "wfi:inst-2b", "conv-2", "invoice", map[string]any{}, fixedClock)
	upstream := from("WF.sales@motherbee", sdk.Message{
		Routing: sdk.Routing{TraceID: "trace-upstream"},
		Meta:    sdk.Meta{MsgType: "user", ThreadID: strPtr("conv-2"), Context: json.RawMessage(`{"wf":{"vars":{"order":1},"subject":"new"}}`)},
		Payload: json.RawMessage(`{"type":"text","content":"invoice it"}`),
	})
	inst.takeCycleTrace(upstream)
	for _, c := range []struct{ subject, context string }{
		{"", `{}`},
		{"same", `{"wf":{"subject":"same"}}`},
	} {
		action := ActionDefinition{Type: "forward", Target: "IO.slack.team@motherbee", Subject: c.subject}
		if err := execForward(context.Background(), action, inst, upstream, actx); err != nil {
			t.Fatal(err)
		}
		if sent := disp.sent[len(disp.sent)-1]; contextWithoutTrace(t, sent) != c.context {
			t.Fatalf("a forward with subject %q: %s, want %s", c.subject, sent.Meta.Context, c.context)
		}
	}
}

// Case 4 (wf-orchestration-v1 §5.4.10) and Case 5 (§5.5.1, FINDINGS A-139): the ILKs of what the WF
// sends are its facts, from the instance's person, the src_ilk and ich of the message that started
// it. A send to an agent, or to a WF, carries the person as its src_ilk and ich, so an agent files
// the WF's own message under the person (the rest of FINDINGS A-116). A send to an IO node names no
// sender, since the WF wrote it: to the person's IO node (the one whose message started the
// instance) it names the person as its dst_ilk and the person's channel as its ich, and to the team's
// IO node no person and no channel, nor to another IO node of start_from. A send to an IO node carries the action's subject in the WF's block, which the IO node
// places it by.
func TestASendNamesThePersonAsItsTargetNeedsAndCarriesTheSubject(t *testing.T) {
	disp := &mockDispatcher{l2name: "WF.sales@motherbee", uuid: "wf-uuid-person"}
	actx := makeActx(t, disp, &mockTimerSender{})
	inst := NewInstance(mustLoadValidDefinition(t), "wfi:inst-3", "conv-3", "invoice", map[string]any{}, fixedClock)
	const personsIO = "IO.api.billing@motherbee" // the definition's start_from, the person's
	inst.PersonILK, inst.PersonICH, inst.PersonIO = "ilk:person", "ich:api:conv-3", personsIO
	send := func(target, subject string) sdk.Message {
		t.Helper()
		action := ActionDefinition{Type: "send_message", Target: target, Subject: subject, Meta: &ActionMeta{Msg: "NOTE"}, Payload: map[string]any{"type": "text", "content": "hi"}}
		if err := execSendMessage(context.Background(), action, inst, msgWithName("GO"), actx); err != nil {
			t.Fatalf("execSendMessage: %v", err)
		}
		return disp.sent[len(disp.sent)-1]
	}
	ilks := func(msg sdk.Message) string {
		return fmt.Sprintf("src_ilk=%q dst_ilk=%q ich=%q", derefString(msg.Meta.SrcIlk), derefString(msg.Meta.DstIlk), derefString(msg.Meta.Ich))
	}
	const carriesThePerson, namesThePerson, noPerson = `src_ilk="ilk:person" dst_ilk="" ich="ich:api:conv-3"`, `src_ilk="" dst_ilk="ilk:person" ich="ich:api:conv-3"`, `src_ilk="" dst_ilk="" ich=""`
	for _, c := range []struct{ target, subject, ilks, context string }{
		{"AI.payments@motherbee", "", carriesThePerson, `{}`},
		{"WF.billing@motherbee", "", carriesThePerson, `{}`},
		{personsIO, "same", namesThePerson, `{"wf":{"subject":"same"}}`},
		{"IO.slack.team@motherbee", "end", noPerson, `{"wf":{"subject":"end"}}`},
	} {
		sent := send(c.target, c.subject)
		if ilks(sent) != c.ilks || derefString(sent.Meta.ThreadID) != "conv-3" || contextWithoutTrace(t, sent) != c.context {
			t.Fatalf("a send to %s: %s on %q, %s; want %s, %s", c.target, ilks(sent), derefString(sent.Meta.ThreadID), sent.Meta.Context, c.ilks, c.context)
		}
	}

	// A forward carries the subject too, and the message's meta as received, but whom it names: to
	// the team's IO node its src_ilk, and no dst_ilk and no ich (FINDINGS A-215); to the person's IO
	// node the person as its dst_ilk and the person's channel as its ich, where an agent's answer to a
	// supervisor names the supervisor, whom it answered, and the supervisors' channel, where the
	// supervisor wrote.
	answer := from("AI.sales@motherbee", sdk.Message{
		Routing: sdk.Routing{TraceID: "trace-answer"},
		Meta:    sdk.Meta{MsgType: "user", SrcIlk: strPtr("ilk:agent"), DstIlk: strPtr("ilk:supervisor"), Ich: strPtr("ich:slack:team"), ThreadID: strPtr("conv-3")},
		Payload: json.RawMessage(`{"type":"text","content":"approved"}`),
	})
	inst.takeCycleTrace(answer) // as its transition takes it
	forward := func(target, subject string) sdk.Message {
		t.Helper()
		if err := execForward(context.Background(), ActionDefinition{Type: "forward", Target: target, Subject: subject}, inst, answer, actx); err != nil {
			t.Fatal(err)
		}
		return disp.sent[len(disp.sent)-1]
	}
	const asReceived, namesNoOne = `src_ilk="ilk:agent" dst_ilk="ilk:supervisor" ich="ich:slack:team"`, `src_ilk="ilk:agent" dst_ilk="" ich=""`
	if sent := forward("IO.slack.team@motherbee", "same"); ilks(sent) != namesNoOne ||
		contextWithoutTrace(t, sent) != `{"wf":{"subject":"same"}}` || tracePath(sent) != "[AI.sales@motherbee WF.sales@motherbee]" {
		t.Fatalf("a forward to the team: %s %s", ilks(sent), sent.Meta.Context)
	}
	if sent := forward(personsIO, ""); ilks(sent) != `src_ilk="ilk:agent" dst_ilk="ilk:person" ich="ich:api:conv-3"` {
		t.Fatalf("a forward to the person's IO node: %s", ilks(sent))
	}
	if ilks(answer) != asReceived {
		t.Fatalf("the forward changed the message it sent: %s", ilks(answer))
	}

	// An IO node of start_from the person did not write from is not the person's: when the person
	// started the instance on another one, what goes to it names no person, and a forward no one.
	inst.PersonIO = "IO.api.shop@motherbee"
	if sent := send(personsIO, ""); ilks(sent) != noPerson {
		t.Fatalf("a send to an IO node of start_from the person did not write from: %s", ilks(sent))
	}
	if sent := forward(personsIO, ""); ilks(sent) != namesNoOne {
		t.Fatalf("a forward to an IO node of start_from the person did not write from: %s", ilks(sent))
	}
	inst.PersonIO = personsIO

	// A person whose message had no ich: what goes to the person's IO node names no channel, never the
	// supervisors'.
	inst.PersonICH = ""
	if sent := forward(personsIO, ""); ilks(sent) != `src_ilk="ilk:agent" dst_ilk="ilk:person" ich=""` {
		t.Fatalf("a forward to a person with no channel: %s", ilks(sent))
	}
	if sent := send(personsIO, ""); ilks(sent) != `src_ilk="" dst_ilk="ilk:person" ich=""` {
		t.Fatalf("a send to a person with no channel: %s", ilks(sent))
	}

	// Without a person (several people write on the thread): a forward to the person's IO node keeps
	// its dst_ilk and its ich, and a send names nobody.
	inst.PersonILK, inst.PersonICH = "", ""
	if sent := forward(personsIO, ""); ilks(sent) != asReceived {
		t.Fatalf("a forward without a person: %s", ilks(sent))
	}
	for _, target := range []string{personsIO, "AI.payments@motherbee"} {
		if sent := send(target, ""); ilks(sent) != noPerson {
			t.Fatalf("a send to %s without a person: %s", target, ilks(sent))
		}
	}
}

// FINDINGS A-215: a forward to an IO node other than the person's names no one there, as a send to
// it does: the message's dst_ilk and ich go, whoever it was said to and on whatever channel, and its
// src_ilk, who speaks, stays. Before, an agent's answer to the person forwarded to the team's channel
// kept the person as its dst_ilk, and cognition, which takes a relay's addressee from it, filed it as
// said to the person, via the team's node. A forward to the person's own IO node keeps A-139's
// rewrite, the person and the person's channel, or, with no person, the message's own; a forward to
// an agent goes as it came.
func TestAForwardToAnotherIONodeNamesNoOne(t *testing.T) {
	disp := &mockDispatcher{l2name: "WF.sales@motherbee", uuid: "wf-uuid-a215"}
	actx := makeActx(t, disp, &mockTimerSender{})
	inst := NewInstance(mustLoadValidDefinition(t), "wfi:inst-a215", "conv-a215", "invoice", map[string]any{}, fixedClock)
	const personsIO, team = "IO.api.billing@motherbee", "IO.slack.team@motherbee"
	inst.PersonILK, inst.PersonICH, inst.PersonIO = "ilk:person", "ich:api:conv-a215", personsIO
	ilks := func(msg sdk.Message) string {
		return fmt.Sprintf("src_ilk=%q dst_ilk=%q ich=%q", derefString(msg.Meta.SrcIlk), derefString(msg.Meta.DstIlk), derefString(msg.Meta.Ich))
	}
	// The agent's answer to the person, on the person's channel, and the person's own message.
	toThePerson := from("AI.sales@motherbee", sdk.Message{
		Routing: sdk.Routing{TraceID: "trace-a215-answer"},
		Meta:    sdk.Meta{MsgType: "user", SrcIlk: strPtr("ilk:agent"), DstIlk: strPtr("ilk:person"), Ich: strPtr("ich:api:conv-a215"), ThreadID: strPtr("conv-a215")},
		Payload: json.RawMessage(`{"type":"text","content":"Your invoice is on its way."}`),
	})
	persons := from(personsIO, sdk.Message{
		Routing: sdk.Routing{TraceID: "trace-a215-person"},
		Meta:    sdk.Meta{MsgType: "user", SrcIlk: strPtr("ilk:person"), Ich: strPtr("ich:api:conv-a215"), ThreadID: strPtr("conv-a215")},
		Payload: json.RawMessage(`{"type":"text","content":"Is anyone there?"}`),
	})
	if err := inst.keep("answer", toThePerson, actx); err != nil {
		t.Fatal(err)
	}
	forward := func(target, kept string, event sdk.Message) sdk.Message {
		t.Helper()
		inst.takeCycleTrace(event) // as its transition takes it
		if err := execForward(context.Background(), ActionDefinition{Type: "forward", Target: target, Message: kept}, inst, event, actx); err != nil {
			t.Fatal(err)
		}
		return disp.sent[len(disp.sent)-1]
	}

	const agentToNoOne, personToNoOne = `src_ilk="ilk:agent" dst_ilk="" ich=""`, `src_ilk="ilk:person" dst_ilk="" ich=""`
	const answerAsItCame = `src_ilk="ilk:agent" dst_ilk="ilk:person" ich="ich:api:conv-a215"`
	for _, c := range []struct {
		label, target, kept string
		event               sdk.Message
		want                string
	}{
		{"the agent's answer to the team", team, "", toThePerson, agentToNoOne},
		{"the answer kept, to the team", team, "answer", persons, agentToNoOne},
		{"the person's message to the team", team, "", persons, personToNoOne},
		{"the agent's answer to the person's IO node", personsIO, "", toThePerson, answerAsItCame},
		{"the agent's answer to another agent", "AI.invoice@motherbee", "", toThePerson, answerAsItCame},
	} {
		if sent := forward(c.target, c.kept, c.event); ilks(sent) != c.want || derefString(sent.Meta.ThreadID) != "conv-a215" {
			t.Fatalf("%s: %s on %q, want %s", c.label, ilks(sent), derefString(sent.Meta.ThreadID), c.want)
		}
	}

	// A supervisor's message forwarded to the person's IO node names the person, on the person's
	// channel (A-139), and to another IO node no one.
	supervisors := from(team, sdk.Message{
		Routing: sdk.Routing{TraceID: "trace-a215-supervisor"},
		Meta:    sdk.Meta{MsgType: "user", SrcIlk: strPtr("ilk:supervisor"), DstIlk: strPtr("ilk:agent"), Ich: strPtr("ich:slack:team"), ThreadID: strPtr("conv-a215")},
		Payload: json.RawMessage(`{"type":"text","content":"Approved."}`),
	})
	if sent := forward(personsIO, "", supervisors); ilks(sent) != `src_ilk="ilk:supervisor" dst_ilk="ilk:person" ich="ich:api:conv-a215"` {
		t.Fatalf("a supervisor's message to the person's IO node: %s", ilks(sent))
	}
	if sent := forward("IO.slack.audit@motherbee", "", supervisors); ilks(sent) != `src_ilk="ilk:supervisor" dst_ilk="" ich=""` {
		t.Fatalf("a supervisor's message to another IO node: %s", ilks(sent))
	}

	// With no person (several people write on the thread): to the person's IO node the message's own
	// dst_ilk and ich, to the team's none.
	inst.PersonILK, inst.PersonICH = "", ""
	if sent := forward(personsIO, "", toThePerson); ilks(sent) != answerAsItCame {
		t.Fatalf("with no person, to the person's IO node: %s", ilks(sent))
	}
	if sent := forward(team, "", toThePerson); ilks(sent) != agentToNoOne {
		t.Fatalf("with no person, to the team: %s", ilks(sent))
	}

	// What the WF forwarded is untouched: the event, and the message it keeps.
	if ilks(toThePerson) != answerAsItCame || ilks(inst.kept["answer"].msg) != answerAsItCame || ilks(persons) != `src_ilk="ilk:person" dst_ilk="" ich="ich:api:conv-a215"` {
		t.Fatalf("a forward changed the message it sent: %s, kept %s, %s", ilks(toThePerson), ilks(inst.kept["answer"].msg), ilks(persons))
	}
}

// The trace a transition's sends carry is its cycle's (docs/message-trace-v1.md): a node's message
// brings its own; an internal event, under its trace_id, keeps the one of the event that emitted it;
// a timer's fire starts a cycle with none. A trace an action's context writes is not the cycle's,
// and never goes out.
func TestATransitionSendsTheTraceOfItsCycle(t *testing.T) {
	inst := NewInstance(mustLoadValidDefinition(t), "wfi:inst-trace", "conv-t", "invoice", map[string]any{}, fixedClock)
	person := from("IO.in@motherbee", sdk.Message{Routing: sdk.Routing{TraceID: "trace-person"}, Meta: sdk.Meta{MsgType: "user"}, Payload: json.RawMessage(`{}`)})
	entries := func() int {
		var list []sdk.TraceEntry
		_ = json.Unmarshal(inst.cycleTrace, &list)
		return len(list)
	}
	inst.takeCycleTrace(person)
	if entries() != 1 {
		t.Fatalf("a node's message brings its trace: %s", inst.cycleTrace)
	}
	internal := sdk.Message{Routing: sdk.Routing{TraceID: "trace-person"}, Meta: sdk.Meta{MsgType: "user", Msg: strPtr("CHECKED")}}
	inst.takeCycleTrace(internal)
	if entries() != 1 {
		t.Fatalf("an internal event keeps its cycle's trace: %s", inst.cycleTrace)
	}

	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-trace"}
	actx := makeActx(t, disp, &mockTimerSender{})
	actx.TransitionActions = []string{"set_variable", "send_message", "schedule_timer"}
	action := ActionDefinition{Type: "send_message", Target: "AI.host@motherbee", Meta: &ActionMeta{Msg: "ask", Context: map[string]any{"trace": "not the cycle's"}}, Payload: map[string]any{}}
	if err := execSendMessage(context.Background(), action, inst, internal, actx); err != nil {
		t.Fatal(err)
	}
	if sent := disp.sent[0]; tracePath(sent) != "[IO.in@motherbee WF.invoice@motherbee]" || traceAct(sent) != "send_message set_variable schedule_timer" {
		t.Fatalf("the send carries the cycle's trace and names the step's actions, its own first: %s", sent.Meta.Context)
	}

	fire := from("SY.timer@motherbee", sdk.Message{Routing: sdk.Routing{TraceID: "trace-fire"}, Meta: sdk.Meta{MsgType: sdk.SYSTEMKind, Msg: strPtr("TIMER_FIRED")}})
	inst.takeCycleTrace(fire)
	if inst.cycleTrace != nil {
		t.Fatalf("a timer's fire starts a cycle with none: %s", inst.cycleTrace)
	}
	if err := execSendMessage(context.Background(), action, inst, fire, actx); err != nil {
		t.Fatal(err)
	}
	if sent := disp.sent[1]; tracePath(sent) != "[WF.invoice@motherbee]" {
		t.Fatalf("a send on a timer's fire starts its trace: %s", sent.Meta.Context)
	}
}
