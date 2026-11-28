package wfcel

import (
	"encoding/json"
	"strings"
	"testing"
)

// A capability's door (wf-orchestration-v1 §5.6): its start_from names the ED's conscious or takes
// the consciouses of its tenant's EDs, it answers with reply and notify, describes its requests in
// request_actions, and never sends a message to a conscious its start_from names (A-118's loop).

// doorJSON is a door of one ED: AI.ed4qm7k2xa@worker1 asks it, and its agent answers.
func doorJSON() string {
	return `{
  "wf_schema_version": "1",
  "workflow_type": "door",
  "description": "invoices",
  "start_from": ["AI.ed4qm7k2xa@worker1"],
  "request_actions": [{"name": "invoice_state", "description": "The state of an invoice."}],
  "input_schema": {"type": "object"},
  "initial_state": "asking",
  "terminal_states": ["done", "failed"],
  "states": [
    {
      "name": "asking",
      "entry_actions": [
        {"type": "send_message", "target": "AI.billing.ed4qm7k2xa@worker1", "meta": {"msg": "ASK"}, "payload": {"type": "text", "content": "x"}}
      ],
      "transitions": [
        {"event_match": {"src": "AI.billing.ed4qm7k2xa@worker1"}, "guard": "true", "target_state": "done",
         "actions": [{"type": "reply", "status": "done", "result": {"$ref": "event.payload"}}]}
      ]
    },
    {"name": "done"},
    {"name": "failed"}
  ]
}`
}

func loadDoor(t *testing.T, from, to string) (*WorkflowDefinition, error) {
	t.Helper()
	data := strings.Replace(doorJSON(), from, to, 1)
	if from != "" && data == doorJSON() {
		t.Fatalf("the fixture did not change: %s", from)
	}
	return LoadDefinitionBytes([]byte(data), "", fixedClock)
}

// start_from takes a node name or the consciouses marker, {"consciouses": "tenant"}, written once.
func TestADoorsStartFrom(t *testing.T) {
	def, err := loadDoor(t, "", "")
	if err != nil {
		t.Fatalf("a door of one ED: %v", err)
	}
	if !def.TakesRequests() || !def.TakesRequestsFrom("AI.ed4qm7k2xa@worker1") || def.TakesRequestsFrom("AI.other@worker1") ||
		def.StartsFrom("AI.ed4qm7k2xa@worker1") || def.Involves("AI.ed4qm7k2xa@worker1") {
		t.Fatalf("its conscious starts an instance with a request only, and drives none with a message: %+v", def.StartFrom)
	}
	marker, err := loadDoor(t, `"start_from": ["AI.ed4qm7k2xa@worker1"]`, `"start_from": [{"consciouses": "tenant"}]`)
	if err != nil {
		t.Fatalf("a door for its tenant's EDs: %v", err)
	}
	if !marker.TakesRequestsFrom("AI.ed2abcdefg@worker1") || !marker.TakesRequestsFrom("AI.ed5zzzzzzz@motherbee") ||
		marker.TakesRequestsFrom("IO.api@worker1") || marker.StartsFrom("AI.ed2abcdefg@worker1") || marker.Involves("AI.ed2abcdefg@worker1") {
		t.Fatalf("the marker takes any agent's request, identity deciding, and no message: %+v", marker.StartFrom)
	}
	if out, _ := json.Marshal(marker.StartFrom); string(out) != `[{"consciouses":"tenant"}]` {
		t.Fatalf("an entry is written back as it was written: %s", out)
	}
	if _, err := loadDoor(t, `"start_from": ["AI.ed4qm7k2xa@worker1"]`, `"start_from": ["IO.api.sales@worker1", {"consciouses": "tenant"}]`); err != nil {
		t.Fatalf("a node and the marker: %v", err)
	}
	for to, want := range map[string]string{
		`"start_from": [{"consciouses": "tenant"}, {"consciouses": "tenant"}]`: "start_from[1]: the consciouses marker is written once",
		`"start_from": [{"consciouses": "ed"}]`:                                `start_from[0].consciouses: must be "tenant"`,
		`"start_from": [{"consciouses": ""}]`:                                  `start_from[0].consciouses: must be "tenant"`,
		`"start_from": [{"consciouses": "tenant", "names": ["x"]}]`:            "start_from[0].names: is not a field of the consciouses marker",
		`"start_from": [{}]`:                                                   "start_from[0]: an entry is a node name with its hive",
		`"start_from": [7]`:                                                    "start_from[0]: an entry is a node name with its hive",
		`"start_from": [{"Consciouses": "tenant"}]`:                            "start_from[0].consciouses: is written Consciouses",
		`"start_from": ["AI.ed4qm7k2xa"]`:                                      "start_from[0]: must be a node name with its hive",
	} {
		_, err := loadDoor(t, `"start_from": ["AI.ed4qm7k2xa@worker1"]`, to)
		assertErrorContains(t, err, want)
	}
}

