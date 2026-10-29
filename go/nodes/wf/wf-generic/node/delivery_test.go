package node

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// askingWorkflowJSON asks AI.host@motherbee, gives up when its timer fires, and tells IO.out of the
// answer.
const askingWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "asking",
  "description": "asks an agent; gives up when its timer fires",
  "input_schema": {"type": "object"},
  "initial_state": "asking",
  "terminal_states": ["answered", "timed_out", "failed"],
  "states": [
    {
      "name": "asking",
      "entry_actions": [
        {"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "ASK"}, "payload": {"text": "hi"}},
        {"type": "schedule_timer", "timer_key": "ask_timeout", "fire_in": "2m"}
      ],
      "transitions": [
        {"event_match": {"msg": "ASK"}, "guard": "true", "target_state": "answered",
         "actions": [{"type": "send_message", "target": "IO.out@motherbee", "meta": {"msg": "ANSWER"}, "payload": {"$ref": "event.payload"}}]},
        {"event_match": {"msg": "BOOM"}, "guard": "true", "target_state": "answered",
         "actions": [{"type": "send_message", "target": "AI.panic@motherbee", "meta": {"msg": "BOOM"}}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'ask_timeout'", "target_state": "timed_out"}
      ]
    },
    {"name": "answered"},
    {"name": "timed_out"},
    {"name": "failed"}
  ]
}`

// startAsking starts an instance of askingWorkflowJSON on rt and returns its id (the thread of its
// first send).
func startAsking(t *testing.T, rt *NodeRuntime, disp *mockDispatcher, traceID string) string {
	t.Helper()
	trigger := from("IO.in@motherbee", msgWithNameAndPayload("START", map[string]any{"customer_id": "c1"}))
	trigger.Routing.TraceID = traceID
	sent := len(disp.sent)
	if err := Dispatch(context.Background(), trigger, rt); err != nil {
		t.Fatalf("Dispatch START: %v", err)
	}
	if len(disp.sent) != sent+1 {
		t.Fatalf("the creation asks the agent: sent %d", len(disp.sent)-sent)
	}
	return derefString(disp.sent[sent].Meta.ThreadID)
}

// realTimerFired is SY.timer's TIMER_FIRED as the router delivers it (the shared wire fixture of the
// Go and Rust SDKs, go/fluxbee-go-sdk/testdata/timer_wire), for a WF timer: SY.timer hands back the
// client_ref and the payload the WF scheduled it with (emitTimerFired in go/sy-timer/scheduler.go).
func realTimerFired(t *testing.T, instanceID, timerKey string) sdk.Message {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "..", "fluxbee-go-sdk", "testdata", "timer_wire", "event_timer_fired.json"))
	if err != nil {
		t.Fatal(err)
	}
	var msg sdk.Message
	if err := json.Unmarshal(raw, &msg); err != nil {
		t.Fatal(err)
	}
	var payload map[string]any
	if err := json.Unmarshal(msg.Payload, &payload); err != nil {
		t.Fatal(err)
	}
	payload["owner_l2_name"] = testNodeName
	payload["client_ref"] = timerClientRef(instanceID, timerKey)
	payload["user_payload"] = map[string]any{"instance_id": instanceID, "timer_key": timerKey}
	if msg.Payload, err = json.Marshal(payload); err != nil {
		t.Fatal(err)
	}
	msg.Routing.Dst = sdk.UnicastDestination(testNodeName)
	return msg
}

// FINDINGS A-117: SY.timer puts the key at payload.user_payload.timer_key, and no guard of the
// examples or the docs read it there. The engine exposes it as event.timer_key; event.payload stays
// as SY.timer sent it.
func TestARealTimerFiredReachesAnEventTimerKeyGuard(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	fired := realTimerFired(t, instanceID, "ask_timeout")
	if fired.SourceL2Name() != "SY.timer@motherbee" || rawMsgName(fired) != sdk.MsgTimerFired {
		t.Fatalf("fixture: %+v", fired.Routing)
	}

	event := messageToMap(fired)
	payload := event["payload"].(map[string]any)
	userPayload := payload["user_payload"].(map[string]any)
	if event["timer_key"] != "ask_timeout" || userPayload["timer_key"] != "ask_timeout" || payload["timer_key"] != nil {
		t.Fatalf("event = %v", event)
	}

	if err := Dispatch(context.Background(), fired, rt); err != nil {
		t.Fatalf("Dispatch TIMER_FIRED: %v", err)
	}
	if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "timed_out" || row.Status != "completed" {
		t.Fatalf("the timeout must fire: %+v", row)
	}
}

// FINDINGS A-119: a TIMER_FIRED is taken only from SY.timer of the WF's own hive; anyone else could
// fire an instance's timer by its client_ref. A refused fire is never answered.
func TestAForgedTimerFiredIsRefused(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	sent := len(disp.sent)
	for _, forger := range []string{"AI.evil@motherbee", "SY.timer@worker1", "WF.other@motherbee"} {
		forged := realTimerFired(t, instanceID, "ask_timeout")
		forged.Routing.SrcL2Name = &forger
		if err := Dispatch(context.Background(), forged, rt); err != nil {
			t.Fatalf("Dispatch from %s: %v", forger, err)
		}
	}
	if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "asking" {
		t.Fatalf("a forged fire moved the instance: %+v", row)
	}
	if len(disp.sent) != sent {
		t.Fatalf("a refused fire is never answered: %v", disp.sent[sent:])
	}
	if events, _ := rt.Store.ListInboundEvents(context.Background()); len(events) != 0 {
		t.Fatalf("a refused fire is never journaled: %+v", events)
	}
}

// FINDINGS A-119: the engine's control verbs answer only SY.admin@motherbee and SY.wf-rules of the
// WF's own hive; anyone else gets UNAUTHORIZED, and a cancel from them cancels nothing.
func TestTheControlVerbsAnswerOnlyTheAdminAndTheWFRulesOfTheHive(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	request := func(verb, sender string) map[string]any {
		t.Helper()
		msg := from(sender, msgWithNameAndPayload(verb, map[string]any{"instance_id": instanceID}))
		sent := len(disp.sent)
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatalf("%s from %s: %v", verb, sender, err)
		}
		if len(disp.sent) != sent+1 {
			t.Fatalf("%s from %s: %d replies", verb, sender, len(disp.sent)-sent)
		}
		reply := disp.sent[len(disp.sent)-1]
		if got := derefString(reply.Meta.Msg); got != wfControlRequests[verb] {
			t.Fatalf("%s from %s answered %q", verb, sender, got)
		}
		return lastReply(t, disp)
	}
	verbs := []string{MsgWFHelp, MsgWFGetInstance, MsgWFListInstances, MsgWFGetCode, MsgWFCancelInstance}
	for _, verb := range verbs {
		for _, sender := range []string{"AI.host@motherbee", "SY.wf-rules@worker1", "SY.admin@worker1", "SY.orchestrator@motherbee", "WF.other@motherbee"} {
			if reply := request(verb, sender); reply["ok"] != false || reply["error"] != errCodeUnauthorized {
				t.Fatalf("%s from %s = %v", verb, sender, reply)
			}
		}
	}
	if row := mustGetInstance(t, rt, instanceID); row.Status != "running" {
		t.Fatalf("an unauthorized cancel cancelled: %+v", row)
	}
	for _, verb := range verbs[:4] {
		for _, sender := range []string{sdk.PrimaryAdminNode, "SY.wf-rules@motherbee"} {
			if reply := request(verb, sender); reply["ok"] != true {
				t.Fatalf("%s from %s = %v", verb, sender, reply)
			}
		}
	}
	if reply := request(MsgWFCancelInstance, "SY.wf-rules@motherbee"); reply["ok"] != true || reply["status"] != "cancelled" {
		t.Fatalf("cancel = %v", reply)
	}
}

// FINDINGS A-112/A-119: a router notice, a reply (a *_RESPONSE, an error answer, an agent's
// <msg>.error) and a message without a name are never answered; a reply never starts an instance
// either, even when the schema would take it. So two WFs cannot bounce an INVALID_INPUT.
func TestErrorsRepliesAndNoticesAreNeverAnswered(t *testing.T) {
	strict := strings.Replace(askingWorkflowJSON, `"input_schema": {"type": "object"}`, `"input_schema": {"type": "object", "required": ["customer_id"]}`, 1)
	for name, definition := range map[string]string{"permissive": askingWorkflowJSON, "strict": strict} {
		t.Run(name, func(t *testing.T) {
			rt, disp := newTestRuntime(t, "", definition, nil)
			unreachable := sdk.MSGUnreachable
			cases := map[string]sdk.Message{
				"another WF's INVALID_INPUT": from("WF.other@motherbee", msgWithNameAndPayload(msgInvalidInput, map[string]any{"ok": false, "error": "INVALID_INPUT", "detail": "x"})),
				"a *_RESPONSE":               from("WF.other@motherbee", msgWithNameAndPayload("WF_LIST_INSTANCES_RESPONSE", map[string]any{"ok": false, "error": "UNAUTHORIZED"})),
				"an agent's error":           from("AI.host@motherbee", msgWithNameAndPayload("ASK.error", map[string]any{"code": "x"})),
				"a router notice":            {Routing: sdk.Routing{Src: "router-uuid", TraceID: "t-notice"}, Meta: sdk.Meta{MsgType: "system", Msg: &unreachable}, Payload: json.RawMessage(`{"reason":"NODE_NOT_FOUND"}`)},
			}
			for label, msg := range cases {
				if err := Dispatch(context.Background(), msg, rt); err != nil {
					t.Fatalf("%s: %v", label, err)
				}
				if len(disp.sent) != 0 || rt.Registry.Count() != 0 {
					t.Fatalf("%s: answered %v, started %d instances", label, disp.sent, rt.Registry.Count())
				}
			}
			if rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0); len(rows) != 0 {
				t.Fatalf("a reply started an instance: %+v", rows)
			}

			// A message without a name (an IO node's text) starts an instance, or, refused, gets no
			// answer; a named request refused gets its INVALID_INPUT.
			text := from("IO.in@motherbee", sdk.Message{Routing: sdk.Routing{Src: "io-uuid", TraceID: "t-text"}, Meta: sdk.Meta{MsgType: "user"}, Payload: json.RawMessage(`{"text":"hello"}`)})
			if err := Dispatch(context.Background(), text, rt); err != nil {
				t.Fatal(err)
			}
			named := from("AI.billing@motherbee", msgWithNameAndPayload("START", map[string]any{"text": "hello"}))
			named.Routing.Src = "billing-uuid"
			if err := Dispatch(context.Background(), named, rt); err != nil {
				t.Fatal(err)
			}
			if name == "permissive" {
				if rt.Registry.Count() != 2 {
					t.Fatalf("a text and a request start instances: %d", rt.Registry.Count())
				}
				return
			}
			if rt.Registry.Count() != 0 || len(disp.sent) != 1 || derefString(disp.sent[0].Meta.Msg) != msgInvalidInput || disp.sent[0].Routing.Dst.Value() != "billing-uuid" {
				t.Fatalf("only the named request gets an INVALID_INPUT: %+v", disp.sent)
			}
		})
	}
}

// crashingDispatcher records what it sends, and closes the store's database right after a send to
// crashAt: the node dies between a send and the commit of its transition.
type crashingDispatcher struct {
	mockDispatcher
	store   *Store
	crashAt string
}

func (d *crashingDispatcher) SendMsg(msg sdk.Message) error {
	if err := d.mockDispatcher.SendMsg(msg); err != nil {
		return err
	}
	if msg.Routing.Dst.Value() == d.crashAt {
		_ = d.store.db.Close()
	}
	return nil
}

// At least once (FINDINGS A-118): an event is journaled before its transition runs, and the
// transition's commit consumes it. A crash in between leaves it, and recovery runs it again: its
// send goes again, with the same trace_id.
func TestAnEventLeftByACrashRunsAgainWithItsTraceID(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	before, disp := newTestRuntime(t, path, askingWorkflowJSON, nil)
	instanceID := startAsking(t, before, disp, "trace-start")
	crash := &crashingDispatcher{store: before.Store, crashAt: "IO.out@motherbee"}
	before.ActCtx.Dispatcher = crash

	answer := from("AI.host@motherbee", msgWithNameAndPayload("ASK", map[string]any{"text": "the answer"}))
	answer.Meta.ThreadID = &instanceID
	answer.Routing.TraceID = "trace-answer"
	if err := Dispatch(context.Background(), answer, before); err == nil {
		t.Fatal("the commit cannot succeed on a closed store")
	}
	if len(crash.sent) != 1 || crash.sent[0].Routing.TraceID != "trace-answer" {
		t.Fatalf("the transition sent before the crash: %+v", crash.sent)
	}

	after, disp := newTestRuntime(t, path, askingWorkflowJSON, &mockTimerLister{})
	if err := after.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}
	if len(disp.sent) != 1 || disp.sent[0].Routing.Dst.Value() != "IO.out@motherbee" || disp.sent[0].Routing.TraceID != "trace-answer" {
		t.Fatalf("the send goes again with its trace_id: %+v", disp.sent)
	}
	var payload map[string]any
	_ = json.Unmarshal(disp.sent[0].Payload, &payload)
	if payload["text"] != "the answer" {
		t.Fatalf("the event runs again as it came: %s", disp.sent[0].Payload)
	}
	if row := mustGetInstance(t, after, instanceID); row.CurrentState != "answered" {
		t.Fatalf("after recovery: %+v", row)
	}
	if events, _ := after.Store.ListInboundEvents(context.Background()); len(events) != 0 {
		t.Fatalf("the journal is empty once the transition commits: %+v", events)
	}
}

// At least once, at creation: a crash between the journaling of the trigger and the commit of the
// new instance leaves no half-started instance; recovery creates it again, under the same
// instance_id, and its first send goes again with the trigger's trace_id.
func TestACreationLeftByACrashRunsAgainUnderItsInstanceID(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	before, _ := newTestRuntime(t, path, askingWorkflowJSON, nil)
	crash := &crashingDispatcher{store: before.Store, crashAt: "AI.host@motherbee"}
	before.ActCtx.Dispatcher = crash
	trigger := from("IO.in@motherbee", msgWithNameAndPayload("START", map[string]any{"customer_id": "c1"}))
	trigger.Routing.TraceID = "trace-start"
	if err := Dispatch(context.Background(), trigger, before); err == nil {
		t.Fatal("the creation cannot commit on a closed store")
	}
	if len(crash.sent) != 1 {
		t.Fatalf("the creation asked before the crash: %+v", crash.sent)
	}
	instanceID := derefString(crash.sent[0].Meta.ThreadID)

	after, disp := newTestRuntime(t, path, askingWorkflowJSON, &mockTimerLister{})
	if rows, _ := after.Store.ListInstances(context.Background(), "", 0, 0); len(rows) != 0 {
		t.Fatalf("no instance is half-started: %+v", rows)
	}
	if err := after.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}
	if len(disp.sent) != 1 || derefString(disp.sent[0].Meta.ThreadID) != instanceID || disp.sent[0].Routing.TraceID != "trace-start" {
		t.Fatalf("the creation runs again under its instance_id and trace_id: %+v", disp.sent)
	}
	row := mustGetInstance(t, after, instanceID)
	if row.CurrentState != "asking" || row.Status != "running" || row.DefinitionHash != after.DefHash {
		t.Fatalf("after recovery: %+v", row)
	}
	if _, ok := after.Registry.Get(instanceID); !ok {
		t.Fatal("the recreated instance is running")
	}
	if events, _ := after.Store.ListInboundEvents(context.Background()); len(events) != 0 {
		t.Fatalf("the journal is empty once the creation commits: %+v", events)
	}
}

// FINDINGS A-118: a fired timer's index row goes when its fire is taken, so a restart (when SY.timer
// has nothing pending) does not fire it a second time.
func TestAFiredTimerDoesNotFireAgainAfterARestart(t *testing.T) {
	const counting = `{
  "wf_schema_version": "1",
  "workflow_type": "counting",
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "waiting",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "tick", "fire_in": "2m"}],
      "transitions": [
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'tick'", "target_state": "fired",
         "actions": [{"type": "set_variable", "name": "fires", "value": "has(state.fires) ? state.fires + 1 : 1"}]}
      ]
    },
    {
      "name": "fired",
      "transitions": [
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'tick'", "target_state": "fired",
         "actions": [{"type": "set_variable", "name": "fires", "value": "state.fires + 1"}]},
        {"event_match": {"msg": "DONE"}, "guard": "true", "target_state": "done"}
      ]
    },
    {"name": "done"}
  ]
}`
	path := filepath.Join(t.TempDir(), "wf.db")
	before, _ := newTestRuntime(t, path, counting, nil)
	instanceID := startInstance(t, before, from("IO.in@motherbee", msgWithName("START")))
	if timers, _ := before.Store.ListTimersForInstance(context.Background(), instanceID); len(timers) != 1 {
		t.Fatalf("the timer is in the index: %+v", timers)
	}
	if err := Dispatch(context.Background(), realTimerFired(t, instanceID, "tick"), before); err != nil {
		t.Fatal(err)
	}
	if timers, _ := before.Store.ListTimersForInstance(context.Background(), instanceID); len(timers) != 0 {
		t.Fatalf("the fired timer left the index: %+v", timers)
	}

	// Ten minutes later the node restarts; SY.timer has nothing pending (the timer fired).
	after, _ := newTestRuntime(t, path, counting, &mockTimerLister{})
	after.ActCtx.Clock = func() time.Time { return fixedClock().Add(10 * time.Minute) }
	if err := after.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	row := mustGetInstance(t, after, instanceID)
	if row.CurrentState != "fired" || row.StateJSON != `{"fires":1}` {
		t.Fatalf("the timer fired once: %+v", row)
	}
}

// notifyAgentWorkflowJSON asks an agent, then tells it the outcome: the agent answers that too, on the
// instance's thread, with the request's msg, after the instance ended.
const notifyAgentWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "notify",
  "input_schema": {"type": "object"},
  "initial_state": "asking",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "asking",
      "entry_actions": [{"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "ASK"}, "payload": {"type": "text", "content": "evaluate"}}],
      "transitions": [
        {"event_match": {"msg": "ASK"}, "guard": "true", "target_state": "done",
         "actions": [{"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "OUTCOME"}, "payload": {"type": "text", "content": "recorded"}}]}
      ]
    },
    {"name": "done"}
  ]
}`

