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
		if !isValidRefPath(p) {
			t.Fatalf("%q should be a valid ref path", p)
		}
	}
	for _, p := range []string{"foo.bar", "state", "state.", ".x", ""} {
		if isValidRefPath(p) {
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