// The guard (A-118): a door never sends a message to a conscious its start_from names, whatever its
// capitals; only reply and notify reach it. Its other agents it asks as any WF does.
func TestADoorNeverSendsToTheConsciousItsStartFromNames(t *testing.T) {
	send := `{"type": "send_message", "target": "AI.billing.ed4qm7k2xa@worker1", "meta": {"msg": "ASK"}, "payload": {"type": "text", "content": "x"}}`
	for to, want := range map[string]string{
		`{"type": "send_message", "target": "AI.ed4qm7k2xa@worker1", "meta": {"msg": "ASK"}, "payload": {"type": "text", "content": "x"}}`: "states[0].entry_actions[0].target: is a conscious whose requests start this WF (start_from): only reply and notify reach it",
		`{"type": "send_message", "target": "ai.ED4QM7K2XA@worker1", "meta": {"msg": "ASK"}, "payload": {"type": "text", "content": "x"}}`: "states[0].entry_actions[0].target: is a conscious whose requests start this WF",
		`{"type": "forward", "target": "AI.ed4qm7k2xa@worker1", "message": "kept"}, {"type": "keep_message", "name": "kept"}`:              "states[0].entry_actions[0].target: is a conscious whose requests start this WF",
	} {
		_, err := loadDoor(t, send, to)
		assertErrorContains(t, err, want)
	}
	// In a transition and in the catch too.
	data := strings.Replace(doorJSON(), `{"type": "reply", "status": "done", "result": {"$ref": "event.payload"}}`,
		`{"type": "forward", "target": "AI.ed4qm7k2xa@worker1"}`, 1)
	data = strings.Replace(data, `"states": [`,
		`"catch": [{"event_match": {}, "guard": "true", "actions": [{"type": "send_message", "target": "AI.ed4qm7k2xa@worker1", "meta": {"msg": "X"}}]}],
  "states": [`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].transitions[0].actions[0].target: is a conscious whose requests start this WF")
	assertErrorContains(t, err, "catch[0].actions[0].target: is a conscious whose requests start this WF")
}

// reply and notify: a door's alone, each with its status (done, pending or error for a reply; done
// or error for a notify), with status error its error_code and with no other, an optional message,
// and a result under the $ref rules; neither has a target or a payload.
func TestReplyAndNotify(t *testing.T) {
	reply := `{"type": "reply", "status": "done", "result": {"$ref": "event.payload"}}`
	for _, ok := range []string{
		`{"type": "reply", "status": "pending", "message": "under way"}`,
		`{"type": "reply", "status": "error", "error_code": "NOT_FOUND", "message": "no such invoice"}`,
		`{"type": "reply", "status": "done"}`,
		`{"type": "notify", "status": "done", "result": {"state": {"$ref": "event.payload.state", "default": "unknown"}}}`,
		`{"type": "notify", "status": "error", "error_code": "crm.timeout"}`,
	} {
		if _, err := loadDoor(t, reply, ok); err != nil {
			t.Fatalf("%s: %v", ok, err)
		}
	}
	for to, want := range map[string]string{
		`{"type": "reply"}`:                                                                                           "actions[0].status: is required: done, pending, error",
		`{"type": "reply", "status": "ok"}`:                                                                           "actions[0].status: must be done, pending or error",
		`{"type": "notify", "status": "pending"}`:                                                                     "actions[0].status: must be done or error: a notify is the result that comes after a reply pending",
		`{"type": "reply", "status": "error"}`:                                                                        "actions[0].error_code: is required with status error",
		`{"type": "reply", "status": "done", "error_code": "X"}`:                                                      "actions[0].error_code: is only for status error",
		`{"type": "reply", "status": "error", "error_code": "NOT OK"}`:                                                "actions[0].error_code: must be a code of 64 characters at most",
		`{"type": "reply", "status": "done", "message": "  "}`:                                                        "actions[0].message: must not be blank",
		`{"type": "reply", "status": "done", "result": {"$ref": "x.y"}}`:                                              "actions[0].result.$ref: root must be input, state, or event",
		`{"type": "reply", "status": "done", "target": "AI.ed4qm7k2xa@worker1"}`:                                      "actions[0].target: is not a field of reply: a reply answers the conscious whose request started the instance",
		`{"type": "notify", "status": "done", "payload": {}}`:                                                         "actions[0].payload: is not a field of notify: a notify's text is the engine's",
		`{"type": "send_message", "target": "AI.billing.ed4qm7k2xa@worker1", "meta": {"msg": "X"}, "status": "done"}`: "actions[0].status: is not a field of send_message",
		`{"type": "reply", "status": "done", "result": {"$ref": "event.context.evaluation.params.nope"}}`:             "actions[0].result.$ref: reads event.context.evaluation.params.nope, a param no menu of the definition offers",
	} {
		_, err := loadDoor(t, reply, to)
		assertErrorContains(t, err, want)
	}
	if _, err := loadDoor(t, reply, `{"type": "reply", "status": "done", "message": "`+strings.Repeat("x", maxAnswerMessage+1)+`"}`); err == nil ||
		!strings.Contains(err.Error(), "actions[0].message: must be 1000 characters at most") {
		t.Fatalf("a long message: %v", err)
	}
	// Only a door answers a request: a WF its start_from makes no door has none to answer.
	_, err := loadDoor(t, `"start_from": ["AI.ed4qm7k2xa@worker1"],
  "request_actions": [{"name": "invoice_state", "description": "The state of an invoice."}],`, `"start_from": ["IO.api.sales@worker1"],`)
	assertErrorContains(t, err, "states[0].transitions[0].actions[0].type: reply answers the capability request that started the instance: only a capability's door")
	// The catch may answer too.
	if _, err := loadDoor(t, `"states": [`, `"catch": [{"event_match": {}, "guard": "true", "actions": [{"type": "reply", "status": "error", "error_code": "UNEXPECTED"}]}],
  "states": [`); err != nil {
		t.Fatalf("a catch that answers: %v", err)
	}
}

