package node

import (
	"context"
	"errors"
	"reflect"
	"testing"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// FINDINGS A-302 — the engine a WF node runs is the runtime version whose directory holds its binary,
// as the orchestrator launches it: <dist>/runtimes/<runtime>/<version>/bin/<binary>. A binary
// elsewhere names none.
func TestEngineOfReadsTheRuntimeVersionItsBinaryIsIn(t *testing.T) {
	for _, tc := range []struct{ executable, runtime, version string }{
		{"/var/lib/fluxbee/dist/runtimes/wf.engine/0.1.139/bin/wf-generic", "wf.engine", "0.1.139"},
		{"/var/lib/fluxbee/dist/runtimes/wf.engine/0.1.139-gate1/bin/wf-generic", "wf.engine", "0.1.139-gate1"},
		{"/srv/dist/runtimes/wf.engine/0.1.140/bin/../bin/wf-generic", "wf.engine", "0.1.140"},
		{"/usr/local/bin/wf-generic", "", ""},
		{"/var/lib/fluxbee/dist/runtimes/wf.engine/wf-generic", "", ""},
		{"", "", ""},
	} {
		engine := EngineOf(tc.executable)
		if engine.Runtime != tc.runtime || engine.Version != tc.version || engine.Executable != tc.executable {
			t.Fatalf("EngineOf(%q) = %+v, want runtime %q, version %q", tc.executable, engine, tc.runtime, tc.version)
		}
	}
}

// FINDINGS A-302 — NODE_STATUS_GET names the engine the node runs under extensions.engine, which the
// orchestrator's get_node_status passes on as node_status.extensions.engine, beside the runtime it
// reads from the config, the workflow package: HEALTHY, DEGRADED with its config in use, and with
// no config in use. Before, nothing the node answered said it: only its unit's ExecStart did.
func TestTheStatusNamesTheEngineTheNodeRuns(t *testing.T) {
	const executable = "/var/lib/fluxbee/dist/runtimes/wf.engine/0.1.139/bin/wf-generic"
	statusName := sdk.MSGNodeStatusGet
	status := sdk.Message{Routing: sdk.Routing{Src: "orchestrator-uuid", TraceID: "t-status"}, Meta: sdk.Meta{MsgType: "system", Msg: &statusName}}
	// answer is the node's status: its health_state, its last_error's code and its extensions.engine.
	answer := func(t *testing.T, rt *NodeRuntime, disp *mockDispatcher) (string, string, map[string]any) {
		t.Helper()
		if err := Dispatch(context.Background(), status, rt); err != nil {
			t.Fatal(err)
		}
		reply := lastReply(t, disp)
		lastError, _ := reply["last_error"].(map[string]any)
		code, _ := lastError["code"].(string)
		extensions, _ := reply["extensions"].(map[string]any)
		engine, _ := extensions["engine"].(map[string]any)
		health, _ := reply["health_state"].(string)
		return health, code, engine
	}
	runs := map[string]any{"runtime": "wf.engine", "version": "0.1.139", "executable": executable}

	plane := bootstrap(t, writeNodeConfig(t, spawnConfig(writeWorkflowPackage(t, validWorkflowJSON()))))
	rt, disp := newRuntime(t, plane, true)
	rt.Engine = EngineOf(executable)
	if health, code, engine := answer(t, rt, disp); health != "HEALTHY" || code != "" || !reflect.DeepEqual(engine, runs) {
		t.Fatalf("HEALTHY: %s %q, engine %v", health, code, engine)
	}
	rt.timersPending = errors.New("SY.timer@motherbee did not answer TIMER_LIST")
	if health, code, engine := answer(t, rt, disp); health != "DEGRADED" || code != errCodeTimersNotReconciled || !reflect.DeepEqual(engine, runs) {
		t.Fatalf("DEGRADED: %s %q, engine %v", health, code, engine)
	}

	unusable := bootstrap(t, writeNodeConfig(t, spawnConfig(writeWorkflowPackage(t, `{"states": 1}`))))
	rt, disp = newRuntime(t, unusable, false)
	rt.Engine = EngineOf(executable)
	if health, code, engine := answer(t, rt, disp); health != "DEGRADED" || code != ErrCodeInvalidDefinition || !reflect.DeepEqual(engine, runs) {
		t.Fatalf("no config in use: %s %q, engine %v", health, code, engine)
	}

	// A binary that is no runtime version's names its path, and no runtime nor version.
	rt.Engine = EngineOf("/usr/local/bin/wf-generic")
	if _, _, engine := answer(t, rt, disp); !reflect.DeepEqual(engine, map[string]any{"runtime": nil, "version": nil, "executable": "/usr/local/bin/wf-generic"}) {
		t.Fatalf("by hand: engine %v", engine)
	}
}
