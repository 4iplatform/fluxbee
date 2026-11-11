package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

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

// FINDINGS A-217: a store the node cannot use stops the engine, never the node, which used to exit
// and restart every 5 s, its reason only in its journal. It stays up and says why: DEGRADED
// store_unavailable when the store's own file or its recovery fails, and FAILED_CONFIG
// invalid_config when the db_path itself cannot hold a store. Meanwhile an event gets a NOT_TAKEN
// unavailable, which its sender sees, a WF_* request STORE_ERROR or node_not_configured, and a
// TIMER_FIRED or a stranger's message nothing. The store's next open, once it can, runs the engine:
// HEALTHY and CONFIGURED, with the instances it keeps, and the next event starts one.
func TestAStoreTheNodeCannotUseLeavesItUpUntilItOpens(t *testing.T) {
	removeAll := func(t *testing.T, paths ...string) {
		t.Helper()
		for _, path := range paths {
			if err := os.RemoveAll(path); err != nil {
				t.Fatal(err)
			}
		}
	}
	writeFile := func(t *testing.T, path, content string) {
		t.Helper()
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	withStore := func(t *testing.T, path string, statements ...string) {
		t.Helper()
		store, err := OpenStore(path)
		if err != nil {
			t.Fatal(err)
		}
		defer store.Close()
		for _, statement := range statements {
			if _, err := store.db.Exec(statement); err != nil {
				t.Fatal(err)
			}
		}
	}
	const keptInstance = "wfi:kept"
	cases := []struct {
		name string
		// spoil makes the store at dbPath unusable, and repair usable again.
		spoil, repair func(t *testing.T, dbPath string)
		state         sdk.ManagedLifecycleState
		code, refusal string
		// instances is how many running instances the engine recovers once the store opens.
		instances int
	}{
		{
			name: "a file that is no database",
			spoil: func(t *testing.T, dbPath string) {
				writeFile(t, dbPath, "not a database, and long enough to be read as one: 0123456789")
			},
			repair:  func(t *testing.T, dbPath string) { removeAll(t, dbPath, dbPath+"-wal", dbPath+"-shm") },
			state:   sdk.ManagedConfigured,
			code:    errCodeStoreUnavailable,
			refusal: errCodeStoreError,
		},
		{
			name: "a recovery that fails",
			spoil: func(t *testing.T, dbPath string) {
				withStore(t, dbPath, fmt.Sprintf(`INSERT INTO wf_instances (instance_id, thread_id, workflow_type, definition_hash, status, current_state, input_json, state_json, created_at_ms, updated_at_ms)
VALUES (%q, %q, 'invoice', %q, 'running', 'collecting_data', '{"customer_id":"c1"}', '{}', 1000, 1000);`, keptInstance, keptInstance, definitionHash([]byte(validWorkflowJSON()))),
					`DROP TABLE wf_inbound_events;`)
			},
			repair:    func(t *testing.T, dbPath string) { withStore(t, dbPath, wfSchemaStatements...) },
			state:     sdk.ManagedConfigured,
			code:      errCodeStoreUnavailable,
			refusal:   errCodeStoreError,
			instances: 1,
		},
		{
			name: "a directory at db_path",
			spoil: func(t *testing.T, dbPath string) {
				if err := os.MkdirAll(dbPath, 0o755); err != nil {
					t.Fatal(err)
				}
			},
			repair:  func(t *testing.T, dbPath string) { removeAll(t, dbPath) },
			state:   sdk.ManagedFailedConfig,
			code:    sdk.ErrCodeInvalidConfig,
			refusal: errCodeNodeNotConfigured,
		},
		{
			name:    "a file where its directory should be",
			spoil:   func(t *testing.T, dbPath string) { writeFile(t, filepath.Dir(dbPath), "a file") },
			repair:  func(t *testing.T, dbPath string) { removeAll(t, filepath.Dir(dbPath)) },
			state:   sdk.ManagedFailedConfig,
			code:    sdk.ErrCodeInvalidConfig,
			refusal: errCodeNodeNotConfigured,
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			dbPath := filepath.Join(t.TempDir(), "store", "wf.db")
			document := spawnConfig(writeWorkflowPackage(t, validWorkflowJSON()))
			document["db_path"] = dbPath
			plane := bootstrap(t, writeNodeConfig(t, document))
			materialized := plane.Configured()
			if materialized == nil {
				t.Fatalf("db_path is no part of the config's validation: %+v", plane.State)
			}
			c.spoil(t, dbPath)
			disp := &mockDispatcher{l2name: testNodeName, uuid: "wf-uuid"}
			rt := &NodeRuntime{ActCtx: ActionContext{Dispatcher: disp, Timer: &mockTimerLister{}, Clock: fixedClock}, NodeUUID: "wf-uuid", NodeName: testNodeName, Plane: plane}
			defer func() {
				if rt.Store != nil {
					rt.Store.Close()
				}
			}()
			dispatch := func(msg sdk.Message) []sdk.Message {
				t.Helper()
				sent := len(disp.sent)
				if err := Dispatch(ctx, msg, rt); err != nil {
					t.Fatal(err)
				}
				return disp.sent[sent:]
			}
			statusName := sdk.MSGNodeStatusGet
			health := func() map[string]any {
				t.Helper()
				status := sdk.Message{Routing: sdk.Routing{Src: "orchestrator-uuid", TraceID: "t-status"}, Meta: sdk.Meta{MsgType: "system", Msg: &statusName}}
				if len(dispatch(status)) != 1 {
					t.Fatal("NODE_STATUS_GET answers once")
				}
				return lastReply(t, disp)
			}
			event := onThread("conv-a217", from("IO.api.billing@motherbee", msgWithNameAndPayload("INVOICE_REQUEST", map[string]any{"customer_id": "c1"})))
			event.Routing.Src = "billing-uuid"

			// The store cannot be used: the engine does not run, and the node stays up, saying why.
			if rt.startEngine(ctx, materialized) || rt.Def != nil || rt.Registry != nil || rt.Store != nil || rt.storeDown == nil {
				t.Fatalf("the engine waits for its store: def=%v store=%v down=%+v", rt.Def != nil, rt.Store != nil, rt.storeDown)
			}
			reply := health()
			lastError, _ := reply["last_error"].(map[string]any)
			message, _ := lastError["message"].(string)
			if reply["health_state"] != "DEGRADED" || lastError["code"] != c.code || !strings.Contains(message, dbPath) {
				t.Fatalf("NODE_STATUS_GET while the store cannot be used = %v", reply)
			}
			dispatch(configGet(t))
			if reply := lastReply(t, disp); reply["state"] != string(c.state) {
				t.Fatalf("CONFIG_GET while the store cannot be used = %v", reply)
			}

			// An event gets a NOT_TAKEN unavailable that says why, on its thread and trace; a WF_*
			// request its refusal.
			answers := dispatch(event)
			if len(answers) != 1 || notTakenReason(t, answers[0]) != notTakenUnavailable || answers[0].Routing.Dst.Value() != "billing-uuid" ||
				derefString(answers[0].Meta.ThreadID) != "conv-a217" || answers[0].Routing.TraceID != event.Routing.TraceID || !strings.Contains(string(answers[0].Payload), "wf.db") {
				t.Fatalf("the event is refused, to its sender: %+v", answers)
			}
			dispatch(from(sdk.PrimaryAdminNode, msgWithName(MsgWFListInstances)))
			if reply := lastReply(t, disp); reply["ok"] != false || reply["error"] != c.refusal || !strings.Contains(fmt.Sprint(reply["detail"]), dbPath) {
				t.Fatalf("WF_LIST_INSTANCES while the store cannot be used = %v", reply)
			}
			// SY.timer's fire and a stranger's message get nothing.
			fire := from("SY.timer@motherbee", sdk.Message{Routing: sdk.Routing{Src: "timer-uuid", TraceID: "t-fire"}, Meta: sdk.Meta{MsgType: sdk.SYSTEMKind, Msg: strPtr(sdk.MsgTimerFired)}, Payload: json.RawMessage(`{}`)})
			stranger := onThread("conv-a217", from("AI.stranger@motherbee", msgWithName("START")))
			stranger.Routing.Src = "stranger-uuid"
			for _, msg := range []sdk.Message{fire, stranger} {
				if answers := dispatch(msg); len(answers) != 0 {
					t.Fatalf("%s from %s is answered: %+v", rawMsgName(msg), msg.SourceL2Name(), answers)
				}
			}

			// Opened again before it can be used: nothing changes.
			if rt.startEngine(ctx, materialized) || rt.Def != nil || plane.State.CurrentState != c.state {
				t.Fatalf("a store still unusable keeps the engine waiting: %+v", plane.State)
			}

			// Once it can be used, the next open runs the engine.
			c.repair(t, dbPath)
			if !rt.startEngine(ctx, materialized) || rt.Def == nil || rt.Store == nil || rt.storeDown != nil || plane.Configured() != materialized {
				t.Fatalf("the engine runs once its store opens: down=%+v state=%+v", rt.storeDown, plane.State)
			}
			if reply := health(); reply["health_state"] != "HEALTHY" || reply["last_error"] != nil {
				t.Fatalf("NODE_STATUS_GET once the store opened = %v", reply)
			}
			dispatch(configGet(t))
			if reply := lastReply(t, disp); reply["ok"] != true || reply["state"] != string(sdk.ManagedConfigured) || reply["error"] != nil {
				t.Fatalf("CONFIG_GET once the store opened = %v", reply)
			}
			if rt.Registry.Count() != c.instances {
				t.Fatalf("the engine recovers the store's running instances: %d, want %d", rt.Registry.Count(), c.instances)
			}
			if answers := dispatch(event); rt.Registry.Count() != c.instances+1 || len(answers) == 0 || notTakenReason(t, answers[0]) != "" {
				t.Fatalf("the event starts an instance: %d running, sent %+v", rt.Registry.Count(), answers)
			}
		})
	}
}

