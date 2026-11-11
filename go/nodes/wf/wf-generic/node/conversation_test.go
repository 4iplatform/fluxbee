package node

import (
	"context"
	"encoding/json"
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// The engine items of Case 1 (wf-orchestration-v1 §5.2.1): one instance per conversation, only the
// nodes involved drive it, event_match.src, forward and keep_message, the WF's block, timer times
// from state and internal transitions.

// forwardingWorkflowJSON keeps the person's message and forwards it to an agent; the agent's answer
// is forwarded to IO.out, and then the kept message too.
const forwardingWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "forwarding",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "asking",
  "terminal_states": ["done", "failed"],
  "states": [
    {
      "name": "asking",
      "entry_actions": [
        {"type": "keep_message", "name": "original"},
        {"type": "forward", "target": "AI.host@motherbee", "meta": {"context": {"channel": "sales"}}, "vars": {"seen": {"$ref": "event.src"}},
         "menu": {"options": [{"name": "ok", "description": "It is fine & done."}]}}
      ],
      "transitions": [
        {"event_match": {"src": "AI.host@motherbee"}, "guard": "true", "target_state": "done",
         "actions": [
           {"type": "forward", "target": "IO.out@motherbee"},
           {"type": "forward", "target": "IO.out@motherbee", "message": "original"}
         ]}
      ]
    },
    {"name": "done"},
    {"name": "failed"}
  ]
}`

// rawFrame is msg as the router writes it to the WF, read as the Go SDK reads a frame.
func rawFrame(t *testing.T, frame string) sdk.Message {
	t.Helper()
	var msg sdk.Message
	if err := json.Unmarshal([]byte(frame), &msg); err != nil {
		t.Fatal(err)
	}
	return msg
}

// 0.1.91 (item 5, item 10): a forwarded payload goes out byte for byte, never decoded: its <, > and
// &, its key order and its 1.10 as they came, through the frame the Go SDK writes, and after a
// restart too (a kept message, the inbound journal). The meta is the message's but memory_package;
// the WF changes the destination, the thread (the instance's), the context keys the action writes
// and its block.
func TestAForwardedPayloadGoesOutByteForByte(t *testing.T) {
	const payload = `{"type":"text","content":"<b>1 & 2</b> > 0","amount":1.10,"z":1,"a":[2.50,"x"]}`
	frame := `{"routing":{"src":"io-uuid","src_l2_name":"IO.in@motherbee","dst":"WF.invoice@motherbee","ttl":16,"trace_id":"trace-raw"},` +
		`"meta":{"type":"user","src_ilk":"ilk:person","dst_ilk":"ilk:wf","ich":"ich:api:1","thread_id":"conv-1","scope":"vpn","priority":"high",` +
		`"memory_package":{"package_version":1,"thread_id":"conv-1"},"context":{"io":{"reply_target":{"kind":"api","address":"a&b"}},` +
		`"trace":[{"t":"2026-11-04T10:17:03.120Z","from":"IO.in@motherbee","to":"WF.invoice@motherbee"}]}},` +
		`"payload":` + payload + `}`
	path := filepath.Join(t.TempDir(), "wf.db")
	rt, disp := newTestRuntime(t, path, forwardingWorkflowJSON, nil)
	instanceID := startInstance(t, rt, rawFrame(t, frame))

	onTheWire := func(msg sdk.Message) string {
		t.Helper()
		wire, err := sdk.MarshalMessage(msg)
		if err != nil {
			t.Fatal(err)
		}
		return string(wire)
	}
	forwarded := disp.sent[0]
	if wire := onTheWire(forwarded); !strings.Contains(wire, `"payload":`+payload) {
		t.Fatalf("the payload changed on the wire: %s", wire)
	}
	meta := forwarded.Meta
	if meta.MsgType != "user" || meta.Msg != nil || derefString(meta.SrcIlk) != "ilk:person" || derefString(meta.DstIlk) != "ilk:wf" ||
		derefString(meta.Ich) != "ich:api:1" || derefString(meta.Scope) != "vpn" || derefString(meta.Priority) != "high" ||
		meta.MemoryPackage != nil || derefString(meta.ThreadID) != "conv-1" {
		t.Fatalf("the meta is the message's, without memory_package: %+v", meta)
	}
	if forwarded.Routing.Dst.Value() != "AI.host@motherbee" || forwarded.Routing.SrcL2Name != nil || forwarded.Routing.Src != "wf-uuid" || forwarded.Routing.TraceID != "trace-raw" {
		t.Fatalf("the routing is the WF's: %+v", forwarded.Routing)
	}
	if got := contextWithoutTrace(t, forwarded); got != `{"channel":"sales","io":{"reply_target":{"address":"a&b","kind":"api"}},"wf":{"menu":{"options":[{"name":"ok","description":"It is fine & done."}]},"vars":{"seen":"IO.in@motherbee"}}}` ||
		tracePath(forwarded) != "[IO.in@motherbee WF.invoice@motherbee]" || traceAct(forwarded) != "forward keep_message" {
		t.Fatalf("the context: the message's, the keys the action writes, its trace and the WF's block: %s", meta.Context)
	}

	// The inbound journal keeps a message byte for byte too, for a replay.
	if _, err := rt.journal(context.Background(), instanceID, inboundEvent, rawFrame(t, frame), ""); err != nil {
		t.Fatal(err)
	}
	journaled, _ := rt.Store.ListInboundEvents(context.Background())
	if len(journaled) != 1 || string(rawFrame(t, journaled[0].MessageJSON).Payload) != payload {
		t.Fatalf("the journal: %+v", journaled)
	}
	if err := rt.Store.DeleteInboundEvent(context.Background(), journaled[0].EventID); err != nil {
		t.Fatal(err)
	}

	// After a restart, the kept message goes out as it came, and the agent's answer, forwarded, keeps
	// the trace it brings: its cycle's.
	rt, disp = newTestRuntime(t, path, forwardingWorkflowJSON, &mockTimerLister{})
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	reply := from("AI.host@motherbee", sdk.Message{
		Routing: sdk.Routing{Src: "agent-uuid", TraceID: "trace-reply"},
		Meta: sdk.Meta{MsgType: "user", ThreadID: forwarded.Meta.ThreadID, Context: json.RawMessage(`{"trace":[{"t":"2026-11-04T10:17:03.120Z","from":"IO.in@motherbee","to":"WF.invoice@motherbee"},` +
			`{"t":"2026-11-04T10:17:03.180Z","from":"WF.invoice@motherbee","to":"AI.host@motherbee","act":["forward"]}]}`)},
		Payload: json.RawMessage(`{"type":"text","content":"a < b"}`),
	})
	if err := Dispatch(context.Background(), reply, rt); err != nil {
		t.Fatal(err)
	}
	if len(disp.sent) != 2 {
		t.Fatalf("sent %d", len(disp.sent))
	}
	if wire := onTheWire(disp.sent[0]); !strings.Contains(wire, `"payload":{"type":"text","content":"a < b"}`) ||
		tracePath(disp.sent[0]) != "[IO.in@motherbee WF.invoice@motherbee AI.host@motherbee WF.invoice@motherbee]" {
		t.Fatalf("the answer, forwarded: %s", wire)
	}
	if wire := onTheWire(disp.sent[1]); !strings.Contains(wire, `"payload":`+payload) || derefString(disp.sent[1].Meta.SrcIlk) != "ilk:person" {
		t.Fatalf("the kept message after a restart: %s", wire)
	}
	if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "done" {
		t.Fatalf("the instance: %+v", row)
	}
}

// A forward fails when there is no message to forward: an internal event, or a name not kept yet; a
// keep_message, when there is no message to keep. Both fail the instance by default. Where the event
// may be no node's message (a TIMER_FIRED, an internal event) the language refuses both at load, so
// the test writes them into the definition after it loaded.
func TestAForwardWithoutAMessageFailsTheInstance(t *testing.T) {
	const definition = `{
  "wf_schema_version": "1",
  "workflow_type": "nothing",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["done", "failed"],
  "states": [
    {
      "name": "waiting",
      "transitions": [
        {"event_match": {"msg": "INTERNAL"}, "guard": "true", "target_state": "done", "actions": []},
        {"event_match": {"msg": "FORWARD_INTERNAL"}, "guard": "true", "target_state": "done", "actions": []},
        {"event_match": {"msg": "LATER"}, "guard": "true", "target_state": "done", "actions": [{"type": "forward", "target": "AI.host@motherbee", "message": "later"}]},
        {"event_match": {"msg": "EMIT"}, "guard": "true", "actions": [{"type": "emit_internal_event", "meta": {"msg": "INTERNAL"}}]},
        {"event_match": {"msg": "EMIT_FORWARD"}, "guard": "true", "actions": [{"type": "emit_internal_event", "meta": {"msg": "FORWARD_INTERNAL"}}]},
        {"event_match": {"msg": "KEEP"}, "guard": "true", "actions": [{"type": "keep_message", "name": "later"}]}
      ]
    },
    {"name": "done"},
    {"name": "failed"}
  ]
}`
	rt, disp := newTestRuntime(t, "", definition, nil)
	waiting := rt.Def.States[0].Transitions
	waiting[0].Actions = []ActionDefinition{{Type: "keep_message", Name: "x"}}
	waiting[1].Actions = []ActionDefinition{{Type: "forward", Target: "AI.host@motherbee"}}
	send := func(instanceID string, msg sdk.Message) {
		t.Helper()
		msg.Meta.ThreadID = threadOf(t, rt, instanceID)
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}
	for want, name := range map[string]string{
		"INTERNAL is no message a node sent":         "EMIT",
		"FORWARD_INTERNAL is no message a node sent": "EMIT_FORWARD",
		`no message is kept under "later"`:           "LATER",
	} {
		instanceID := startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
		send(instanceID, from("IO.in@motherbee", msgWithName(name)))
		if row := mustGetInstance(t, rt, instanceID); row.Status != "failed" || !strings.Contains(lastLog(t, rt, instanceID).ErrorDetail, want) {
			t.Fatalf("%s: %+v %+v", want, row, lastLog(t, rt, instanceID))
		}
	}
	// Kept, it goes.
	instanceID := startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
	send(instanceID, from("IO.in@motherbee", msgWithName("KEEP")))
	send(instanceID, from("IO.in@motherbee", msgWithName("LATER")))
	if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "done" || derefString(disp.sent[len(disp.sent)-1].Meta.Msg) != "KEEP" {
		t.Fatalf("the kept message goes: %+v", row)
	}
}

// conversationWorkflowJSON starts on IO.in's messages, asks AI.host, and takes IO.watch's notes, by
// their sender, while it waits.
const conversationWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "conversation",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object", "required": ["text"]},
  "initial_state": "asking",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "asking",
      "entry_actions": [{"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "ASK"}, "payload": {"text": {"$ref": "input.text"}}}],
      "transitions": [
        {"event_match": {"msg": "NOTE", "src": "IO.watch@motherbee"}, "guard": "true", "actions": [{"type": "set_variable", "name": "notes", "value": "has(state.notes) ? state.notes + 1 : 1"}]},
        {"event_match": {"src": "AI.host@motherbee"}, "guard": "true", "target_state": "done"}
      ]
    },
    {"name": "done"}
  ]
}`

