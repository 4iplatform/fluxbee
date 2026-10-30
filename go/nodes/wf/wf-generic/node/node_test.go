package node

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

const testNodeName = "WF.invoice@motherbee"

// writeWorkflowPackage writes a workflow package with the given definition and returns its path.
func writeWorkflowPackage(t *testing.T, definition string) string {
	t.Helper()
	pkgPath := filepath.Join(t.TempDir(), "runtimes", "wf.invoice", "3")
	if err := os.MkdirAll(filepath.Join(pkgPath, "flow"), 0o755); err != nil {
		t.Fatal(err)
	}
	if definition != "" {
		if err := os.WriteFile(filepath.Join(pkgPath, "flow", "definition.json"), []byte(definition), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return pkgPath
}

// writeNodeConfig writes config.json in a node dir, as the orchestrator lays it out.
func writeNodeConfig(t *testing.T, document map[string]any) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "WF", testNodeName, "config.json")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	raw, err := json.MarshalIndent(document, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, raw, 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

// spawnConfig is config.json as the orchestrator writes it for a workflow package: the package's
// default config, the tenant, and `_system`.
func spawnConfig(pkgPath string) map[string]any {
	return map[string]any{
		"sy_timer_l2_name":    "SY.timer@motherbee",
		"gc_retention_days":   7,
		"gc_interval_seconds": 3600,
		"tenant_id":           "tnt:43d576a3-d712-4d91-9245-5d5463dd693e",
		"_system": map[string]any{
			"managed_by":                "SY.orchestrator",
			"node_name":                 testNodeName,
			"hive_id":                   "motherbee",
			"relaunch_on_boot":          true,
			"config_version":            4,
			"runtime":                   "wf.invoice",
			"runtime_version":           "3",
			"requested_runtime_version": "current",
			"runtime_base":              "wf.engine",
			"package_path":              pkgPath,
			"ilk_id":                    "ilk:11111111-1111-4111-8111-111111111111",
			"tenant_id":                 "tnt:43d576a3-d712-4d91-9245-5d5463dd693e",
			"launched_by":               "operator",
		},
	}
}

func bootstrap(t *testing.T, configPath string) *ControlPlane {
	t.Helper()
	plane, err := BootstrapControlPlane(configPath, testNodeName, fixedClock)
	if err != nil {
		t.Fatalf("BootstrapControlPlane: %v", err)
	}
	return plane
}

func TestAWorkflowPackageRunsOnWhatTheOrchestratorWrites(t *testing.T) {
	pkgPath := writeWorkflowPackage(t, validWorkflowJSON())
	configPath := writeNodeConfig(t, spawnConfig(pkgPath))
	plane := bootstrap(t, configPath)
	materialized := plane.Configured()
	if materialized == nil {
		t.Fatalf("expected CONFIGURED, got %+v", plane.State)
	}
	cfg := materialized.Config
	if cfg.WorkflowDefinitionPath != filepath.Join(pkgPath, "flow", "definition.json") {
		t.Fatalf("definition = %q", cfg.WorkflowDefinitionPath)
	}
	if cfg.DBPath != filepath.Join(filepath.Dir(configPath), "wf_instances.db") {
		t.Fatalf("db_path = %q", cfg.DBPath)
	}
	if cfg.SYTimerL2Name != "SY.timer@motherbee" || cfg.TenantID != "tnt:43d576a3-d712-4d91-9245-5d5463dd693e" {
		t.Fatalf("cfg = %+v", cfg)
	}
	if materialized.Definition.WorkflowType != "invoice" || plane.State.ConfigVersion != 4 {
		t.Fatalf("materialized = %+v, state = %+v", materialized.Definition, plane.State)
	}
}

// The orchestrator owns `_system` and adds keys over time (launched_by in stage 4 crash-looped the
// strict decoder this node used to have): a key the node does not know never stops it.
func TestAnUnknownSystemKeyNeverStopsTheNode(t *testing.T) {
	document := spawnConfig(writeWorkflowPackage(t, validWorkflowJSON()))
	document["_system"].(map[string]any)["a_key_from_a_newer_orchestrator"] = map[string]any{"nested": true}
	plane := bootstrap(t, writeNodeConfig(t, document))
	if plane.Configured() == nil {
		t.Fatalf("expected CONFIGURED, got %+v", plane.State)
	}
}

func TestTenantAndHiveComeFromSystemOrTheNodeName(t *testing.T) {
	pkgPath := writeWorkflowPackage(t, validWorkflowJSON())
	plane := bootstrap(t, writeNodeConfig(t, map[string]any{
		"_system": map[string]any{"tenant_id": "tnt:from-system", "package_path": pkgPath},
	}))
	materialized := plane.Configured()
	if materialized == nil {
		t.Fatalf("expected CONFIGURED, got %+v", plane.State)
	}
	if materialized.Config.TenantID != "tnt:from-system" || materialized.Config.SYTimerL2Name != "SY.timer@motherbee" {
		t.Fatalf("cfg = %+v", materialized.Config)
	}
	if materialized.Config.GCRetentionDays != 7 || materialized.Config.GCIntervalSeconds != 3600 {
		t.Fatalf("GC defaults: %+v", materialized.Config)
	}
}

func TestAConfigTheNodeCannotUseIsFailedConfigNotAnExit(t *testing.T) {
	brokenDefinition := writeWorkflowPackage(t, `{"wf_schema_version": "1", "states": "not a list"}`)
	missingDefinition := writeWorkflowPackage(t, "")
	typo := spawnConfig(writeWorkflowPackage(t, validWorkflowJSON()))
	typo["gc_retention_day"] = 3
	cases := []struct {
		name     string
		document map[string]any
		raw      string
		code     string
		mention  string
	}{
		{name: "broken definition", document: spawnConfig(brokenDefinition), code: ErrCodeInvalidDefinition},
		{name: "missing definition", document: spawnConfig(missingDefinition), code: ErrCodeInvalidDefinition, mention: "definition.json"},
		{name: "unknown operator key", document: typo, code: sdk.ErrCodeInvalidConfig, mention: "gc_retention_day"},
		{name: "not JSON", raw: `{"sy_timer_l2_name":`, code: sdk.ErrCodeInvalidConfig},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			var configPath string
			if c.raw != "" {
				configPath = writeNodeConfig(t, map[string]any{})
				if err := os.WriteFile(configPath, []byte(c.raw), 0o600); err != nil {
					t.Fatal(err)
				}
			} else {
				configPath = writeNodeConfig(t, c.document)
			}
			plane := bootstrap(t, configPath)
			if plane.State.CurrentState != sdk.ManagedFailedConfig || plane.State.LastError == nil {
				t.Fatalf("expected FAILED_CONFIG, got %+v", plane.State)
			}
			if plane.State.LastError.Code != c.code || !strings.Contains(plane.State.LastError.Message, c.mention) {
				t.Fatalf("last_error = %+v", plane.State.LastError)
			}
			if err := Run(context.Background(), RunOptions{Plane: plane}); err == nil {
				t.Fatal("a dry run reports the config it cannot use")
			}
		})
	}
}

