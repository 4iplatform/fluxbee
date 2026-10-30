package node

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"strings"
	"testing"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// The engine items of Case 3 (wf-orchestration-v1 §5.3.6): the menu in the WF's block, the catch,
// the default and the cap on agent calls.

// ioMessage is a message of IO.in@motherbee on thread, named msgName when it is not empty.
func ioMessage(thread, msgName, payload string) sdk.Message {
	msg := sdk.Message{
		Routing: sdk.Routing{Src: "io-uuid", TraceID: "trace-" + thread + "-" + msgName},
		Meta:    sdk.Meta{MsgType: "user", ThreadID: &thread},
		Payload: json.RawMessage(payload),
	}
	if msgName != "" {
		msg.Meta.Msg = &msgName
	}
	return from("IO.in@motherbee", msg)
}

// rawBlock is the WF's block of a message as it went on the wire, field by field.
func rawBlock(t *testing.T, msg sdk.Message) map[string]json.RawMessage {
	t.Helper()
	var metaContext map[string]json.RawMessage
	if err := json.Unmarshal(msg.Meta.Context, &metaContext); err != nil {
		t.Fatalf("meta.context %s: %v", msg.Meta.Context, err)
	}
	var block map[string]json.RawMessage
	if err := json.Unmarshal(metaContext["wf"], &block); err != nil {
		t.Fatalf("meta.context.wf %s: %v", metaContext["wf"], err)
	}
	return block
}

// nodeLog is what the node logs while run runs.
func nodeLog(t *testing.T, run func()) string {
	t.Helper()
	var buf bytes.Buffer
	log.SetOutput(&buf)
	defer log.SetOutput(os.Stderr)
	run()
	return buf.String()
}

func mustState(t *testing.T, rt *NodeRuntime, instanceID string) map[string]any {
	t.Helper()
	state, err := unmarshalJSON(mustGetInstance(t, rt, instanceID).StateJSON)
	if err != nil {
		t.Fatal(err)
	}
	return state
}

const menuWireWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "menu_wire",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "asking",
  "terminal_states": ["done", "failed"],
  "states": [
    {
      "name": "asking",
      "entry_actions": [
        {"type": "forward", "target": "AI.host@motherbee",
         "menu": {"help": "", "options": [
           {"name": "answer", "description": "You answer <the> person & stop.", "params": {}},
           {"name": "pick", "description": "You pick a plan.",
            "params": {"seats": {"type": "integer", "description": "How many."}, "plan": {"type": "string", "description": "", "enum": ["basic", "pro"]}}}
         ]}},
        {"type": "send_message", "target": "AI.other@motherbee", "meta": {"msg": "ASK"}, "payload": {"text": "hi"}}
      ]
    },
    {"name": "done"},
    {"name": "failed"}
  ]
}`

// Case 3 (§5.3.6): the engine writes an action's menu in the WF's block (meta.context.wf.menu) as it
// is written, but with no empty field, which the agent would read as present and refuse: an empty
// help, an empty params and an empty description are left out. A send without a menu has none.
func TestTheMenuGoesInTheWFBlockAsWritten(t *testing.T) {
	rt, disp := newTestRuntime(t, "", menuWireWorkflowJSON, nil)
	startInstance(t, rt, ioMessage("conv-menu", "", `{"type":"text","content":"hi"}`))
	toHost := sentTo(t, disp, "AI.host@motherbee")
	const want = `{"options":[{"name":"answer","description":"You answer <the> person & stop."},{"name":"pick","description":"You pick a plan.","params":{"plan":{"type":"string","enum":["basic","pro"]},"seats":{"type":"integer","description":"How many."}}}]}`
	if got := string(rawBlock(t, toHost)["menu"]); got != want {
		t.Fatalf("the menu on the wire:\n got %s\nwant %s", got, want)
	}
	if block := rawBlock(t, sentTo(t, disp, "AI.other@motherbee")); block["menu"] != nil || block["trail"] == nil {
		t.Fatalf("a send without a menu: %v", block)
	}
}

// catchingWorkflowJSON asks AI.host with a menu. The state takes the agent's yes; its catch takes
// the agent's none (to declined, through the state's exit actions and declined's entry actions), a
// PING the state's guard refuses (internal), any error and any timer's fire (an empty event_match).
// Anything else is the default's.
const catchingWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "catching",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "asking",
  "terminal_states": ["done", "failed"],
  "catch": [
    {"event_match": {"src": "AI.host@motherbee"}, "guard": "event.context.evaluation.option == 'none'", "target_state": "declined",
     "actions": [{"type": "set_variable", "name": "reason", "value": "event.context.evaluation.params.reason"}]},
    {"event_match": {"msg": "PING"}, "guard": "true",
     "actions": [{"type": "set_variable", "name": "pings", "value": "has(state.pings) ? state.pings + 1 : 1"}]},
    {"event_match": {}, "guard": "event.payload.type == 'error'",
     "actions": [{"type": "set_variable", "name": "last_error", "value": "event.payload.code"}]},
    {"event_match": {}, "guard": "event.type == 'system'",
     "actions": [{"type": "set_variable", "name": "fired", "value": "event.timer_key"}]}
  ],
  "states": [
    {
      "name": "asking",
      "entry_actions": [{"type": "forward", "target": "AI.host@motherbee", "menu": {"options": [{"name": "yes", "description": "It is yes."}]}}],
      "exit_actions": [{"type": "set_variable", "name": "left", "value": "'asking'"}],
      "transitions": [
        {"event_match": {"src": "AI.host@motherbee"}, "guard": "event.context.evaluation.option == 'yes'", "target_state": "done"},
        {"event_match": {"msg": "PING"}, "guard": "has(state.take_pings)"}
      ]
    },
    {"name": "declined", "entry_actions": [{"type": "set_variable", "name": "entered", "value": "'declined'"}]},
    {"name": "done"},
    {"name": "failed"}
  ]
}`