// 0.1.91 (items 1 and 3, FINDINGS A-123) and Case 4 (wf-orchestration-v1 §5.4.10): an instance lives
// on the thread of the message that started it, and only the nodes written in the definition drive
// it. A stranger, a reply and an agent's message start nothing, and none is answered. An IO node the
// definition involves hears, with a NOT_TAKEN, why its message went nowhere: it has no thread (a WF
// never makes one up, A-135), no conversation takes it, the input_schema refuses it, or the instance
// expects nothing of the kind. A person who writes again after the instance ended starts a new one on
// the same thread.
func TestTheCorrelationOrderOfAConversation(t *testing.T) {
	rt, disp := newTestRuntime(t, "", conversationWorkflowJSON, nil)
	message := func(sender, name, thread string, payload map[string]any) sdk.Message {
		msg := from(sender, msgWithNameAndPayload(name, payload))
		msg.Routing.Src = sender + "-uuid"
		if name == "" {
			msg.Meta.Msg = nil
		}
		if thread != "" {
			msg.Meta.ThreadID = &thread
		}
		return msg
	}
	dispatch := func(msg sdk.Message) {
		t.Helper()
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}
	instances := func() int {
		t.Helper()
		rows, err := rt.Store.ListInstances(context.Background(), "", 0, 0)
		if err != nil {
			t.Fatal(err)
		}
		return len(rows)
	}

	// notTaken checks that the last message the WF sent is a NOT_TAKEN for msg: to its sender's uuid,
	// on its thread and trace, with reason.
	notTaken := func(label string, msg sdk.Message, reason string) {
		t.Helper()
		answer := disp.sent[len(disp.sent)-1]
		if notTakenReason(t, answer) != reason || answer.Routing.Dst.Value() != msg.Routing.Src || answer.Routing.TraceID != msg.Routing.TraceID ||
			derefString(answer.Meta.ThreadID) != derefString(msg.Meta.ThreadID) {
			t.Fatalf("%s: a NOT_TAKEN %s to its sender, on its thread and trace: %+v %s", label, reason, answer, answer.Payload)
		}
	}

	// Nothing starts but a start_from node's message that is no reply, on a thread, and only what the
	// input_schema takes. A stranger, an agent and a reply are never answered.
	for label, msg := range map[string]sdk.Message{
		"a stranger":                      message("AI.stranger@motherbee", "START", "", map[string]any{"text": "hi"}),
		"a stranger on a thread":          message("IO.stranger@motherbee", "START", "conv-1", map[string]any{"text": "hi"}),
		"the agent":                       message("AI.host@motherbee", "", "conv-1", map[string]any{"text": "hi"}),
		"the agent, named":                message("AI.host@motherbee", "ASK", "conv-1", map[string]any{"text": "hi"}),
		"a reply from a start_from node":  message("IO.in@motherbee", "X_RESPONSE", "conv-1", map[string]any{"text": "hi"}),
		"an error from a start_from node": message("IO.in@motherbee", "X.error", "conv-1", map[string]any{"text": "hi"}),
		"the engine's NOT_TAKEN":          message("IO.in@motherbee", msgNotTaken, "conv-1", map[string]any{"text": "hi"}),
	} {
		dispatch(msg)
		if instances() != 0 || len(disp.sent) != 0 {
			t.Fatalf("%s: %d instances, sent %+v", label, instances(), disp.sent)
		}
	}
	// An IO node the definition involves hears why, named or not.
	for _, c := range []struct {
		label  string
		msg    sdk.Message
		reason string
	}{
		{"a person's message with no thread", message("IO.in@motherbee", "", "", map[string]any{"text": "hi"}), notTakenNoThread},
		{"a named request with a blank thread", message("IO.in@motherbee", "START", " ", map[string]any{"text": "hi"}), notTakenNoThread},
		{"a node of an event_match.src with no thread", message("IO.watch@motherbee", "NOTE", "", nil), notTakenNoThread},
		{"a node of an event_match.src", message("IO.watch@motherbee", "NOTE", "conv-1", map[string]any{"text": "hi"}), notTakenNoConversation},
		{"a message the schema refuses", message("IO.in@motherbee", "", "conv-1", map[string]any{}), notTakenInvalidInput},
		{"a named request the schema refuses", message("IO.in@motherbee", "START", "conv-1", map[string]any{}), notTakenInvalidInput},
	} {
		sent := len(disp.sent)
		dispatch(c.msg)
		if instances() != 0 || len(disp.sent) != sent+1 {
			t.Fatalf("%s: %d instances, sent %+v", c.label, instances(), disp.sent[sent:])
		}
		notTaken(c.label, c.msg, c.reason)
	}
	disp.sent = nil

	// The person's message starts an instance on its thread, and every send carries it.
	first := startInstance(t, rt, message("IO.in@motherbee", "", "conv-1", map[string]any{"text": "hi"}))
	if row := mustGetInstance(t, rt, first); row.ThreadID != "conv-1" || derefString(disp.sent[0].Meta.ThreadID) != "conv-1" {
		t.Fatalf("the instance and its send are on the conversation's thread: %+v %+v", row, disp.sent[0].Meta)
	}
	// On its thread: a stranger drives nothing, a node of an event_match.src drives it, by its
	// sender, and a message from the right sender but with another msg matches nothing: the instance's
	// log keeps it, and the IO node hears it was not expected.
	dispatch(message("AI.stranger@motherbee", "NOTE", "conv-1", nil))
	dispatch(message("IO.watch@motherbee", "NOTE", "conv-1", nil))
	if len(disp.sent) != 1 {
		t.Fatalf("a stranger, and a message taken, are not answered: %+v", disp.sent[1:])
	}
	other := message("IO.watch@motherbee", "OTHER", "conv-1", nil)
	dispatch(other)
	notTaken("another msg", other, notTakenNotExpected)
	unexpected := message("IO.in@motherbee", "NOTE", "conv-1", nil)
	dispatch(unexpected)
	notTaken("a start_from node's message the state does not take", unexpected, notTakenNotExpected)
	if row := mustGetInstance(t, rt, first); row.StateJSON != `{"notes":1}` || row.CurrentState != "asking" || len(disp.sent) != 3 {
		t.Fatalf("only IO.watch's NOTE is taken: %+v", row)
	}
	if logs, _ := rt.Store.GetRecentLog(context.Background(), first, 3); logs[0].Summary != "unhandled event NOTE from IO.in@motherbee in state asking" ||
		logs[1].Summary != "unhandled event OTHER from IO.watch@motherbee in state asking" {
		t.Fatalf("the log names the event and its sender: %+v", logs)
	}
	// The agent's answer, without a msg, matches by its sender, and ends the instance.
	dispatch(message("AI.host@motherbee", "", "conv-1", map[string]any{"type": "text", "content": "ok"}))
	if row := mustGetInstance(t, rt, first); row.CurrentState != "done" {
		t.Fatalf("the agent's answer: %+v", row)
	}

	// After the end: the agent's late answer starts nothing, unanswered; a node of an event_match.src
	// hears no conversation takes its message; the person's next message starts a new instance, on
	// the same thread.
	sent := len(disp.sent)
	dispatch(message("AI.host@motherbee", "", "conv-1", map[string]any{"type": "text", "content": "late"}))
	if instances() != 1 || len(disp.sent) != sent {
		t.Fatalf("a late answer: %d instances", instances())
	}
	late := message("IO.watch@motherbee", "NOTE", "conv-1", nil)
	dispatch(late)
	notTaken("a note after the end", late, notTakenNoConversation)
	second := startInstance(t, rt, message("IO.in@motherbee", "", "conv-1", map[string]any{"text": "again"}))
	if row := mustGetInstance(t, rt, second); second == first || row.ThreadID != "conv-1" || row.CurrentState != "asking" {
		t.Fatalf("a new instance on the conversation: %+v", row)
	}
}

