package node

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	wfcel "github.com/4iplatform/json-router/pkg/wfcel"
	"github.com/google/uuid"
)

// A capability's door (wf-orchestration-v1 §5.6; docs/digital-executive-v1-stage-3.md §5, §6.1): the
// ED's conscious asks it with a CAPABILITY_REQUEST, which starts an instance once identity says the
// sender is a conscious the door serves; the instance answers it once with reply, and, after a reply
// pending, gives the result later with notify.

// doorWorkflowJSON is a door of one ED, ilk:ed1, whose conscious is AI.ed1@motherbee: invoice_state
// asks its agent and replies within the turn (a second reply is refused); reconcile replies pending
// and notifies when its timer fires.
const doorWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "door",
  "description": "invoices: the state of an invoice, by its number",
  "start_from": ["AI.ed1@motherbee"],
  "request_actions": [
    {"name": "invoice_state", "description": "The state of an invoice, by its number."},
    {"name": "reconcile"}
  ],
  "input_schema": {
    "type": "object",
    "required": ["action"],
    "properties": {"action": {"type": "string", "enum": ["invoice_state", "reconcile"]}, "number": {"type": "string"}}
  },
  "initial_state": "routing",
  "terminal_states": ["answered", "failed"],
  "states": [
    {
      "name": "routing",
      "entry_actions": [{"type": "emit_internal_event", "meta": {"msg": "ROUTE"}}],
      "transitions": [
        {"event_match": {"msg": "ROUTE"}, "guard": "input.action == 'invoice_state'", "target_state": "reading"},
        {"event_match": {"msg": "ROUTE"}, "guard": "input.action == 'reconcile'", "target_state": "waiting",
         "actions": [{"type": "reply", "status": "pending", "message": "under way"}, {"type": "schedule_timer", "timer_key": "check", "fire_in": "2m"}]}
      ]
    },
    {
      "name": "reading",
      "entry_actions": [
        {"type": "send_message", "target": "AI.billing.ed1@motherbee", "meta": {"msg": "READ"}, "payload": {"type": "text", "content": "read it"}, "vars": {"number": {"$ref": "input.number"}}}
      ],
      "transitions": [
        {"event_match": {"src": "AI.billing.ed1@motherbee"}, "guard": "event.payload.content == 'missing'", "target_state": "answered",
         "actions": [{"type": "reply", "status": "error", "error_code": "NOT_FOUND", "message": "no such invoice"}]},
        {"event_match": {"src": "AI.billing.ed1@motherbee"}, "guard": "true", "target_state": "answered",
         "actions": [
           {"type": "reply", "status": "done", "result": {"number": {"$ref": "input.number"}, "state": {"$ref": "event.payload.content"}}},
           {"type": "reply", "status": "done", "message": "a second answer"}
         ]}
      ]
    },
    {
      "name": "waiting",
      "transitions": [
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'check'", "target_state": "answered",
         "actions": [{"type": "notify", "status": "done", "result": {"reconciled": true}, "message": "all paid"}]}
      ]
    },
    {"name": "answered"},
    {"name": "failed"}
  ]
}`

// doorForTheTenant is doorWorkflowJSON for all of its tenant's EDs: the consciouses marker.
func doorForTheTenant() string {
	return strings.Replace(doorWorkflowJSON, `"start_from": ["AI.ed1@motherbee"]`, `"start_from": [{"consciouses": "tenant"}]`, 1)
}

// fakeIdentity is a hive's identity as a door reads it: the ILK each node handles, and each ILK.
type fakeIdentity struct {
	nodes map[string]string
	ilks  map[string]ilkFacts
}

func (f fakeIdentity) nodeILK(node string) (string, bool, error) {
	ilk, ok := f.nodes[node]
	return ilk, ok, nil
}

func (f fakeIdentity) ilk(ilk string) (ilkFacts, bool, error) {
	facts, ok := f.ilks[ilk]
	return facts, ok, nil
}

// testIdentity is a tenant, tnt:t1, with two EDs, ilk:ed1 and ilk:ed2, each with its conscious
// (AI.ed1 and AI.ed2), the agent AI.billing.ed1 of ilk:ed1, and an ED of another tenant, ilk:ed3,
// with its conscious AI.ed3. The door, testNodeName, works for doorFor: an ED, or nobody.
func testIdentity(doorFor string) fakeIdentity {
	id := fakeIdentity{
		nodes: map[string]string{
			"AI.ed1@motherbee":         "ilk:c1",
			"AI.ed2@motherbee":         "ilk:c2",
			"AI.ed3@motherbee":         "ilk:c3",
			"AI.billing.ed1@motherbee": "ilk:a1",
			testNodeName:               "ilk:door",
		},
		ilks: map[string]ilkFacts{
			"ilk:ed1":  {Type: "human", TenantID: "tnt:t1"},
			"ilk:ed2":  {Type: "human", TenantID: "tnt:t1"},
			"ilk:ed3":  {Type: "human", TenantID: "tnt:t2"},
			"ilk:c1":   {Type: "agent", TenantID: "tnt:t1", WorksFor: "ilk:ed1", WorkRole: "conscious"},
			"ilk:c2":   {Type: "agent", TenantID: "tnt:t1", WorksFor: "ilk:ed2", WorkRole: "conscious"},
			"ilk:c3":   {Type: "agent", TenantID: "tnt:t2", WorksFor: "ilk:ed3", WorkRole: "conscious"},
			"ilk:a1":   {Type: "agent", TenantID: "tnt:t1", WorksFor: "ilk:ed1", WorkRole: "agent"},
			"ilk:door": {Type: "system", TenantID: "tnt:t1", WorksFor: doorFor},
		},
	}
	return id
}

// newDoorRuntime is a runtime of definition, whose door works for doorFor in testIdentity.
func newDoorRuntime(t *testing.T, path, definition, doorFor string, timer TimerSender) (*NodeRuntime, *mockDispatcher) {
	t.Helper()
	rt, disp := newTestRuntime(t, path, definition, timer)
	rt.Identity = testIdentity(doorFor)
	return rt, disp
}

// personConversation is the conversation a request carries, from the conscious's turn.
const personConversation = `{"thread_id":"conv-person-1","person_ilk":"ilk:person","io_node":"IO.wapp.ed1@motherbee","reply_target":{"kind":"wapp","address":"+5491100000000"}}`

// capabilityRequest is a CAPABILITY_REQUEST from sender for ed, as the router delivers it: a system
// message, on its own trace, with no thread.
func capabilityRequestFrom(sender, requestID, ed, input string) sdk.Message {
	name := MsgCapabilityRequest
	payload := fmt.Sprintf(`{"request_id":%q,"ed_ilk":%q,"input":%s,"conversation":%s}`, requestID, ed, input, personConversation)
	return from(sender, sdk.Message{
		Routing: sdk.Routing{Src: "conscious-uuid", TTL: 16, TraceID: "trace-" + requestID},
		Meta:    sdk.Meta{MsgType: sdk.SYSTEMKind, Msg: &name},
		Payload: json.RawMessage(payload),
	})
}

// requestAnswer is msg's payload when msg is the response to the request requestID: a
// CAPABILITY_REQUEST_RESPONSE, a system message to its sender, on its trace.
func requestAnswer(t *testing.T, msg sdk.Message, requestID string) map[string]any {
	t.Helper()
	if msg.Meta.MsgType != sdk.SYSTEMKind || derefString(msg.Meta.Msg) != MsgCapabilityRequestResponse ||
		msg.Routing.Dst.Value() != "conscious-uuid" || msg.Routing.TraceID != "trace-"+requestID {
		t.Fatalf("a %s to the request's sender on its trace: %+v %+v", MsgCapabilityRequestResponse, msg.Routing, msg.Meta)
	}
	var payload map[string]any
	if err := json.Unmarshal(msg.Payload, &payload); err != nil {
		t.Fatal(err)
	}
	if payload["request_id"] != requestID {
		t.Fatalf("the answer names its request: %v", payload)
	}
	return payload
}

// dispatchOK dispatches msg on rt.
func dispatchOK(t *testing.T, rt *NodeRuntime, msg sdk.Message) {
	t.Helper()
	if err := Dispatch(context.Background(), msg, rt); err != nil {
		t.Fatalf("Dispatch %s: %v", rawMsgName(msg), err)
	}
}

// The ED's conscious starts an instance with its request: its input the request's, its thread its
// own id (one instance per request), the request kept with it. The door asks its agent on that
// thread, naming no person, and replies, once, to the conscious, as the response to its request,
// whatever the trace of the event the reply runs on: a second reply is refused and logged, and the
// instance goes on.
func TestADoorTakesARequestFromItsConscious(t *testing.T) {
	rt, disp := newDoorRuntime(t, "", doorWorkflowJSON, "ilk:ed1", nil)
	instanceID := startInstance(t, rt, capabilityRequestFrom("AI.ed1@motherbee", "r-1", "ilk:ed1", `{"action":"invoice_state","number":"A-1032"}`))
	row := mustGetInstance(t, rt, instanceID)
	if row.ThreadID != instanceID || row.InputJSON != `{"action":"invoice_state","number":"A-1032"}` || row.CurrentState != "reading" ||
		row.PersonILK != "" || row.PersonIO != "" {
		t.Fatalf("the instance: %+v", row)
	}
	var kept capabilityRequest
	if err := json.Unmarshal([]byte(row.RequestJSON), &kept); err != nil || kept.RequestID != "r-1" || kept.EDILK != "ilk:ed1" ||
		kept.Sender != "AI.ed1@motherbee" || kept.SenderUUID != "conscious-uuid" || kept.TraceID != "trace-r-1" ||
		string(kept.Conversation) != personConversation || kept.Replied != "" {
		t.Fatalf("the instance keeps the request: %s %v", row.RequestJSON, err)
	}
	asked := sentTo(t, disp, "AI.billing.ed1@motherbee")
	if derefString(asked.Meta.ThreadID) != instanceID || asked.Meta.SrcIlk != nil || asked.Routing.TraceID != "trace-r-1" || len(disp.sent) != 1 {
		t.Fatalf("the agent is asked on the instance's thread: %+v %+v", asked.Routing, asked.Meta)
	}

	answer := agentAnswer(t, "AI.billing.ed1@motherbee", asked, `{"type":"text","content":"paid"}`, nil)
	answer.Routing.TraceID = "trace-agent"
	dispatchOK(t, rt, answer)
	if len(disp.sent) != 2 {
		t.Fatalf("one reply goes: sent %d", len(disp.sent)-1)
	}
	reply := requestAnswer(t, disp.sent[1], "r-1")
	if reply["status"] != "done" || fmt.Sprint(reply["result"]) != "map[number:A-1032 state:paid]" || len(reply) != 3 {
		t.Fatalf("the reply: %v", reply)
	}
	row = mustGetInstance(t, rt, instanceID)
	if row.Status != "completed" || row.CurrentState != "answered" || !strings.Contains(row.RequestJSON, `"replied":"done"`) {
		t.Fatalf("the instance goes on, its request answered: %+v", row)
	}
	logs, _ := rt.Store.GetRecentLog(context.Background(), instanceID, 10)
	refused := false
	for _, entry := range logs {
		if entry.ActionType == "reply" && !entry.OK && strings.Contains(entry.ErrorDetail, "reply refused: request r-1 was answered already (done)") {
			refused = true
		}
	}
	if !refused {
		t.Fatalf("the second reply is refused, and the log says so: %+v", logs)
	}

	// An error answer goes the same way.
	instanceID = startInstance(t, rt, capabilityRequestFrom("AI.ed1@motherbee", "r-2", "ilk:ed1", `{"action":"invoice_state","number":"A-9"}`))
	dispatchOK(t, rt, agentAnswer(t, "AI.billing.ed1@motherbee", sentTo(t, disp, "AI.billing.ed1@motherbee"), `{"type":"text","content":"missing"}`, nil))
	if reply := requestAnswer(t, disp.sent[len(disp.sent)-1], "r-2"); reply["status"] != "error" || reply["error_code"] != "NOT_FOUND" || reply["message"] != "no such invoice" {
		t.Fatalf("an error reply: %v", reply)
	}
	if row := mustGetInstance(t, rt, instanceID); row.Status != "completed" {
		t.Fatalf("the instance: %+v", row)
	}
}

// A request from anyone but a conscious the door serves starts nothing, and is answered, status
// error, on its trace: UNAUTHORIZED for a sender start_from does not take or identity does not
// confirm, INVALID_REQUEST for a payload that is no request, INVALID_INPUT for an input the
// input_schema refuses. A message of the conscious that is no request starts nothing either, and
// gets no answer.
func TestADoorRefusesWhatIsNoRequestOfItsConscious(t *testing.T) {
	type refusal struct {
		name    string
		doorFor string
		msg     sdk.Message
		code    string
		message string
	}
	invoice := `{"action":"invoice_state","number":"A-1"}`
	noRequestID := capabilityRequestFrom("AI.ed1@motherbee", "", "ilk:ed1", invoice)
	notAnObject := capabilityRequestFrom("AI.ed1@motherbee", "r-x", "ilk:ed1", invoice)
	notAnObject.Payload = json.RawMessage(`["r-x"]`)
	for _, c := range []refusal{
		{"an IO node", "ilk:ed1", capabilityRequestFrom("IO.api@motherbee", "r-1", "ilk:ed1", invoice), "UNAUTHORIZED", "takes capability requests only from the consciouses its start_from takes, not from \"IO.api@motherbee\""},
		{"a conscious start_from does not name", "ilk:ed1", capabilityRequestFrom("AI.ed2@motherbee", "r-1", "ilk:ed2", invoice), "UNAUTHORIZED", "not from \"AI.ed2@motherbee\""},
		{"another ED's request", "ilk:ed1", capabilityRequestFrom("AI.ed1@motherbee", "r-1", "ilk:ed2", invoice), "UNAUTHORIZED", "AI.ed1@motherbee is the conscious of ilk:ed1, not of ilk:ed2"},
		{"a door of another ED", "ilk:ed2", capabilityRequestFrom("AI.ed1@motherbee", "r-1", "ilk:ed1", invoice), "UNAUTHORIZED", "this capability is ilk:ed2's, not ilk:ed1's"},
		{"no request_id", "ilk:ed1", noRequestID, "INVALID_REQUEST", "request_id is required"},
		{"no request", "ilk:ed1", notAnObject, "INVALID_REQUEST", "the payload is no CAPABILITY_REQUEST"},
		{"an input the schema refuses", "ilk:ed1", capabilityRequestFrom("AI.ed1@motherbee", "r-1", "ilk:ed1", `{"action":"refund"}`), "INVALID_INPUT", "action"},
		{"an input that is no object", "ilk:ed1", capabilityRequestFrom("AI.ed1@motherbee", "r-1", "ilk:ed1", `"A-1"`), "INVALID_INPUT", "input is not a JSON object"},
	} {
		t.Run(c.name, func(t *testing.T) {
			rt, disp := newDoorRuntime(t, "", doorWorkflowJSON, c.doorFor, nil)
			dispatchOK(t, rt, c.msg)
			if rt.Registry.Count() != 0 || len(disp.sent) != 1 {
				t.Fatalf("no instance, one answer: %d instances, %d sent", rt.Registry.Count(), len(disp.sent))
			}
			answer := disp.sent[0]
			var payload map[string]any
			_ = json.Unmarshal(answer.Payload, &payload)
			if derefString(answer.Meta.Msg) != MsgCapabilityRequestResponse || answer.Routing.TraceID != c.msg.Routing.TraceID ||
				answer.Routing.Dst.Value() != "conscious-uuid" || payload["status"] != "error" || payload["error_code"] != c.code ||
				!strings.Contains(fmt.Sprint(payload["message"]), c.message) {
				t.Fatalf("the answer: %+v %v", answer.Routing, payload)
			}
		})
	}

	// Identity decides who a conscious is: a node start_from names whose ILK is none, or is no
	// conscious, is refused.
	for name, node := range map[string]string{"no ILK": "", "no conscious": "ilk:a1"} {
		rt, disp := newDoorRuntime(t, "", doorWorkflowJSON, "ilk:ed1", nil)
		id := rt.Identity.(fakeIdentity)
		if node == "" {
			delete(id.nodes, "AI.ed1@motherbee")
		} else {
			id.nodes["AI.ed1@motherbee"] = node
		}
		dispatchOK(t, rt, capabilityRequestFrom("AI.ed1@motherbee", "r-1", "ilk:ed1", invoice))
		want := map[string]string{"no ILK": "AI.ed1@motherbee has no ILK in identity", "no conscious": "AI.ed1@motherbee is no ED's conscious in identity"}[name]
		if reply := requestAnswer(t, disp.sent[0], "r-1"); rt.Registry.Count() != 0 || reply["error_code"] != "UNAUTHORIZED" || reply["message"] != want {
			t.Fatalf("%s: %v", name, reply)
		}
	}

	// The conscious's own user message, an answer above all, starts nothing and drives nothing.
	rt, disp := newDoorRuntime(t, "", doorWorkflowJSON, "ilk:ed1", nil)
	instanceID := startInstance(t, rt, capabilityRequestFrom("AI.ed1@motherbee", "r-1", "ilk:ed1", invoice))
	sent := len(disp.sent)
	for _, thread := range []string{"conv-person-1", instanceID} {
		dispatchOK(t, rt, onThread(thread, from("AI.ed1@motherbee", msgWithName("READ"))))
	}
	if rt.Registry.Count() != 1 || len(disp.sent) != sent || mustGetInstance(t, rt, instanceID).CurrentState != "reading" {
		t.Fatalf("the conscious's message is dropped: %d instances, sent %d", rt.Registry.Count(), len(disp.sent)-sent)
	}
}

// A door for all of its tenant's EDs takes the request of any ED of its tenant's conscious, checked
// in identity, and no other: an ED of another tenant's, an agent that is no conscious. It never sends
// a message to the conscious that asked, which the language cannot see: only reply and notify reach
// it.
func TestADoorForItsTenantsEDs(t *testing.T) {
	rt, disp := newDoorRuntime(t, "", doorForTheTenant(), "", nil)
	invoice := `{"action":"invoice_state","number":"A-1"}`
	for _, conscious := range []string{"AI.ed1@motherbee", "AI.ed2@motherbee"} {
		ed := "ilk:" + strings.TrimSuffix(strings.TrimPrefix(conscious, "AI."), "@motherbee")
		startInstance(t, rt, capabilityRequestFrom(conscious, "r-"+ed, ed, invoice))
	}
	if rt.Registry.Count() != 2 {
		t.Fatalf("both EDs of the tenant: %d", rt.Registry.Count())
	}
	for _, c := range []struct {
		request sdk.Message
		want    string
	}{
		{capabilityRequestFrom("AI.ed3@motherbee", "r-3", "ilk:ed3", invoice), "ilk:ed3 is not of this capability's tenant (tnt:t1)"},
		{capabilityRequestFrom("AI.billing.ed1@motherbee", "r-4", "ilk:ed1", invoice), "AI.billing.ed1@motherbee is no ED's conscious in identity"},
	} {
		request, want := c.request, c.want
		sent := len(disp.sent)
		dispatchOK(t, rt, request)
		if len(disp.sent) != sent+1 || rt.Registry.Count() != 2 {
			t.Fatalf("%s: refused, with no instance", want)
		}
		_, payload := lastSent(t, disp)
		if payload["error_code"] != "UNAUTHORIZED" || payload["message"] != want {
			t.Fatalf("the refusal: %v", payload)
		}
	}
	// An ED that is no person is refused too.
	id := rt.Identity.(fakeIdentity)
	id.ilks["ilk:ed2"] = ilkFacts{Type: "agent", TenantID: "tnt:t1"}
	dispatchOK(t, rt, capabilityRequestFrom("AI.ed2@motherbee", "r-5", "ilk:ed2", invoice))
	if _, payload := lastSent(t, disp); payload["message"] != "ilk:ed2 is no person" {
		t.Fatalf("an ED that is no person: %v", payload)
	}

	// A send to the conscious that asked fails the action, whatever start_from names.
	toTheConscious := strings.Replace(doorForTheTenant(), `"target": "AI.billing.ed1@motherbee"`, `"target": "AI.ed1@motherbee"`, 1)
	rt, disp = newDoorRuntime(t, "", toTheConscious, "", nil)
	dispatchOK(t, rt, capabilityRequestFrom("AI.ed1@motherbee", "r-6", "ilk:ed1", invoice))
	instances, _ := rt.Store.ListInstances(context.Background(), "failed", 1, 0)
	if len(instances) != 1 || len(disp.sent) != 0 ||
		!strings.Contains(lastLog(t, rt, instances[0].InstanceID).ErrorDetail, "send_message: AI.ed1@motherbee is the conscious whose request started the instance: only reply and notify reach it") {
		t.Fatalf("the send is refused, and the instance fails: %+v, sent %d", instances, len(disp.sent))
	}
}

// A request that takes long (docs/digital-executive-v1-stage-3.md §6.1): the door replies pending at
// once, its timer wakes the instance, across a restart, and notify gives the conscious the result: a
// user message to it alone, on the person's conversation, its meta.context.capability_result naming
// the request, the door, the result and the conversation as the request carried it, and a text that
// renders it for the model.
func TestAReplyPendingThenATimerThenANotify(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	rt, disp := newDoorRuntime(t, path, doorWorkflowJSON, "ilk:ed1", nil)
	instanceID := startInstance(t, rt, capabilityRequestFrom("AI.ed1@motherbee", "r-1", "ilk:ed1", `{"action":"reconcile"}`))
	if len(disp.sent) != 1 {
		t.Fatalf("the pending reply goes at once: sent %d", len(disp.sent))
	}
	if reply := requestAnswer(t, disp.sent[0], "r-1"); reply["status"] != "pending" || reply["message"] != "under way" || len(reply) != 3 {
		t.Fatalf("the pending reply: %v", reply)
	}

	rt, disp = newDoorRuntime(t, path, doorWorkflowJSON, "ilk:ed1", &mockTimerLister{})
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	dispatchOK(t, rt, realTimerFired(t, rt, instanceID, "check"))
	if len(disp.sent) != 1 {
		t.Fatalf("one notify: sent %d", len(disp.sent))
	}
	notice := disp.sent[0]
	if notice.Meta.MsgType != "user" || notice.Routing.Dst.Value() != "AI.ed1@motherbee" || derefString(notice.Meta.ThreadID) != "conv-person-1" ||
		notice.Meta.Msg != nil || notice.Meta.SrcIlk != nil || notice.Meta.DstIlk != nil {
		t.Fatalf("a user message to the conscious, on the person's thread: %+v %+v", notice.Routing, notice.Meta)
	}
	const result = `{"capability_result":{"capability":"WF.invoice@motherbee","conversation":` + personConversation +
		`,"message":"all paid","request_id":"r-1","result":{"reconciled":true},"status":"done"}}`
	if got := contextWithoutTrace(t, notice); got != result {
		t.Fatalf("its context:\n%s\nwant\n%s", got, result)
	}
	if writer, ok := sdk.WriterOf(notice.Meta.Context); !ok || writer.Name != testNodeName {
		t.Fatalf("the WF wrote it: %+v", writer)
	}
	var text map[string]string
	if err := json.Unmarshal(notice.Payload, &text); err != nil || text["type"] != "text" ||
		text["content"] != "The capability WF.invoice@motherbee completed your request r-1. all paid\nResult: {\"reconciled\":true}" {
		t.Fatalf("its text: %s %v", notice.Payload, err)
	}
	if row := mustGetInstance(t, rt, instanceID); row.Status != "completed" {
		t.Fatalf("the instance: %+v", row)
	}
}

// reply and notify answer only a request: an instance a request did not start has none, and a notify
// comes after a reply pending, never before one, nor after a reply done.
func TestReplyAndNotifyNeedTheirRequest(t *testing.T) {
	disp := &mockDispatcher{l2name: testNodeName}
	actx := makeActx(t, disp, &mockTimerSender{})
	def, err := LoadDefinitionBytes([]byte(doorWorkflowJSON), "", fixedClock)
	if err != nil {
		t.Fatal(err)
	}
	notify := ActionDefinition{Type: "notify", Status: "done"}
	reply := ActionDefinition{Type: "reply", Status: "done"}
	inst := NewInstance(def, "wfi:1", "wfi:1", def.WorkflowType, map[string]any{}, fixedClock)
	for _, action := range []ActionDefinition{reply, notify} {
		if err := executeAction(context.Background(), "a", action, inst, msgWithName("X"), actx, nil); err == nil ||
			!strings.Contains(err.Error(), action.Type+": the instance was not started by a capability request") {
			t.Fatalf("%s without a request: %v", action.Type, err)
		}
	}
	inst.request = &capabilityRequest{RequestID: "r-1", Sender: "AI.ed1@motherbee", SenderUUID: "conscious-uuid", TraceID: "trace-r-1", Conversation: json.RawMessage(personConversation)}
	for _, replied := range []string{"", "done"} {
		inst.request.Replied = replied
		if err := executeAction(context.Background(), "a", notify, inst, msgWithName("X"), actx, nil); err == nil ||
			!strings.Contains(err.Error(), "notify: a notify gives the result after a reply pending, and request r-1 was "+repliedWords(replied)) {
			t.Fatalf("a notify after %q: %v", replied, err)
		}
	}
	inst.request.Replied, inst.request.Conversation = wfcel.AnswerPending, json.RawMessage(`{"person_ilk":"ilk:person"}`)
	if err := executeAction(context.Background(), "a", notify, inst, msgWithName("X"), actx, nil); err == nil ||
		!strings.Contains(err.Error(), "request r-1 carried no conversation.thread_id") {
		t.Fatalf("a notify with no thread to go on: %v", err)
	}
	if len(disp.sent) != 0 {
		t.Fatalf("nothing went: %d", len(disp.sent))
	}
}

// WF_HELP is the capability's interface: a door answers it to the consciouses its start_from takes,
// checked in identity as a request is, with its request actions; any other agent is refused, and so
// is every other control verb.
func TestWFHelpToAConscious(t *testing.T) {
	help := func(rt *NodeRuntime, disp *mockDispatcher, sender, verb string) map[string]any {
		t.Helper()
		name := verb
		dispatchOK(t, rt, from(sender, sdk.Message{Routing: sdk.Routing{Src: "conscious-uuid", TraceID: "trace-help"}, Meta: sdk.Meta{MsgType: sdk.SYSTEMKind, Msg: &name}, Payload: json.RawMessage(`{}`)}))
		_, payload := lastSent(t, disp)
		return payload
	}
	rt, disp := newDoorRuntime(t, "", doorWorkflowJSON, "ilk:ed1", nil)
	answer := help(rt, disp, "AI.ed1@motherbee", MsgWFHelp)
	actions, _ := json.Marshal(answer["actions"])
	if answer["ok"] != true || answer["description"] != "invoices: the state of an invoice, by its number" ||
		string(actions) != `[{"description":"The state of an invoice, by its number.","name":"invoice_state"},{"name":"reconcile"}]` || answer["input_schema"] == nil {
		t.Fatalf("its help, with its actions: %v", answer)
	}
	for sender, verb := range map[string]string{"AI.ed2@motherbee": MsgWFHelp, "AI.billing.ed1@motherbee": MsgWFHelp, "AI.ed1@motherbee": MsgWFListInstances} {
		if refused := help(rt, disp, sender, verb); refused["ok"] != false || refused["error"] != "UNAUTHORIZED" {
			t.Fatalf("%s %s: %v", sender, verb, refused)
		}
	}
	if refused := help(rt, disp, "AI.ed2@motherbee", MsgWFHelp); !strings.Contains(fmt.Sprint(refused["detail"]), "the consciouses a capability's start_from takes") {
		t.Fatalf("the refusal says who is answered: %v", refused)
	}

	rt, disp = newDoorRuntime(t, "", doorForTheTenant(), "", nil)
	if answer := help(rt, disp, "AI.ed2@motherbee", MsgWFHelp); answer["ok"] != true {
		t.Fatalf("a door for the tenant's EDs answers an ED of its tenant's conscious: %v", answer)
	}
	if refused := help(rt, disp, "AI.ed3@motherbee", MsgWFHelp); refused["error"] != "UNAUTHORIZED" {
		t.Fatalf("and refuses another tenant's: %v", refused)
	}
}

// A request journaled before a crash starts its instance at the next start, with its own id as its
// thread and the request kept, once the current definition admits it again; one identity no longer
// confirms is dropped, and answered.
func TestARequestJournaledBeforeACrashIsTakenAgain(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	rt, _ := newDoorRuntime(t, path, doorWorkflowJSON, "ilk:ed1", nil)
	invoice := `{"action":"invoice_state","number":"A-1"}`
	for id, request := range map[string]sdk.Message{
		"wfi:taken":   capabilityRequestFrom("AI.ed1@motherbee", "r-1", "ilk:ed1", invoice),
		"wfi:refused": capabilityRequestFrom("AI.ed1@motherbee", "r-2", "ilk:ed2", invoice),
	} {
		if _, err := rt.journal(context.Background(), id, inboundCreate, request, ""); err != nil {
			t.Fatal(err)
		}
	}
	rt, disp := newDoorRuntime(t, path, doorWorkflowJSON, "ilk:ed1", &mockTimerLister{})
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	row := mustGetInstance(t, rt, "wfi:taken")
	if row.ThreadID != "wfi:taken" || row.CurrentState != "reading" || !strings.Contains(row.RequestJSON, `"request_id":"r-1"`) {
		t.Fatalf("the request's instance: %+v", row)
	}
	if asked := sentTo(t, disp, "AI.billing.ed1@motherbee"); derefString(asked.Meta.ThreadID) != "wfi:taken" {
		t.Fatalf("its agent is asked on its thread: %+v", asked.Meta)
	}
	if _, err := rt.Store.GetInstance(context.Background(), "wfi:refused"); err == nil {
		t.Fatal("the refused request starts no instance")
	}
	refused := false
	for _, msg := range disp.sent {
		if derefString(msg.Meta.Msg) == MsgCapabilityRequestResponse {
			refused = requestAnswer(t, msg, "r-2")["error_code"] == "UNAUTHORIZED"
		}
	}
	if journaled, _ := rt.Store.ListInboundEvents(context.Background()); !refused || len(journaled) != 0 {
		t.Fatalf("the refused one is answered and dropped: %v %+v", refused, journaled)
	}
}

// The shipped example door (go/nodes/wf/examples/packages/wf.capability.billing) on the engine: an
// invoice's state, answered within the turn from the agent's evaluation, and a reconciliation,
// answered pending and given by notify when the agent answers.
func TestTheExampleDoor(t *testing.T) {
	definition := exampleDefinition(t, "wf.capability.billing")
	rt, disp := newTestRuntime(t, "", definition, nil)
	id := testIdentity("ilk:ed1")
	id.nodes["AI.ed4qm7k2xa@worker1"] = "ilk:c1"
	rt.Identity = id
	const agent = "AI.billing.ed4qm7k2xa@worker1"

	startInstance(t, rt, capabilityRequestFrom("AI.ed4qm7k2xa@worker1", "r-1", "ilk:ed1", `{"action":"invoice_state","number":"A-1032"}`))
	asked := sentTo(t, disp, agent)
	dispatchOK(t, rt, agentAnswer(t, agent, asked, `{"type":"text","content":"It is paid."}`,
		map[string]any{"reply": "It is paid.", "option": "found", "params": map[string]any{"state": "paid", "since": "2026-11-12"}}))
	if reply := requestAnswer(t, disp.sent[len(disp.sent)-1], "r-1"); reply["status"] != "done" ||
		fmt.Sprint(reply["result"]) != "map[number:A-1032 since:2026-11-12 state:paid]" {
		t.Fatalf("the invoice's state: %v", reply)
	}

	startInstance(t, rt, capabilityRequestFrom("AI.ed4qm7k2xa@worker1", "r-2", "ilk:ed1", `{"action":"reconcile","customer_id":"c-7"}`))
	pending := disp.sent[len(disp.sent)-2]
	if reply := requestAnswer(t, pending, "r-2"); reply["status"] != "pending" {
		t.Fatalf("the reconciliation is under way: %v", reply)
	}
	dispatchOK(t, rt, agentAnswer(t, agent, sentTo(t, disp, agent), `{"type":"text","content":"Reconciled."}`,
		map[string]any{"reply": "Reconciled.", "option": "reconciled", "params": map[string]any{"open_invoices": 1, "summary": "One invoice is unpaid."}}))
	notice := disp.sent[len(disp.sent)-1]
	if notice.Routing.Dst.Value() != "AI.ed4qm7k2xa@worker1" || derefString(notice.Meta.ThreadID) != "conv-person-1" ||
		!strings.Contains(string(notice.Meta.Context), `"result":{"customer_id":"c-7","open_invoices":1,"summary":"One invoice is unpaid."},"status":"done"`) {
		t.Fatalf("the reconciliation's result: %+v %s", notice.Routing, notice.Meta.Context)
	}

	// A request without its action's field is refused within the turn.
	dispatchOK(t, rt, capabilityRequestFrom("AI.ed4qm7k2xa@worker1", "r-3", "ilk:ed1", `{"action":"reconcile"}`))
	if reply := requestAnswer(t, disp.sent[len(disp.sent)-1], "r-3"); reply["error_code"] != "MISSING_FIELD" {
		t.Fatalf("a request without its field: %v", reply)
	}
}

// The tenant of an ILK is read from the identity SHM's IlkEntry, right before its display name
// (src/shm/mod.rs, and the layout fixture the Go SDK shares with Rust), under its seqlock.
func TestAnILKsTenantIsReadFromTheIdentitySHM(t *testing.T) {
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "..", "fluxbee-go-sdk", "testdata", "identity_layout", "ilk_entry.json"))
	if err != nil {
		t.Fatal(err)
	}
	var layout struct {
		Version int            `json:"version"`
		Size    int            `json:"ilk_entry_size"`
		Offsets map[string]int `json:"offsets"`
	}
	if err := json.Unmarshal(raw, &layout); err != nil {
		t.Fatal(err)
	}
	if layout.Version != int(shmIdentityVersion) || layout.Size != shmIlkEntrySize || layout.Offsets["flags"] != shmIlkOffFlags ||
		layout.Offsets["display_name"] != shmIlkOffTenantID+16 {
		t.Fatalf("the layout the tenant is read with is the shared one: %+v", layout)
	}

	const maxTenants, maxIlks = 2, 3
	offset := alignUp(alignUp(shmIdentityHeaderSize, shmIdentityRegionAlign)+shmTenantEntrySize*maxTenants, shmIdentityRegionAlign)
	region := make([]byte, offset+maxIlks*shmIlkEntrySize)
	le := func(at int, value uint32) {
		region[at], region[at+1], region[at+2], region[at+3] = byte(value), byte(value>>8), byte(value>>16), byte(value>>24)
	}
	le(0, shmIdentityMagic)
	le(shmHeaderVersion, shmIdentityVersion)
	le(shmHeaderIlkCount, 2)
	le(shmHeaderMaxIlks, maxIlks)
	le(shmHeaderMaxTenants, maxTenants)
	entry := func(i int, ilk, tenant string, active bool) {
		at := offset + i*shmIlkEntrySize
		copy(region[at:], uuidBytes(t, ilk))
		copy(region[at+shmIlkOffTenantID:], uuidBytes(t, tenant))
		if active {
			region[at+shmIlkOffFlags] = byte(shmIlkFlagActive)
		}
	}
	entry(0, "11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222", true)
	entry(1, "33333333-3333-4333-8333-333333333333", "22222222-2222-4222-8222-222222222222", false)
	path := filepath.Join(t.TempDir(), "jsr-identity-test")
	if err := os.WriteFile(path, region, 0o600); err != nil {
		t.Fatal(err)
	}
	if tenant, found, err := ilkTenantAt(path, "ilk:11111111-1111-4111-8111-111111111111"); err != nil || !found || tenant != "tnt:22222222-2222-4222-8222-222222222222" {
		t.Fatalf("an active ILK's tenant: %q %v %v", tenant, found, err)
	}
	for _, ilk := range []string{"ilk:33333333-3333-4333-8333-333333333333", "ilk:44444444-4444-4444-8444-444444444444", "not an ilk"} {
		if _, found, err := ilkTenantAt(path, ilk); found || err != nil {
			t.Fatalf("%s: found %v, %v", ilk, found, err)
		}
	}
	if _, found, err := ilkTenantAt(filepath.Join(t.TempDir(), "absent"), "ilk:11111111-1111-4111-8111-111111111111"); found || err != nil {
		t.Fatalf("no region: %v %v", found, err)
	}
}

func uuidBytes(t *testing.T, id string) []byte {
	t.Helper()
	parsed := uuid.MustParse(id)
	return parsed[:]
}