// Case 3 (§5.3.2): when no transition of the current state takes an event, the catch's transitions
// are tried in order. A chosen one runs as a state's: with a target_state, the state's exit actions,
// its own and the target's entry actions; without one it stays. An empty event_match matches every
// event. When the catch takes nothing either, the default logs the event, in the instance's log and
// in the node's, and the instance stays where it was.
func TestTheCatchTakesWhatTheStateDoesNot(t *testing.T) {
	rt, disp := newTestRuntime(t, "", catchingWorkflowJSON, nil)
	ask := func(thread string) (string, sdk.Message) {
		t.Helper()
		instanceID := startInstance(t, rt, ioMessage(thread, "", `{"type":"text","content":"hi"}`))
		return instanceID, sentTo(t, disp, "AI.host@motherbee")
	}
	dispatch := func(msg sdk.Message) {
		t.Helper()
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}
	answer := func(request sdk.Message, evaluation map[string]any) sdk.Message {
		return agentAnswer(t, "AI.host@motherbee", request, `{"type":"text","content":"reply"}`, evaluation)
	}

	// The state's transition comes first.
	yes, request := ask("conv-yes")
	dispatch(answer(request, map[string]any{"reply": "reply", "option": "yes", "params": map[string]any{}}))
	if row := mustGetInstance(t, rt, yes); row.CurrentState != "done" || mustState(t, rt, yes)["reason"] != nil {
		t.Fatalf("the state takes yes: %+v", row)
	}

	// none: the catch goes to declined, through asking's exit actions and declined's entry actions.
	declined, request := ask("conv-none")
	dispatch(answer(request, map[string]any{"reply": "reply", "option": "none", "params": map[string]any{"reason": "out of scope"}}))
	state := mustState(t, rt, declined)
	if row := mustGetInstance(t, rt, declined); row.CurrentState != "declined" || row.Status != "running" ||
		state["reason"] != "out of scope" || state["left"] != "asking" || state["entered"] != "declined" {
		t.Fatalf("the catch takes none: %+v %v", row, state)
	}
	if entry := lastLog(t, rt, declined); entry.Summary != "catch[0]: transition asking → declined" {
		t.Fatalf("the instance's log: %+v", entry)
	}

	// A PING the state's guard refuses, an agent's error and a timer's fire: the catch takes each,
	// and the instance stays.
	stays, request := ask("conv-stays")
	dispatch(ioMessage("conv-stays", "PING", `{}`))
	dispatch(ioMessage("conv-stays", "PING", `{}`))
	if entry := lastLog(t, rt, stays); entry.Summary != "catch[1]: internal transition in asking on PING from IO.in@motherbee" {
		t.Fatalf("the instance's log: %+v", entry)
	}
	dispatch(agentAnswer(t, "AI.host@motherbee", request, `{"type":"error","code":"invalid_structured_output","message":"unknown_option:maybe"}`, nil))
	if err := Dispatch(context.Background(), realTimerFired(t, stays, "late"), rt); err != nil {
		t.Fatal(err)
	}
	state = mustState(t, rt, stays)
	if row := mustGetInstance(t, rt, stays); row.CurrentState != "asking" || state["pings"] != int64(2) ||
		state["last_error"] != "invalid_structured_output" || state["fired"] != "late" || state["left"] != nil {
		t.Fatalf("the catch's internal transitions: %+v %v", row, state)
	}

	// Nothing takes a plain message: the default logs it, in the instance's log and the node's.
	sent := len(disp.sent)
	logged := nodeLog(t, func() { dispatch(ioMessage("conv-stays", "", `{"type":"text","content":"hello?"}`)) })
	if !strings.Contains(logged, "instance "+stays+": unhandled event (no msg) from IO.in@motherbee in state asking (trace_id=trace-conv-stays-)") {
		t.Fatalf("the node's log: %s", logged)
	}
	if entry := lastLog(t, rt, stays); entry.Summary != "unhandled event (no msg) from IO.in@motherbee in state asking" || entry.OK || entry.ErrorDetail != "no matching transition" {
		t.Fatalf("the instance's log: %+v", entry)
	}
	if row := mustGetInstance(t, rt, stays); row.CurrentState != "asking" || row.Status != "running" || len(disp.sent) != sent {
		t.Fatalf("the instance stays where it was: %+v", row)
	}
}