// The node's log gets a payload's first characters only for a message that reaches an instance or
// starts one: when every message of a node comes to the WF, as the supervisors' Slack channel's
// does, the log must not keep the text of those the WF drops. A dropped message's lines keep its
// sender, msg and thread.
func TestTheNodesLogKeepsNoTextOfADroppedMessage(t *testing.T) {
	rt, _ := newTestRuntime(t, "", conversationWorkflowJSON, nil)
	message := func(sender, name, thread, payload string) sdk.Message {
		msg := from(sender, sdk.Message{
			Routing: sdk.Routing{Src: sender + "-uuid", TraceID: "trace-" + thread},
			Meta:    sdk.Meta{MsgType: "user", ThreadID: &thread},
			Payload: json.RawMessage(payload),
		})
		if name != "" {
			msg.Meta.Msg = &name
		}
		return msg
	}
	logOf := func(msg sdk.Message) string {
		t.Helper()
		return nodeLog(t, func() {
			if err := Dispatch(context.Background(), msg, rt); err != nil {
				t.Fatal(err)
			}
		})
	}
	const secret = `{"type":"text","content":"a 20% discount for Acme"}`
	for label, c := range map[string]struct{ sender, name string }{
		"a node the definition does not involve":     {"IO.stranger@motherbee", ""},
		"a node of an event_match.src, on no thread": {"IO.watch@motherbee", "NOTE"},
		"an agent's late answer":                     {"AI.host@motherbee", ""},
		"a reply from a start_from node":             {"IO.in@motherbee", "X_RESPONSE"},
		"a message the input_schema refuses":         {"IO.in@motherbee", "START"},
	} {
		logged := logOf(message(c.sender, c.name, "conv-dropped", secret))
		if strings.Contains(logged, "discount") || !strings.Contains(logged, `src_l2="`+c.sender+`"`) || !strings.Contains(logged, `thread_id="conv-dropped"`) ||
			!strings.Contains(logged, fmt.Sprintf("msg=%q", c.name)) {
			t.Fatalf("%s: %s", label, logged)
		}
	}

	// The message that starts an instance, and one that reaches it.
	logged := logOf(message("IO.in@motherbee", "", "conv-1", `{"text":"a 20% discount for Acme"}`))
	if !strings.Contains(logged, "wf: create accepted") || !strings.Contains(logged, `payload={"text":"a 20% discount for Acme"}`) {
		t.Fatalf("the message that starts an instance: %s", logged)
	}
	logged = logOf(message("IO.watch@motherbee", "NOTE", "conv-1", `{"note":"Acme pays late"}`))
	if !strings.Contains(logged, "wf: correlate matched thread_id=conv-1") || !strings.Contains(logged, `payload={"note":"Acme pays late"}`) {
		t.Fatalf("a message that reaches an instance: %s", logged)
	}
}

