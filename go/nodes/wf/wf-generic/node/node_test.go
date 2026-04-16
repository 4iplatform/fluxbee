package node

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

func TestLoadConfigAcceptsManagedSystemBlock(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "config.json")
	data := `{
  "workflow_definition_path": "/tmp/wf.invoice.json",
  "db_path": "/tmp/wf.db",
  "sy_timer_l2_name": "SY.timer@motherbee",
  "_system": {
    "node_name": "WF.invoice@motherbee"
  }
}`
	if err := os.WriteFile(path, []byte(data), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	cfg, err := LoadConfig(path)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}
	if cfg.System == nil || cfg.System.NodeName != "WF.invoice@motherbee" {
		t.Fatalf("expected _system.node_name to be loaded, got %+v", cfg.System)
	}
}

func TestLoadConfigAcceptsTenantIDAndTopLevelConfigVersion(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "config.json")
	data := `{
  "config_version": 1,
  "workflow_definition_path": "/tmp/wf.invoice.json",
  "db_path": "/tmp/wf.db",
  "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e",
  "sy_timer_l2_name": "SY.timer@motherbee",
  "_system": {
    "node_name": "WF.invoice@motherbee"
  }
}`
	if err := os.WriteFile(path, []byte(data), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	cfg, err := LoadConfig(path)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}
	if cfg.TenantID != "tnt:43d576a3-d712-4d91-9245-5d5463dd693e" {
		t.Fatalf("expected tenant_id to be loaded, got %q", cfg.TenantID)
	}
}

func TestLoadConfigAcceptsWrappedConfigEnvelope(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "config.json")
	data := `{
  "config_version": 1,
  "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e",
  "_system": {
    "node_name": "WF.invoice@motherbee"
  },
  "config": {
    "workflow_definition_path": "/tmp/wf.invoice.json",
    "db_path": "/tmp/wf.db",
    "sy_timer_l2_name": "SY.timer@motherbee"
  }
}`
	if err := os.WriteFile(path, []byte(data), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	cfg, err := LoadConfig(path)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}
	if cfg.System == nil || cfg.System.NodeName != "WF.invoice@motherbee" {
		t.Fatalf("expected _system.node_name to be merged, got %+v", cfg.System)
	}
	if cfg.TenantID != "tnt:43d576a3-d712-4d91-9245-5d5463dd693e" {
		t.Fatalf("expected tenant_id to be merged, got %q", cfg.TenantID)
	}
}

func TestLoadConfigIgnoresOrchestratorMetadataEnvelope(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "config.json")
	data := `{
  "config_version": 1,
  "managed_by": "SY.orchestrator",
  "runtime": "wf.engine",
  "runtime_version": "0.1.0",
  "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e",
  "_system": {
    "node_name": "WF.invoice@motherbee"
  },
  "workflow_definition_path": "/tmp/wf.invoice.json",
  "db_path": "/tmp/wf.db",
  "sy_timer_l2_name": "SY.timer@motherbee"
}`
	if err := os.WriteFile(path, []byte(data), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	cfg, err := LoadConfig(path)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}
	if cfg.WorkflowDefinitionPath != "/tmp/wf.invoice.json" {
		t.Fatalf("unexpected workflow_definition_path %q", cfg.WorkflowDefinitionPath)
	}
	if cfg.DBPath != "/tmp/wf.db" {
		t.Fatalf("unexpected db_path %q", cfg.DBPath)
	}
}

func TestLoadConfigDerivesWorkflowDefinitionAndRuntimeDefaultsFromManagedPackage(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "config.json")
	data := `{
  "config_version": 1,
  "_system": {
    "node_name": "WF.invoice@motherbee",
    "package_path": "/var/lib/fluxbee/dist/runtimes/wf.invoice/0.1.0"
  }
}`
	if err := os.WriteFile(path, []byte(data), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	cfg, err := LoadConfig(path)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}
	if got := cfg.WorkflowDefinitionPath; got != "/var/lib/fluxbee/dist/runtimes/wf.invoice/0.1.0/flow/definition.json" {
		t.Fatalf("unexpected workflow_definition_path %q", got)
	}
	if got := cfg.DBPath; got != filepath.Join(dir, "wf_instances.db") {
		t.Fatalf("unexpected db_path %q", got)
	}
	if got := cfg.SYTimerL2Name; got != "SY.timer@motherbee" {
		t.Fatalf("unexpected sy_timer_l2_name %q", got)
	}
}

