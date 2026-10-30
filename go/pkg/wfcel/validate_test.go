package wfcel

import (
	"encoding/json"
	"fmt"
	"reflect"
	"strings"
	"testing"
	"time"
)

func TestLoadDefinitionBytesAcceptsValidWorkflow(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("LoadDefinitionBytes returned error: %v", err)
	}
	if def.WorkflowType != "invoice" {
		t.Fatalf("unexpected workflow_type: %q", def.WorkflowType)
	}
	if got := len(def.States); got != 4 {
		t.Fatalf("unexpected state count: %d", got)
	}
	if def.States[0].Transitions[0].Program == nil {
		t.Fatal("expected compiled guard program")
	}
}

func TestLoadDefinitionBytesRejectsUnknownField(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"workflow_type": "invoice",`, "\"workflow_type\": \"invoice\",\n  \"unknown_field\": true,", 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	if err == nil || !strings.Contains(err.Error(), "unknown field") {
		t.Fatalf("expected unknown field error, got %v", err)
	}
}

func TestValidateDefinitionRejectsInvalidInitialState(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"initial_state": "collecting_data"`, `"initial_state": "missing_state"`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "initial_state")
}

func TestValidateDefinitionRejectsInvalidTargetState(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"target_state": "completed"`, `"target_state": "missing_state"`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].transitions[0].target_state")
}

func TestValidateDefinitionRejectsInvalidJSONSchema(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"type": "object"`, `"type": "nope"`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "input_schema")
}

func TestValidateDefinitionRejectsInvalidGuard(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"guard": "event.payload.complete == true"`, `"guard": "event.payload.complete =="`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].transitions[0].guard")
}

func TestValidateDefinitionRejectsInvalidTargetL2Name(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"target": "IO.quickbooks@motherbee"`, `"target": "not a node"`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].entry_actions[0].target")
}

func TestValidateDefinitionRejectsShortTimerDuration(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"fire_in": "5m"`, `"fire_in": "30s"`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].entry_actions[1].fire_in")
}

func TestValidateDefinitionRejectsInvalidVariableName(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"name": "validated_at"`, `"name": "1-invalid"`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].transitions[0].actions[0].name")
}

func TestValidateDefinitionRejectsInvalidRefSyntax(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"customer_id": {"$ref": "input.customer_id"}`, `"customer_id": {"$ref": "input."}`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].entry_actions[0].payload.customer_id.$ref: path segments must not be empty")
}

func TestValidateDefinitionAcceptsEmitInternalEvent(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"entry_actions": [],`, `"entry_actions": [{"type":"emit_internal_event","meta":{"msg":"INTERNAL_READY"},"payload":{"value":"ok"}}],`, 1)
	if _, err := LoadDefinitionBytes([]byte(data), "", fixedClock); err != nil {
		t.Fatalf("expected emit_internal_event to validate, got %v", err)
	}
}

func TestValidateDefinitionRejectsEmitInternalEventWithoutMsg(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"entry_actions": [],`, `"entry_actions": [{"type":"emit_internal_event","meta":{},"payload":{"value":"ok"}}],`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[1].entry_actions[0].meta.msg")
}

func TestValidateInputPayloadAcceptsNumericFields(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("LoadDefinitionBytes: %v", err)
	}
	input := map[string]any{
		"customer_id":  "cust-001",
		"amount_cents": float64(25000),
		"currency":     "USD",
	}
	if err := ValidateInputPayload(def, input); err != nil {
		t.Fatalf("ValidateInputPayload: unexpected error: %v", err)
	}
}

func TestParseWorkflowDurationSupportsDays(t *testing.T) {
	got, err := ParseWorkflowDuration("1d")
	if err != nil {
		t.Fatalf("ParseWorkflowDuration returned error: %v", err)
	}
	if got != 24*time.Hour {
		t.Fatalf("unexpected duration: %v", got)
	}
}

func fixedClock() time.Time {
	return time.UnixMilli(1776100000000).UTC()
}

func assertErrorContains(t *testing.T, err error, want string) {
	t.Helper()
	if err == nil {
		t.Fatalf("expected error containing %q, got nil", want)
	}
	if !strings.Contains(err.Error(), want) {
		t.Fatalf("expected error containing %q, got %v", want, err)
	}
}

func validWorkflowJSON() string {
	return `{
  "wf_schema_version": "1",
  "workflow_type": "invoice",
  "description": "issues invoice",
  "start_from": ["IO.api.billing@motherbee"],
  "input_schema": {
    "type": "object",
    "required": ["customer_id"],
    "properties": {
      "customer_id": { "type": "string" }
    }
  },
  "initial_state": "collecting_data",
  "terminal_states": ["completed", "failed", "cancelled"],
  "states": [
    {
      "name": "collecting_data",
      "description": "collects invoice data",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "IO.quickbooks@motherbee",
          "meta": { "msg": "INVOICE_CREATE_REQUEST" },
          "payload": {
            "customer_id": {"$ref": "input.customer_id"},
            "note": "literal"
          }
        },
        {
          "type": "schedule_timer",
          "timer_key": "invoice_timeout",
          "fire_in": "5m",
          "missed_policy": "fire"
        }
      ],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": { "msg": "DATA_VALIDATION_RESPONSE" },
          "guard": "event.payload.complete == true",
          "target_state": "completed",
          "actions": [
            { "type": "set_variable", "name": "validated_at", "value": "now()" }
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
    },
    {
      "name": "failed",
      "description": "failed",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "cancelled",
      "description": "cancelled",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    }
  ]
}`
}

// A WF send_message must never target a SY.*/RT.* system node (those bypass VPN isolation in the
// router).
func TestValidateDefinitionRejectsSystemNodeTarget(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"target": "IO.quickbooks@motherbee"`, `"target": "SY.vault@motherbee"`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "SY.* / RT.*")
}

func TestValidateDefinitionRejectsSendWithoutTarget(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"target": "IO.quickbooks@motherbee",`, ``, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].entry_actions[0].target: must not be empty")
}

// 0.1.90 (wf-orchestration-v1 §5.1): a WF always sends user messages, and sends to node names written
// in the definition. meta.type and target_ref left the definition language: a definition that still
// has one is refused, by its path, saying why.
func TestValidateDefinitionRefusesMetaTypeAndTargetRef(t *testing.T) {
	cases := []struct {
		name, from, to, path, why string
	}{
		{
			name: "meta.type on a send",
			from: `"meta": { "msg": "INVOICE_CREATE_REQUEST" },`,
			to:   `"meta": { "msg": "INVOICE_CREATE_REQUEST", "type": "user" },`,
			path: "states[0].entry_actions[0].meta.type",
			why:  "every message a WF sends is a user message",
		},
		{
			name: "meta.type on an internal event",
			from: `"entry_actions": [],`,
			to:   `"entry_actions": [{"type":"emit_internal_event","meta":{"msg":"NEXT","type":"system"}}],`,
			path: "states[1].entry_actions[0].meta.type",
			why:  "every message a WF sends is a user message",
		},
		{
			name: "target_ref",
			from: `"target": "IO.quickbooks@motherbee",`,
			to:   `"target_ref": "state.chosen_target",`,
			path: "states[0].entry_actions[0].target_ref",
			why:  "one transition per destination",
		},
		{
			name: "target_ref in a transition",
			from: `{ "type": "set_variable", "name": "validated_at", "value": "now()" }`,
			to:   `{ "type": "send_message", "target": "AI.host@motherbee", "target_ref": "state.x", "meta": {"msg": "ASK"} }`,
			path: "states[0].transitions[0].actions[0].target_ref",
			why:  "node names written in the definition",
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			data := strings.Replace(validWorkflowJSON(), c.from, c.to, 1)
			if data == validWorkflowJSON() {
				t.Fatal("the fixture did not change")
			}
			_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
			assertErrorContains(t, err, c.path+": is not part of the definition language")
			assertErrorContains(t, err, c.why)
		})
	}
}

// 0.1.90: an object whose "$ref" is a dot path is a WF $ref: it is rooted at input., state. or
// event., and it holds only "$ref" and, optionally, a literal "default". Any other object is a
// literal: a JSON Schema's own "$ref" in a response_envelope included.
func TestValidateDefinitionRefRules(t *testing.T) {
	note := func(object string) error {
		t.Helper()
		data := strings.Replace(validWorkflowJSON(), `"note": "literal"`, `"note": `+object, 1)
		_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
		return err
	}
	for _, object := range []string{
		`{"$ref": "event.context", "default": {}}`,
		`{"$ref": "input.x", "default": {"nested": "literal"}}`,
		`{"$ref": "#/definitions/x"}`,
		`{"$ref": "#/definitions/x", "description": "a JSON Schema's own reference"}`,
		`{"type": "object", "properties": {"item": {"$ref": "#/$defs/item"}}}`,
	} {
		if err := note(object); err != nil {
			t.Fatalf("%s must validate, got %v", object, err)
		}
	}
	for object, want := range map[string]string{
		`{"$ref": "input.currency", "defualt": "USD"}`:            "payload.note: a $ref holds only $ref and default",
		`{"$ref": "input.x", "default": 1, "other": 2}`:           "payload.note: a $ref holds only $ref and default",
		`{"$ref": "input.x", "default": {"$ref": "state.y"}}`:     "payload.note.default: a default is a literal: it holds no $ref",
		`{"$ref": "input.x", "default": [{"$ref": "event.y.z"}]}`: "payload.note.default: a default is a literal: it holds no $ref",
		`{"$ref": "state."}`:                    "payload.note.$ref: path segments must not be empty",
		`{"$ref": "inptu.currency"}`:            "payload.note.$ref: root must be input, state, or event",
		`{"$ref": "context"}`:                   "payload.note.$ref: must begin with input., state., or event.",
		`{"$ref": "ctx.thing", "default": "x"}`: "payload.note.$ref: root must be input, state, or event",
		`{"$ref": 5}`:                           "payload.note.$ref: must be a string",
	} {
		assertErrorContains(t, note(object), want)
	}

	for object, isRef := range map[string]bool{
		`{"$ref": "input.x"}`:                       true,
		`{"$ref": "input.x", "default": null}`:      true,
		`{"$ref": "input.x", "default": [1, 2]}`:    true,
		`{"$ref": "input.x", "other": 1}`:           false,
		`{"$ref": "input.x", "default": 1, "x": 2}`: false,
		`{"$ref": "#/definitions/x"}`:               false,
		`{"$ref": "https://example.com/s.json"}`:    false,
		`{"$ref": "./other.json"}`:                  false,
		`{"$ref": "inptu.x"}`:                       true,
		`{"default": 1}`:                            false,
	} {
		var decoded map[string]any
		if err := json.Unmarshal([]byte(object), &decoded); err != nil {
			t.Fatal(err)
		}
		if _, ok := AsRef(decoded); ok != isRef {
			t.Fatalf("%s: AsRef = %v, want %v", object, ok, isRef)
		}
	}
}

// 0.1.90: event_match.type left the definition language, like meta.type.
func TestValidateDefinitionRefusesEventMatchType(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"event_match": { "msg": "DATA_VALIDATION_RESPONSE" }`, `"event_match": { "msg": "DATA_VALIDATION_RESPONSE", "type": "system" }`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].transitions[0].event_match.type: is not part of the definition language")
}