// 0.1.91 (item 3): a running instance is judged by its own definition: after a redeploy that writes
// a node in, that node still drives nothing of an instance born before it.
func TestARunningInstanceIsDrivenOnlyByTheNodesOfItsDefinition(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	old, _ := newTestRuntime(t, path, conversationWorkflowJSON, nil)
	thread := "conv-old"
	trigger := from("IO.in@motherbee", msgWithNameAndPayload("", map[string]any{"text": "hi"}))
	trigger.Meta.Msg, trigger.Meta.ThreadID = nil, &thread
	instanceID := startInstance(t, old, trigger)

	widened := strings.Replace(conversationWorkflowJSON, `"start_from": ["IO.in@motherbee"]`, `"start_from": ["IO.in@motherbee", "IO.watch2@motherbee"]`, 1)
	widened = strings.Replace(widened, `"src": "IO.watch@motherbee"`, `"src": "IO.watch2@motherbee"`, 1)
	rt, _ := newTestRuntime(t, path, widened, &mockTimerLister{})
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	note := from("IO.watch2@motherbee", msgWithName("NOTE"))
	note.Meta.ThreadID = &thread
	if err := Dispatch(context.Background(), note, rt); err != nil {
		t.Fatal(err)
	}
	if row := mustGetInstance(t, rt, instanceID); row.StateJSON != `{}` {
		t.Fatalf("a node the instance's definition does not write drove it: %+v", row)
	}
}