func TestLoadConfigUsesSystemTenantAndHiveDefaults(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "config.json")
	data := `{
  "_system": {
    "node_name": "WF.invoice@motherbee",
    "hive_id": "motherbee",
    "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e",
    "package_path": "/pkg/wf.invoice"
  }
}`
	if err := os.WriteFile(path, []byte(data), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	cfg, err := LoadConfig(path)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}
	if got := cfg.TenantID; got != "tnt:43d576a3-d712-4d91-9245-5d5463dd693e" {
		t.Fatalf("unexpected tenant_id %q", got)
	}
	if got := cfg.SYTimerL2Name; got != "SY.timer@motherbee" {
		t.Fatalf("unexpected sy_timer_l2_name %q", got)
	}
}

func TestManagedConfigPathFromEnv(t *testing.T) {
	t.Setenv("FLUXBEE_NODE_NAME", "WF.invoice@motherbee")
	got, err := ManagedConfigPathFromEnv()
	if err != nil {
		t.Fatalf("ManagedConfigPathFromEnv: %v", err)
	}
	want := "/var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/config.json"
	if got != want {
		t.Fatalf("expected %q, got %q", want, got)
	}
}

func TestWFConfigGetReturnsStandardConfigResponse(t *testing.T) {
	cfg := &Config{
		WorkflowDefinitionPath: "/pkg/wf.invoice/flow/definition.json",
		DBPath:                 "/var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/wf_instances.db",
		SYTimerL2Name:          "SY.timer@motherbee",
		TenantID:               "tnt:test-1",
		GCRetentionDays:        7,
		GCIntervalSeconds:      3600,
		System: &ManagedSystemConfig{
			NodeName:      "WF.invoice@motherbee",
			HiveID:        "motherbee",
			ConfigVersion: 4,
			PackagePath:   "/pkg/wf.invoice",
		},
	}
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-cfg-get"}
	actx := makeActx(t, disp, &mockTimerSender{})
	rt := &NodeRuntime{
		Def:        mustLoadValidDefinition(t),
		DefJSON:    validWorkflowJSON(),
		Registry:   NewInstanceRegistry(),
		Store:      actx.Store,
		ActCtx:     actx,
		NodeUUID:   "wf-uuid-cfg-get",
		NodeName:   "WF.invoice@motherbee",
		Config:     cloneConfig(cfg),
		ConfigPath: filepath.Join(t.TempDir(), "config.json"),
	}

	request, err := sdk.BuildNodeConfigGetMessage(
		"caller-uuid-1",
		"WF.invoice@motherbee",
		sdk.NodeConfigGetPayload{NodeName: "WF.invoice@motherbee"},
		sdk.NodeConfigEnvelopeOptions{},
		"trace-config-get-1",
	)
	if err != nil {
		t.Fatalf("BuildNodeConfigGetMessage: %v", err)
	}

	if err := Dispatch(context.Background(), request, rt); err != nil {
		t.Fatalf("Dispatch: %v", err)
	}
	if len(disp.sent) != 1 {
		t.Fatalf("expected one CONFIG_RESPONSE, got %d", len(disp.sent))
	}
	if got := derefString(disp.sent[0].Meta.Msg); got != sdk.MSGConfigResponse {
		t.Fatalf("expected %q, got %q", sdk.MSGConfigResponse, got)
	}
	resp, err := sdk.ParseNodeConfigResponse(&disp.sent[0])
	if err != nil {
		t.Fatalf("ParseNodeConfigResponse: %v", err)
	}
	if !resp.OK {
		t.Fatalf("expected ok response, got %+v", resp)
	}
	if resp.NodeName != "WF.invoice@motherbee" {
		t.Fatalf("unexpected node_name %q", resp.NodeName)
	}
}