// The literals of a definition's actions keep their type: an integer literal is an int64 and any
// other number a float64 (decoded as float64, 3 went out on the wire as 3.0). The input_schema keeps
// its numbers as written.
func TestADefinitionKeepsItsNumbersAsWritten(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"note": "literal"`, `"note": {"count": 3, "ratio": 2.5, "whole": 1.0, "items": [7], "fallback": {"$ref": "input.limit", "default": 10}}`, 1)
	data = strings.Replace(data, `"customer_id": { "type": "string" }`, `"customer_id": { "type": "string" }, "limit": { "type": "integer", "maximum": 100 }`, 1)
	def, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	if err != nil {
		t.Fatal(err)
	}
	note := def.States[0].EntryActions[0].Payload.(map[string]any)["note"].(map[string]any)
	want := map[string]any{
		"count": int64(3), "ratio": 2.5, "whole": float64(1), "items": []any{int64(7)},
		"fallback": map[string]any{"$ref": "input.limit", "default": int64(10)},
	}
	if !reflect.DeepEqual(note, want) {
		t.Fatalf("note = %#v", note)
	}
	encoded, err := EncodeJSON(note)
	if err != nil {
		t.Fatal(err)
	}
	for _, literal := range []string{`"count":3`, `"ratio":2.5`, `"whole":1.0`, `"items":[7]`} {
		if !strings.Contains(string(encoded), literal) {
			t.Fatalf("%s lacks %s", encoded, literal)
		}
	}
	limit := def.InputSchema["properties"].(map[string]any)["limit"].(map[string]any)
	if limit["maximum"] != json.Number("100") {
		t.Fatalf("the input_schema keeps its numbers as written: %#v", limit["maximum"])
	}
	if err := ValidateInputPayload(def, map[string]any{"customer_id": "c1", "limit": int64(101)}); err == nil {
		t.Fatal("the schema's maximum still applies")
	}
}

func TestForbiddenSystemTargetAndRefPathHelpers(t *testing.T) {
	for _, n := range []string{"SY.vault@motherbee", "SY.admin@motherbee", "RT.gateway@motherbee", "sy.vault@motherbee", "rt.gateway@motherbee", "  SY.vault@x"} {
		if !IsForbiddenSystemTarget(n) {
			t.Fatalf("%q should be forbidden", n)
		}
	}
	for _, n := range []string{"AI.host@motherbee", "IO.api@motherbee", "WF.router@motherbee"} {
		if IsForbiddenSystemTarget(n) {
			t.Fatalf("%q should be allowed", n)
		}
	}
	for _, p := range []string{"state.x", "input.a.b", "event.payload.node"} {
		if err := validateRefPath(p); err != nil {
			t.Fatalf("%q should be a valid ref path: %v", p, err)
		}
	}
	for _, p := range []string{"foo.bar", "state", "state.", ".x", ""} {
		if validateRefPath(p) == nil {
			t.Fatalf("%q should be an invalid ref path", p)
		}
	}
}

// FIX-R3: "failed" is the reserved fatal-error sink; a state named "failed" that is not terminal is
// rejected at publish so it can never be silently hijacked/terminated by routeToFailed.
func TestValidateDefinitionRejectsNonTerminalFailedState(t *testing.T) {
	data := strings.Replace(
		validWorkflowJSON(),
		`"terminal_states": ["completed", "failed", "cancelled"]`,
		`"terminal_states": ["completed", "cancelled"]`,
		1,
	)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	if err == nil {
		t.Fatal("expected a non-terminal state named \"failed\" to be rejected")
	}
	if !strings.Contains(err.Error(), "reserved") {
		t.Fatalf("expected a reservation error, got: %v", err)
	}
}

// 0.1.91 (wf-orchestration-v1 §5.2.1, item 2): start_from is required, and names the nodes whose
// messages start an instance, each with its hive, never a SY.* or RT.* node.
func TestValidateDefinitionStartFrom(t *testing.T) {
	load := func(startFrom string) error {
		t.Helper()
		data := strings.Replace(validWorkflowJSON(), `"start_from": ["IO.api.billing@motherbee"]`, startFrom, 1)
		if data == validWorkflowJSON() {
			t.Fatal("the fixture did not change")
		}
		_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
		return err
	}
	if err := load(`"start_from": ["IO.api.sales@worker1", "WF.upstream@motherbee"]`); err != nil {
		t.Fatalf("two starters: %v", err)
	}
	for startFrom, want := range map[string]string{
		`"start_from": []`:                                                   "start_from: must name the nodes whose messages start an instance",
		`"description": "x"`:                                                 "start_from: must name the nodes whose messages start an instance",
		`"start_from": ["IO.api.sales"]`:                                     "start_from[0]: must be a node name with its hive",
		`"start_from": ["IO.api.billing@motherbee", " IO.x@w "]`:             "start_from[1]: must be a node name with its hive",
		`"start_from": ["SY.admin@motherbee"]`:                               "start_from[0]: must not be a SY.* / RT.* system node",
		`"start_from": ["IO.api.billing@motherbee", "rt.gw@motherbee"]`:      "start_from[1]: must not be a SY.* / RT.* system node",
		`"start_from": ["IO.api.billing@motherbee", "AI.billing@motherbee"]`: "start_from[1]: must not be an AI.* node: an agent only answers",
		`"start_from": ["ai.sales@worker1"]`:                                 "start_from[0]: must not be an AI.* node",
	} {
		assertErrorContains(t, load(startFrom), want)
	}
}

// 0.1.91 (item 1): thread_id_ref left the language. Every message an instance sends carries the
// instance's thread, and a definition that still has it is refused, by its path, saying why.
func TestValidateDefinitionRefusesThreadIDRef(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"target": "IO.quickbooks@motherbee",`, `"target": "IO.quickbooks@motherbee", "thread_id_ref": "event.thread_id",`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].entry_actions[0].thread_id_ref: is not part of the definition language")
	assertErrorContains(t, err, "the instance's thread")
}

// 0.1.91 (items 4 and 9): a transition matches the sender (event_match.src), the msg, or both; one
// without a target_state is internal, and stays in its state.
func TestValidateDefinitionEventMatchSrcAndInternalTransitions(t *testing.T) {
	transition := func(match, target string) error {
		t.Helper()
		data := strings.Replace(validWorkflowJSON(), `"event_match": { "msg": "DATA_VALIDATION_RESPONSE" },
          "guard": "event.payload.complete == true",
          "target_state": "completed",`, `"event_match": `+match+`, "guard": "true", `+target, 1)
		if data == validWorkflowJSON() {
			t.Fatal("the fixture did not change")
		}
		_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
		return err
	}
	for _, match := range []string{`{"src": "IO.quickbooks@motherbee"}`, `{"msg": "X", "src": "AI.other@worker1"}`, `{"msg": "X"}`} {
		if err := transition(match, `"target_state": "completed",`); err != nil {
			t.Fatalf("%s: %v", match, err)
		}
		if err := transition(match, ``); err != nil {
			t.Fatalf("%s, internal: %v", match, err)
		}
	}
	assertErrorContains(t, transition(`{}`, `"target_state": "completed",`), "states[0].transitions[0].event_match: must name a msg, a src or both")
	assertErrorContains(t, transition(`{"msg": "  ", "src": "IO.quickbooks@motherbee"}`, ``), "states[0].transitions[0].event_match.msg: must not be blank")
	assertErrorContains(t, transition(`{"src": "IO.quickbooks"}`, ``), "states[0].transitions[0].event_match.src: must be a node name with its hive")
	assertErrorContains(t, transition(`{"src": "SY.timer@motherbee"}`, ``), "states[0].transitions[0].event_match.src: must not be a SY.* / RT.* system node")
	assertErrorContains(t, transition(`{"msg": "X"}`, `"target_state": "nowhere",`), "states[0].transitions[0].target_state: must reference an existing state")

	def, err := LoadDefinitionBytes([]byte(strings.Replace(validWorkflowJSON(), `"event_match": { "msg": "DATA_VALIDATION_RESPONSE" },
          "guard": "event.payload.complete == true",
          "target_state": "completed",`, `"event_match": {"msg": "X"}, "guard": "true",`, 1)), "", fixedClock)
	if err != nil || def.States[0].Transitions[0].TargetState != "" || def.States[0].Transitions[0].Program == nil {
		t.Fatalf("an internal transition: %+v %v", def, err)
	}
}