// answerAs is an agent's answer to sent, as ai-generic builds it: the request's msg and thread, user.
func answerAs(agent string, sent sdk.Message, payload string) sdk.Message {
	return from(agent, sdk.Message{
		Routing: sdk.Routing{Src: "agent-uuid", TraceID: sent.Routing.TraceID},
		Meta:    sdk.Meta{MsgType: "user", Msg: sent.Meta.Msg, ThreadID: sent.Meta.ThreadID},
		Payload: json.RawMessage(payload),
	})
}

// An agent answers with the request's own msg and thread, so a late answer (its instance ended: its
// timer fired, it was cancelled or failed, or a first copy of the answer finished it) is no reply by
// name. It is dropped and never answered: started as a new instance it would ask the agent again, a
// WF<->AI loop. A thread naming another node's instance (a WF that sends to this one) still starts
// one.
func TestALateAnswerNeverStartsAnInstance(t *testing.T) {
	t.Run("after its timeout", func(t *testing.T) {
		rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
		instanceID := startAsking(t, rt, disp, "trace-start")
		ask := disp.sent[len(disp.sent)-1]
		if err := Dispatch(context.Background(), realTimerFired(t, instanceID, "ask_timeout"), rt); err != nil {
			t.Fatal(err)
		}
		sent := len(disp.sent)
		if err := Dispatch(context.Background(), answerAs("AI.host@motherbee", ask, `{"type":"text","content":"the late answer"}`), rt); err != nil {
			t.Fatal(err)
		}
		if rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0); len(rows) != 1 || len(disp.sent) != sent {
			t.Fatalf("the late answer started %d instances and sent %d messages", len(rows)-1, len(disp.sent)-sent)
		}
	})
	t.Run("the outcome loop", func(t *testing.T) {
		rt, disp := newTestRuntime(t, "", notifyAgentWorkflowJSON, nil)
		startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
		// Play the agent: answer every message the WF sends it, as ai-generic does.
		for answered := 0; answered < len(disp.sent) && answered < 10; answered++ {
			if err := Dispatch(context.Background(), answerAs("AI.host@motherbee", disp.sent[answered], `{"type":"text","content":"ok"}`), rt); err != nil {
				t.Fatal(err)
			}
		}
		rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0)
		if len(rows) != 1 || len(disp.sent) != 2 || rows[0].CurrentState != "done" {
			t.Fatalf("the agent's answer to the outcome started instances: %d instances, %d sends", len(rows), len(disp.sent))
		}
	})
	t.Run("another node's instance on the thread", func(t *testing.T) {
		rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
		thread := "wfi:00000000-0000-4000-8000-000000000001"
		msg := from("WF.upstream@motherbee", msgWithName("START"))
		msg.Meta.ThreadID = &thread
		startInstance(t, rt, msg)
		if len(disp.sent) != 1 {
			t.Fatalf("a WF's send starts an instance on this one: %d sends", len(disp.sent))
		}
	})
}