// A definition without a catch has the default only: the instance's log and the node's.
func TestWithoutACatchTheDefaultLogs(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	logged := nodeLog(t, func() {
		if err := Dispatch(context.Background(), from("IO.in@motherbee", sdk.Message{
			Routing: sdk.Routing{TraceID: "trace-odd"},
			Meta:    sdk.Meta{MsgType: "user", Msg: strPtr("ODD"), ThreadID: strPtr(instanceID)},
			Payload: json.RawMessage(`{}`),
		}), rt); err != nil {
			t.Fatal(err)
		}
	})
	if !strings.Contains(logged, "instance "+instanceID+": unhandled event ODD from IO.in@motherbee in state asking (trace_id=trace-odd)") {
		t.Fatalf("the node's log: %s", logged)
	}
	if entry := lastLog(t, rt, instanceID); entry.Summary != "unhandled event ODD from IO.in@motherbee in state asking" {
		t.Fatalf("the instance's log: %+v", entry)
	}
}

// loopingWorkflowJSON asks AI.host again at each of its answers, a loop the cap on agent calls stops;
// a message of IO.in and a timer's fire ask it too. The catch takes the cap's AGENT_CALL_CAP: it
// keeps the error, asks the agent once more (past the cap, so not sent) and tells IO.out.
const loopingWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "looping",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "asking",
  "terminal_states": ["done", "failed"],
  "catch": [
    {"event_match": {"msg": "AGENT_CALL_CAP"}, "guard": "event.payload.type == 'error' && event.payload.code == 'agent_call_cap'",
     "actions": [
       {"type": "set_variable", "name": "capped", "value": "event.payload"},
       {"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "AGAIN"}, "payload": {"text": "once more"}},
       {"type": "send_message", "target": "IO.out@motherbee", "meta": {"msg": "STOPPED"}, "payload": {"target": {"$ref": "event.payload.target"}}}
     ]}
  ],
  "states": [
    {
      "name": "asking",
      "entry_actions": [{"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "ASK"}, "payload": {"text": "go"}}],
      "transitions": [
        {"event_match": {"src": "AI.host@motherbee"}, "guard": "true",
         "actions": [
           {"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "ASK"}, "payload": {"text": "again"}},
           {"type": "set_variable", "name": "answers", "value": "has(state.answers) ? state.answers + 1 : 1"}
         ]},
        {"event_match": {"src": "IO.in@motherbee"}, "guard": "true", "actions": [{"type": "forward", "target": "AI.host@motherbee"}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "true",
         "actions": [{"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "ASK"}, "payload": {"text": "tick"}}]}
      ]
    },
    {"name": "done"},
    {"name": "failed"}
  ]
}`

// Case 3 (§5.3.3, FINDINGS A-125): at most maxAgentCallsPerTrigger messages go to agents between two
// triggers (a message from an IO node or a WF, or a timer's fire; an agent's answer and an internal
// event go on with the count). Past it the send is not made and the instance's log says so, the
// transition's other actions run, and the instance gets one AGENT_CALL_CAP per trigger, which its
// catch takes. The instance never fails for it. The count lives in memory: a restart starts it again.
func TestTheCapOnAgentCallsStopsALoop(t *testing.T) {
	path := t.TempDir() + "/wf.db"
	rt, disp := newTestRuntime(t, path, loopingWorkflowJSON, nil)
	const thread = "conv-loop"
	instanceID := startInstance(t, rt, ioMessage(thread, "", `{"type":"text","content":"go"}`))
	count := func(node string) int {
		n := 0
		for _, msg := range disp.sent {
			if msg.Routing.Dst.Value() == node {
				n++
			}
		}
		return n
	}
	answer := func() {
		t.Helper()
		if err := Dispatch(context.Background(), agentAnswer(t, "AI.host@motherbee", sentTo(t, disp, "AI.host@motherbee"), `{"type":"text","content":"ok"}`, nil), rt); err != nil {
			t.Fatal(err)
		}
	}
	notSent := func() int {
		logs, err := rt.Store.GetRecentLog(context.Background(), instanceID, 500)
		if err != nil {
			t.Fatal(err)
		}
		n := 0
		for _, entry := range logs {
			if entry.ActionType == "send_message" && !entry.OK && strings.HasPrefix(entry.ErrorDetail, "agent_call_cap: 8 messages went to agents since the last trigger") {
				n++
			}
		}
		return n
	}

	// The message that started the instance asked the agent once; seven answers ask it seven times
	// more, and the eighth answer's ask is past the cap.
	for i := 0; i < 7; i++ {
		answer()
	}
	if count("AI.host@motherbee") != 8 || count("IO.out@motherbee") != 0 {
		t.Fatalf("eight asks: %d", count("AI.host@motherbee"))
	}
	answer()
	state := mustState(t, rt, instanceID)
	capped, _ := state["capped"].(map[string]any)
	if count("AI.host@motherbee") != 8 || state["answers"] != int64(8) || capped["code"] != "agent_call_cap" || capped["type"] != "error" ||
		capped["target"] != "AI.host@motherbee" || !strings.HasPrefix(fmt.Sprint(capped["message"]), "8 messages went to agents since the last trigger") {
		t.Fatalf("past the cap: %d asks, state %v", count("AI.host@motherbee"), state)
	}
	if stopped := sentTo(t, disp, "IO.out@motherbee"); count("IO.out@motherbee") != 1 || string(stopped.Payload) != `{"target":"AI.host@motherbee"}` {
		t.Fatalf("the catch tells IO.out: %s", stopped.Payload)
	}
	// The eighth answer's ask and the catch's were not sent, and the instance's log says so.
	if row := mustGetInstance(t, rt, instanceID); row.Status != "running" || notSent() != 2 {
		t.Fatalf("the instance goes on: %+v, %d not sent", row, notSent())
	}

	// An agent's answer is no trigger: its ask is not sent either, and no second AGENT_CALL_CAP comes.
	answer()
	if count("AI.host@motherbee") != 8 || count("IO.out@motherbee") != 1 || mustState(t, rt, instanceID)["answers"] != int64(9) || notSent() != 3 {
		t.Fatalf("one AGENT_CALL_CAP per trigger: %d asks, %d stops", count("AI.host@motherbee"), count("IO.out@motherbee"))
	}

	// A message of IO.in is a trigger: a new count, and a new AGENT_CALL_CAP when it is spent.
	if err := Dispatch(context.Background(), ioMessage(thread, "", `{"type":"text","content":"again"}`), rt); err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 7; i++ {
		answer()
	}
	if count("AI.host@motherbee") != 16 || count("IO.out@motherbee") != 1 {
		t.Fatalf("a trigger starts a new count: %d asks", count("AI.host@motherbee"))
	}
	answer()
	if count("AI.host@motherbee") != 16 || count("IO.out@motherbee") != 2 {
		t.Fatalf("a new AGENT_CALL_CAP: %d asks, %d stops", count("AI.host@motherbee"), count("IO.out@motherbee"))
	}

	// A timer's fire is a trigger too.
	if err := Dispatch(context.Background(), realTimerFired(t, instanceID, "tick"), rt); err != nil {
		t.Fatal(err)
	}
	if count("AI.host@motherbee") != 17 {
		t.Fatalf("a timer's fire starts a new count: %d asks", count("AI.host@motherbee"))
	}
	for i := 0; i < 7; i++ {
		answer()
	}
	if count("AI.host@motherbee") != 24 {
		t.Fatalf("the count is spent again: %d asks", count("AI.host@motherbee"))
	}

	// A restart starts the count again: the next answer's ask is sent.
	rt, disp = newTestRuntime(t, path, loopingWorkflowJSON, &mockTimerLister{})
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	afterRestart := agentAnswer(t, "AI.host@motherbee", sdk.Message{Routing: sdk.Routing{TraceID: "trace-after"}, Meta: sdk.Meta{MsgType: "user", Msg: strPtr("ASK"), ThreadID: strPtr(thread)}}, `{"type":"text","content":"ok"}`, nil)
	if err := Dispatch(context.Background(), afterRestart, rt); err != nil {
		t.Fatal(err)
	}
	if count := len(disp.sent); count != 1 || disp.sent[0].Routing.Dst.Value() != "AI.host@motherbee" {
		t.Fatalf("after a restart the count starts again: %+v", disp.sent)
	}
	if row := mustGetInstance(t, rt, instanceID); row.Status != "running" {
		t.Fatalf("the instance never failed: %+v", row)
	}
}

// The message that starts an instance is its first trigger: a send past the cap in the initial
// state's entry actions is not made, and the AGENT_CALL_CAP goes through the state's transitions
// first.
func TestTheCapCountsFromTheMessageThatStartsAnInstance(t *testing.T) {
	asks := strings.Repeat(`{"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "ASK"}, "payload": {}}, `, maxAgentCallsPerTrigger+1)
	definition := `{
  "wf_schema_version": "1",
  "workflow_type": "fanout",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "asking",
  "terminal_states": ["done", "failed"],
  "catch": [{"event_match": {}, "guard": "true", "actions": [{"type": "set_variable", "name": "caught", "value": "true"}]}],
  "states": [
    {
      "name": "asking",
      "entry_actions": [` + asks + `{"type": "send_message", "target": "IO.out@motherbee", "meta": {"msg": "DONE"}, "payload": {}}],
      "transitions": [
        {"event_match": {"msg": "AGENT_CALL_CAP"}, "guard": "true", "actions": [{"type": "set_variable", "name": "capped", "value": "event.payload.target"}]}
      ]
    },
    {"name": "done"},
    {"name": "failed"}
  ]
}`
	rt, disp := newTestRuntime(t, "", definition, nil)
	instanceID := startInstance(t, rt, ioMessage("conv-fanout", "", `{}`))
	toHost := 0
	for _, msg := range disp.sent {
		if msg.Routing.Dst.Value() == "AI.host@motherbee" {
			toHost++
		}
	}
	state := mustState(t, rt, instanceID)
	if toHost != maxAgentCallsPerTrigger || sentTo(t, disp, "IO.out@motherbee").Meta.Msg == nil || state["capped"] != "AI.host@motherbee" || state["caught"] != nil {
		t.Fatalf("%d asks, state %v", toHost, state)
	}
	if row := mustGetInstance(t, rt, instanceID); row.Status != "running" {
		t.Fatalf("the instance never fails for the cap: %+v", row)
	}
}

// What starts a new count of agent calls: a timer's fire (SY.timer's, or recovery's synthetic one)
// and a message from a node that is no agent; never an agent's answer, an internal event (even one
// named TIMER_FIRED) or a control message.
func TestWhatStartsATrigger(t *testing.T) {
	named := func(sender, msgType, msgName string) sdk.Message {
		msg := sdk.Message{Meta: sdk.Meta{MsgType: msgType, Msg: strPtr(msgName)}}
		if sender != "" {
			msg = from(sender, msg)
		}
		return msg
	}
	internal, err := buildInternalEventMessage(InternalEventRow{MsgType: wfMsgType, MsgName: "TIMER_FIRED"}, "conv-1")
	if err != nil {
		t.Fatal(err)
	}
	for name, c := range map[string]struct {
		msg  sdk.Message
		want bool
	}{
		"SY.timer's TIMER_FIRED":       {realTimerFired(t, "wfi:1", "k"), true},
		"recovery's TIMER_FIRED":       {buildSyntheticTimerFired("wfi:1", "k", "wf:wfi:1::k", sdk.TimerInfo{OwnerL2Name: testNodeName}, fixedClock), true},
		"an IO node's message":         {named("IO.api.sales@worker1", "user", ""), true},
		"a WF's message":               {named("WF.upstream@motherbee", "user", "ASK"), true},
		"an agent's answer":            {named("AI.sales@worker1", "user", ""), false},
		"an agent's answer, lowercase": {named("ai.sales@worker1", "user", "ASK"), false},
		"an internal event":            {internal, false},
		"a control message":            {named(sdk.PrimaryAdminNode, sdk.SYSTEMKind, MsgWFCancelInstance), false},
	} {
		if got := startsATrigger(c.msg); got != c.want {
			t.Fatalf("%s: starts a trigger = %v, want %v", name, got, c.want)
		}
	}
}