// 0.1.91 (items 5 to 7): forward sends a message as it was received, the event's or one a
// keep_message kept; a send_message and a forward write the WF's block (meta.context.wf), which
// their own context never writes, and its vars.
func TestValidateDefinitionForwardKeepMessageAndTheWFBlock(t *testing.T) {
	action := func(actions string) error {
		t.Helper()
		data := strings.Replace(validWorkflowJSON(), `{ "type": "set_variable", "name": "validated_at", "value": "now()" }`, actions, 1)
		if data == validWorkflowJSON() {
			t.Fatal("the fixture did not change")
		}
		_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
		return err
	}
	for _, valid := range []string{
		`{"type": "forward", "target": "AI.sales@worker1"}`,
		`{"type": "keep_message", "name": "customer"}, {"type": "forward", "target": "AI.sales@worker1", "message": "customer", "meta": {"context": {"io": {"$ref": "state.io"}}}, "vars": {"order": {"$ref": "state.order"}, "note": "literal"}}`,
		`{"type": "forward", "target": "AI.sales@worker1", "meta": {"context": {"$ref": "state.ctx", "default": {}}}}`,
		`{"type": "send_message", "target": "AI.payments@worker1", "meta": {"msg": "DUE", "context": {"io": {"$ref": "state.io"}}}, "vars": {"invoice": {"$ref": "state.invoice"}}, "payload": {"type": "text", "content": "due"}}`,
	} {
		if err := action(valid); err != nil {
			t.Fatalf("%s: %v", valid, err)
		}
	}
	const at = "states[0].transitions[0].actions[0]"
	for actions, want := range map[string]string{
		`{"type": "forward"}`: at + ".target: must not be empty",
		`{"type": "forward", "target": "SY.admin@motherbee"}`:                                                        at + ".target: must not target a SY.* / RT.* system node",
		`{"type": "forward", "target": "AI.sales@worker1", "meta": {"msg": "TURN"}}`:                                 at + ".meta.msg: is not a field of forward: a forward keeps the message's own msg",
		`{"type": "forward", "target": "AI.sales@worker1", "payload": {"text": "x"}}`:                                at + ".payload: is not a field of forward: a forward sends the message's payload as it was received",
		`{"type": "forward", "target": "AI.sales@worker1", "message": "customer"}`:                                   at + ".message: must name a message a keep_message keeps",
		`{"type": "forward", "target": "AI.sales@worker1", "message": "a-b"}`:                                        at + ".message: must be a valid identifier",
		`{"type": "forward", "target": "AI.sales@worker1", "meta": {"context": "x"}}`:                                at + ".meta.context: must be a JSON object",
		`{"type": "forward", "target": "AI.sales@worker1", "meta": {"context": {"wf": 1}}}`:                          at + ".meta.context.wf: is the WF's own block",
		`{"type": "forward", "target": "AI.sales@worker1", "vars": [1]}`:                                             at + ".vars: must be a JSON object",
		`{"type": "forward", "target": "AI.sales@worker1", "vars": {"$ref": "state.x"}}`:                             at + ".vars: must be a JSON object",
		`{"type": "forward", "target": "AI.sales@worker1", "vars": {"x": {"$ref": "nope.x"}}}`:                       at + ".vars.x.$ref: root must be input, state, or event",
		`{"type": "send_message", "target": "AI.x@worker1", "meta": {"msg": "M", "context": {"wf": {"trail": []}}}}`: at + ".meta.context.wf: is the WF's own block",
		`{"type": "send_message", "target": "AI.x@worker1", "meta": {"msg": "M", "context": [1]}}`:                   at + ".meta.context: must be a JSON object",
		`{"type": "keep_message"}`: at + ".name: must be a valid identifier",
		`{"type": "keep_message", "name": "customer", "message": "customer"}`:      at + ".message: is not a field of keep_message",
		`{"type": "set_variable", "name": "x", "value": "1", "vars": {"a": 1}}`:    at + ".vars: is not a field of set_variable",
		`{"type": "emit_internal_event", "meta": {"msg": "GO"}, "vars": {"a": 1}}`: at + ".vars: is not a field of emit_internal_event",
	} {
		assertErrorContains(t, action(actions), want)
	}
}

// 0.1.91 (item 8): fire_in and fire_at take a literal or a $ref, resolved when the action runs (and
// the 60 s floor checked then); a $ref's default is a literal, checked as one.
func TestValidateDefinitionTimerTimesFromState(t *testing.T) {
	timer := func(spec string) error {
		t.Helper()
		data := strings.Replace(validWorkflowJSON(), `"fire_in": "5m",`, spec, 1)
		if data == validWorkflowJSON() {
			t.Fatal("the fixture did not change")
		}
		_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
		return err
	}
	for _, valid := range []string{
		`"fire_in": {"$ref": "state.invoice.due_in"},`,
		`"fire_in": {"$ref": "event.payload.due_in", "default": "30d"},`,
		`"fire_at": {"$ref": "state.due_at"},`,
		`"fire_at": "2026-11-01T12:00:00Z",`,
	} {
		if err := timer(valid); err != nil {
			t.Fatalf("%s: %v", valid, err)
		}
	}
	const at = "states[0].entry_actions[1]"
	for spec, want := range map[string]string{
		`"fire_in": "30s",`:                                 at + ".fire_in: must be at least 60s",
		`"fire_in": 300,`:                                   at + `.fire_in: must be a duration (e.g. "5m") or a $ref`,
		`"fire_in": {"$ref": "state."},`:                    at + ".fire_in.$ref: path segments must not be empty",
		`"fire_in": {"$ref": "state.x", "defualt": "5m"},`:  at + ".fire_in: a $ref holds only $ref and default",
		`"fire_in": {"$ref": "state.x", "default": "10s"},`: at + ".fire_in.default: must be at least 60s",
		`"fire_in": {"$ref": "state.x", "default": 600},`:   at + `.fire_in.default: must be a duration (e.g. "5m")`,
		`"fire_at": "tomorrow",`:                            at + ".fire_at: must be RFC3339 UTC timestamp",
		`"fire_in": "5m", "fire_at": {"$ref": "state.x"},`:  at + ": fire_in and fire_at are mutually exclusive",
		`"fire_in": "",`:                                    at + ": exactly one of fire_in or fire_at is required",
	} {
		assertErrorContains(t, timer(spec), want)
	}
}

// 0.1.91 (item 3, FINDINGS A-123): only the nodes written in a definition drive its instances:
// those of its start_from, of an event_match.src, and its send_message and forward targets.
func TestTheNodesADefinitionInvolves(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `{ "type": "set_variable", "name": "validated_at", "value": "now()" }`,
		`{"type": "forward", "target": "AI.sales@worker1"}`, 1)
	data = strings.Replace(data, `"event_match": { "msg": "DATA_VALIDATION_RESPONSE" },`, `"event_match": { "msg": "DATA_VALIDATION_RESPONSE", "src": "IO.validator@motherbee" },`, 1)
	def, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	if err != nil {
		t.Fatal(err)
	}
	for name, want := range map[string][2]bool{
		"IO.api.billing@motherbee": {true, true},
		"IO.quickbooks@motherbee":  {false, true},
		"IO.validator@motherbee":   {false, true},
		"AI.sales@worker1":         {false, true},
		"AI.stranger@motherbee":    {false, false},
		"SY.timer@motherbee":       {false, false},
		"":                         {false, false},
	} {
		if got := [2]bool{def.StartsFrom(name), def.Involves(name)}; got != want {
			t.Fatalf("%q: starts from, involves = %v, want %v", name, got, want)
		}
	}
}

// Every action takes the fields of its type only: a field of another type is refused, by its path
// (a forward with a name instead of a message loaded, and forwarded the event).
func TestAnActionRefusesTheFieldsOfOtherTypes(t *testing.T) {
	action := func(actions string) error {
		t.Helper()
		data := strings.Replace(validWorkflowJSON(), `{ "type": "set_variable", "name": "validated_at", "value": "now()" }`, actions, 1)
		if data == validWorkflowJSON() {
			t.Fatal("the fixture did not change")
		}
		_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
		return err
	}
	const at = "states[0].transitions[0].actions[0]"
	for actions, want := range map[string]string{
		`{"type": "keep_message", "name": "customer"}, {"type": "forward", "target": "AI.sales@worker1", "name": "customer"}`: "states[0].transitions[0].actions[1].name: is not a field of forward: a forward names the kept message it sends with message",
		`{"type": "send_message", "target": "AI.x@worker1", "meta": {"msg": "M"}, "message": "m"}`:                            at + ".message: is not a field of send_message",
		`{"type": "send_message", "target": "AI.x@worker1", "meta": {"msg": "M"}, "timer_key": "t"}`:                          at + ".timer_key: is not a field of send_message",
		`{"type": "emit_internal_event", "meta": {"msg": "GO", "context": {}}}`:                                               at + ".meta.context: is not a field of emit_internal_event",
		`{"type": "keep_message", "name": "x", "meta": {"msg": "M"}}`:                                                         at + ".meta: is not a field of keep_message",
		`{"type": "cancel_timer", "timer_key": "t", "fire_in": "5m"}`:                                                         at + ".fire_in: is not a field of cancel_timer",
		`{"type": "reschedule_timer", "timer_key": "t", "fire_in": "5m", "missed_policy": "fire"}`:                            at + ".missed_policy: is not a field of reschedule_timer",
		`{"type": "schedule_timer", "timer_key": "t", "fire_in": "5m", "target": "AI.x@worker1"}`:                             at + ".target: is not a field of schedule_timer",
		`{"type": "set_variable", "name": "x", "value": "1", "payload": {}}`:                                                  at + ".payload: is not a field of set_variable",
	} {
		assertErrorContains(t, action(actions), want)
	}
	// on_error is every type's.
	if err := action(`{"type": "keep_message", "name": "x", "on_error": "continue"}`); err != nil {
		t.Fatal(err)
	}
}

