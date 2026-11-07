package node

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"sort"
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

// A workflow name follows the runtime naming policy, since its runtime is wf.<workflow_name>: a
// hyphen, which the package's publish refused after the slots rotated (WF Case 5's gate), is refused
// at compile and at check, with the rule in the detail.
func TestCompileWorkflowRejectsInvalidWorkflowName(t *testing.T) {
	svc := newTestService(t)
	for _, name := range []string{"Invoice", "ventas-gate0195", "sales.gate-1", "9sales", "sales..gate", "sales."} {
		_, err := svc.CompileWorkflow(CompileRequest{WorkflowName: name, Definition: validWorkflowDefinition()})
		if err == nil || !strings.Contains(err.Error(), "INVALID_WORKFLOW_NAME") || !strings.Contains(err.Error(), "no hyphens") {
			t.Fatalf("%q: expected invalid name error, got %v", name, err)
		}
		if _, err := svc.CheckWorkflow(CompileRequest{WorkflowName: name, Definition: validWorkflowDefinition()}); err == nil {
			t.Fatalf("%q: the check passed a name the publish refuses", name)
		}
	}
	for _, name := range []string{"invoice", "ventas.gate0195", "sales.2026", "ventasgate0195"} {
		if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: name, Definition: validWorkflowDefinition()}); err != nil {
			t.Fatalf("%q: %v", name, err)
		}
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

// A solution's WFs and agents run on one hive (cognition-hands-v1 §12.4): a check and a compile
// refuse a definition that names an agent or a WF on another hive, naming each path, and stage
// nothing; an IO node, a door, may be on any hive.
func TestASolutionsAgentsAndWFsRunOnItsHive(t *testing.T) {
	naming := func(start string, targets ...string) map[string]any {
		definition := validWorkflowDefinition()
		definition["start_from"] = []any{start}
		state := definition["states"].([]any)[0].(map[string]any)
		for _, target := range targets {
			state["entry_actions"] = append(state["entry_actions"].([]any), map[string]any{
				"type": "send_message", "target": target, "meta": map[string]any{"msg": "ASK"},
				"payload": map[string]any{"text": "ready?"},
			})
		}
		return definition
	}
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: naming("IO.slack@worker1", "AI.billing@motherbee", "WF.audit@motherbee", "IO.out@worker1")}); err != nil {
		t.Fatalf("its own hive's agents and WFs, and IO nodes anywhere: %v", err)
	}

	svc = newTestService(t)
	away := naming("WF.client@worker1", "AI.sales@worker1", "WF.audit@motherbee")
	const why = "a solution's WFs and agents run on one hive, this one (motherbee): start_from[0]: WF.client@worker1 is on another hive; states[0].entry_actions[1].target: AI.sales@worker1 is on another hive"
	if _, err := svc.CheckWorkflow(CompileRequest{WorkflowName: "invoice", Definition: away}); err == nil || !strings.Contains(err.Error(), "COMPILE_ERROR") || !strings.Contains(err.Error(), why) {
		t.Fatalf("a check refuses another hive's agent and WF: %v", err)
	}
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: away}); err == nil || !strings.Contains(err.Error(), why) {
		t.Fatalf("a compile refuses another hive's agent and WF: %v", err)
	}
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice", "staged", "definition.json")); !os.IsNotExist(err) {
		t.Fatalf("expected no staged definition, got err=%v", err)
	}
}

// Case 4 (wf-orchestration-v1 §5.4.10): a known option compiles written by its name alone, and is
// staged as written: the WF node's load writes the catalog's description and params in, and
// WF_GET_CODE answers the definition as it was written. A known option written with any other field
// is refused, and nothing is staged.
func TestAKnownOptionIsStagedByItsNameAlone(t *testing.T) {
	withMenu := func(option map[string]any) map[string]any {
		definition := validWorkflowDefinition()
		state := definition["states"].([]any)[0].(map[string]any)
		state["entry_actions"] = append(state["entry_actions"].([]any), map[string]any{
			"type": "send_message", "target": "AI.billing@motherbee", "meta": map[string]any{"msg": "ASK"},
			"menu": map[string]any{"options": []any{option}}, "payload": map[string]any{"text": "ready?"},
		})
		return definition
	}
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: withMenu(map[string]any{"name": "answer"})}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	staged, err := svc.store.ReadDefinitionBytes("invoice", "staged")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(staged), `"name": "answer"`) || strings.Contains(string(staged), "Your reply goes to the person.") {
		t.Fatalf("the known option is staged as written:\n%s", staged)
	}

	svc = newTestService(t)
	_, err = svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: withMenu(map[string]any{"name": "answer", "description": "You answer."})})
	if err == nil || !strings.Contains(err.Error(), "COMPILE_ERROR") || !strings.Contains(err.Error(), "answer is a known option: write it by its name alone") {
		t.Fatalf("expected COMPILE_ERROR, got %v", err)
	}
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice", "staged", "definition.json")); !os.IsNotExist(err) {
		t.Fatalf("expected no staged definition, got err=%v", err)
	}
}