// request_actions: a door's alone; each named as a menu option is, once, its description optional.
func TestRequestActions(t *testing.T) {
	def, err := loadDoor(t, `[{"name": "invoice_state", "description": "The state of an invoice."}]`,
		`[{"name": "invoice_state", "description": "The state of an invoice."}, {"name": "reconcile"}]`)
	if err != nil || len(def.RequestActions) != 2 || def.RequestActions[1].Description != "" {
		t.Fatalf("two actions, one without a description: %+v %v", def, err)
	}
	for to, want := range map[string]string{
		`[{"name": "Invoice state"}]`:                                        "request_actions[0].name: must be a lowercase name",
		`[{"name": "a"}, {"name": "a"}]`:                                     `request_actions[1].name: must be unique in request_actions ("a" is taken)`,
		`[{"name": "a", "description": " "}]`:                                "request_actions[0].description: must not be blank",
		`[{"name": "a", "params": {}}]`:                                      `unknown field "params"`,
		`[{"Name": "a"}]`:                                                    "request_actions[0].name: is written Name",
		`[{"name": "a", "description": "` + strings.Repeat("x", 201) + `"}]`: "request_actions[0].description: must be 200 characters at most",
	} {
		_, err := loadDoor(t, `[{"name": "invoice_state", "description": "The state of an invoice."}]`, to)
		assertErrorContains(t, err, want)
	}
	many := make([]string, maxRequestActions+1)
	for i := range many {
		many[i] = `{"name": "a` + strings.Repeat("b", i) + `"}`
	}
	_, err = loadDoor(t, `[{"name": "invoice_state", "description": "The state of an invoice."}]`, "["+strings.Join(many, ", ")+"]")
	assertErrorContains(t, err, "request_actions: must hold 16 actions at most")
	_, err = loadDoor(t, `"start_from": ["AI.ed4qm7k2xa@worker1"],`, `"start_from": ["IO.api.sales@worker1"],`)
	assertErrorContains(t, err, "request_actions: describe the requests a capability's door takes: only a capability's door")
}

// A door's instance starts on a CAPABILITY_REQUEST, a system message: its initial state's entry
// actions keep no message and forward none of the event.
func TestADoorsInitialStateKeepsAndForwardsNothing(t *testing.T) {
	send := `{"type": "send_message", "target": "AI.billing.ed4qm7k2xa@worker1", "meta": {"msg": "ASK"}, "payload": {"type": "text", "content": "x"}}`
	_, err := loadDoor(t, send, `{"type": "keep_message", "name": "request"}, {"type": "forward", "target": "AI.billing.ed4qm7k2xa@worker1"}`)
	assertErrorContains(t, err, "states[0].entry_actions[0]: a keep_message runs on the capability request that starts an instance")
	assertErrorContains(t, err, "states[0].entry_actions[1]: a forward without message runs on the capability request that starts an instance")
}
