package wfcel

import (
	"encoding/json"
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
		`{"type": "keep_message", "name": "customer"}, {"type": "forward", "target": "AI.sales@worker1", "message": "customer", "meta": {"context": {"response_envelope": {"kind": "json_object_v1", "required": ["ok"], "properties": {"ok": {"type": "boolean"}}}}}, "vars": {"order": {"$ref": "state.order"}, "note": "literal"}}`,
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
		`"fire_in": {"$ref": "event.context.evaluation.due_in", "default": "30d"},`,
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

// A response contract written in the definition is checked at load by the rules an agent reads it
// with (response_contract_to_output_schema_spec in crates/fluxbee_ai_sdk/src/message.rs), on a
// send_message and on a forward; a $ref is not.
func TestAResponseContractIsCheckedAsTheAgentReadsIt(t *testing.T) {
	envelope := func(contract string) error {
		t.Helper()
		data := strings.Replace(validWorkflowJSON(), `{ "type": "set_variable", "name": "validated_at", "value": "now()" }`,
			`{"type": "forward", "target": "AI.x@worker1", "meta": {"context": {"response_envelope": `+contract+`}}}`, 1)
		_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
		return err
	}
	for _, valid := range []string{
		`{"kind": "json_object_v1", "strict": true, "text_field": "reply", "required": ["reply", "area"], "properties": {"reply": {"type": "string"}, "area": {"type": "string", "enum": ["a", "b"]}, "n": {"type": "integer", "description": "not read"}}}`,
		`{"kind": "json_object_v1", "strict": "yes", "required": ["ok"], "properties": {"ok": {"type": "boolean"}}}`,
		`{"$ref": "state.contract"}`,
		`{"kind": "json_object_v1", "required": ["ok"], "properties": {"$ref": "state.properties"}}`,
	} {
		if err := envelope(valid); err != nil {
			t.Fatalf("%s: %v", valid, err)
		}
	}
	const at = "states[0].transitions[0].actions[0].meta.context.response_envelope"
	for contract, want := range map[string]string{
		`"json"`: at + ": must be a JSON object",
		`{"required": ["ok"], "properties": {"ok": {"type": "boolean"}}}`:                                                                              at + `.kind: must be "json_object_v1"`,
		`{"kind": "json_schema", "required": ["ok"], "properties": {"ok": {"type": "boolean"}}}`:                                                       at + `.kind: must be "json_object_v1"`,
		`{"kind": "json_object_v1", "properties": {"ok": {"type": "boolean"}}}`:                                                                        at + ".required: must be a non-empty list of property names",
		`{"kind": "json_object_v1", "required": [], "properties": {"ok": {"type": "boolean"}}}`:                                                        at + ".required: must be a non-empty list of property names",
		`{"kind": "json_object_v1", "required": ["ok"]}`:                                                                                               at + ".properties: must be a non-empty object",
		`{"kind": "json_object_v1", "required": ["ok", 3], "properties": {"ok": {"type": "boolean"}}}`:                                                 at + ".required[1]: must be a string",
		`{"kind": "json_object_v1", "required": [" "], "properties": {"ok": {"type": "boolean"}}}`:                                                     at + ".required[0]: must not be empty",
		`{"kind": "json_object_v1", "required": ["ko"], "properties": {"ok": {"type": "boolean"}}}`:                                                    at + `.required[0]: must be one of the properties ("ko" is not)`,
		`{"kind": "json_object_v1", "required": ["ok"], "properties": {"ok": "boolean"}}`:                                                              at + ".properties.ok: must be an object",
		`{"kind": "json_object_v1", "required": ["ok"], "properties": {"ok": {}}}`:                                                                     at + ".properties.ok.type: is required",
		`{"kind": "json_object_v1", "required": ["ok"], "properties": {"ok": {"type": "array"}}}`:                                                      at + ".properties.ok.type: must be string, boolean, integer or number",
		`{"kind": "json_object_v1", "required": ["ok"], "properties": {"ok": {"type": "string", "enum": "a"}}}`:                                        at + ".properties.ok.enum: must be a list",
		`{"kind": "json_object_v1", "required": ["ok"], "properties": {"ok": {"type": "integer", "enum": [1]}}}`:                                       at + ".properties.ok.enum: is only for a property of type string",
		`{"kind": "json_object_v1", "required": ["ok"], "properties": {"ok": {"type": "string", "enum": ["a", 1]}}}`:                                   at + ".properties.ok.enum: must hold strings only",
		`{"kind": "json_object_v1", "text_field": 1, "required": ["ok"], "properties": {"ok": {"type": "string"}}}`:                                    at + ".text_field: must be a string",
		`{"kind": "json_object_v1", "text_field": "reply", "required": ["ok"], "properties": {"ok": {"type": "string"}, "reply": {"type": "string"}}}`: at + ".text_field: must be listed in required",
		`{"kind": "json_object_v1", "text_field": "ok", "required": ["ok"], "properties": {"ok": {"type": "boolean"}}}`:                                at + ".text_field: must be a property of type string",
	} {
		assertErrorContains(t, envelope(contract), want)
	}
	// On a send_message too.
	data := strings.Replace(validWorkflowJSON(), `"meta": { "msg": "INVOICE_CREATE_REQUEST" },`, `"meta": { "msg": "INVOICE_CREATE_REQUEST", "context": {"response_envelope": {"kind": "json_object_v1"}} },`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].entry_actions[0].meta.context.response_envelope.required: must be a non-empty list of property names")
}
