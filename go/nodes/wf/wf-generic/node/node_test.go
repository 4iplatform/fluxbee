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