// --- control messages ---

func newRuntime(t *testing.T, plane *ControlPlane, withEngine bool) (*NodeRuntime, *mockDispatcher) {
	t.Helper()
	disp := &mockDispatcher{l2name: testNodeName, uuid: "wf-uuid"}
	actx := makeActx(t, disp, &mockTimerSender{})
	rt := &NodeRuntime{ActCtx: actx, NodeUUID: "wf-uuid", NodeName: testNodeName, Plane: plane}
	if withEngine {
		rt.Def = mustLoadValidDefinition(t)
		rt.DefJSON = validWorkflowJSON()
		rt.Registry = NewInstanceRegistry()
		rt.Store = actx.Store
	}
	return rt, disp
}

func configSet(t *testing.T, version uint64, config map[string]any) sdk.Message {
	t.Helper()
	msg, err := sdk.BuildNodeConfigSetMessage("caller-uuid", testNodeName, sdk.NodeConfigSetPayload{
		NodeName:      testNodeName,
		SchemaVersion: 1,
		ConfigVersion: version,
		ApplyMode:     sdk.NodeConfigApplyModeReplace,
		Config:        config,
	}, sdk.NodeConfigEnvelopeOptions{}, "trace-config-set")
	if err != nil {
		t.Fatal(err)
	}
	return msg
}