func TestWFConfigSetPersistsManagedConfigAndRequiresRestart(t *testing.T) {
	dir := t.TempDir()
	configPath := filepath.Join(dir, "config.json")
	initialConfig := `{
  "config_version": 4,
  "managed_by": "SY.orchestrator",
  "workflow_definition_path": "/pkg/wf.invoice/flow/definition.json",
  "db_path": "/var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/wf_instances.db",
  "sy_timer_l2_name": "SY.timer@motherbee",
  "gc_retention_days": 7,
  "gc_interval_seconds": 3600,
  "_system": {
    "node_name": "WF.invoice@motherbee",
    "hive_id": "motherbee",
    "package_path": "/pkg/wf.invoice",
    "config_version": 4
  }
}`
	if err := os.WriteFile(configPath, []byte(initialConfig), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-cfg-set"}
	actx := makeActx(t, disp, &mockTimerSender{})
	rt := &NodeRuntime{
		Def:        mustLoadValidDefinition(t),
		DefJSON:    validWorkflowJSON(),
		Registry:   NewInstanceRegistry(),
		Store:      actx.Store,
		ActCtx:     actx,
		NodeUUID:   "wf-uuid-cfg-set",
		NodeName:   "WF.invoice@motherbee",
		Config:     cfg,
		ConfigPath: configPath,
	}

	request, err := sdk.BuildNodeConfigSetMessage(
		"caller-uuid-2",
		"WF.invoice@motherbee",
		sdk.NodeConfigSetPayload{
			NodeName:      "WF.invoice@motherbee",
			SchemaVersion: 1,
			ConfigVersion: 5,
			ApplyMode:     sdk.NodeConfigApplyModeReplace,
			Config: map[string]any{
				"workflow_definition_path": "/pkg/wf.invoice/flow/definition.json",
				"db_path":                  "/var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/new.db",
				"sy_timer_l2_name":         "SY.timer@motherbee",
				"gc_retention_days":        14,
				"gc_interval_seconds":      900,
			},
		},
		sdk.NodeConfigEnvelopeOptions{},
		"trace-config-set-1",
	)
	if err != nil {
		t.Fatalf("BuildNodeConfigSetMessage: %v", err)
	}

	if err := Dispatch(context.Background(), request, rt); err != nil {
		t.Fatalf("Dispatch: %v", err)
	}
	resp, err := sdk.ParseNodeConfigResponse(&disp.sent[0])
	if err != nil {
		t.Fatalf("ParseNodeConfigResponse: %v", err)
	}
	if !resp.OK {
		t.Fatalf("expected ok CONFIG_SET response, got %+v", resp)
	}
	if state := derefString(resp.State); state != "restart_required" {
		t.Fatalf("expected state restart_required, got %q", state)
	}
	if version := *resp.ConfigVersion; version != 5 {
		t.Fatalf("expected config_version 5, got %d", version)
	}

	data, err := os.ReadFile(configPath)
	if err != nil {
		t.Fatalf("read persisted config: %v", err)
	}
	text := string(data)
	if !strings.Contains(text, `"managed_by": "SY.orchestrator"`) {
		t.Fatalf("expected top-level orchestrator metadata to be preserved, got %s", text)
	}
	if !strings.Contains(text, `"db_path": "/var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/new.db"`) {
		t.Fatalf("expected new db_path in persisted config, got %s", text)
	}

	var raw map[string]any
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatalf("json.Unmarshal persisted config: %v", err)
	}
	if got := int(raw["config_version"].(float64)); got != 5 {
		t.Fatalf("expected top-level config_version 5, got %d", got)
	}
}

func TestWFConfigSetRejectsSystemMutation(t *testing.T) {
	dir := t.TempDir()
	configPath := filepath.Join(dir, "config.json")
	initialConfig := `{
  "_system": {
    "node_name": "WF.invoice@motherbee",
    "hive_id": "motherbee",
    "package_path": "/pkg/wf.invoice",
    "config_version": 1
  }
}`
	if err := os.WriteFile(configPath, []byte(initialConfig), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}
	disp := &mockDispatcher{l2name: "WF.invoice@motherbee", uuid: "wf-uuid-cfg-set-reject"}
	actx := makeActx(t, disp, &mockTimerSender{})
	rt := &NodeRuntime{
		Def:        mustLoadValidDefinition(t),
		DefJSON:    validWorkflowJSON(),
		Registry:   NewInstanceRegistry(),
		Store:      actx.Store,
		ActCtx:     actx,
		NodeUUID:   "wf-uuid-cfg-set-reject",
		NodeName:   "WF.invoice@motherbee",
		Config:     cfg,
		ConfigPath: configPath,
	}

	request, err := sdk.BuildNodeConfigSetMessage(
		"caller-uuid-3",
		"WF.invoice@motherbee",
		sdk.NodeConfigSetPayload{
			NodeName:      "WF.invoice@motherbee",
			SchemaVersion: 1,
			ConfigVersion: 2,
			ApplyMode:     sdk.NodeConfigApplyModeReplace,
			Config: map[string]any{
				"_system": map[string]any{"node_name": "WF.evil@motherbee"},
			},
		},
		sdk.NodeConfigEnvelopeOptions{},
		"trace-config-set-2",
	)
	if err != nil {
		t.Fatalf("BuildNodeConfigSetMessage: %v", err)
	}

	if err := Dispatch(context.Background(), request, rt); err != nil {
		t.Fatalf("Dispatch: %v", err)
	}
	resp, err := sdk.ParseNodeConfigResponse(&disp.sent[0])
	if err != nil {
		t.Fatalf("ParseNodeConfigResponse: %v", err)
	}
	if resp.OK {
		t.Fatalf("expected CONFIG_SET rejection, got %+v", resp)
	}
}

func mustLoadValidDefinition(t *testing.T) *WorkflowDefinition {
	t.Helper()
	def, err := LoadDefinitionBytes([]byte(validWorkflowJSON()), "", fixedClock)
	if err != nil {
		t.Fatalf("LoadDefinitionBytes: %v", err)
	}
	return def
}
