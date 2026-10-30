package node

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

func TestCompileWorkflowWritesStagedFiles(t *testing.T) {
	svc := newTestService(t)
	meta, err := svc.CompileWorkflow(CompileRequest{
		WorkflowName: "invoice",
		Definition:   validWorkflowDefinition(),
	})
	if err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if meta.Version != 1 {
		t.Fatalf("unexpected version %d", meta.Version)
	}
	if meta.GuardCount != 1 || meta.StateCount != 2 || meta.ActionCount != 3 {
		t.Fatalf("unexpected counts %#v", meta)
	}
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice", "staged", "definition.json")); err != nil {
		t.Fatalf("missing staged definition: %v", err)
	}
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice", "staged", "metadata.json")); err != nil {
		t.Fatalf("missing staged metadata: %v", err)
	}
}

// The catch's guards and actions count as the definition's own (wf-orchestration-v1 §5.3.6).
func TestCompileWorkflowCountsTheCatch(t *testing.T) {
	svc := newTestService(t)
	definition := validWorkflowDefinition()
	definition["catch"] = []any{
		map[string]any{
			"event_match": map[string]any{},
			"guard":       "event.payload.type == 'error'",
			"actions": []any{
				map[string]any{"type": "set_variable", "name": "last_error", "value": "event.payload.code"},
				map[string]any{"type": "emit_internal_event", "meta": map[string]any{"msg": "ERRED"}},
			},
		},
	}
	meta, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: definition})
	if err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if meta.GuardCount != 2 || meta.StateCount != 2 || meta.ActionCount != 5 {
		t.Fatalf("unexpected counts %#v", meta)
	}
}

func TestCompileWorkflowAutoIncrementsVersion(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("first CompileWorkflow: %v", err)
	}
	meta, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()})
	if err != nil {
		t.Fatalf("second CompileWorkflow: %v", err)
	}
	if meta.Version != 2 {
		t.Fatalf("unexpected version %d", meta.Version)
	}
}

func TestCompileWorkflowRejectsInvalidWorkflowName(t *testing.T) {
	svc := newTestService(t)
	_, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "Invoice", Definition: validWorkflowDefinition()})
	if err == nil || !strings.Contains(err.Error(), "INVALID_WORKFLOW_NAME") {
		t.Fatalf("expected invalid name error, got %v", err)
	}
}

func TestCompileWorkflowRejectsInvalidDefinitionAndDoesNotWriteStaged(t *testing.T) {
	svc := newTestService(t)
	bad := validWorkflowDefinition()
	states := bad["states"].([]any)
	firstState := states[0].(map[string]any)
	transitions := firstState["transitions"].([]any)
	firstTransition := transitions[0].(map[string]any)
	firstTransition["guard"] = "event.payload.complete =="

	_, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: bad})
	if err == nil || !strings.Contains(err.Error(), "COMPILE_ERROR") {
		t.Fatalf("expected COMPILE_ERROR, got %v", err)
	}
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice", "staged", "metadata.json")); !os.IsNotExist(err) {
		t.Fatalf("expected no staged metadata, got err=%v", err)
	}
}

func newTestService(t *testing.T) *Service {
	t.Helper()
	cfg := NodeConfig{
		HiveID:          "motherbee",
		NodeName:        "SY.wf-rules@motherbee",
		StateDir:        t.TempDir(),
		DistRuntimeRoot: filepath.Join(t.TempDir(), "dist", "runtimes"),
	}
	svc := NewService(cfg, nil, nil, func() time.Time {
		return time.Date(2026, 4, 16, 12, 0, 0, 0, time.UTC)
	})
	svc.admin = &fakeAdminClient{
		publishRuntimePackageFunc: func(_ context.Context, packageFiles map[string]string) (*PackagePublishResult, error) {
			return installWorkflowPackageViaFakeAdmin(svc, packageFiles)
		},
	}
	return svc
}

func TestApplyWorkflowRotatesAndPublishesPackage(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{
		WorkflowName: "invoice",
		Definition:   validWorkflowDefinition(),
	}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	result, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"})
	if err != nil {
		t.Fatalf("ApplyWorkflow: %v", err)
	}
	if result.Current.Version != 1 {
		t.Fatalf("unexpected current version %d", result.Current.Version)
	}
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice", "current", "metadata.json")); err != nil {
		t.Fatalf("missing current metadata: %v", err)
	}
	if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", "0.0.1", "package.json")); err != nil {
		t.Fatalf("missing package.json: %v", err)
	}
	if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", "0.0.1", "flow", "definition.json")); err != nil {
		t.Fatalf("missing flow/definition.json: %v", err)
	}
	manifestPath := filepath.Join(svc.cfg.DistRuntimeRoot, "manifest.json")
	manifestBytes, err := os.ReadFile(manifestPath)
	if err != nil {
		t.Fatalf("read manifest: %v", err)
	}
	if !strings.Contains(string(manifestBytes), `"wf.invoice"`) {
		t.Fatalf("manifest missing runtime entry: %s", string(manifestBytes))
	}
}