// On a transition matched on TIMER_FIRED there is no node's message: a keep_message, and a forward
// without message, are refused, among its own actions, its state's exit actions and its target's
// entry actions, all of which run on the TIMER_FIRED.
func TestATimerFiredTransitionKeepsAndForwardsNoEvent(t *testing.T) {
	// timeout adds to the first state a TIMER_FIRED transition to failed, with actions, the state's
	// exit actions and failed's entry actions; elsewhere replaces the actions of its other
	// transition.
	timeout := func(actions, exitActions, entryActions, elsewhere string) error {
		t.Helper()
		data := strings.Replace(validWorkflowJSON(), `"exit_actions": [],
      "transitions": [
        {
          "event_match": { "msg": "DATA_VALIDATION_RESPONSE" },`, `"exit_actions": [`+exitActions+`],
      "transitions": [
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "true", "target_state": "failed", "actions": [`+actions+`]},
        {
          "event_match": { "msg": "DATA_VALIDATION_RESPONSE" },`, 1)
		data = strings.Replace(data, `"name": "failed",
      "description": "failed",
      "entry_actions": [],`, `"name": "failed",
      "description": "failed",
      "entry_actions": [`+entryActions+`],`, 1)
		data = strings.Replace(data, `{ "type": "set_variable", "name": "validated_at", "value": "now()" }`, elsewhere, 1)
		if !strings.Contains(data, `"msg": "TIMER_FIRED"`) {
			t.Fatal("the fixture did not change")
		}
		_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
		return err
	}
	const keep, forward = `{"type": "keep_message", "name": "late"}`, `{"type": "forward", "target": "AI.x@worker1"}`
	// A kept message is no event's: a forward of one goes, on a TIMER_FIRED too.
	if err := timeout(`{"type": "forward", "target": "AI.x@worker1", "message": "late"}`, ``, ``, keep); err != nil {
		t.Fatalf("a forward of a kept message: %v", err)
	}
	for _, c := range []struct{ actions, exit, entry, want string }{
		{keep, ``, ``, "states[0].transitions[0].actions[0]: a keep_message runs on the TIMER_FIRED of states[0].transitions[0], which is no node's message to keep"},
		{forward, ``, ``, "states[0].transitions[0].actions[0]: a forward without message runs on the TIMER_FIRED of states[0].transitions[0]"},
		{``, forward, ``, "states[0].exit_actions[0]: a forward without message runs on the TIMER_FIRED of states[0].transitions[0]"},
		{``, ``, keep, "states[2].entry_actions[0]: a keep_message runs on the TIMER_FIRED of states[0].transitions[0]"},
	} {
		assertErrorContains(t, timeout(c.actions, c.exit, c.entry, `{ "type": "set_variable", "name": "validated_at", "value": "now()" }`), c.want)
	}
}

// A state's transition that takes an internal event, which has no sender, keeps and forwards no event
// either: one whose event_match names no src and whose msg is AGENT_CALL_CAP (the engine's, past the
// cap on agent calls) or one an emit_internal_event of the definition emits. Such a definition
// loaded, and the forward failed the instance, though the cap must never fail it.
func TestAnInternalEventTransitionKeepsAndForwardsNoEvent(t *testing.T) {
	// internal adds to the first state a transition matched on match to failed, with actions, the
	// state's exit actions and failed's entry actions. Completed's entry actions emit NEXT, and the
	// first state's other transition keeps the node's message it takes as late.
	internal := func(match, actions, exitActions, entryActions string) error {
		t.Helper()
		data := strings.Replace(validWorkflowJSON(), `"exit_actions": [],
      "transitions": [
        {
          "event_match": { "msg": "DATA_VALIDATION_RESPONSE" },`, `"exit_actions": [`+exitActions+`],
      "transitions": [
        {"event_match": `+match+`, "guard": "true", "target_state": "failed", "actions": [`+actions+`]},
        {
          "event_match": { "msg": "DATA_VALIDATION_RESPONSE" },`, 1)
		data = strings.Replace(data, `{ "type": "set_variable", "name": "validated_at", "value": "now()" }`, `{"type": "keep_message", "name": "late"}`, 1)
		data = strings.Replace(data, `"name": "completed",
      "description": "done",
      "entry_actions": [],`, `"name": "completed",
      "description": "done",
      "entry_actions": [{"type": "emit_internal_event", "meta": {"msg": "NEXT"}}],`, 1)
		data = strings.Replace(data, `"name": "failed",
      "description": "failed",
      "entry_actions": [],`, `"name": "failed",
      "description": "failed",
      "entry_actions": [`+entryActions+`],`, 1)
		if !strings.Contains(data, `"event_match": `+match) || !strings.Contains(data, `"name": "late"`) || !strings.Contains(data, `"emit_internal_event"`) {
			t.Fatal("the fixture did not change")
		}
		_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
		return err
	}
	const keep, forward = `{"type": "keep_message", "name": "kept"}`, `{"type": "forward", "target": "AI.x@worker1"}`
	// A node's message: a msg nothing emits, or a src (an internal event has none).
	for _, match := range []string{`{"msg": "OTHER"}`, `{"msg": "AGENT_CALL_CAP", "src": "AI.x@worker1"}`, `{"msg": "NEXT", "src": "IO.x@worker1"}`} {
		if err := internal(match, keep+", "+forward, forward, keep); err != nil {
			t.Fatalf("%s: %v", match, err)
		}
	}
	// A kept message is no event's: a forward of one goes, on an internal event too.
	for _, match := range []string{`{"msg": "AGENT_CALL_CAP"}`, `{"msg": "NEXT"}`} {
		if err := internal(match, `{"type": "forward", "target": "AI.x@worker1", "message": "late"}`, ``, ``); err != nil {
			t.Fatalf("%s, a forward of a kept message: %v", match, err)
		}
	}
	for _, c := range []struct{ match, actions, exit, entry, want string }{
		{`{"msg": "AGENT_CALL_CAP"}`, keep, ``, ``, "states[0].transitions[0].actions[0]: a keep_message runs on the AGENT_CALL_CAP of states[0].transitions[0], which is no node's message to keep"},
		{`{"msg": "AGENT_CALL_CAP"}`, forward, ``, ``, "states[0].transitions[0].actions[0]: a forward without message runs on the AGENT_CALL_CAP of states[0].transitions[0], which is no node's message to forward: forward a kept one"},
		{`{"msg": "AGENT_CALL_CAP"}`, ``, forward, ``, "states[0].exit_actions[0]: a forward without message runs on the AGENT_CALL_CAP of states[0].transitions[0]"},
		{`{"msg": "AGENT_CALL_CAP"}`, ``, ``, keep, "states[2].entry_actions[0]: a keep_message runs on the AGENT_CALL_CAP of states[0].transitions[0]"},
		{`{"msg": "NEXT"}`, keep, ``, ``, "states[0].transitions[0].actions[0]: a keep_message runs on the internal event NEXT of states[0].transitions[0], which is no node's message to keep"},
		{`{"msg": "NEXT"}`, forward, ``, ``, "states[0].transitions[0].actions[0]: a forward without message runs on the internal event NEXT of states[0].transitions[0], which is no node's message to forward: forward a kept one"},
		{`{"msg": "NEXT"}`, ``, forward, ``, "states[0].exit_actions[0]: a forward without message runs on the internal event NEXT of states[0].transitions[0]"},
		{`{"msg": "NEXT"}`, ``, ``, keep, "states[2].entry_actions[0]: a keep_message runs on the internal event NEXT of states[0].transitions[0]"},
	} {
		assertErrorContains(t, internal(c.match, c.actions, c.exit, c.entry), c.want)
	}
}