func configGet(t *testing.T) sdk.Message {
	t.Helper()
	msg, err := sdk.BuildNodeConfigGetMessage("caller-uuid", testNodeName, sdk.NodeConfigGetPayload{NodeName: testNodeName}, sdk.NodeConfigEnvelopeOptions{}, "trace-config-get")
	if err != nil {
		t.Fatal(err)
	}
	return msg
}

func lastReply(t *testing.T, disp *mockDispatcher) map[string]any {
	t.Helper()
	if len(disp.sent) == 0 {
		t.Fatal("no reply sent")
	}
	var payload map[string]any
	if err := json.Unmarshal(disp.sent[len(disp.sent)-1].Payload, &payload); err != nil {
		t.Fatal(err)
	}
	return payload
}

func TestConfigGetAnswersTheConfigState(t *testing.T) {
	plane := bootstrap(t, writeNodeConfig(t, spawnConfig(writeWorkflowPackage(t, validWorkflowJSON()))))
	rt, disp := newRuntime(t, plane, true)
	if err := Dispatch(context.Background(), configGet(t), rt); err != nil {
		t.Fatal(err)
	}
	if got := derefString(disp.sent[0].Meta.Msg); got != sdk.MSGConfigResponse {
		t.Fatalf("reply msg = %q", got)
	}
	reply := lastReply(t, disp)
	if reply["ok"] != true || reply["state"] != "CONFIGURED" || reply["node_name"] != testNodeName || reply["error"] != nil {
		t.Fatalf("reply = %v", reply)
	}
	effective := reply["effective_config"].(map[string]any)
	if effective["workflow_definition_source"] != "managed_package" || effective["sy_timer_l2_name"] != "SY.timer@motherbee" {
		t.Fatalf("effective_config = %v", effective)
	}
}

func TestConfigSetKeepsSystemAndWaitsForARestart(t *testing.T) {
	document := spawnConfig(writeWorkflowPackage(t, validWorkflowJSON()))
	document["_system"].(map[string]any)["relaunch_on_boot"] = false
	document["_system"].(map[string]any)["a_key_from_a_newer_orchestrator"] = "kept"
	configPath := writeNodeConfig(t, document)
	plane := bootstrap(t, configPath)
	rt, disp := newRuntime(t, plane, true)

	err := Dispatch(context.Background(), configSet(t, 5, map[string]any{
		"sy_timer_l2_name":    "SY.timer@motherbee",
		"gc_retention_days":   14,
		"gc_interval_seconds": 900,
		"tenant_id":           "tnt:43d576a3-d712-4d91-9245-5d5463dd693e",
	}), rt)
	if err != nil {
		t.Fatal(err)
	}
	reply := lastReply(t, disp)
	if reply["ok"] != true || reply["restart_required"] != true || reply["applied"] != false || reply["config_version"] != float64(5) {
		t.Fatalf("reply = %v", reply)
	}

	persisted := readJSONFile(t, configPath)
	system := persisted["_system"].(map[string]any)
	if system["config_version"] != float64(5) || system["relaunch_on_boot"] != false || system["a_key_from_a_newer_orchestrator"] != "kept" || system["launched_by"] != "operator" {
		t.Fatalf("_system after CONFIG_SET = %v", system)
	}
	if persisted["gc_retention_days"] != float64(14) {
		t.Fatalf("persisted = %v", persisted)
	}
	for _, key := range []string{"config_version", "workflow_definition_path"} {
		if _, present := persisted[key]; present {
			t.Fatalf("persisted carries %s: %v", key, persisted)
		}
	}

	if plane.Configured().Config.GCRetentionDays != 7 {
		t.Fatal("the running config does not change before a restart")
	}
	disp.sent = nil
	if err := Dispatch(context.Background(), configGet(t), rt); err != nil {
		t.Fatal(err)
	}
	pending, _ := lastReply(t, disp)["pending_restart"].(map[string]any)
	if pending["config_version"] != float64(5) {
		t.Fatalf("CONFIG_GET shows the pending config: %v", lastReply(t, disp))
	}
	restarted := bootstrap(t, configPath)
	if restarted.Configured() == nil || restarted.Configured().Config.GCRetentionDays != 14 || restarted.State.ConfigVersion != 5 {
		t.Fatalf("a restart applies it: %+v", restarted.State)
	}
}

