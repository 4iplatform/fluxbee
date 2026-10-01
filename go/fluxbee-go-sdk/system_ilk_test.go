package sdk

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// The vectors are shared with the Rust SDK (crates/fluxbee_sdk/src/identity.rs), which seeds these
// ILKs in SY.identity: the same name must give the same ILK on both sides, untrimmed.
func TestDeterministicSystemIlkIDMatchesTheVectorsSharedWithRust(t *testing.T) {
	raw, err := os.ReadFile(filepath.Join("testdata", "system_ilk", "vectors.json"))
	if err != nil {
		t.Fatalf("read vectors: %v", err)
	}
	var fixture struct {
		Vectors []struct {
			NodeName string `json:"node_name"`
			Ilk      string `json:"ilk"`
		} `json:"vectors"`
	}
	if err := json.Unmarshal(raw, &fixture); err != nil {
		t.Fatalf("decode vectors: %v", err)
	}
	if len(fixture.Vectors) == 0 {
		t.Fatal("no vectors")
	}
	for _, v := range fixture.Vectors {
		if got := DeterministicSystemIlkID(v.NodeName); got != v.Ilk {
			t.Fatalf("%q: got %s, want %s", v.NodeName, got, v.Ilk)
		}
	}
}