// An agent's evaluation is a menu's answer, {reply, option, params}: a guard, a set_variable value
// or a $ref that reads another field of it is refused (event.context.evaluation.optoin never
// matched, and event.context.evaluation.param.question failed the instance). A param name looked for
// in its params (in) is checked as a read of the param is, and the option compared with the empty
// string, any menu's answer, is no typo.
func TestTheEvaluationIsReplyOptionAndParams(t *testing.T) {
	guard := func(expr string) error {
		t.Helper()
		quoted, _ := json.Marshal(expr)
		_, err := loadChanged(t, `"guard": "event.context.evaluation.option == 'invoice'"`, `"guard": `+string(quoted))
		return err
	}
	for _, valid := range []string{
		"event.context.evaluation.reply != ''",
		"has(event.context.evaluation.reply) && size(event.context.evaluation.params) > 0",
		"has(event.context.evaluation)",
		"event.context.evaluation.option != ''",
		"'' != event.context.evaluation.option",
		"event.context.evaluation.option in ['', 'answer']",
		"'question' in event.context.evaluation.params",
		"'reason' in event['context']['evaluation']['params']",
		"!('plan' in event.context.evaluation.params)",
		"'qty' in state.params",
	} {
		if err := guard(valid); err != nil {
			t.Fatalf("%s: %v", valid, err)
		}
	}
	const at = "states[0].transitions[0].guard: "
	field := func(name string) string {
		return at + "reads event.context.evaluation." + name + ", a field no agent's evaluation has: reply, option or params"
	}
	for expr, want := range map[string]string{
		"event.context.evaluation.optoin == 'invoice'":           field("optoin"),
		"event.context.evaluation.param.question != ''":          field("param"),
		"has(event.context.evaluation.options)":                  field("options"),
		"event['context']['evaluation']['Option'] == 'invoice'":  field("Option"),
		"'qustion' in event.context.evaluation.params":           at + `looks for "qustion" in event.context.evaluation.params, a param no menu of the definition offers: plan, quantity, question or reason`,
		"'Question' in event['context']['evaluation']['params']": at + `looks for "Question" in event.context.evaluation.params`,
		"event.context.evaluation.option in ['', 'invoce']":      at + `compares event.context.evaluation.option with "invoce"`,
	} {
		assertErrorContains(t, guard(expr), want)
	}

	// A set_variable value, and a $ref in a payload or a timer's time.
	_, err := loadChanged(t, `"value": "{'quantity': event.context.evaluation.params.quantity}"`, `"value": "event.context.evaluation.reason"`)
	assertErrorContains(t, err, "states[0].transitions[0].actions[0].value: reads event.context.evaluation.reason, a field no agent's evaluation has")
	for to, want := range map[string]string{
		`{"$ref": "event.context.evaluation.question"}`:                      "states[0].transitions[1].actions[0].payload.content.$ref: reads event.context.evaluation.question, a field no agent's evaluation has: reply, option or params",
		`{"$ref": "event.context.evaluation.param.question", "default": ""}`: "states[0].transitions[1].actions[0].payload.content.$ref: reads event.context.evaluation.param, a field",
	} {
		_, err := loadChanged(t, `{"$ref": "event.context.evaluation.params.question"}`, to)
		assertErrorContains(t, err, want)
	}
	_, err = loadChanged(t, `{"type": "keep_message", "name": "customer"}`, `{"type": "keep_message", "name": "customer"}, {"type": "schedule_timer", "timer_key": "t", "fire_in": {"$ref": "event.context.evaluation.due_in", "default": "30d"}}`)
	assertErrorContains(t, err, "states[0].entry_actions[1].fire_in.$ref: reads event.context.evaluation.due_in, a field no agent's evaluation has")
	if _, err := loadChanged(t, `{"$ref": "event.context.evaluation.params.question"}`, `{"$ref": "event.context.evaluation.reply"}`); err != nil {
		t.Fatal(err)
	}

	// Only a send_message or a forward offers its menu: one on another action (which a definition read
	// as JSON cannot have, it is no field of that action) widens no name a guard may use.
	data := strings.Replace(menuWorkflowJSON(), `{"type": "keep_message", "name": "customer"}`,
		`{"type": "keep_message", "name": "customer", "menu": {"options": [{"name": "stray", "description": "d", "params": {"hint": {"type": "string"}}}]}}`, 1)
	data = strings.Replace(data, `"guard": "event.context.evaluation.option == 'invoice'"`, `"guard": "event.context.evaluation.option == 'stray' && event.context.evaluation.params.hint != ''"`, 1)
	var def WorkflowDefinition
	if err := json.Unmarshal([]byte(data), &def); err != nil {
		t.Fatal(err)
	}
	errs := ValidateDefinition(&def, fixedClock)
	assertErrorContains(t, errs, `states[0].transitions[0].guard: compares event.context.evaluation.option with "stray"`)
	assertErrorContains(t, errs, "states[0].transitions[0].guard: reads event.context.evaluation.params.hint")
}

// encoding/json takes a key for the field it equals ignoring case, so the strict decoder loaded a
// forward's "Name" as its name (and the forward sent the event instead of the kept message), and
// "States" as the states. At every level of the language a key that is a field written with other
// capitals is refused, by the field's path. The user data (a payload, vars, meta.context's contents,
// the input_schema) is never checked.
func TestAFieldWrittenWithOtherCapitalsIsRefused(t *testing.T) {
	const menuAt = "states[0].entry_actions[1].menu"
	for _, c := range []struct{ from, to, want string }{
		{`"states": [`, `"States": [`, "states: is written States: the field is states"},
		{`"states": [`, "\"ſtates\": [", "states: is written ſtates: the field is states"},
		{`"start_from":`, `"Start_From":`, "start_from: is written Start_From: the field is start_from"},
		{`"name": "asking"`, `"Name": "asking"`, "states[0].name: is written Name: the field is name"},
		{`"guard": "event.context.evaluation.option == 'invoice'"`, `"GUARD": "event.context.evaluation.option == 'invoice'"`, "states[0].transitions[0].guard: is written GUARD: the field is guard"},
		{`"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'consult'"`, `"event_match": {"Src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'consult'"`, "states[0].transitions[1].event_match.src: is written Src: the field is src"},
		{`{"type": "keep_message", "name": "customer"}`, `{"Type": "keep_message", "name": "customer"}`, "states[0].entry_actions[0].type: is written Type: the field is type"},
		{`{"type": "keep_message", "name": "customer"}`, `{"type": "keep_message", "name": "customer", "On_Error": "continue"}`, "states[0].entry_actions[0].on_error: is written On_Error: the field is on_error"},
		{`{"type": "keep_message", "name": "customer"}`, `{"type": "keep_message", "name": "customer", "Menu": {"options": []}}`, "states[0].entry_actions[0].menu: is written Menu: the field is menu"},
		{`"target": "AI.sales@worker1", "message": "customer"`, `"target": "AI.sales@worker1", "Name": "customer"`, "states[0].entry_actions[1].name: is written Name: the field is name"},
		{`"meta": {"msg": "CONSULT"}`, `"Meta": {"msg": "CONSULT"}`, "states[0].transitions[1].actions[0].meta: is written Meta: the field is meta"},
		{`"meta": {"msg": "CONSULT"}`, `"meta": {"Msg": "CONSULT"}`, "states[0].transitions[1].actions[0].meta.msg: is written Msg: the field is msg"},
		{`"help": "The person is buying licenses."`, `"Help": "The person is buying licenses."`, menuAt + ".help: is written Help: the field is help"},
		{`"options": [`, `"Options": [`, menuAt + ".options: is written Options: the field is options"},
		{`{"name": "answer", "description"`, `{"name": "answer", "Description"`, menuAt + ".options[0].description: is written Description: the field is description"},
		{`"quantity": {"type": "integer"}`, `"quantity": {"Type": "integer"}`, menuAt + ".options[1].params.quantity.type: is written Type: the field is type"},
	} {
		_, err := loadChanged(t, c.from, c.to)
		assertErrorContains(t, err, c.want)
		// The menu's own check leaves a field written with other capitals to this one.
		if strings.Contains(err.Error(), "is not a field of a menu") {
			t.Fatalf("%s: %v", c.to, err)
		}
	}
	_, err := loadCatch(t, `{"Event_Match": {}, "guard": "true"}`)
	assertErrorContains(t, err, "catch[0].event_match: is written Event_Match: the field is event_match")

	// User data keeps its keys as written.
	for from, to := range map[string]string{
		`"payload": {"type": "text", "content": {"$ref": "event.context.evaluation.params.question"}}`: `"payload": {"Type": "text", "Content": {"$ref": "event.context.evaluation.params.question"}}`,
		`"meta": {"msg": "CONSULT"}`:         `"meta": {"msg": "CONSULT", "context": {"IO": {"Reply_Target": "C1"}}}, "vars": {"Order": 1}`,
		`"input_schema": {"type": "object"}`: `"input_schema": {"type": "object", "properties": {"Name": {"type": "string"}}}`,
	} {
		if _, err := loadChanged(t, from, to); err != nil {
			t.Fatalf("%s: %v", to, err)
		}
	}
}

// A target is a node name written as the router stamps it: one with spaces around it is refused,
// as start_from and event_match.src are.
func TestATargetWithSpacesIsRefused(t *testing.T) {
	data := strings.Replace(validWorkflowJSON(), `"target": "IO.quickbooks@motherbee"`, `"target": " IO.quickbooks@motherbee "`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].entry_actions[0].target: must be a node name without spaces around it")
}

// A day count past what a duration holds is refused: "213504d" wrapped to 25m26s, and an agent's
// answer reaches it through a $ref.
func TestADurationOfTooManyDaysIsRefused(t *testing.T) {
	if got, err := ParseWorkflowDuration("106751d"); err != nil || got != 106751*24*time.Hour {
		t.Fatalf("the most days: %v %v", got, err)
	}
	for _, value := range []string{"106752d", "213504d", "99999999999999999999d"} {
		if got, err := ParseWorkflowDuration(value); err == nil || !strings.Contains(err.Error(), "at most 106751d") {
			t.Fatalf("%s: %v %v", value, got, err)
		}
	}
}

// salesMenuJSON is the menu menuWorkflowJSON offers AI.sales@worker1.
const salesMenuJSON = `{"help": "The person is buying licenses.", "options": [
  {"name": "answer", "description": "You answer the person yourself."},
  {"name": "invoice", "description": "They accepted the quote.", "params": {"quantity": {"type": "integer"}, "plan": {"type": "string", "enum": ["basic", "pro"]}}},
  {"name": "consult", "description": "A supervisor must approve it.", "params": {"question": {"type": "string", "description": "What you ask."}}}
]}`