// 0.1.91 (item 9): a transition without a target_state runs its actions and stays in its state, with
// no exit or entry actions: a timer the state's exit would cancel keeps running.
func TestAnInternalTransitionStaysWithoutExitOrEntryActions(t *testing.T) {
	const definition = `{
  "wf_schema_version": "1",
  "workflow_type": "noting",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "waiting",
      "entry_actions": [
        {"type": "set_variable", "name": "entered", "value": "has(state.entered) ? state.entered + 1 : 1"},
        {"type": "schedule_timer", "timer_key": "deadline", "fire_in": "5m"}
      ],
      "exit_actions": [{"type": "cancel_timer", "timer_key": "deadline"}],
      "transitions": [
        {"event_match": {"msg": "NOTE"}, "guard": "true", "actions": [{"type": "set_variable", "name": "noted", "value": "true"}]},
        {"event_match": {"msg": "AGAIN"}, "guard": "true", "target_state": "waiting"},
        {"event_match": {"msg": "DONE"}, "guard": "state.noted", "target_state": "done"}
      ]
    },
    {"name": "done"}
  ]
}`
	timer := &mockTimerSender{}
	rt, _ := newTestRuntime(t, "", definition, timer)
	instanceID := startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
	send := func(name string) {
		t.Helper()
		msg := from("IO.in@motherbee", msgWithName(name))
		msg.Meta.ThreadID = threadOf(t, rt, instanceID)
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}
	send("NOTE")
	row := mustGetInstance(t, rt, instanceID)
	if row.CurrentState != "waiting" || row.StateJSON != `{"entered":1,"noted":true}` || len(timer.cancelled) != 0 || len(timer.scheduled) != 1 {
		t.Fatalf("an internal transition: %+v, cancelled %v, scheduled %v", row, timer.cancelled, timer.scheduled)
	}
	if entry := lastLog(t, rt, instanceID); entry.Summary != "internal transition in waiting on NOTE from IO.in@motherbee" {
		t.Fatalf("its log: %+v", entry)
	}
	// A transition back to its own state leaves it and enters it again.
	send("AGAIN")
	if row := mustGetInstance(t, rt, instanceID); row.StateJSON != `{"entered":2,"noted":true}` || len(timer.cancelled) != 1 || len(timer.scheduled) != 2 {
		t.Fatalf("a transition to its own state: %+v, cancelled %v, scheduled %v", row, timer.cancelled, timer.scheduled)
	}
	send("DONE")
	if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "done" {
		t.Fatalf("the instance: %+v", row)
	}
}

// 0.1.91 (item 6): a kept message is committed with its transition, a later keep under the same name
// replaces it, a restart keeps it, and it goes when the instance ends.
func TestAKeptMessageLivesAsLongAsItsInstance(t *testing.T) {
	const definition = `{
  "wf_schema_version": "1",
  "workflow_type": "keeping",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "keeping",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "keeping",
      "entry_actions": [{"type": "keep_message", "name": "last"}],
      "transitions": [
        {"event_match": {"msg": "MORE"}, "guard": "true", "actions": [{"type": "keep_message", "name": "last"}]},
        {"event_match": {"msg": "SEND"}, "guard": "true", "actions": [{"type": "forward", "target": "IO.out@motherbee", "message": "last"}]},
        {"event_match": {"msg": "DONE"}, "guard": "true", "target_state": "done"}
      ]
    },
    {"name": "done"}
  ]
}`
	path := filepath.Join(t.TempDir(), "wf.db")
	rt, disp := newTestRuntime(t, path, definition, nil)
	instanceID := startInstance(t, rt, from("IO.in@motherbee", msgWithNameAndPayload("START", map[string]any{"n": 1})))
	send := func(name string, payload map[string]any) {
		t.Helper()
		msg := from("IO.in@motherbee", msgWithNameAndPayload(name, payload))
		msg.Meta.ThreadID = threadOf(t, rt, instanceID)
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}
	kept := func() []KeptMessageRow {
		t.Helper()
		rows, err := rt.Store.ListKeptMessages(context.Background(), instanceID)
		if err != nil {
			t.Fatal(err)
		}
		return rows
	}
	if rows := kept(); len(rows) != 1 || rows[0].Name != "last" || !strings.Contains(rows[0].MessageJSON, `"payload":{"n":1}`) {
		t.Fatalf("the creation commits the kept message: %+v", rows)
	}
	send("MORE", map[string]any{"n": 2})
	if rows := kept(); len(rows) != 1 || !strings.Contains(rows[0].MessageJSON, `"payload":{"n":2}`) {
		t.Fatalf("a later keep replaces it: %+v", rows)
	}

	rt, disp = newTestRuntime(t, path, definition, &mockTimerLister{})
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	send("SEND", nil)
	if len(disp.sent) != 1 || string(disp.sent[0].Payload) != `{"n":2}` || derefString(disp.sent[0].Meta.Msg) != "MORE" {
		t.Fatalf("a restart keeps it: %+v", disp.sent)
	}
	send("DONE", nil)
	if rows := kept(); len(rows) != 0 {
		t.Fatalf("it goes when the instance ends: %+v", rows)
	}
}