// FINDINGS A-217's review: a store's failure is its config's (FAILED_CONFIG, invalid_config) only
// when the db_path is at fault, which a corrected db_path fixes. A journal file of the store that the
// node cannot write, one another user left (<db>-wal, <db>-shm), is: SQLite opens the store
// read-only with it, and the failure read store_unavailable. A read-only file system is not: the
// kernel remounts one read-only after I/O errors, a fault of the disk (store_unavailable), and the
// message says the file system is read-only.
func TestAStoreFailureIsTheConfigsOnlyWhenItsPathIsAtFault(t *testing.T) {
	for _, journal := range []string{"-wal", "-shm"} {
		t.Run("a "+journal+" file it cannot write", func(t *testing.T) {
			if os.Geteuid() == 0 {
				t.Skip("root writes a file whatever its mode")
			}
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			dbPath := filepath.Join(t.TempDir(), "wf.db")
			left, err := OpenStore(dbPath) // the store of an earlier run, closed
			if err != nil {
				t.Fatal(err)
			}
			if err := left.Close(); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(dbPath+journal, nil, 0o444); err != nil {
				t.Fatal(err)
			}
			document := spawnConfig(writeWorkflowPackage(t, validWorkflowJSON()))
			document["db_path"] = dbPath
			plane := bootstrap(t, writeNodeConfig(t, document))
			materialized := plane.Configured()
			rt := &NodeRuntime{ActCtx: ActionContext{Dispatcher: &mockDispatcher{l2name: testNodeName, uuid: "wf-uuid"}, Timer: &mockTimerLister{}, Clock: fixedClock},
				NodeUUID: "wf-uuid", NodeName: testNodeName, Plane: plane}
			defer func() {
				if rt.Store != nil {
					rt.Store.Close()
				}
			}()
			if rt.startEngine(ctx, materialized) || plane.State.CurrentState != sdk.ManagedFailedConfig ||
				rt.storeDown.cause.Code != sdk.ErrCodeInvalidConfig || !strings.Contains(rt.storeDown.cause.Message, dbPath+journal) {
				t.Fatalf("a journal file it cannot write is the db_path's fault: %+v %+v", plane.State, rt.storeDown)
			}
			// SQLite may have replaced the file meanwhile; the next open runs either way.
			if err := os.Chmod(dbPath+journal, 0o644); err != nil && !errors.Is(err, fs.ErrNotExist) {
				t.Fatal(err)
			}
			if !rt.startEngine(ctx, materialized) || plane.Configured() != materialized {
				t.Fatalf("the engine runs once the journal file can be written: %+v %+v", plane.State, rt.storeDown)
			}
		})
	}

	// A read-only file system, as dbPathProblem finds it on the store's file.
	const dbPath = "/var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/wf_instances.db"
	readOnly := fmt.Errorf("a file of the store cannot be written: %w", &os.PathError{Op: "open", Path: dbPath, Err: syscall.EROFS})
	cause, onPath := storeFailure(dbPath, errors.New("keep definition: attempt to write a readonly database (8)"), readOnly)
	if onPath || cause.Code != errCodeStoreUnavailable || !strings.Contains(cause.Message, "read-only file system") ||
		!strings.Contains(cause.Message, "attempt to write a readonly database") {
		t.Fatalf("a read-only file system is the disk's fault, and the message says so: %+v %v", cause, onPath)
	}

	// What is the path's fault, and what is not.
	for _, c := range []struct {
		err  error
		path bool
	}{
		{&os.PathError{Op: "open", Path: "x", Err: syscall.EACCES}, true},
		{&os.PathError{Op: "open", Path: "x", Err: syscall.EPERM}, true},
		{&os.PathError{Op: "mkdir", Path: "x", Err: syscall.ENOTDIR}, true},
		{&os.PathError{Op: "open", Path: "x", Err: syscall.EISDIR}, true},
		{&os.PathError{Op: "open", Path: "x", Err: syscall.ENAMETOOLONG}, true},
		{&os.PathError{Op: "open", Path: "x", Err: syscall.ELOOP}, true},
		{fmt.Errorf("x is a directory: %w", errNotAFile), true},
		{&os.PathError{Op: "open", Path: "x", Err: syscall.EROFS}, false},
		{&os.PathError{Op: "open", Path: "x", Err: syscall.ENOSPC}, false},
		{&os.PathError{Op: "open", Path: "x", Err: syscall.EIO}, false},
		{nil, false},
	} {
		if pathFault(c.err) != c.path {
			t.Errorf("pathFault(%v) = %v, want %v", c.err, !c.path, c.path)
		}
	}
}

