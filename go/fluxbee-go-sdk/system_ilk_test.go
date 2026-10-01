package sdk

import "testing"

// Values seen live on the 8.x hive (SY.identity's seeds and the Rust nodes' own derivation).
func TestDeterministicSystemIlkIDMatchesIdentitySeeds(t *testing.T) {
	for name, want := range map[string]string{
		"SY.opa.rules@motherbee": "ilk:895382ab-e4de-5cb5-88d7-c79b33e0578f",
		"SY.opa.rules@worker1":   "ilk:04f477b8-1569-5010-9095-9a71cdc68fc8",
		"SY.cognition@worker1":   "ilk:4689af11-6103-5cf7-a46f-700ab1ac8c6b",
	} {
		if got := DeterministicSystemIlkID(name); got != want {
			t.Fatalf("%s: got %s, want %s", name, got, want)
		}
	}
}