// FINDINGS A-119: SY.timer lets any node schedule a timer whose fire goes to another node, under
// SY.timer's own name, so the sender check alone lets it through. A TIMER_FIRED is taken only for a
// timer this node scheduled (its owner_l2_name) and only by its client_ref.
func TestATimerFiredOfAnotherOwnerIsRefused(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	sent := len(disp.sent)
	forge := func(edit func(payload map[string]any)) sdk.Message {
		fired := realTimerFired(t, instanceID, "ask_timeout")
		var payload map[string]any
		if err := json.Unmarshal(fired.Payload, &payload); err != nil {
			t.Fatal(err)
		}
		edit(payload)
		fired.Payload, _ = json.Marshal(payload)
		return fired
	}
	for name, fired := range map[string]sdk.Message{
		"scheduled by another node": forge(func(payload map[string]any) { payload["owner_l2_name"] = "AI.evil@motherbee" }),
		"no client_ref":             forge(func(payload map[string]any) { delete(payload, "client_ref") }),
	} {
		if err := Dispatch(context.Background(), fired, rt); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "asking" || len(disp.sent) != sent {
			t.Fatalf("%s: the fire moved the instance (%s) or was answered", name, row.CurrentState)
		}
	}
	// SY.timer's own fire of the WF's timer still reaches the guard.
	if err := Dispatch(context.Background(), realTimerFired(t, instanceID, "ask_timeout"), rt); err != nil {
		t.Fatal(err)
	}
	if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "timed_out" {
		t.Fatalf("the WF's own timer: %+v", row)
	}
}

// A system message is control, never a workflow event, but a TIMER_FIRED (the AI and IO nodes take
// the system kind only as control too): it neither starts an instance nor reaches one, and it is
// never answered.
func TestASystemMessageIsNoWorkflowEvent(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	sent := len(disp.sent)
	start := from("AI.billing@motherbee", msgWithName("START"))
	start.Meta.MsgType = "system"
	answer := from("AI.host@motherbee", msgWithNameAndPayload("ASK", map[string]any{"text": "an answer"}))
	answer.Meta.MsgType = "SYSTEM"
	answer.Meta.ThreadID = &instanceID
	for _, msg := range []sdk.Message{start, answer} {
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}
	rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0)
	if len(rows) != 1 || rows[0].CurrentState != "asking" || len(disp.sent) != sent {
		t.Fatalf("a system message acted: %d instances, state %s, %d sends", len(rows), rows[0].CurrentState, len(disp.sent)-sent)
	}
}