func TestConfigSetRefusalsSayWhyAndChangeNothing(t *testing.T) {
	configPath := writeNodeConfig(t, spawnConfig(writeWorkflowPackage(t, validWorkflowJSON())))
	before, _ := os.ReadFile(configPath)
	plane := bootstrap(t, configPath)
	cases := []struct {
		name    string
		version uint64
		config  map[string]any
		code    string
	}{
		{name: "_system", version: 5, config: map[string]any{"_system": map[string]any{"node_name": "WF.evil@motherbee"}}, code: sdk.ErrCodeInvalidConfig},
		{name: "stale version", version: 4, config: map[string]any{"gc_retention_days": 3}, code: errCodeStaleConfigVersion},
		{name: "unknown key", version: 5, config: map[string]any{"gc_retention_day": 3}, code: sdk.ErrCodeInvalidConfig},
		{name: "empty", version: 5, config: map[string]any{}, code: sdk.ErrCodeInvalidConfig},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			rt, disp := newRuntime(t, plane, true)
			if err := Dispatch(context.Background(), configSet(t, c.version, c.config), rt); err != nil {
				t.Fatal(err)
			}
			reply := lastReply(t, disp)
			errObj, _ := reply["error"].(map[string]any)
			message, _ := errObj["message"].(string)
			if reply["ok"] != false || errObj["code"] != c.code || message == "" {
				t.Fatalf("reply = %v", reply)
			}
			if reply["state"] != "CONFIGURED" || reply["config_version"] != float64(4) {
				t.Fatalf("a refusal reports the node's own state and version: %v", reply)
			}
		})
	}
	after, _ := os.ReadFile(configPath)
	if string(before) != string(after) {
		t.Fatal("a refused CONFIG_SET changes nothing")
	}
}

func TestANodeWithoutAConfigInUseAnswersControlAndDropsWorkflowTraffic(t *testing.T) {
	configPath := writeNodeConfig(t, spawnConfig(writeWorkflowPackage(t, `{"states": 1}`)))
	plane := bootstrap(t, configPath)
	rt, disp := newRuntime(t, plane, false)

	statusName := sdk.MSGNodeStatusGet
	status := sdk.Message{Routing: sdk.Routing{Src: "orchestrator-uuid", TraceID: "t-status"}, Meta: sdk.Meta{MsgType: "SYSTEM", Msg: &statusName}}
	if err := Dispatch(context.Background(), status, rt); err != nil {
		t.Fatal(err)
	}
	reply := lastReply(t, disp)
	lastError, _ := reply["last_error"].(map[string]any)
	if reply["health_state"] != "DEGRADED" || lastError["code"] != ErrCodeInvalidDefinition {
		t.Fatalf("NODE_STATUS_GET = %v", reply)
	}

	if err := Dispatch(context.Background(), configGet(t), rt); err != nil {
		t.Fatal(err)
	}
	reply = lastReply(t, disp)
	if reply["ok"] != false || reply["state"] != "FAILED_CONFIG" || reply["error"] == nil || reply["effective_config"] != nil {
		t.Fatalf("CONFIG_GET = %v", reply)
	}

	if err := Dispatch(context.Background(), from(sdk.PrimaryAdminNode, msgWithName(MsgWFListInstances)), rt); err != nil {
		t.Fatal(err)
	}
	if got := derefString(disp.sent[len(disp.sent)-1].Meta.Msg); got != MsgWFListInstancesResponse {
		t.Fatalf("reply msg = %q", got)
	}
	if reply = lastReply(t, disp); reply["ok"] != false || reply["error"] != errCodeNodeNotConfigured {
		t.Fatalf("WF_LIST_INSTANCES = %v", reply)
	}

	sent := len(disp.sent)
	if err := Dispatch(context.Background(), from("IO.api.billing@motherbee", msgWithName("INVOICE_REQUEST")), rt); err != nil {
		t.Fatal(err)
	}
	if len(disp.sent) != sent {
		t.Fatal("workflow traffic is dropped, not answered")
	}
}