// 0.1.91 (item 8): fire_in and fire_at take a $ref, resolved when the action runs, with the 60 s
// floor checked then: a time from an agent's answer sets the timer, one under the floor or no time
// at all fails the action.
func TestATimerTimeComesFromState(t *testing.T) {
	const definition = `{
  "wf_schema_version": "1",
  "workflow_type": "timing",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["done", "failed"],
  "states": [
    {
      "name": "waiting",
      "transitions": [
        {"event_match": {"msg": "DUE_IN"}, "guard": "true", "target_state": "waiting",
         "actions": [{"type": "schedule_timer", "timer_key": "due", "fire_in": {"$ref": "event.payload.due_in"}, "on_error": "fail"}]},
        {"event_match": {"msg": "DUE_AT"}, "guard": "true",
         "actions": [
           {"type": "set_variable", "name": "due_at", "value": "event.payload.due_at"},
           {"type": "schedule_timer", "timer_key": "due", "fire_at": {"$ref": "state.due_at"}, "on_error": "fail"}
         ]},
        {"event_match": {"msg": "LATER"}, "guard": "true",
         "actions": [{"type": "reschedule_timer", "timer_key": "due", "fire_in": {"$ref": "event.payload.due_in", "default": "1h"}, "on_error": "fail"}]}
      ]
    },
    {"name": "done"},
    {"name": "failed"}
  ]
}`
	timer := &fakeSYTimer{pending: map[string]time.Time{}}
	rt, _ := newTestRuntime(t, "", definition, timer)
	instance := func() string {
		t.Helper()
		return startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
	}
	send := func(instanceID, name string, payload map[string]any) WFInstanceRow {
		t.Helper()
		msg := from("IO.in@motherbee", msgWithNameAndPayload(name, payload))
		msg.Meta.ThreadID = threadOf(t, rt, instanceID)
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
		return mustGetInstance(t, rt, instanceID)
	}
	due := func(instanceID string) time.Time {
		return timer.pending[timerClientRef(instanceID, "due")]
	}

	id := instance()
	send(id, "DUE_IN", map[string]any{"due_in": "2m"})
	if !due(id).Equal(fixedClock().Add(2 * time.Minute)) {
		t.Fatalf("fire_in from the event: %v", timer.pending)
	}
	if row, ok, _ := rt.Store.GetTimerForInstance(context.Background(), id, "due"); !ok || row.FireAtMS != fixedClock().Add(2*time.Minute).UnixMilli() {
		t.Fatalf("the index keeps its time: %+v", row)
	}
	send(id, "LATER", nil)
	if !due(id).Equal(fixedClock().Add(time.Hour)) {
		t.Fatalf("reschedule_timer, at the $ref's default: %v", timer.pending)
	}
	atID := instance()
	at := fixedClock().Add(3 * time.Hour).Format(time.RFC3339)
	send(atID, "DUE_AT", map[string]any{"due_at": at})
	if !due(atID).Equal(fixedClock().Add(3 * time.Hour)) {
		t.Fatalf("fire_at from state: %v", timer.pending)
	}

	for payload, want := range map[string]map[string]any{
		`fire_in "30s": must be at least 60s`:       {"due_in": "30s"},
		`fire_in "400d": must be at most 366d`:      {"due_in": "400d"},
		`fire_in "soon": invalid duration "soon"`:   {"due_in": "soon"},
		`fire_in resolved to int64, not a string`:   {"due_in": 120},
		`$ref "event.payload.due_in": key "due_in"`: {},
	} {
		failing := instance()
		if row := send(failing, "DUE_IN", want); row.Status != "failed" || !strings.Contains(lastLog(t, rt, failing).ErrorDetail, payload) {
			t.Fatalf("%s: %+v %+v", payload, row, lastLog(t, rt, failing))
		}
	}
	tooSoon := instance()
	if row := send(tooSoon, "DUE_AT", map[string]any{"due_at": fixedClock().Add(30 * time.Second).Format(time.RFC3339)}); row.Status != "failed" ||
		!strings.Contains(lastLog(t, rt, tooSoon).ErrorDetail, "is less than 60s ahead") {
		t.Fatalf("a fire_at under the floor: %+v %+v", row, lastLog(t, rt, tooSoon))
	}
	// SY.timer's ceiling (FINDINGS A-254): it refuses a timer more than 366 days ahead every time.
	tooFar := instance()
	if row := send(tooFar, "DUE_AT", map[string]any{"due_at": fixedClock().Add(367 * 24 * time.Hour).Format(time.RFC3339)}); row.Status != "failed" ||
		!strings.Contains(lastLog(t, rt, tooFar).ErrorDetail, "is more than 366d ahead") {
		t.Fatalf("a fire_at over the ceiling: %+v %+v", row, lastLog(t, rt, tooFar))
	}
	if _, pending := timer.pending[timerClientRef(tooFar, "due")]; pending {
		t.Fatal("a fire_at over the ceiling never goes to SY.timer")
	}
	atCeiling := instance()
	if row := send(atCeiling, "DUE_AT", map[string]any{"due_at": fixedClock().Add(366 * 24 * time.Hour).Format(time.RFC3339)}); row.Status != "running" {
		t.Fatalf("a fire_at 366 days ahead is SY.timer's: %+v %+v", row, lastLog(t, rt, atCeiling))
	}
}

// listeningWorkflowJSON starts on IO.in's messages and notes the text of each, the first one
// included.
const listeningWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "listening",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object", "required": ["text"]},
  "initial_state": "listening",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "listening",
      "entry_actions": [{"type": "set_variable", "name": "heard", "value": "[event.payload.text]"}],
      "transitions": [
        {"event_match": {"src": "IO.in@motherbee"}, "guard": "true", "actions": [{"type": "set_variable", "name": "heard", "value": "state.heard + [event.payload.text]"}]}
      ]
    },
    {"name": "done"}
  ]
}`

// A creation whose commit failed stays journaled; a later message on the same thread starts the
// conversation's instance meanwhile. At the next start the creation is no new instance on that
// thread, which would leave two running instances and the first one unreachable: it is the running
// instance's event, as live dispatch would take it now, and its commit consumes it.
func TestAReplayedCreationOnAHeldThreadIsThatInstancesEvent(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	rt, _ := newTestRuntime(t, path, listeningWorkflowJSON, nil)
	say := func(text string) error {
		thread := "conv-1"
		msg := from("IO.in@motherbee", msgWithNameAndPayload("", map[string]any{"text": text}))
		msg.Meta.Msg, msg.Meta.ThreadID = nil, &thread
		return Dispatch(context.Background(), msg, rt)
	}
	if _, err := rt.Store.db.Exec(`CREATE TRIGGER fail_first BEFORE INSERT ON wf_instances