// menuWorkflowJSON asks AI.sales@worker1 with a menu, and branches on its evaluation: an invoice
// keeps the order, a consult writes the question to the supervisors.
func menuWorkflowJSON() string {
	return `{
  "wf_schema_version": "1",
  "workflow_type": "sales",
  "start_from": ["IO.api.sales@worker1"],
  "input_schema": {"type": "object"},
  "initial_state": "asking",
  "terminal_states": ["done", "failed"],
  "states": [
    {
      "name": "asking",
      "entry_actions": [
        {"type": "keep_message", "name": "customer"},
        {"type": "forward", "target": "AI.sales@worker1", "message": "customer", "menu": ` + salesMenuJSON + `}
      ],
      "transitions": [
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'invoice'", "target_state": "done",
         "actions": [{"type": "set_variable", "name": "order", "value": "{'quantity': event.context.evaluation.params.quantity}"}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'consult'", "target_state": "done",
         "actions": [{"type": "send_message", "target": "IO.slack.supervisores@motherbee", "meta": {"msg": "CONSULT"}, "payload": {"type": "text", "content": {"$ref": "event.context.evaluation.params.question"}}}]}
      ]
    },
    {"name": "done"},
    {"name": "failed"}
  ]
}`
}

// loadChanged loads menuWorkflowJSON with from replaced by to.
func loadChanged(t *testing.T, from, to string) (*WorkflowDefinition, error) {
	t.Helper()
	data := strings.Replace(menuWorkflowJSON(), from, to, 1)
	if data == menuWorkflowJSON() {
		t.Fatalf("the fixture did not change: %s", from)
	}
	return LoadDefinitionBytes([]byte(data), "", fixedClock)
}

