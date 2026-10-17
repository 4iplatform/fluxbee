package sdk

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// testContract accepts a config with a non-empty "model", rejects any other.
type testContract struct {
	runsOnSpawnConfig bool
	sawSystem         map[string]any
}

func (c *testContract) ValidateAndMaterialize(effective map[string]any, system map[string]any) (any, error) {
	c.sawSystem = system
	model, _ := effective["model"].(string)
	if model == "" && !c.runsOnSpawnConfig {
		return nil, InvalidConfig("model is required")
	}
	out := map[string]any{"model": model, "materialized": true}
	return out, nil
}

func (c *testContract) RunsOnSpawnConfig() bool { return c.runsOnSpawnConfig }

func writeConfig(t *testing.T, document string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "AI", "AI.demo@motherbee", "config.json")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(document), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func readDocument(t *testing.T, path string) map[string]any {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	document, err := decodeManagedDocument(data)
	if err != nil {
		t.Fatal(err)
	}
	return document.(map[string]any)
}

func TestManagedNodeConfigPathFollowsKindAndFullName(t *testing.T) {
	path, err := ManagedNodeConfigPath(" WF.demo.test@motherbee ", "/tmp/fluxbee-managed-test")
	if err != nil {
		t.Fatal(err)
	}
	if path != "/tmp/fluxbee-managed-test/WF/WF.demo.test@motherbee/config.json" {
		t.Fatalf("path = %s", path)
	}
	for _, bad := range []string{"", "  ", "WF.demo", "WF.demo@", ".demo@motherbee"} {
		if _, err := ManagedNodeConfigPath(bad, "/tmp"); err == nil {
			t.Fatalf("%q should be refused", bad)
		}
	}
}

func TestBootstrapWithoutAFileIsUnconfigured(t *testing.T) {
	state, err := BootstrapManagedControlPlane(filepath.Join(t.TempDir(), "config.json"), &testContract{})
	if err != nil {
		t.Fatal(err)
	}
	if state.CurrentState != ManagedUnconfigured || state.ConfigVersion != 0 || state.LastError != nil {
		t.Fatalf("state = %+v", state)
	}
	if ManagedHealthState(state) != "DEGRADED" {
		t.Fatal("an unconfigured node is not healthy")
	}
}

func TestBootstrapAcceptsAValidConfigAndReadsTheVersionFromSystem(t *testing.T) {
	path := writeConfig(t, `{"_system":{"config_version":5},"model":"gpt"}`)
	state, err := BootstrapManagedControlPlane(path, &testContract{})
	if err != nil {
		t.Fatal(err)
	}
	if state.CurrentState != ManagedConfigured || state.ConfigVersion != 5 || state.SchemaVersion != 1 {
		t.Fatalf("state = %+v", state)
	}
	effective := state.EffectiveConfig.(map[string]any)
	if effective["materialized"] != true || effective["model"] != "gpt" {
		t.Fatalf("effective = %v", effective)
	}
	if ManagedHealthState(state) != "HEALTHY" {
		t.Fatal("a configured node is healthy")
	}
}

func TestBootstrapKeepsARejectedConfigOutAndSaysWhy(t *testing.T) {
	path := writeConfig(t, `{"_system":{"config_version":9},"model":""}`)
	state, err := BootstrapManagedControlPlane(path, &testContract{})
	if err != nil {
		t.Fatal(err)
	}
	if state.CurrentState != ManagedFailedConfig || state.ConfigVersion != 9 || state.EffectiveConfig != nil {
		t.Fatalf("state = %+v", state)
	}
	if state.LastError == nil || state.LastError.Code != ErrCodeInvalidConfig || state.LastError.Message != "model is required" {
		t.Fatalf("last_error = %+v", state.LastError)
	}
	if ManagedHealthState(state) != "DEGRADED" {
		t.Fatal("a rejected config is not healthy")
	}
}

func TestBootstrapReportsAContractErrorCode(t *testing.T) {
	path := writeConfig(t, `{"model":"x"}`)
	contract := contractFunc(func(map[string]any, map[string]any) (any, error) {
		return nil, &ControlPlaneError{Code: "invalid_definition", Message: "states missing"}
	})
	state, _ := BootstrapManagedControlPlane(path, contract)
	if state.LastError == nil || state.LastError.Code != "invalid_definition" || state.LastError.Message != "states missing" {
		t.Fatalf("last_error = %+v", state.LastError)
	}
	plain := contractFunc(func(map[string]any, map[string]any) (any, error) { return nil, errors.New("boom") })
	state, _ = BootstrapManagedControlPlane(path, plain)
	if state.LastError == nil || state.LastError.Code != ErrCodeInvalidConfig || state.LastError.Message != "boom" {
		t.Fatalf("a plain error reads invalid_config: %+v", state.LastError)
	}
}