WHEN NEW.input_json = '{"text":"first"}' BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;`); err != nil {
		t.Fatal(err)
	}
	if err := say("first"); err == nil {
		t.Fatal("the first creation's commit fails")
	}
	if _, err := rt.Store.db.Exec(`DROP TRIGGER fail_first;`); err != nil {
		t.Fatal(err)
	}
	if err := say("second"); err != nil {
		t.Fatal(err)
	}
	running, ok := rt.Registry.GetByThread("conv-1")
	if !ok {
		t.Fatal("the second message started the conversation's instance")
	}

	rt, _ = newTestRuntime(t, path, listeningWorkflowJSON, &mockTimerLister{})
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0)
	if len(rows) != 1 || rows[0].InstanceID != running.InstanceID || rows[0].StateJSON != `{"heard":["second","first"]}` {
		t.Fatalf("the replayed creation is the running instance's event: %+v", rows)
	}
	if events, _ := rt.Store.ListInboundEvents(context.Background()); len(events) != 0 {
		t.Fatalf("its commit consumes the replayed creation: %+v", events)
	}
}

// relistenedWorkflowJSON is listeningWorkflowJSON redeployed to start from IO.other: it still hears
// IO.in, and a WF.upstream's message ends it.
const relistenedWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "listening",
  "start_from": ["IO.other@motherbee"],
  "input_schema": {"type": "object", "required": ["text"]},
  "initial_state": "listening",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "listening",
      "entry_actions": [{"type": "set_variable", "name": "heard", "value": "[event.payload.text]"}],
      "transitions": [
        {"event_match": {"src": "IO.in@motherbee"}, "guard": "true", "actions": [{"type": "set_variable", "name": "heard", "value": "state.heard + [event.payload.text]"}]},
        {"event_match": {"src": "WF.upstream@motherbee"}, "guard": "true", "target_state": "done"}
      ]
    },
    {"name": "done"}
  ]
}`

// A creation a crash left journaled, which starts nothing at the next start, is dropped and answered
// as live dispatch answers it (wf-orchestration-v1 §5.4.6): one an engine of before FINDINGS A-135
// journaled with no thread (no_thread), one from a node the current definition no longer starts from
// (no_conversation), and one its input_schema refuses (invalid_input). Who hears is answerNotTaken's
// rule: an IO node, for each; a WF only for a named request, and only no_thread or invalid_input; a
// node the current definition does not involve, never.
func TestAReplayedCreationThatStartsNothingIsAnswered(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	rt, _ := newTestRuntime(t, path, listeningWorkflowJSON, nil)
	journal := func(instanceID, sender, name, thread string, payload map[string]any) {
		t.Helper()
		msg := from(sender, msgWithNameAndPayload(name, payload))
		msg.Routing.Src, msg.Routing.TraceID = sender+"-uuid", "trace-"+instanceID
		if name == "" {
			msg.Meta.Msg = nil
		}
		if thread != "" {
			msg.Meta.ThreadID = &thread
		}
		if _, err := rt.journal(context.Background(), instanceID, inboundCreate, msg, ""); err != nil {
			t.Fatal(err)
		}
	}
	journal("wfi:threadless", "IO.in@motherbee", "", "", map[string]any{"text": "left"})
	journal("wfi:moved", "IO.in@motherbee", "", "conv-moved", map[string]any{"text": "hi"})
	journal("wfi:refused", "IO.other@motherbee", "", "conv-refused", map[string]any{"note": "no text"})
	journal("wfi:upstream-threadless", "WF.upstream@motherbee", "START", "", map[string]any{"text": "hi"})
	journal("wfi:upstream", "WF.upstream@motherbee", "START", "conv-upstream", map[string]any{"text": "hi"})
	journal("wfi:upstream-text", "WF.upstream@motherbee", "", "", map[string]any{"text": "hi"})
	journal("wfi:stranger", "IO.stranger@motherbee", "", "conv-stranger", map[string]any{"text": "hi"})

	// The node starts again on the definition redeployed.
	rt, disp := newTestRuntime(t, path, relistenedWorkflowJSON, &mockTimerLister{})
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	if rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0); len(rows) != 0 {
		t.Fatalf("a creation started an instance: %+v", rows)
	}
	if events, _ := rt.Store.ListInboundEvents(context.Background()); len(events) != 0 {
		t.Fatalf("the creations are dropped: %+v", events)
	}
	var answered []string
	for _, msg := range disp.sent {
		answered = append(answered, fmt.Sprintf("%s to %s on %q (%s)", notTakenReason(t, msg), msg.Routing.Dst.Value(), derefString(msg.Meta.ThreadID), msg.Routing.TraceID))
	}
	want := []string{
		`no_thread to IO.in@motherbee-uuid on "" (trace-wfi:threadless)`,
		`no_conversation to IO.in@motherbee-uuid on "conv-moved" (trace-wfi:moved)`,
		`invalid_input to IO.other@motherbee-uuid on "conv-refused" (trace-wfi:refused)`,
		`no_thread to WF.upstream@motherbee-uuid on "" (trace-wfi:upstream-threadless)`,
	}
	if strings.Join(answered, "; ") != strings.Join(want, "; ") {
		t.Fatalf("the replayed creations' answers:\n got %v\nwant %v", answered, want)
	}
}

