package node

import (
	"os"
	"path/filepath"
	"testing"
)

func TestStoreWriteStagedAndReadMetadata(t *testing.T) {
	store := NewStore(t.TempDir())
	meta := WfRulesMetadata{
		Version:         1,
		Hash:            "sha256:test",
		WorkflowName:    "invoice",
		WorkflowType:    "invoice",
		WFSchemaVersion: "1",
		CompiledAt:      "2026-04-16T00:00:00Z",
		GuardCount:      1,
		StateCount:      2,
		ActionCount:     3,
	}
	definition := []byte("{\"wf_schema_version\":\"1\"}\n")
	if err := store.WriteStaged("invoice", definition, meta); err != nil {
		t.Fatalf("WriteStaged: %v", err)
	}
	gotMeta, err := store.ReadMetadata("invoice", "staged")
	if err != nil {
		t.Fatalf("ReadMetadata: %v", err)
	}
	if gotMeta.Version != 1 {
		t.Fatalf("unexpected version %d", gotMeta.Version)
	}
	gotDef, err := store.ReadDefinitionBytes("invoice", "staged")
	if err != nil {
		t.Fatalf("ReadDefinitionBytes: %v", err)
	}
	if string(gotDef) != string(definition) {
		t.Fatalf("unexpected definition bytes %q", string(gotDef))
	}
}

func TestStoreNextVersionUsesHighestKnownSlot(t *testing.T) {
	store := NewStore(t.TempDir())
	_ = store.WriteSlot("invoice", "backup", []byte("{}\n"), WfRulesMetadata{Version: 3})
	_ = store.WriteSlot("invoice", "current", []byte("{}\n"), WfRulesMetadata{Version: 4})
	_ = store.WriteSlot("invoice", "staged", []byte("{}\n"), WfRulesMetadata{Version: 5})
	if got := store.NextVersion("invoice"); got != 6 {
		t.Fatalf("unexpected next version %d", got)
	}
}

func TestStoreListWorkflows(t *testing.T) {
	root := t.TempDir()
	store := NewStore(root)
	_ = os.MkdirAll(filepath.Join(root, "invoice"), 0o755)
	_ = os.MkdirAll(filepath.Join(root, "onboarding"), 0o755)
	got, err := store.ListWorkflows()
	if err != nil {
		t.Fatalf("ListWorkflows: %v", err)
	}
	if len(got) != 2 || got[0] != "invoice" || got[1] != "onboarding" {
		t.Fatalf("unexpected workflows %#v", got)
	}
}
