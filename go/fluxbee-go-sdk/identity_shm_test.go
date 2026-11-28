package sdk

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// The identity SHM's layout is shared with src/shm and the Rust SDK (crates/fluxbee_sdk/src/
// identity.rs), whose tests read the same file: an entry read at another offset misses every ILK.
func TestIlkEntryLayoutMatchesTheFixtureSharedWithRust(t *testing.T) {
	raw, err := os.ReadFile(filepath.Join("testdata", "identity_layout", "ilk_entry.json"))
	if err != nil {
		t.Fatalf("read the layout: %v", err)
	}
	var fixture struct {
		Version      uint32          `json:"version"`
		IlkEntrySize int             `json:"ilk_entry_size"`
		Offsets      map[string]int  `json:"offsets"`
		WorkRoles    map[string]byte `json:"work_roles"`
	}
	if err := json.Unmarshal(raw, &fixture); err != nil {
		t.Fatalf("decode the layout: %v", err)
	}
	if fixture.Version != identityVersionCurrent {
		t.Fatalf("version: fixture %d, Go %d", fixture.Version, identityVersionCurrent)
	}
	if fixture.IlkEntrySize != ilkEntrySize {
		t.Fatalf("entry size: fixture %d, Go %d", fixture.IlkEntrySize, ilkEntrySize)
	}
	// The offsets this SDK reads, and document_hash, which it names without reading.
	for field, offset := range map[string]int{
		"ilk_id":        ilkOffIlkID,
		"ilk_type":      ilkOffType,
		"flags":         ilkOffFlags,
		"tenant_id":     ilkOffTenantID,
		"display_name":  ilkOffDisplayName,
		"handler_node":  ilkOffHandlerNode,
		"works_for":     ilkOffWorksFor,
		"work_role":     ilkOffWorkRole,
		"document_hash": ilkOffDocumentHash,
	} {
		got, ok := fixture.Offsets[field]
		if !ok || got != offset {
			t.Fatalf("%s: fixture %d (%v), Go %d", field, got, ok, offset)
		}
	}
	if len(fixture.WorkRoles) != 6 || fixture.WorkRoles["none"] != 0 {
		t.Fatalf("work roles: %v", fixture.WorkRoles)
	}
	for name, code := range fixture.WorkRoles {
		want := name
		if name == "none" {
			want = ""
		}
		if got := ilkWorkRoleName(code); got != want {
			t.Fatalf("work role %d: %q, want %q", code, got, want)
		}
	}
}
