package node

import (
	"os"
	"path/filepath"
	"testing"
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