type contractFunc func(map[string]any, map[string]any) (any, error)

func (f contractFunc) ValidateAndMaterialize(effective, system map[string]any) (any, error) {
	return f(effective, system)
}
func (f contractFunc) RunsOnSpawnConfig() bool { return true }

func TestBootstrapTreatsTheSpawnSeedAsUnconfigured(t *testing.T) {
	for _, document := range []string{
		`{"_system":{"config_version":3},"tenant_id":"tnt:1"}`,
		`{"_system":{"config_version":3}}`,
	} {
		state, err := BootstrapManagedControlPlane(writeConfig(t, document), &testContract{})
		if err != nil {
			t.Fatal(err)
		}
		if state.CurrentState != ManagedUnconfigured || state.ConfigVersion != 3 || state.LastError != nil {
			t.Fatalf("%s: state = %+v", document, state)
		}
	}
}

func TestBootstrapValidatesTheSpawnSeedOfANodeThatRunsOnIt(t *testing.T) {
	contract := &testContract{runsOnSpawnConfig: true}
	state, err := BootstrapManagedControlPlane(writeConfig(t, `{"_system":{"package_path":"/p"},"tenant_id":"tnt:1"}`), contract)
	if err != nil {
		t.Fatal(err)
	}
	if state.CurrentState != ManagedConfigured {
		t.Fatalf("state = %+v", state)
	}
	if ManagedSystemString(contract.sawSystem, "package_path") != "/p" {
		t.Fatalf("the contract sees _system: %v", contract.sawSystem)
	}
}

func TestBootstrapRejectsWhatIsNotAJSONObject(t *testing.T) {
	for _, document := range []string{`{not json`, `[1,2]`, `{"config":"x"}`, `{"a":1} {"b":2}`} {
		state, err := BootstrapManagedControlPlane(writeConfig(t, document), &testContract{})
		if err != nil {
			t.Fatal(err)
		}
		if state.CurrentState != ManagedFailedConfig || state.LastError == nil || state.LastError.Code != ErrCodeInvalidConfig {
			t.Fatalf("%s: state = %+v", document, state)
		}
	}
}

func TestBootstrapReadsAWrappedConfig(t *testing.T) {
	state, err := BootstrapManagedControlPlane(writeConfig(t, `{"_system":{"config_version":2},"config":{"model":"m","_system":{"x":1}}}`), &testContract{})
	if err != nil {
		t.Fatal(err)
	}
	if state.CurrentState != ManagedConfigured || state.EffectiveConfig.(map[string]any)["model"] != "m" {
		t.Fatalf("state = %+v", state)
	}
}

func TestTheContractSeesSystemButNeverHasToKnowItsKeys(t *testing.T) {
	contract := &testContract{}
	path := writeConfig(t, `{"_system":{"config_version":4,"node_name":"AI.demo@motherbee","a_key_from_a_newer_orchestrator":{"nested":true}},"model":"m"}`)
	state, err := BootstrapManagedControlPlane(path, contract)
	if err != nil || state.CurrentState != ManagedConfigured {
		t.Fatalf("state = %+v, err = %v", state, err)
	}
	if ManagedSystemString(contract.sawSystem, "node_name") != "AI.demo@motherbee" {
		t.Fatalf("system = %v", contract.sawSystem)
	}
	if _, ok := contract.sawSystem["a_key_from_a_newer_orchestrator"]; !ok {
		t.Fatal("the contract gets _system as it is")
	}
}

func TestPersistKeepsSystemAsItIsAndSetsOnlyTheVersion(t *testing.T) {
	path := writeConfig(t, `{
  "_system": {"config_version": 1, "ilk_id": "ilk:abc", "created_at_ms": 1792048429123, "relaunch_on_boot": false, "a_key_from_a_newer_orchestrator": {"nested": [1, 2]}},
  "model": "a",
  "tenant_id": "tnt:1"
}`)
	if err := PersistManagedConfig(path, 2, map[string]any{"model": "b"}); err != nil {
		t.Fatal(err)
	}
	document := readDocument(t, path)
	system := document["_system"].(map[string]any)
	if system["config_version"] != json.Number("2") || system["ilk_id"] != "ilk:abc" {
		t.Fatalf("system = %v", system)
	}
	if system["created_at_ms"] != json.Number("1792048429123") || system["relaunch_on_boot"] != false {
		t.Fatalf("numbers and false values come back as they were: %v", system)
	}
	if _, ok := system["a_key_from_a_newer_orchestrator"]; !ok {
		t.Fatal("a _system key the node does not know survives")
	}
	if document["model"] != "b" {
		t.Fatalf("document = %v", document)
	}
	if _, ok := document["tenant_id"]; ok {
		t.Fatal("the effective config replaces the other top-level keys")
	}
	state, err := BootstrapManagedControlPlane(path, &testContract{})
	if err != nil || state.CurrentState != ManagedConfigured || state.ConfigVersion != 2 {
		t.Fatalf("reboots configured: %+v, %v", state, err)
	}
}