// A reply refused as over the frame limit is answered with the error its handler uses for other
// failures, so the requester does not wait out its timeout. Any other send failure (the queue full
// during a reconnect) gets no error reply: the work may be done, and the requester's timeout reads
// "unknown, not failed". Either failure reaches the receive loop's log; the status reply has no
// error form (A-75).
func TestARefusedReplyIsAnsweredWithAnError(t *testing.T) {
	plane := bootstrap(t, writeNodeConfig(t, spawnConfig(writeWorkflowPackage(t, validWorkflowJSON()))))
	statusGet := sdk.MSGNodeStatusGet
	tooLarge := &sdk.MessageTooLargeError{Size: 200 * 1024, Max: 128 * 1024}
	queueFull := errors.New("sender queue full")
	cases := []struct {
		name    string
		msg     sdk.Message
		sendErr error  // how the first send fails
		reply   string // the error reply's msg; empty when nothing replaces the failed reply
		failed  string // the error code, as the error reply carries it
	}{
		{name: "WF_GET_CODE", msg: from(sdk.PrimaryAdminNode, msgWithName(MsgWFGetCode)), sendErr: tooLarge, reply: MsgWFGetCodeResponse, failed: `"error":"INTERNAL_ERROR"`},
		{name: "CONFIG_GET", msg: configGet(t), sendErr: tooLarge, reply: sdk.MSGConfigResponse, failed: `"code":"` + sdk.ErrCodeInternal + `"`},
		{name: "NODE_STATUS_GET", msg: sdk.Message{Routing: sdk.Routing{Src: "orchestrator-uuid", TraceID: "t-status"}, Meta: sdk.Meta{MsgType: "system", Msg: &statusGet}}, sendErr: tooLarge},
		{name: "queue full", msg: from(sdk.PrimaryAdminNode, msgWithName(MsgWFGetCode)), sendErr: queueFull},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			rt, _ := newRuntime(t, plane, true)
			disp := &flakyDispatcher{failFirst: 1, err: c.sendErr}
			rt.ActCtx.Dispatcher = disp
			if err := Dispatch(context.Background(), c.msg, rt); !errors.Is(err, c.sendErr) {
				t.Fatalf("the failed reply must reach the receive loop's log, got %v", err)
			}
			if c.reply == "" {
				if len(disp.sent) != 0 {
					t.Fatalf("sent %d messages after the failed reply, want none", len(disp.sent))
				}
				return
			}
			if len(disp.sent) != 1 {
				t.Fatalf("sent %d messages after the refused reply, want the error reply", len(disp.sent))
			}
			reply := disp.sent[0]
			body := string(reply.Payload)
			if derefString(reply.Meta.Msg) != c.reply || !strings.Contains(body, `"ok":false`) ||
				!strings.Contains(body, c.failed) || !strings.Contains(body, "message too large") {
				t.Fatalf("error reply %s: %s", derefString(reply.Meta.Msg), body)
			}
		})
	}
}

func TestAFailedNodeTakesACorrectedConfigAtTheNextRestart(t *testing.T) {
	document := spawnConfig(writeWorkflowPackage(t, validWorkflowJSON()))
	document["gc_retention_day"] = 3
	configPath := writeNodeConfig(t, document)
	plane := bootstrap(t, configPath)
	if plane.State.CurrentState != sdk.ManagedFailedConfig {
		t.Fatalf("state = %+v", plane.State)
	}
	rt, disp := newRuntime(t, plane, false)
	if err := Dispatch(context.Background(), configSet(t, 0, map[string]any{"gc_retention_days": 3}), rt); err != nil {
		t.Fatal(err)
	}
	if reply := lastReply(t, disp); reply["ok"] != true || reply["config_version"] != float64(5) {
		t.Fatalf("reply = %v", reply)
	}
	restarted := bootstrap(t, configPath)
	if restarted.Configured() == nil || restarted.Configured().Config.GCRetentionDays != 3 {
		t.Fatalf("state after restart = %+v", restarted.State)
	}
}