// A definition staged or backed up by an earlier release may no longer load: a known option written
// whole, say, refused since Case 4. Apply and rollback load it as the WF node will before they
// publish it, and answer COMPILE_ERROR with the paths the load names; the slots stay as they were,
// and nothing is published. Before, the node took it, and failed at its next start.
func TestApplyAndRollbackPublishNoDefinitionThatNoLongerLoads(t *testing.T) {
	stale := validWorkflowDefinition()
	state := stale["states"].([]any)[0].(map[string]any)
	state["entry_actions"] = append(state["entry_actions"].([]any), map[string]any{
		"type": "send_message", "target": "AI.billing@motherbee", "meta": map[string]any{"msg": "ASK"},
		"menu":    map[string]any{"options": []any{map[string]any{"name": "answer", "description": "You answer."}}},
		"payload": map[string]any{"text": "ready?"},
	})
	staleBytes, err := normalizeDefinitionBytes(stale)
	if err != nil {
		t.Fatal(err)
	}
	const why = "states[0].entry_actions[1].menu.options[0]: answer is a known option: write it by its name alone"
	svc := newTestService(t)
	admin := svc.admin.(*fakeAdminClient)
	slotBecomesStale := func(slot string) {
		t.Helper()
		if err := os.WriteFile(filepath.Join(svc.store.Root(), "invoice", slot, "definition.json"), staleBytes, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	compileAndApply := func() {
		t.Helper()
		if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
			t.Fatal(err)
		}
		if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
			t.Fatal(err)
		}
	}
	current := func() uint64 {
		t.Helper()
		meta, err := svc.store.ReadCurrentMetadata("invoice")
		if err != nil {
			t.Fatal(err)
		}
		return meta.Version
	}

	// Apply: version 2 is staged, and its definition no longer loads.
	compileAndApply()
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatal(err)
	}
	slotBecomesStale("staged")
	published := admin.publishCalls
	_, err = svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"})
	assertCompileError := func(err error, slot string, version uint64) {
		t.Helper()
		if err == nil || !strings.Contains(err.Error(), "COMPILE_ERROR") || !strings.Contains(err.Error(), fmt.Sprintf("the %s definition (version %d) no longer loads", slot, version)) ||
			!strings.Contains(err.Error(), why) {
			t.Fatalf("expected COMPILE_ERROR for the %s definition, got %v", slot, err)
		}
	}
	assertCompileError(err, "staged", 2)
	if _, err := svc.store.ReadStagedMetadata("invoice"); err != nil || current() != 1 || admin.publishCalls != published {
		t.Fatalf("an apply that fails changes nothing: staged %v, current %d, %d publishes", err, current(), admin.publishCalls-published)
	}

	// Rollback: version 3 is applied over version 1, whose backed-up definition no longer loads.
	compileAndApply()
	slotBecomesStale("backup")
	published = admin.publishCalls
	_, err = svc.RollbackWorkflow(RollbackRequest{WorkflowName: "invoice"})
	assertCompileError(err, "backup", 1)
	if _, err := svc.store.ReadBackupMetadata("invoice"); err != nil || current() != 3 || admin.publishCalls != published {
		t.Fatalf("a rollback that fails changes nothing: backup %v, current %d, %d publishes", err, current(), admin.publishCalls-published)
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

// wfRulesCheck is SY.admin's check of a definition (wf-orchestration-v1 §5.5.1), from sender: a
// CONFIG_SET whose config is a compile request's fields, with operation check and a check_id.
func wfRulesCheck(t *testing.T, sender, checkID, workflow string, definition any) fluxbeesdk.Message {
	t.Helper()
	payload, err := json.Marshal(map[string]any{
		"node_name": "SY.wf-rules@motherbee", "schema_version": 1, "config_version": 0, "apply_mode": "replace",
		"config": map[string]any{"operation": "check", "check_id": checkID, "workflow_name": workflow, "definition": definition},
	})
	if err != nil {
		t.Fatal(err)
	}
	configSet := fluxbeesdk.MSGConfigSet
	return fluxbeesdk.Message{
		Routing: fluxbeesdk.Routing{Src: "admin-uuid", SrcL2Name: &sender, TraceID: "trace-" + checkID},
		Meta:    fluxbeesdk.Meta{MsgType: fluxbeesdk.SYSTEMKind, Msg: &configSet},
		Payload: payload,
	}
}

// snapshotTree maps each directory and file under roots to what it holds: a file its content, a
// directory "(dir)".
func snapshotTree(t *testing.T, roots ...string) map[string]string {
	t.Helper()
	tree := map[string]string{}
	for _, root := range roots {
		err := filepath.WalkDir(root, func(path string, entry os.DirEntry, err error) error {
			if err != nil {
				return err
			}
			if entry.IsDir() {
				tree[path] = "(dir)"
				return nil
			}
			data, err := os.ReadFile(path)
			tree[path] = string(data)
			return err
		})
		if err != nil {
			t.Fatal(err)
		}
	}
	return tree
}

// A check is a read (wf-orchestration-v1 §5.5.1), as SY.opa-rules' is: SY.admin's CONFIG_SET with
// operation check validates the definition as a compile does, with a compile's codes and the exact
// paths of its errors, and is answered at once, echoing its check_id, whatever the outcome. Between a
// compile and its apply it changes nothing: no file under the state dir or the dist, no package
// published, no version taken. The apply that follows applies what was compiled before the checks,
// at its version.
func TestACheckIsAReadThatEchoesItsCheckID(t *testing.T) {
	svc := newTestService(t)
	admin := svc.admin.(*fakeAdminClient)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatal(err)
	}
	second := validWorkflowDefinition()
	second["description"] = "issues invoice, its second version"
	staged, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: second})
	if err != nil {
		t.Fatal(err)
	}
	before, published := snapshotTree(t, svc.store.Root(), svc.cfg.DistRuntimeRoot), admin.publishCalls

	checked := validWorkflowDefinition()
	checked["description"] = "a definition that is only checked"
	badGuard := validWorkflowDefinition()
	badGuard["states"].([]any)[0].(map[string]any)["transitions"].([]any)[0].(map[string]any)["guard"] = "event.payload.complete =="
	knownOptionWhole := validWorkflowDefinition()
	state := knownOptionWhole["states"].([]any)[0].(map[string]any)
	state["entry_actions"] = append(state["entry_actions"].([]any), map[string]any{
		"type": "send_message", "target": "AI.billing@motherbee", "meta": map[string]any{"msg": "ASK"},
		"menu":    map[string]any{"options": []any{map[string]any{"name": "answer", "description": "You answer."}}},
		"payload": map[string]any{"text": "ready?"},
	})
	cases := []struct {
		name, sender, workflow string
		definition             any
		code, path             string // a refusal's code, and the path its detail starts with
	}{
		{name: "a valid definition", workflow: "invoice", definition: checked},
		{name: "a workflow the store does not have", workflow: "billing", definition: checked},
		{name: "a guard that does not compile", workflow: "invoice", definition: badGuard, code: "COMPILE_ERROR", path: "states[0].transitions[0].guard: "},
		{name: "a known option written whole", workflow: "invoice", definition: knownOptionWhole, code: "COMPILE_ERROR",
			path: "states[0].entry_actions[1].menu.options[0]: answer is a known option: write it by its name alone"},
		{name: "an invalid workflow name", workflow: "Invoice", definition: checked, code: "INVALID_WORKFLOW_NAME"},
		{name: "no definition", workflow: "invoice", code: "INVALID_CONFIG_SET"},
		{name: "not from the admin", sender: "AI.rogue@motherbee", workflow: "invoice", definition: checked, code: "UNAUTHORIZED"},
	}
	for i, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			sender := &recordingSender{}
			svc.sender = sender
			from := tc.sender
			if from == "" {
				from = fluxbeesdk.PrimaryAdminNode
			}
			checkID := fmt.Sprintf("check-%d", i)
			svc.handleMessage(wfRulesCheck(t, from, checkID, tc.workflow, tc.definition))
			if len(sender.sent) != 1 {
				t.Fatalf("sent %d answers, want 1", len(sender.sent))
			}
			reply := sender.sent[0]
			if stringValue(reply.Meta.Msg) != fluxbeesdk.MSGConfigResponse || reply.Routing.Dst.Value() != "admin-uuid" || reply.Routing.TraceID != "trace-"+checkID {
				t.Fatalf("a CONFIG_RESPONSE to the requester, on its trace: %s to %s on %s", stringValue(reply.Meta.Msg), reply.Routing.Dst.Value(), reply.Routing.TraceID)
			}
			var answer map[string]any
			if err := json.Unmarshal(reply.Payload, &answer); err != nil {
				t.Fatal(err)
			}
			if answer["operation"] != "check" || answer["check_id"] != checkID || answer["node_name"] != "SY.wf-rules@motherbee" {
				t.Fatalf("an answer to a check names it and echoes its check_id: %v", answer)
			}
			if tc.code == "" {
				keys := make([]string, 0, len(answer))
				for key := range answer {
					keys = append(keys, key)
				}
				sort.Strings(keys)
				if strings.Join(keys, ",") != "check_id,guard_count,node_name,ok,operation,state_count,workflow_name" || answer["ok"] != true ||
					answer["workflow_name"] != tc.workflow || answer["guard_count"] != float64(1) || answer["state_count"] != float64(2) {
					t.Fatalf("a definition that loads: %v", answer)
				}
			} else {
				refusal, _ := answer["error"].(map[string]any)
				detail, _ := refusal["detail"].(string)
				if answer["ok"] != false || answer["state"] != "error" || refusal["code"] != tc.code || !strings.HasPrefix(detail, tc.path) {
					t.Fatalf("want %s at %q, got %v", tc.code, tc.path, answer)
				}
				// From the admin, the refusal is the one a compile of the same request answers, with the
				// request's config_version.
				if tc.sender == "" {
					_, err := newTestService(t).CompileWorkflow(CompileRequest{WorkflowName: tc.workflow, Definition: tc.definition})
					compiled, ok := err.(WfRulesError)
					if !ok || compiled.Code != tc.code || compiled.Detail != detail || answer["config_version"] != float64(0) {
						t.Fatalf("a compile answers %v, the check %v", err, answer)
					}
				}
			}
			if after := snapshotTree(t, svc.store.Root(), svc.cfg.DistRuntimeRoot); !reflect.DeepEqual(after, before) || admin.publishCalls != published {
				t.Fatalf("a check changed the store or the dist (%d publishes):\nbefore %v\nafter  %v", admin.publishCalls-published, before, after)
			}
		})
	}

	// An answer refused as over the frame limit is replaced by the error reply, which echoes the
	// check_id too.
	sender := &recordingSender{refuse: 1}
	svc.sender = sender
	svc.handleMessage(wfRulesCheck(t, fluxbeesdk.PrimaryAdminNode, "check-refused", "invoice", checked))
	if len(sender.sent) != 1 || !strings.Contains(string(sender.sent[0].Payload), `"check_id":"check-refused"`) ||
		!strings.Contains(string(sender.sent[0].Payload), `"WFRULES_ERROR"`) {
		t.Fatalf("the error reply to a check: %+v", sender.sent)
	}

	// The apply after the checks applies what was staged before them, at its version, and the next
	// compile takes the version after it.
	result, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice", Version: staged.Version})
	if err != nil || result.Current.Version != 2 || result.Current.Hash != staged.Hash {
		t.Fatalf("the apply after the checks: %+v %v", result, err)
	}
	if next := svc.store.NextVersion("invoice"); next != 3 || svc.store.WorkflowExists("billing") {
		t.Fatalf("the checks took a version (next %d) or wrote a workflow", next)
	}
}