func TestPersistIsAtomicAndKeepsTheFileMode(t *testing.T) {
	path := writeConfig(t, `{"_system":{"config_version":1},"model":"a"}`)
	if err := os.Chmod(path, 0o640); err != nil {
		t.Fatal(err)
	}
	if err := PersistManagedConfig(path, 2, map[string]any{"model": "b"}); err != nil {
		t.Fatal(err)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o640 {
		t.Fatalf("mode = %v", info.Mode().Perm())
	}
	entries, _ := os.ReadDir(filepath.Dir(path))
	for _, entry := range entries {
		if strings.Contains(entry.Name(), ".tmp") {
			t.Fatalf("temporary file left behind: %s", entry.Name())
		}
	}

	fresh := filepath.Join(t.TempDir(), "WF", "WF.x@motherbee", "config.json")
	if err := PersistManagedConfig(fresh, 1, map[string]any{"a": 1}); err != nil {
		t.Fatal(err)
	}
	info, err = os.Stat(fresh)
	if err != nil || info.Mode().Perm() != 0o600 {
		t.Fatalf("a new file is 0600 like the orchestrator's: %v, %v", info.Mode().Perm(), err)
	}
	if readDocument(t, fresh)["_system"].(map[string]any)["config_version"] != json.Number("1") {
		t.Fatal("a missing _system starts with the version")
	}
}

func TestPersistWrapsAConfigThatIsNotAnObject(t *testing.T) {
	path := writeConfig(t, `{"_system":{"config_version":1}}`)
	if err := PersistManagedConfig(path, 2, []any{"x"}); err != nil {
		t.Fatal(err)
	}
	document := readDocument(t, path)
	if _, ok := document["config"].([]any); !ok {
		t.Fatalf("document = %v", document)
	}
}

func TestIsSystemKindIgnoresASCIICaseOnly(t *testing.T) {
	for _, kind := range []string{"system", "SYSTEM", "System"} {
		if !IsSystemKind(kind) {
			t.Fatalf("%q is the system kind", kind)
		}
	}
	for _, kind := range []string{"", "systems", "syste", "ſystem", "admin"} {
		if IsSystemKind(kind) {
			t.Fatalf("%q is not the system kind", kind)
		}
	}
}

func TestManagedNodeStatusResponseFollowsTheConfigState(t *testing.T) {
	msgName := MSGNodeStatusGet
	incoming := Message{
		Routing: Routing{Src: "caller-uuid", TraceID: "trace-1"},
		Meta:    Meta{MsgType: "SYSTEM", Msg: &msgName},
	}
	failed := ManagedControlPlaneState{
		CurrentState: ManagedFailedConfig,
		LastError:    &ControlPlaneError{Code: "invalid_definition", Message: "flow/definition.json: missing"},
	}
	reply, err := BuildManagedNodeStatusResponse(&incoming, "node-uuid", failed)
	if err != nil {
		t.Fatal(err)
	}
	var payload map[string]any
	if err := json.Unmarshal(reply.Payload, &payload); err != nil {
		t.Fatal(err)
	}
	lastError, _ := payload["last_error"].(map[string]any)
	if payload["health_state"] != "DEGRADED" || lastError["code"] != "invalid_definition" || lastError["message"] != "flow/definition.json: missing" {
		t.Fatalf("payload = %v", payload)
	}
	if reply.Routing.TraceID != "trace-1" || stringValue(reply.Meta.Msg) != MSGNodeStatusGetResponse {
		t.Fatalf("reply = %+v", reply)
	}

	reply, err = BuildManagedNodeStatusResponse(&incoming, "node-uuid", ManagedControlPlaneState{CurrentState: ManagedConfigured})
	if err != nil {
		t.Fatal(err)
	}
	payload = map[string]any{}
	_ = json.Unmarshal(reply.Payload, &payload)
	if payload["health_state"] != "HEALTHY" {
		t.Fatalf("payload = %v", payload)
	}
	if _, ok := payload["last_error"]; ok {
		t.Fatal("a configured node reports no error")
	}
}

func TestPersistRefusesToWriteOverAFileItCannotRead(t *testing.T) {
	path := writeConfig(t, `{"_system":{"config_version":3,"node_name":"AI.demo@motherbee"`)
	if err := PersistManagedConfig(path, 4, map[string]any{"model": "b"}); err == nil {
		t.Fatal("a config.json that is not JSON is not overwritten")
	}
	data, _ := os.ReadFile(path)
	if !strings.Contains(string(data), "node_name") {
		t.Fatalf("the file is left as it was: %s", data)
	}
}