// The orchestrator's set_node_config bumps `_system.config_version` on disk without telling the
// node: a CONFIG_SET still has to be newer than that.
func TestConfigSetMustBeNewerThanWhatTheOrchestratorWrote(t *testing.T) {
	configPath := writeNodeConfig(t, spawnConfig(writeWorkflowPackage(t, validWorkflowJSON())))
	plane := bootstrap(t, configPath)
	document := readJSONFile(t, configPath)
	document["_system"].(map[string]any)["config_version"] = 9
	raw, _ := json.Marshal(document)
	if err := os.WriteFile(configPath, raw, 0o600); err != nil {
		t.Fatal(err)
	}
	rt, disp := newRuntime(t, plane, true)
	if err := Dispatch(context.Background(), configSet(t, 6, map[string]any{"gc_retention_days": 3}), rt); err != nil {
		t.Fatal(err)
	}
	if errObj, _ := lastReply(t, disp)["error"].(map[string]any); errObj["code"] != errCodeStaleConfigVersion {
		t.Fatalf("reply = %v", lastReply(t, disp))
	}
	if err := Dispatch(context.Background(), configSet(t, 0, map[string]any{"gc_retention_days": 3}), rt); err != nil {
		t.Fatal(err)
	}
	if reply := lastReply(t, disp); reply["config_version"] != float64(10) {
		t.Fatalf("reply = %v", reply)
	}
}

func readJSONFile(t *testing.T, path string) map[string]any {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var document map[string]any
	if err := json.Unmarshal(data, &document); err != nil {
		t.Fatal(err)
	}
	return document
}

func mustLoadValidDefinition(t *testing.T) *WorkflowDefinition {
	t.Helper()
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("LoadDefinitionBytes: %v", err)
	}
	return def
}

// The answers of WF_LIST_INSTANCES, WF_GET_INSTANCE and WF_HELP use the snake_case keys the spec
// shows (§15, §16.1): SY.wf-rules reads ok and count of WF_LIST_INSTANCES, Archi and an operator
// the rest. Before 0.1.90 the instances went out with the store's Go field names.
func TestTheWFAnswersUseTheKeysTheSpecShows(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	ask := func(name, payload string) map[string]any {
		t.Helper()
		request := from(sdk.PrimaryAdminNode, msgWithName(name))
		request.Meta.MsgType = sdk.SYSTEMKind
		request.Payload = json.RawMessage(payload)
		if err := Dispatch(context.Background(), request, rt); err != nil {
			t.Fatal(err)
		}
		return lastReply(t, disp)
	}
	keys := func(object any) string {
		t.Helper()
		fields, ok := object.(map[string]any)
		if !ok {
			t.Fatalf("not an object: %v", object)
		}
		names := make([]string, 0, len(fields))
		for name := range fields {
			names = append(names, name)
		}
		sort.Strings(names)
		return strings.Join(names, ",")
	}
	const summaryKeys = "created_at_ms,current_state,definition_hash,instance_id,status,terminated_at_ms,thread_id,updated_at_ms,workflow_type"

	list := ask(MsgWFListInstances, `{"status": "running", "limit": 0}`)
	if keys(list) != "count,instances,ok" || list["ok"] != true || list["count"] != float64(1) {
		t.Fatalf("WF_LIST_INSTANCES = %v", list)
	}
	instances, _ := list["instances"].([]any)
	if len(instances) != 1 || keys(instances[0]) != summaryKeys {
		t.Fatalf("an instance listed: %v", instances)
	}

	got := ask(MsgWFGetInstance, `{"instance_id": "`+instanceID+`"}`)
	if keys(got) != "instance,ok" || got["ok"] != true {
		t.Fatalf("WF_GET_INSTANCE = %v", got)
	}
	instance := got["instance"].(map[string]any)
	if keys(instance) != "created_at_ms,current_state,definition_hash,input,instance_id,recent_log,state_variables,status,terminated_at_ms,thread_id,updated_at_ms,workflow_type" ||
		instance["instance_id"] != instanceID || instance["thread_id"] != instanceID || instance["status"] != "running" || instance["terminated_at_ms"] != nil {
		t.Fatalf("the instance: %v", instance)
	}
	if input := instance["input"].(map[string]any); input["customer_id"] != "c1" {
		t.Fatalf("its input: %v", input)
	}
	recent, _ := instance["recent_log"].([]any)
	if len(recent) == 0 || keys(recent[0]) != "action_type,error_detail,logged_at_ms,ok,summary" {
		t.Fatalf("its log: %v", recent)
	}

	help := ask(MsgWFHelp, `{}`)
	if keys(help) != "description,initial_state,input_schema,ok,states,terminal_states,workflow_type" || help["workflow_type"] != "asking" {
		t.Fatalf("WF_HELP = %v", help)
	}
	if states, _ := help["states"].([]any); len(states) != 4 || states[0] != "asking" {
		t.Fatalf("WF_HELP lists the states by name: %v", help["states"])
	}
}