// Case 3 (wf-orchestration-v1 §5.3.6): a send_message or a forward to an agent offers it a menu,
// literal and within the limits the agent checks again; each error names its path.
func TestAMenuIsCheckedAsTheAgentReadsIt(t *testing.T) {
	def, err := LoadDefinitionBytes([]byte(menuWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatal(err)
	}
	if menu := def.States[0].EntryActions[1].Menu; menu == nil || menu.Help != "The person is buying licenses." || len(menu.Options) != 3 ||
		menu.Options[1].Params["plan"].Enum[1] != "pro" || menu.Options[2].Params["question"].Description != "What you ask." {
		t.Fatalf("the menu as written: %+v", menu)
	}
	invoice := `{"name": "invoice", "description": "d", "params": {"quantity": {"type": "integer"}, "plan": {"type": "string"}}}`
	consult := `{"name": "consult", "description": "d", "params": {"question": {"type": "string"}}}`
	// menuOf is a menu with options, and the invoice and consult the fixture's guards read.
	menuOf := func(options ...string) string {
		return `{"options": [` + strings.Join(append(options, invoice, consult), ", ") + `]}`
	}
	option := func(name string) string { return fmt.Sprintf(`{"name": %q, "description": "d"}`, name) }
	params := func(n int) string {
		var listed []string
		for i := 0; i < n; i++ {
			listed = append(listed, fmt.Sprintf(`"p%d": {"type": "boolean"}`, i))
		}
		return `{"name": "many", "description": "d", "params": {` + strings.Join(listed, ", ") + `}}`
	}
	values := func(n int) string {
		var listed []string
		for i := 0; i < n; i++ {
			listed = append(listed, fmt.Sprintf(`"v%d"`, i))
		}
		return `[` + strings.Join(listed, ", ") + `]`
	}
	withMenu := func(menu string) error {
		t.Helper()
		_, err := loadChanged(t, salesMenuJSON, menu)
		return err
	}
	for _, valid := range []string{
		menuOf(),
		menuOf(option("a1"), option("a2"), option("a3"), option("a4"), option("a5"), option("a6")),
		`{"help": "` + strings.Repeat("ñ", 600) + `", "options": [` + invoice + `, ` + consult + `]}`,
		menuOf(`{"name": "answer", "description": "` + strings.Repeat("é", 200) + `"}`),
		menuOf(params(8)),
		menuOf(`{"name": "typed", "description": "d", "params": {"s": {"type": "string", "enum": ` + values(20) + `}, "b": {"type": "boolean"}, "i": {"type": "integer"}, "n": {"type": "number", "description": "` + strings.Repeat("x", 200) + `"}}}`),
		menuOf(option("a_" + strings.Repeat("b", 30))),
	} {
		if err := withMenu(valid); err != nil {
			t.Fatalf("%s: %v", valid, err)
		}
	}
	const at = "states[0].entry_actions[1].menu"
	for menu, want := range map[string]string{
		`{"options": []}`: at + ".options: must hold 1 to 8 options (the agent adds none)",
		menuOf(option("a1"), option("a2"), option("a3"), option("a4"), option("a5"), option("a6"), option("a7")): at + ".options: must hold 1 to 8 options",
		menuOf(option("none")):                            at + `.options[0].name: must not be "none": the agent adds none to every menu`,
		menuOf(option("Answer")):                          at + ".options[0].name: must be a lowercase name of 32 characters at most",
		menuOf(option("1st")):                             at + ".options[0].name: must be a lowercase name",
		menuOf(option("")):                                at + ".options[0].name: must be a lowercase name",
		menuOf(option("a" + strings.Repeat("b", 32))):     at + ".options[0].name: must be a lowercase name",
		menuOf(option("answer"), option("answer")):        at + `.options[1].name: must be unique in its menu ("answer" is taken)`,
		menuOf(`{"name": "answer"}`):                      at + ".options[0].description: must not be empty",
		menuOf(`{"name": "answer", "description": "  "}`): at + ".options[0].description: must not be empty",
		menuOf(`{"name": "answer", "description": "` + strings.Repeat("é", 201) + `"}`): at + ".options[0].description: must be 200 characters at most",
		`{"help": "` + strings.Repeat("a", 601) + `", "options": [` + invoice + `]}`:    at + ".help: must be 600 characters at most",
		`{"help": "  ", "options": [` + invoice + `]}`:                                  at + ".help: must not be blank",
		menuOf(params(9)): at + ".options[0].params: must hold 8 params at most",
		menuOf(`{"name": "answer", "description": "d", "params": {"Qty": {"type": "integer"}}}`):                                                 at + ".options[0].params.Qty: must be named with a lowercase name",
		menuOf(`{"name": "answer", "description": "d", "params": {"items": {"type": "array"}}}`):                                                 at + ".options[0].params.items.type: must be string, boolean, integer or number",
		menuOf(`{"name": "answer", "description": "d", "params": {"items": {}}}`):                                                                at + ".options[0].params.items.type: is required",
		menuOf(`{"name": "answer", "description": "d", "params": {"n": {"type": "integer", "enum": ["1"]}}}`):                                    at + ".options[0].params.n.enum: is only for a param of type string",
		menuOf(`{"name": "answer", "description": "d", "params": {"s": {"type": "string", "enum": []}}}`):                                        at + ".options[0].params.s.enum: must hold 1 to 20 values",
		menuOf(`{"name": "answer", "description": "d", "params": {"s": {"type": "string", "enum": ` + values(21) + `}}}`):                        at + ".options[0].params.s.enum: must hold 1 to 20 values",
		menuOf(`{"name": "answer", "description": "d", "params": {"s": {"type": "string", "description": " "}}}`):                                at + ".options[0].params.s.description: must not be blank",
		menuOf(`{"name": "answer", "description": "d", "params": {"s": {"type": "string", "description": "` + strings.Repeat("x", 201) + `"}}}`): at + ".options[0].params.s.description: must be 200 characters at most",
		menuOf(`{"name": "answer", "description": "d", "params": {"s": {"type": "string", "enum": ["a", ""]}}}`):                                 at + ".options[0].params.s.enum[1]: must not be empty",
		menuOf(`{"name": "answer", "description": "d", "params": {"s": {"type": "string", "enum": [" "]}}}`):                                     at + ".options[0].params.s.enum[0]: must not be empty",
		menuOf(`{"name": "answer", "description": "d", "params": {"s": {"type": "string", "enum": ["a", "a"]}}}`):                                at + `.options[0].params.s.enum[1]: must be unique in its enum ("a" is taken)`,
		// The menu has only its own fields, at every level, as the agent reads it; and it is literal:
		// the strict decoder refuses a $ref where a string goes.
		`{"options": [` + invoice + `], "contract": {}}`:                                                      at + ".contract: is not a field of a menu",
		menuOf(`{"name": "answer", "descripton": "d"}`):                                                       at + ".options[0].descripton: is not a field of a menu option",
		menuOf(`{"name": "answer", "description": "d", "params": {"s": {"type": "string", "default": "x"}}}`): at + ".options[0].params.s.default: is not a field of a menu param",
		`{"help": {"$ref": "state.help"}, "options": [` + invoice + `]}`:                                      "menu.help",
	} {
		assertErrorContains(t, withMenu(menu), want)
	}

	// A menu is for an agent, whatever the case of its name, on a send_message or a forward, and on
	// no other action.
	if _, err := loadChanged(t, `{"type": "forward", "target": "AI.sales@worker1"`, `{"type": "forward", "target": "ai.sales@worker1"`); err != nil {
		t.Fatalf("an agent's name in lowercase: %v", err)
	}
	if _, err := loadChanged(t, `"target": "IO.slack.supervisores@motherbee", "meta": {"msg": "CONSULT"},`, `"target": "AI.supervisor@motherbee", "meta": {"msg": "CONSULT"}, "menu": {"options": [{"name": "approve", "description": "You approve it."}]},`); err != nil {
		t.Fatalf("a send_message with a menu: %v", err)
	}
	_, err = loadChanged(t, `{"type": "forward", "target": "AI.sales@worker1"`, `{"type": "forward", "target": "IO.api.sales@worker1"`)
	assertErrorContains(t, err, at+": is only for an agent (AI.*) target")
	_, err = loadChanged(t, `"target": "IO.slack.supervisores@motherbee", "meta": {"msg": "CONSULT"},`, `"target": "IO.slack.supervisores@motherbee", "meta": {"msg": "CONSULT"}, "menu": {"options": [{"name": "approve", "description": "d"}]},`)
	assertErrorContains(t, err, "states[0].transitions[1].actions[0].menu: is only for an agent (AI.*) target")
	for action, want := range map[string]string{
		`{"type": "keep_message", "name": "customer", "menu": {"options": []}}`:                                            "states[0].entry_actions[0].menu: is not a field of keep_message",
		`{"type": "keep_message", "name": "customer"}, {"type": "emit_internal_event", "meta": {"msg": "GO"}, "menu": {}}`: "states[0].entry_actions[1].menu: is not a field of emit_internal_event",
	} {
		_, err := loadChanged(t, `{"type": "keep_message", "name": "customer"}`, action)
		assertErrorContains(t, err, want)
	}
}

// Case 3: a WF asks an agent with menu. response_envelope, which IO nodes keep for their own callers,
// is refused in a WF's action, written or as a $ref, on a forward and on a send_message; a context
// that is a $ref is resolved when the action runs.
func TestAResponseEnvelopeIsRefusedInAWFsAction(t *testing.T) {
	const why = ": is not part of the definition language: a WF asks an agent with menu"
	for _, c := range []struct{ from, to, path string }{
		{`"message": "customer", "menu"`, `"message": "customer", "meta": {"context": {"response_envelope": {"kind": "json_object_v1", "text_field": "reply", "required": ["reply"], "properties": {"reply": {"type": "string"}}}}}, "menu"`, "states[0].entry_actions[1].meta.context.response_envelope"},
		{`"message": "customer", "menu"`, `"message": "customer", "meta": {"context": {"response_envelope": {"$ref": "state.contract"}}}, "menu"`, "states[0].entry_actions[1].meta.context.response_envelope"},
		{`"meta": {"msg": "CONSULT"}`, `"meta": {"msg": "CONSULT", "context": {"response_envelope": {}}}`, "states[0].transitions[1].actions[0].meta.context.response_envelope"},
	} {
		_, err := loadChanged(t, c.from, c.to)
		assertErrorContains(t, err, c.path+why)
	}
	if _, err := loadChanged(t, `"message": "customer", "menu"`, `"message": "customer", "meta": {"context": {"$ref": "state.context", "default": {}}}, "menu"`); err != nil {
		t.Fatal(err)
	}
}

// catchWorkflowJSON is menuWorkflowJSON with a catch.
func catchWorkflowJSON(catch string) string {
	return strings.Replace(menuWorkflowJSON(), `"states": [`, `"catch": [`+catch+`],
  "states": [`, 1)
}

func loadCatch(t *testing.T, catch string) (*WorkflowDefinition, error) {
	t.Helper()
	return LoadDefinitionBytes([]byte(catchWorkflowJSON(catch)), "", fixedClock)
}

// Case 3 (wf-orchestration-v1 §5.3.6): the catch is a list of transitions with a state transition's
// shape, but its event_match may be empty (it matches every event), its target_state is optional
// and it sets no timers. A keep_message, or a forward of the event, needs a src. Each error names
// its path.
func TestTheCatchIsCheckedAsAStatesTransitions(t *testing.T) {
	def, err := loadCatch(t, `
    {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'none'", "target_state": "done",
     "actions": [
       {"type": "keep_message", "name": "declined"},
       {"type": "forward", "target": "IO.api.sales@worker1"},
       {"type": "set_variable", "name": "last_error", "value": "event.context.evaluation.params.reason"},
       {"type": "emit_internal_event", "meta": {"msg": "DECLINED"}},
       {"type": "send_message", "target": "IO.slack.supervisores@motherbee", "meta": {"msg": "DECLINED"}, "payload": {"type": "text", "content": {"$ref": "event.context.evaluation.params.reason"}}}
     ]},
    {"event_match": {"msg": "AGENT_CALL_CAP"}, "guard": "true", "actions": [{"type": "set_variable", "name": "capped", "value": "event.payload.target"}]},
    {"event_match": {}, "guard": "event.payload.type == 'error'", "actions": [{"type": "forward", "target": "IO.api.sales@worker1", "message": "declined"}]}`)
	if err != nil {
		t.Fatal(err)
	}
	if len(def.Catch) != 3 || def.Catch[0].TargetState != "done" || def.Catch[1].TargetState != "" || def.Catch[2].EventMatch != (EventMatch{}) {
		t.Fatalf("the catch: %+v", def.Catch)
	}
	for i, transition := range def.Catch {
		if transition.Program == nil {
			t.Fatalf("catch[%d] has no compiled guard", i)
		}
	}
	for catch, want := range map[string]string{
		`{"event_match": {}, "guard": "", "actions": []}`:                                                                                                      "catch[0].guard: must not be empty",
		`{"event_match": {}, "guard": "event.payload.type ==", "actions": []}`:                                                                                 "catch[0].guard: ",
		`{"event_match": {}, "guard": "true", "target_state": "nowhere"}`:                                                                                      "catch[0].target_state: must reference an existing state",
		`{"event_match": {"msg": " "}, "guard": "true"}`:                                                                                                       "catch[0].event_match.msg: must not be blank",
		`{"event_match": {"src": "IO.slack"}, "guard": "true"}`:                                                                                                "catch[0].event_match.src: must be a node name with its hive",
		`{"event_match": {"src": "SY.timer@worker1"}, "guard": "true"}`:                                                                                        "catch[0].event_match.src: must not be a SY.* / RT.* system node",
		`{"event_match": {"type": "user"}, "guard": "true"}`:                                                                                                   "catch[0].event_match.type: is not part of the definition language",
		`{"event_match": {}, "guard": "true", "actions": [{"type": "nope"}]}`:                                                                                  "catch[0].actions[0].type: unknown action type",
		`{"event_match": {}, "guard": "true", "actions": [{"type": "set_variable", "name": "1x", "value": "1"}]}`:                                              "catch[0].actions[0].name: must be a valid identifier",
		`{"event_match": {"src": "AI.sales@worker1"}, "guard": "true", "actions": [{"type": "forward", "target": "IO.api.sales@worker1", "name": "x"}]}`:       "catch[0].actions[0].name: is not a field of forward",
		`{"event_match": {"src": "AI.sales@worker1"}, "guard": "true", "actions": [{"type": "forward", "target": "IO.api.sales@worker1", "message": "kept"}]}`: "catch[0].actions[0].message: must name a message a keep_message keeps",
		`{"event_match": {}, "guard": "true", "actions": [{"type": "keep_message", "name": "late"}]}`:                                                          "catch[0].actions[0]: a keep_message runs on catch[0], whose event_match names no src, so its event may be a timer's fire or an internal event, no node's message to keep: name a src",
		`{"event_match": {"msg": "TIMER_FIRED"}, "guard": "true", "actions": [{"type": "forward", "target": "IO.api.sales@worker1"}]}`:                         "catch[0].actions[0]: a forward without message runs on catch[0], whose event_match names no src, so its event may be a timer's fire or an internal event, no node's message to forward: forward a kept one, or name a src",
		`{"event_match": {}, "guard": "true"}, {"event_match": {"msg": "X"}, "guard": "true", "actions": [{"type": "keep_message", "name": "late"}]}`:          "catch[1].actions[0]: a keep_message runs on catch[1]",
	} {
		_, err := loadCatch(t, catch)
		assertErrorContains(t, err, want)
	}
	// Timers belong to states: the catch goes to a state whose entry actions set them.
	for _, timer := range []string{
		`{"type": "schedule_timer", "timer_key": "t", "fire_in": "5m"}`,
		`{"type": "cancel_timer", "timer_key": "t"}`,
		`{"type": "reschedule_timer", "timer_key": "t", "fire_in": "5m"}`,
	} {
		_, err := loadCatch(t, `{"event_match": {}, "guard": "true", "actions": [`+timer+`]}`)
		assertErrorContains(t, err, "catch[0].actions[0].type: is a timer action, and the catch sets no timers: it goes to a state whose entry actions set them")
	}

	// A catch transition that names no src and goes to a state runs, on an event that may be no
	// node's message, the exit actions of the state it leaves, any state but a terminal one, and the
	// entry actions of its target: neither may keep or forward the event. With a src, or internal, it
	// may.
	exitForwards := func(catch string) error {
		t.Helper()
		data := strings.Replace(catchWorkflowJSON(catch), `"transitions": [
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'invoice'"`, `"exit_actions": [{"type": "forward", "target": "IO.api.sales@worker1"}],
      "transitions": [
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'invoice'"`, 1)
		data = strings.Replace(data, `{"name": "done"}`, `{"name": "done", "entry_actions": [{"type": "keep_message", "name": "last"}], "exit_actions": [{"type": "keep_message", "name": "gone"}]}`, 1)
		if !strings.Contains(data, `"name": "gone"`) {
			t.Fatal("the fixture did not change")
		}
		_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
		return err
	}
	err = exitForwards(`{"event_match": {"msg": "STOP"}, "guard": "true", "target_state": "done"}`)
	assertErrorContains(t, err, "states[0].exit_actions[0]: a forward without message runs on catch[0], whose event_match names no src")
	assertErrorContains(t, err, "states[1].entry_actions[0]: a keep_message runs on catch[0], whose event_match names no src")
	if strings.Contains(err.Error(), "states[1].exit_actions") {
		t.Fatalf("a terminal state's exit actions never run: %v", err)
	}
	for _, catch := range []string{
		`{"event_match": {"src": "AI.sales@worker1"}, "guard": "true", "target_state": "done"}`,
		`{"event_match": {}, "guard": "true"}`,
	} {
		if err := exitForwards(catch); err != nil {
			t.Fatalf("%s: %v", catch, err)
		}
	}
}

// Case 3: the catch's actions and targets count as the definition's own: a keep_message there keeps
// a message a forward may send, its event_match.src and its targets are involved, and its numbers
// keep their type.
func TestTheCatchCountsAsTheDefinitionsOwn(t *testing.T) {
	def, err := loadCatch(t, `
    {"event_match": {"src": "IO.slack.ops@motherbee"}, "guard": "true",
     "actions": [{"type": "keep_message", "name": "ops"}, {"type": "forward", "target": "AI.helper@motherbee"}]},
    {"event_match": {}, "guard": "true", "actions": [
      {"type": "forward", "target": "IO.api.sales@worker1", "message": "ops"},
      {"type": "send_message", "target": "IO.audit@motherbee", "meta": {"msg": "UNHANDLED"}, "payload": {"count": 3, "ratio": 1.5}}
    ]}`)
	if err != nil {
		t.Fatal(err)
	}
	for name, want := range map[string]bool{
		"IO.slack.ops@motherbee":          true,
		"AI.helper@motherbee":             true,
		"IO.audit@motherbee":              true,
		"IO.slack.supervisores@motherbee": true,
		"AI.stranger@motherbee":           false,
	} {
		if got := def.Involves(name); got != want {
			t.Fatalf("%s: involves = %v, want %v", name, got, want)
		}
	}
	if payload := def.Catch[1].Actions[1].Payload.(map[string]any); payload["count"] != int64(3) || payload["ratio"] != 1.5 {
		t.Fatalf("the catch's numbers: %#v", payload)
	}
	var paths []string
	def.eachAction(func(path string, _ ActionDefinition) { paths = append(paths, path) })
	if got := strings.Join(paths[len(paths)-4:], " "); got != "catch[0].actions[0] catch[0].actions[1] catch[1].actions[0] catch[1].actions[1]" {
		t.Fatalf("eachAction ends with the catch's actions: %v", paths)
	}
}

// Case 3 (wf-orchestration-v1 §5.3.6): what a guard, a set_variable value or a $ref reads of an
// agent's evaluation is checked against the definition's menus. A comparison of the option with a
// string literal (==, !=, in) names an option of one of them, or none; a read of a param names a
// param of one of them, or reason.
func TestTheEvaluationReadsAreCheckedAgainstTheMenus(t *testing.T) {
	guard := func(expr string) error {
		t.Helper()
		quoted, _ := json.Marshal(expr)
		_, err := loadChanged(t, `"guard": "event.context.evaluation.option == 'invoice'"`, `"guard": `+string(quoted))
		return err
	}
	for _, valid := range []string{
		"event.context.evaluation.option == 'none'",
		"event.context.evaluation.option != 'answer'",
		"'invoice' == event.context.evaluation.option",
		"event.context.evaluation.option in ['answer', 'none']",
		"event['context']['evaluation']['option'] == 'consult'",
		"event.context.evaluation.params.reason != ''",
		"has(event.context.evaluation.params.plan) && event.context.evaluation.params['quantity'] > 1",
		"event.context.evaluation.option == state.expected",
		"event.context.evaluation.option.startsWith('inv')",
		"event.payload.option == 'anything' && state.params.x == 1",
	} {
		if err := guard(valid); err != nil {
			t.Fatalf("%s: %v", valid, err)
		}
	}
	const at = "states[0].transitions[0].guard: "
	const option = `compares event.context.evaluation.option with "invoce", an option no menu of the definition offers: answer, consult, invoice or none`
	const param = "reads event.context.evaluation.params.qty, a param no menu of the definition offers: plan, quantity, question or reason"
	for expr, want := range map[string]string{
		"event.context.evaluation.option == 'invoce'":                      at + option,
		"event.context.evaluation.option != 'invoce'":                      at + option,
		"'invoce' == event.context.evaluation.option":                      at + option,
		"event.context.evaluation.option in ['answer', 'invoce']":          at + option,
		"event['context']['evaluation']['option'] == 'invoce'":             at + option,
		"true && (state.x || event.context.evaluation.option == 'invoce')": at + option,
		"event.context.evaluation.params.qty > 1":                          at + param,
		"has(event.context.evaluation.params.qty)":                         at + param,
		"event.context.evaluation.params['qty'] == 1":                      at + param,
		"event.context.evaluation.params.qty.size() > 0":                   at + param,
		"[1].exists(x, x == event.context.evaluation.params.qty)":          at + param,
		"event.context.evaluation.option == 'Invoice'":                     at + `compares event.context.evaluation.option with "Invoice"`,
	} {
		assertErrorContains(t, guard(expr), want)
	}
	// A typo read twice is one error.
	if err := guard("event.context.evaluation.option == 'invoce' || event.context.evaluation.option == 'invoce'"); err == nil || strings.Count(err.Error(), `"invoce"`) != 1 {
		t.Fatalf("one error per typo: %v", err)
	}

	// A set_variable value, and a $ref in a payload, a meta.context, vars or a timer's time.
	_, err := loadChanged(t, `"value": "{'quantity': event.context.evaluation.params.quantity}"`, `"value": "{'quantity': event.context.evaluation.params.qty}"`)
	assertErrorContains(t, err, "states[0].transitions[0].actions[0].value: "+param)
	const ref = "reads event.context.evaluation.params.qustion, a param no menu of the definition offers: plan, quantity, question or reason"
	for to, want := range map[string]string{
		`{"$ref": "event.context.evaluation.params.qustion"}`:                 "states[0].transitions[1].actions[0].payload.content.$ref: " + ref,
		`{"$ref": "event.context.evaluation.params.qustion", "default": "?"}`: "states[0].transitions[1].actions[0].payload.content.$ref: " + ref,
		`[{"$ref": "event.context.evaluation.params.qustion.text"}]`:          "states[0].transitions[1].actions[0].payload.content[0].$ref: " + ref,
	} {
		_, err := loadChanged(t, `{"$ref": "event.context.evaluation.params.question"}`, to)
		assertErrorContains(t, err, want)
	}
	for to, want := range map[string]string{
		`"meta": {"msg": "CONSULT", "context": {"q": {"$ref": "event.context.evaluation.params.qustion"}}}`: "states[0].transitions[1].actions[0].meta.context.q.$ref: " + ref,
		`"meta": {"msg": "CONSULT"}, "vars": {"q": {"$ref": "event.context.evaluation.params.qustion"}}`:    "states[0].transitions[1].actions[0].vars.q.$ref: " + ref,
	} {
		_, err := loadChanged(t, `"meta": {"msg": "CONSULT"}`, to)
		assertErrorContains(t, err, want)
	}
	_, err = loadChanged(t, `{"type": "keep_message", "name": "customer"}`, `{"type": "keep_message", "name": "customer"}, {"type": "schedule_timer", "timer_key": "t", "fire_in": {"$ref": "event.context.evaluation.params.due_in"}}`)
	assertErrorContains(t, err, "states[0].entry_actions[1].fire_in.$ref: reads event.context.evaluation.params.due_in")
	for _, valid := range []string{`{"$ref": "event.context.evaluation.params.reason"}`, `{"$ref": "event.context.evaluation.option"}`, `{"$ref": "event.context.evaluation"}`} {
		if _, err := loadChanged(t, `{"$ref": "event.context.evaluation.params.question"}`, valid); err != nil {
			t.Fatalf("%s: %v", valid, err)
		}
	}

	// The catch's guards are checked too, and a menu in the catch counts.
	_, err = loadCatch(t, `{"event_match": {}, "guard": "event.context.evaluation.option == 'nada'"}`)
	assertErrorContains(t, err, `catch[0].guard: compares event.context.evaluation.option with "nada"`)
	if _, err := LoadDefinitionBytes([]byte(strings.Replace(catchWorkflowJSON(`{"event_match": {"src": "AI.sales@worker1"}, "guard": "true", "actions": [{"type": "forward", "target": "AI.helper@motherbee", "menu": {"options": [{"name": "retry", "description": "Try again.", "params": {"hint": {"type": "string"}}}]}}]}`),
		`"guard": "event.context.evaluation.option == 'invoice'"`, `"guard": "event.context.evaluation.option == 'retry' && event.context.evaluation.params.hint != ''"`, 1)), "", fixedClock); err != nil {
		t.Fatalf("a menu in the catch: %v", err)
	}

	// Without a menu, only none and reason are an agent's.
	data := strings.Replace(validWorkflowJSON(), `"guard": "event.payload.complete == true"`, `"guard": "event.context.evaluation.option == 'ok' && event.context.evaluation.params.why != ''"`, 1)
	_, err = LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, `states[0].transitions[0].guard: compares event.context.evaluation.option with "ok", an option no menu of the definition offers: none`)
	assertErrorContains(t, err, "states[0].transitions[0].guard: reads event.context.evaluation.params.why, a param no menu of the definition offers: reason")
}