// chanDispatcher hands each message the node sends to sent: a dispatcher for a test whose receive
// loop runs on a goroutine of its own.
type chanDispatcher struct{ sent chan sdk.Message }

func (d *chanDispatcher) SendMsg(msg sdk.Message, note sdk.TraceNote) error {
	sdk.WriteTraceEntry(&msg, testNodeName, note)
	d.sent <- msg
	return nil
}
func (d *chanDispatcher) NodeL2Name() string { return testNodeName }
func (d *chanDispatcher) NodeUUID() string   { return "wf-uuid" }

// switchedTimer is SY.timer as the receive loop's goroutines reach it: it answers TIMER_LIST, with no
// timer pending, only once answering is set, and takes any other request.
type switchedTimer struct{ answering atomic.Bool }

func (s *switchedTimer) ScheduleIn(context.Context, time.Duration, sdk.ScheduleOptions) (string, error) {
	return "timer-trace", nil
}
func (s *switchedTimer) Schedule(context.Context, time.Time, sdk.ScheduleOptions) (string, error) {
	return "timer-trace", nil
}
func (s *switchedTimer) CancelByClientRef(context.Context, string) error          { return nil }
func (s *switchedTimer) CancelByClientRefConfirmed(context.Context, string) error { return nil }
func (s *switchedTimer) RescheduleByClientRef(context.Context, string, time.Time) (string, error) {
	return "timer-trace", nil
}
func (s *switchedTimer) List(context.Context, sdk.ListFilter) ([]sdk.TimerInfo, error) {
	if !s.answering.Load() {
		return nil, errSendFailed
	}
	return nil, nil
}