func TestApplyWorkflowRequiresStaged(t *testing.T) {
	svc := newTestService(t)
	_, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"})
	if err == nil || !strings.Contains(err.Error(), "NOTHING_STAGED") {
		t.Fatalf("expected NOTHING_STAGED, got %v", err)
	}
}

func validWorkflowDefinition() map[string]any {
	return map[string]any{
		"wf_schema_version": "1",
		"workflow_type":     "invoice",
		"description":       "issues invoice",
		"start_from":        []any{"IO.api.billing@motherbee"},
		"input_schema": map[string]any{
			"type":     "object",
			"required": []any{"customer_id"},
			"properties": map[string]any{
				"customer_id": map[string]any{"type": "string"},
			},
		},
		"initial_state":   "collecting_data",
		"terminal_states": []any{"completed"},
		"states": []any{
			map[string]any{
				"name":        "collecting_data",
				"description": "collects invoice data",
				"entry_actions": []any{
					map[string]any{
						"type":   "send_message",
						"target": "IO.quickbooks@motherbee",
						"meta": map[string]any{
							"msg": "INVOICE_CREATE_REQUEST",
						},
						"payload": map[string]any{
							"customer_id": map[string]any{"$ref": "input.customer_id"},
						},
					},
				},
				"exit_actions": []any{},
				"transitions": []any{
					map[string]any{
						"event_match":  map[string]any{"msg": "DATA_VALIDATION_RESPONSE"},
						"guard":        "event.payload.complete == true",
						"target_state": "completed",
						"actions": []any{
							map[string]any{
								"type":  "set_variable",
								"name":  "validated_at",
								"value": "now()",
							},
							map[string]any{
								"type":      "schedule_timer",
								"timer_key": "invoice_timeout",
								"fire_in":   "5m",
							},
						},
					},
				},
			},
			map[string]any{
				"name":          "completed",
				"description":   "done",
				"entry_actions": []any{},
				"exit_actions":  []any{},
				"transitions":   []any{},
			},
		},
	}
}

// A definition's numbers are staged as written, through the command and through CONFIG_SET: decoded
// as float64, 1.0 was staged as 1 (an integer to the engine, which keeps numbers as written since
// 0.1.90) and an integer past 2^53 lost its last digits.
func TestADefinitionIsStagedWithItsNumbersAsWritten(t *testing.T) {
	const definition = `{"wf_schema_version": "1", "workflow_type": "numbers", "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object", "properties": {"limit": {"type": "integer", "maximum": 10}}},
  "initial_state": "sending", "terminal_states": ["sent"],
  "states": [
    {"name": "sending",
     "entry_actions": [{"type": "send_message", "target": "IO.out@motherbee", "meta": {"msg": "NUMBERS"},
                        "payload": {"count": 3, "ratio": 2.5, "one": 1.0, "big": 9007199254740993}}],
     "transitions": [{"event_match": {"msg": "NUMBERS"}, "guard": "true", "target_state": "sent"}]},
    {"name": "sent"}
  ]}`
	admin := fluxbeesdk.PrimaryAdminNode
	compile, configSet := "compile_workflow", fluxbeesdk.MSGConfigSet
	for name, msg := range map[string]fluxbeesdk.Message{
		"compile_workflow": {
			Meta:    fluxbeesdk.Meta{MsgType: "command", Action: &compile},
			Payload: json.RawMessage(`{"workflow_name": "numbers", "definition": ` + definition + `}`),
		},
		"CONFIG_SET": {
			Meta: fluxbeesdk.Meta{MsgType: fluxbeesdk.SYSTEMKind, Msg: &configSet},
			Payload: json.RawMessage(`{"node_name": "SY.wf-rules@motherbee", "schema_version": 1, "config_version": 0, "apply_mode": "replace",
  "config": {"operation": "compile", "workflow_name": "numbers", "definition": ` + definition + `}}`),
		},
	} {
		t.Run(name, func(t *testing.T) {
			svc := newTestService(t)
			sender := &recordingSender{}
			svc.sender = sender
			msg.Routing = fluxbeesdk.Routing{Src: "admin-uuid", SrcL2Name: &admin, TraceID: "t-1"}
			svc.handleMessage(msg)
			if len(sender.sent) != 1 {
				t.Fatalf("sent %d", len(sender.sent))
			}
			staged, err := svc.store.ReadDefinitionBytes("numbers", "staged")
			if err != nil {
				t.Fatalf("not staged (%s): %v", sender.sent[0].Payload, err)
			}
			for _, want := range []string{`"count": 3,`, `"ratio": 2.5`, `"one": 1.0,`, `"big": 9007199254740993,`, `"maximum": 10`} {
				if !strings.Contains(string(staged), want) {
					t.Fatalf("%s is not staged as written:\n%s", want, staged)
				}
			}
		})
	}
}
