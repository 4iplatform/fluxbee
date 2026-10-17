package node

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// WF-BUILD-3 — the package-native contract (docs/onworking COA/wf_v1_tasks.md §15):
//  1. In managed package mode the definition is `_system.package_path/flow/definition.json`.
//  2. CONFIG_SET in managed package mode refuses to point workflow_definition_path elsewhere.
//  3. CONFIG_GET in managed package mode shows the package-native binding.
//  4. Local-dev mode, with an explicit workflow_definition_path and no package, still works.

// Case 1: managed package mode takes the package's definition, whatever the config says.
func TestManagedPackageModeResolvesDefinitionFromPackagePath(t *testing.T) {
	pkgPath := writeWorkflowPackage(t, validWorkflowJSON())
	document := spawnConfig(pkgPath)
	document["workflow_definition_path"] = "/some/other/path/wf.json"
	plane := bootstrap(t, writeNodeConfig(t, document))
	materialized := plane.Configured()
	if materialized == nil {
		t.Fatalf("expected CONFIGURED, got %+v", plane.State)
	}
	want := filepath.Join(pkgPath, "flow", "definition.json")
	if !materialized.Config.IsManagedPackageMode() || materialized.Config.WorkflowDefinitionPath != want || materialized.Config.PackageDefinitionPath() != want {
		t.Fatalf("cfg = %+v", materialized.Config)
	}
}

// Case 2: a CONFIG_SET pointing a package-bound node's definition elsewhere is refused.
func TestConfigSetRefusesToMoveAPackageBoundDefinition(t *testing.T) {
	configPath := writeNodeConfig(t, spawnConfig(writeWorkflowPackage(t, validWorkflowJSON())))
	before, _ := os.ReadFile(configPath)
	rt, disp := newRuntime(t, bootstrap(t, configPath), true)
	if err := Dispatch(context.Background(), configSet(t, 5, map[string]any{
		"workflow_definition_path": "/etc/fluxbee/operator-tried-to-change.json",
	}), rt); err != nil {
		t.Fatal(err)
	}
	errObj, _ := lastReply(t, disp)["error"].(map[string]any)
	if errObj["code"] != ErrCodeManagedPackagePathLocked {
		t.Fatalf("reply = %v", lastReply(t, disp))
	}
	after, _ := os.ReadFile(configPath)
	if string(before) != string(after) {
		t.Fatal("a refused CONFIG_SET changes nothing")
	}
}

// Case 2b: a GET→SET round trip with the package's own path is not a change, and the path is not
// persisted (the package decides it, and changes with each deploy).
func TestConfigSetAcceptsThePackagePathItself(t *testing.T) {
	pkgPath := writeWorkflowPackage(t, validWorkflowJSON())
	configPath := writeNodeConfig(t, spawnConfig(pkgPath))
	rt, disp := newRuntime(t, bootstrap(t, configPath), true)
	if err := Dispatch(context.Background(), configSet(t, 5, map[string]any{
		"workflow_definition_path": filepath.Join(pkgPath, "flow", "definition.json"),
		"gc_retention_days":        9,
	}), rt); err != nil {
		t.Fatal(err)
	}
	if reply := lastReply(t, disp); reply["ok"] != true {
		t.Fatalf("reply = %v", reply)
	}
	if _, present := readJSONFile(t, configPath)["workflow_definition_path"]; present {
		t.Fatal("a package-bound node does not persist the definition path")
	}
}

// Case 3: CONFIG_GET shows the package-native binding, also while the config is not in use.
func TestConfigGetDescribesThePackageBinding(t *testing.T) {
	pkgPath := writeWorkflowPackage(t, validWorkflowJSON())
	rt, disp := newRuntime(t, bootstrap(t, writeNodeConfig(t, spawnConfig(pkgPath))), true)
	if err := Dispatch(context.Background(), configGet(t), rt); err != nil {
		t.Fatal(err)
	}
	reply := lastReply(t, disp)
	effective := reply["effective_config"].(map[string]any)
	want := filepath.Join(pkgPath, "flow", "definition.json")
	if effective["managed_package_mode"] != true || effective["workflow_definition_source"] != "managed_package" ||
		effective["package_path"] != pkgPath || effective["package_definition_path"] != want || effective["workflow_definition_path"] != want {
		t.Fatalf("effective_config = %v", effective)
	}
	contract := reply["contract"].(map[string]any)
	if contract["package_native_binding"] != true || contract["workflow_definition_path_locked"] != true {
		t.Fatalf("contract = %v", contract)
	}
	notes, _ := contract["notes"].([]any)
	joined := ""
	for _, note := range notes {
		joined += note.(string) + " | "
	}
	if !strings.Contains(joined, ErrCodeManagedPackagePathLocked) {
		t.Fatalf("notes = %v", notes)
	}

	broken := bootstrap(t, writeNodeConfig(t, spawnConfig(writeWorkflowPackage(t, `{}`))))
	rt, disp = newRuntime(t, broken, false)
	if err := Dispatch(context.Background(), configGet(t), rt); err != nil {
		t.Fatal(err)
	}
	if contract := lastReply(t, disp)["contract"].(map[string]any); contract["package_native_binding"] != true {
		t.Fatalf("a rejected config still shows its binding: %v", contract)
	}
}

// Case 4: local-dev mode (no `_system`): the definition path is the operator's, and CONFIG_SET can
// change it.
func TestLocalDevModeWorkflowDefinitionPathRemainsMutable(t *testing.T) {
	dir := t.TempDir()
	defOne := filepath.Join(dir, "wf.one.json")
	defTwo := filepath.Join(dir, "wf.two.json")
	for _, path := range []string{defOne, defTwo} {
		if err := os.WriteFile(path, []byte(validWorkflowJSON()), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	configPath := writeNodeConfig(t, map[string]any{
		"workflow_definition_path": defOne,
		"db_path":                  filepath.Join(dir, "wf.db"),
	})
	plane := bootstrap(t, configPath)
	materialized := plane.Configured()
	if materialized == nil || materialized.Config.IsManagedPackageMode() || materialized.Config.WorkflowDefinitionPath != defOne {
		t.Fatalf("state = %+v", plane.State)
	}
	if materialized.Config.SYTimerL2Name != "SY.timer@motherbee" {
		t.Fatalf("the timer follows the managed node name's hive: %+v", materialized.Config)
	}

	rt, disp := newRuntime(t, plane, true)
	if err := Dispatch(context.Background(), configSet(t, 0, map[string]any{
		"workflow_definition_path": defTwo,
		"db_path":                  filepath.Join(dir, "wf.db"),
	}), rt); err != nil {
		t.Fatal(err)
	}
	reply := lastReply(t, disp)
	effective, _ := reply["effective_config"].(map[string]any)
	if reply["ok"] != true || effective["workflow_definition_path"] != defTwo || effective["workflow_definition_source"] != "config" || effective["package_path"] != "" {
		t.Fatalf("reply = %v", reply)
	}
	contract := reply["contract"].(map[string]any)
	if contract["package_native_binding"] != false {
		t.Fatalf("contract = %v", contract)
	}
	if _, locked := contract["workflow_definition_path_locked"]; locked {
		t.Fatalf("contract = %v", contract)
	}
	if readJSONFile(t, configPath)["workflow_definition_path"] != defTwo {
		t.Fatal("local dev persists its definition path")
	}
}
