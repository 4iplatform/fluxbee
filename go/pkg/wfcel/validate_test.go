package wfcel

import (
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
	data := strings.Replace(validWorkflowJSON(), `"customer_id": {"$ref": "input.customer_id"}`, `"customer_id": {"$ref": "customer_id"}`, 1)
	_, err := LoadDefinitionBytes([]byte(data), "", fixedClock)
	assertErrorContains(t, err, "states[0].entry_actions[0].payload.customer_id.$ref")
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