// FINDINGS A-217's review: the receive loop's wiring (serve, Run once the router is connected),
// driven without a router. A store that does not open at the start is opened again on the loop's
// tick, until it opens; the engine then runs, and its timers, which SY.timer has not answered for,
// are reconciled on the reconcile's tick, which the late open starts, once SY.timer answers: DEGRADED
// store_unavailable, then DEGRADED timers_not_reconciled, then HEALTHY, with nothing from outside the
// loop but the status requests.
func TestTheReceiveLoopOpensTheStoreAgainAndThenReconcilesItsTimers(t *testing.T) {
	dbPath := filepath.Join(t.TempDir(), "wf.db")
	if err := os.WriteFile(dbPath, []byte("not a database, and long enough to be read as one: 0123456789"), 0o644); err != nil {
		t.Fatal(err)
	}
	document := spawnConfig(writeWorkflowPackage(t, validWorkflowJSON()))
	document["db_path"] = dbPath
	plane := bootstrap(t, writeNodeConfig(t, document))
	materialized := plane.Configured()
	disp := &chanDispatcher{sent: make(chan sdk.Message, 16)}
	timer := &switchedTimer{}
	rt := &NodeRuntime{ActCtx: ActionContext{Dispatcher: disp, Timer: timer, Clock: fixedClock}, NodeUUID: "wf-uuid", NodeName: testNodeName, Plane: plane}

	ctx, cancel := context.WithCancel(context.Background())
	incoming := make(chan sdk.Message)
	served := make(chan struct{})
	go func() {
		defer close(served)
		rt.serve(ctx, incoming, nil, materialized, loopIntervals{storeRetry: 10 * time.Millisecond, timerRetry: 10 * time.Millisecond, timerRequests: 10 * time.Millisecond})
	}()
	defer func() {
		cancel()
		<-served
		if rt.Store != nil {
			rt.Store.Close()
		}
	}()

	// status asks the loop for the node's health, as the orchestrator does: its health_state and the
	// code of its last_error.
	statusName := sdk.MSGNodeStatusGet
	status := func() (string, string) {
		t.Helper()
		select {
		case incoming <- sdk.Message{Routing: sdk.Routing{Src: "orchestrator-uuid", TraceID: "t-status"}, Meta: sdk.Meta{MsgType: "system", Msg: &statusName}}:
		case <-time.After(5 * time.Second):
			t.Fatal("the loop takes no message")
		}
		for {
			select {
			case msg := <-disp.sent:
				if derefString(msg.Meta.Msg) != sdk.MSGNodeStatusGetResponse {
					continue
				}
				var reply struct {
					Health    string                 `json:"health_state"`
					LastError *sdk.ControlPlaneError `json:"last_error"`
				}
				if err := json.Unmarshal(msg.Payload, &reply); err != nil {
					t.Fatal(err)
				}
				if reply.LastError == nil {
					return reply.Health, ""
				}
				return reply.Health, reply.LastError.Code
			case <-time.After(5 * time.Second):
				t.Fatal("no answer to NODE_STATUS_GET")
			}
		}
	}
	// until asks again until the node's health is health, with code.
	until := func(health, code string) {
		t.Helper()
		for deadline := time.Now().Add(10 * time.Second); ; time.Sleep(5 * time.Millisecond) {
			got, gotCode := status()
			if got == health && gotCode == code {
				return
			}
			if time.Now().After(deadline) {
				t.Fatalf("the loop never reached %s %q: %s %q", health, code, got, gotCode)
			}
		}
	}

	// The start found no store it can use: the loop serves, DEGRADED, and opens it again on its tick.
	if got, code := status(); got != "DEGRADED" || code != errCodeStoreUnavailable {
		t.Fatalf("at the start: %s %q", got, code)
	}
	// The store can be used: a tick opens it, the engine runs, and its timers wait for SY.timer.
	if err := os.Remove(dbPath); err != nil {
		t.Fatal(err)
	}
	until("DEGRADED", errCodeTimersNotReconciled)
	// SY.timer answers: the reconcile's tick, started by the late open, reconciles the timers.
	timer.answering.Store(true)
	until("HEALTHY", "")
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
		instance["instance_id"] != instanceID || instance["thread_id"] != *threadOf(t, rt, instanceID) || instance["status"] != "running" || instance["terminated_at_ms"] != nil {
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
	if keys(help) != "description,idle_timeout,initial_state,input_schema,ok,states,terminal_states,workflow_type" || help["workflow_type"] != "asking" || help["idle_timeout"] != "4d" {
		t.Fatalf("WF_HELP = %v", help)
	}
	if states, _ := help["states"].([]any); len(states) != 4 || states[0] != "asking" {
		t.Fatalf("WF_HELP lists the states by name: %v", help["states"])
	}
}
