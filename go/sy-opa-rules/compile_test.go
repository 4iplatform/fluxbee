package main

import (
	"bytes"
	"os"
	"strings"
	"testing"
)

// The same Rego compiles to the same wasm, and the wasm names no temporary directory
// (FINDINGS A-179).
func TestCompilingTheSameRegoTwiceGivesTheSameWasm(t *testing.T) {
	rego := "package router\n\ndefault allow = false\n\nallow { input.x == 1 }\n"
	first, firstHash, _, err := compileRego(rego, "router/allow")
	if err != nil {
		t.Fatalf("compile: %v", err)
	}
	second, secondHash, _, err := compileRego(rego, "router/allow")
	if err != nil {
		t.Fatalf("compile again: %v", err)
	}
	if !bytes.Equal(first, second) || firstHash != secondHash {
		t.Fatalf("two compiles of one Rego differ: %s and %s", firstHash, secondHash)
	}
	if bytes.Contains(first, []byte("opa-compile")) {
		t.Fatal("the wasm names the temporary directory it was compiled in")
	}
}

// What an operator's check shows of a broken policy names the file and the line, never a
// temporary directory.
func TestACompileErrorNamesThePolicyFileAndLine(t *testing.T) {
	_, _, _, err := compileRego("package router\n\nallow {\n", "router/allow")
	if err == nil {
		t.Fatal("a broken policy compiled")
	}
	if !strings.Contains(err.Error(), "policy.rego:") || strings.Contains(err.Error(), "opa-compile") {
		t.Fatalf("the error should name policy.rego and its line: %v", err)
	}
}

// The router bakes policy/system.wasm and policy/system_route.wasm (src/router/system_policy.rs),
// compiled from policy/system.rego with this compiler. A Rego change committed without them would
// leave the router enforcing the old policy (FINDINGS A-179). UPDATE_SYSTEM_WASM=1 writes them
// instead, on any OS; commit them with the Rego.
func TestTheBakedSystemPolicyIsTheRegoCompiled(t *testing.T) {
	rego, err := os.ReadFile("../../policy/system.rego")
	if err != nil {
		t.Fatalf("read the Rego: %v", err)
	}
	for _, baked := range []struct{ entrypoint, path string }{
		{"fluxbee/system/allow", "../../policy/system.wasm"},
		{"fluxbee/system/frontdesk_route", "../../policy/system_route.wasm"},
	} {
		wasm, hash, _, err := compileRego(string(rego), baked.entrypoint)
		if err != nil {
			t.Fatalf("compile %s: %v", baked.entrypoint, err)
		}
		if os.Getenv("UPDATE_SYSTEM_WASM") == "1" {
			if err := os.WriteFile(baked.path, wasm, 0o644); err != nil {
				t.Fatalf("write %s: %v", baked.path, err)
			}
			t.Logf("wrote %s (%s, %d bytes)", baked.path, hash, len(wasm))
			continue
		}
		committed, err := os.ReadFile(baked.path)
		if err != nil {
			t.Fatalf("read %s: %v", baked.path, err)
		}
		if !bytes.Equal(wasm, committed) {
			t.Errorf("%s is not what policy/system.rego compiles to (%s): run "+
				"UPDATE_SYSTEM_WASM=1 go test -run TestTheBakedSystemPolicyIsTheRegoCompiled . "+
				"in go/sy-opa-rules and commit it with the Rego", baked.path, baked.entrypoint)
		}
	}
}