// personWorkflowJSON starts on IO.channel's messages, the people of a channel, and writes AI.host at
// each message: the people's, the team's (IO.team) and the agent's own answers.
const personWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "person",
  "start_from": ["IO.channel@motherbee", "IO.team@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "noting",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "noting",
      "entry_actions": [{"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "NOTE"}, "payload": {"text": "first"}}],
      "transitions": [
        {"event_match": {"src": "IO.channel@motherbee"}, "guard": "true",
         "actions": [{"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "NOTE"}, "payload": {"text": "person"}}]},
        {"event_match": {"src": "IO.team@motherbee"}, "guard": "true",
         "actions": [{"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "NOTE"}, "payload": {"text": "team"}}]},
        {"event_match": {"src": "AI.host@motherbee"}, "guard": "true",
         "actions": [{"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "NOTE"}, "payload": {"text": "agent"}}]}
      ]
    },
    {"name": "done"}
  ]
}`

// The instance's person is the src_ilk the messages of the person's IO node share (wf-orchestration-v1
// §5.4.3, §5.5.1): a send_message carries it, whoever else writes on the thread (the team's IO node,
// though start_from names it too, or an agent), and a message of theirs with no src_ilk changes
// nothing. A message from the person's IO node with another person, as on a Slack channel's top
// level, where io.slack gives everyone one thread, leaves the instance with no single person: its own
// messages carry none from then on, after a restart too, and the node's log says so once. Before, they
// carried the first writer's identity, and an agent bound per-person state to it for everyone.
func TestAThreadSeveralPeopleWriteOnLeavesTheInstanceNoPerson(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	rt, disp := newTestRuntime(t, path, personWorkflowJSON, nil)
	const ana, anaICH, bruno, brunoICH = "ilk:00000000-0000-4000-8000-0000000000a1", "ich:slack:ana", "ilk:00000000-0000-4000-8000-0000000000b2", "ich:slack:bruno"
	say := func(sender, ilk, ich string) {
		t.Helper()
		thread := "conv-channel"
		msg := from(sender, sdk.Message{Routing: sdk.Routing{Src: "x-uuid", TraceID: "trace-" + ilk}, Meta: sdk.Meta{MsgType: "user", ThreadID: &thread}, Payload: json.RawMessage(`{"text":"hi"}`)})
		if ilk != "" {
			msg.Meta.SrcIlk = &ilk
		}
		if ich != "" {
			msg.Meta.Ich = &ich
		}
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}
	// carries is the person the WF's last message to the agent carries.
	carries := func() string {
		t.Helper()
		msg := sentTo(t, disp, "AI.host@motherbee")
		return strings.TrimSpace(derefString(msg.Meta.SrcIlk) + " " + derefString(msg.Meta.Ich))
	}

	// One person: every message the WF writes carries them.
	say("IO.channel@motherbee", ana, anaICH)
	say("IO.team@motherbee", "ilk:00000000-0000-4000-8000-0000000000cc", "ich:slack:juan")
	say("AI.host@motherbee", "ilk:00000000-0000-4000-8000-0000000000dd", "")
	say("IO.channel@motherbee", "", "")
	say("IO.channel@motherbee", ana, anaICH)
	if len(disp.sent) != 5 || carries() != ana+" "+anaICH {
		t.Fatalf("one person's instance: %d sends, the last carries %q", len(disp.sent), carries())
	}

	// Bruno writes on the same thread: the instance has no single person.
	logged := nodeLog(t, func() {
		say("IO.channel@motherbee", bruno, brunoICH)
		if carries() != "" {
			t.Fatalf("with two people, the WF's message carries %q", carries())
		}
		say("IO.channel@motherbee", ana, anaICH)
		if carries() != "" {
			t.Fatalf("once two people write, the WF's message carries %q", carries())
		}
	})
	if strings.Count(logged, "more than one person writes on thread conv-channel") != 1 {
		t.Fatalf("the node's log says so once: %s", logged)
	}
	inst, _ := rt.Registry.GetByThread("conv-channel")
	if row := mustGetInstance(t, rt, inst.InstanceID); row.PersonILK != "" || row.PersonICH != "" {
		t.Fatalf("the store keeps no person: %+v", row)
	}

	// After a restart, still none.
	rt, disp = newTestRuntime(t, path, personWorkflowJSON, &mockTimerLister{})
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	say("IO.channel@motherbee", ana, anaICH)
	if carries() != "" {
		t.Fatalf("after a restart, the WF's message carries %q", carries())
	}
}

// The registry never puts a second running instance on a thread: it refuses it, and recovery fails
// it (thread_taken), keeping the one that started first.
func TestASecondInstanceOnAThreadIsRefused(t *testing.T) {
	registry := NewInstanceRegistry()
	first := &WFInstance{InstanceID: "wfi:first", ThreadID: "conv-1"}
	if err := registry.Add(first); err != nil {
		t.Fatal(err)
	}
	if err := registry.Add(&WFInstance{InstanceID: "wfi:second", ThreadID: "conv-1"}); err == nil || !strings.Contains(err.Error(), "held by running instance wfi:first") {
		t.Fatalf("a second instance on the thread: %v", err)
	}
	if holder, _ := registry.GetByThread("conv-1"); holder != first || registry.Count() != 1 {
		t.Fatalf("the thread stays with the first instance: %+v", holder)
	}
	if err := registry.Add(first); err != nil {
		t.Fatalf("the same instance again: %v", err)
	}

	rt, _ := newTestRuntime(t, "", listeningWorkflowJSON, &mockTimerLister{})
	for id, createdAt := range map[string]int64{"wfi:older": 1000, "wfi:newer": 2000} {
		if err := rt.Store.CreateInstance(context.Background(), WFInstanceRow{
			InstanceID: id, ThreadID: "conv-1", WorkflowType: "listening", DefinitionHash: rt.DefHash, Status: "running",
			CurrentState: "listening", InputJSON: `{"text":"x"}`, StateJSON: `{}`, CreatedAtMS: createdAt, UpdatedAtMS: createdAt,
		}); err != nil {
			t.Fatal(err)
		}
	}
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	if holder, _ := rt.Registry.GetByThread("conv-1"); holder == nil || holder.InstanceID != "wfi:older" || rt.Registry.Count() != 1 {
		t.Fatalf("the instance that started first keeps the thread: %+v", holder)
	}
	if row := mustGetInstance(t, rt, "wfi:newer"); row.Status != "failed" || !strings.HasPrefix(lastLog(t, rt, "wfi:newer").ErrorDetail, causeThreadTaken) {
		t.Fatalf("the other one fails: %+v %+v", row, lastLog(t, rt, "wfi:newer"))
	}
}
