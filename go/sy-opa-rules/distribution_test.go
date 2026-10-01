//go:build linux

package main

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/uuid"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

const testRego = "package router\n\ndefault target = null\n"

// newTestHive builds a SY.opa.rules for hiveID with its own state dir and SHM region.
func newTestHive(t *testing.T, hiveID string) (*Service, *stubRouterTransport, string) {
	t.Helper()
	state := t.TempDir()
	region, err := openOrCreateOpaRegion("/jsr-opa-test-"+uuid.NewString()[:8], uuid.New())
	if err != nil {
		t.Fatalf("opa region: %v", err)
	}
	t.Cleanup(func() { _ = os.Remove(filepath.Join("/dev/shm", region.name[1:])) })
	router := &stubRouterTransport{}
	return &Service{
		hiveID:    hiveID,
		nodeUUID:  uuid.New(),
		nodeName:  "SY.opa.rules@" + hiveID,
		opaRegion: region,
		syncKick:  make(chan struct{}, 1),
		testSend:  router.SendSDK,
	}, router, state
}

// on runs fn with the package-level state dir pointed at state (the services share globals).
func on(state string, fn func()) {
	old := stateDir
	stateDir = state
	defer func() { stateDir = old }()
	fn()
}

func TestPublishedPolicyReachesAReplica(t *testing.T) {
	oldDist := policyDistDir
	policyDistDir = t.TempDir() // the Syncthing folder, shared by both ends here
	defer func() { policyDistDir = oldDist }()

	primary, _, primaryState := newTestHive(t, fluxbeesdk.PrimaryHiveID)
	replica, replicaRouter, replicaState := newTestHive(t, "worker1")
	for _, state := range []string{primaryState, replicaState} {
		on(state, func() {
			if err := ensureDirs(); err != nil {
				t.Fatalf("dirs: %v", err)
			}
		})
	}

	// The motherbee compiles, applies and publishes.
	var published PolicyManifest
	on(primaryState, func() {
		if ok, err := primary.handleOpaAction("admin", "compile_apply", 7, &OpaConfigPayload{Rego: testRego}, false, false); !ok || err != nil {
			t.Fatalf("compile_apply: ok=%v err=%v", ok, err)
		}
		var err error
		if published, err = readPolicyManifest(); err != nil || published.Hash == "" || published.Version != 7 {
			t.Fatalf("manifest after publish: %+v err=%v", published, err)
		}
	})

	// A replica refuses to compile: one policy, compiled on the motherbee only.
	on(replicaState, func() {
		if ok, _ := replica.handleOpaAction("admin", "compile_apply", 8, &OpaConfigPayload{Rego: testRego}, false, false); ok {
			t.Fatal("a replica must not compile")
		}
	})

	// The manifest arrived but not the wasm yet: nothing changes, retry later.
	wasmPath := filepath.Join(policyDistDir, published.WasmFile)
	wasm, err := os.ReadFile(wasmPath)
	if err != nil {
		t.Fatalf("published wasm: %v", err)
	}
	if err := os.Remove(wasmPath); err != nil {
		t.Fatal(err)
	}
	on(replicaState, func() {
		if _, err := replica.syncFromPublishedPolicy(); !errors.Is(err, errPolicyNotArrived) {
			t.Fatalf("want errPolicyNotArrived, got %v", err)
		}
		if current, _ := currentPolicy(); current.Hash != "" {
			t.Fatalf("nothing should be installed yet, got %s", current.Hash)
		}
	})
	if err := os.WriteFile(wasmPath, wasm, 0o640); err != nil {
		t.Fatal(err)
	}

	// The admin's notice arrives; the replica installs the policy and answers "sync ok".
	on(replicaState, func() {
		replica.handleSyncNotice("admin-uuid", 7, published.Hash)
		replica.syncOnce()
		if current, _ := currentPolicy(); current.Hash != published.Hash {
			t.Fatalf("replica runs %q, want %q", current.Hash, published.Hash)
		}
	})
	if !sentSyncOK(t, replicaRouter, published.Hash) {
		t.Fatalf("replica did not answer the sync notice: %+v", replicaRouter.sent)
	}

	// A clear on the motherbee reaches the replica too.
	on(primaryState, func() {
		if ok, err := primary.handleOpaAction("admin", "clear", 0, nil, false, false); !ok || err != nil {
			t.Fatalf("clear: ok=%v err=%v", ok, err)
		}
	})
	on(replicaState, func() {
		replica.syncOnce()
		if _, wasm := currentPolicy(); wasm != nil {
			t.Fatal("replica still runs a policy after the clear")
		}
	})
}

// CONFIG_CHANGED passes the router's gate for orchestrators too, and the motherbee publishes what
// it applies to every hive: only the primary admin's are acted on.
func TestConfigChangedIsTakenOnlyFromThePrimaryAdmin(t *testing.T) {
	oldDist := policyDistDir
	policyDistDir = t.TempDir()
	defer func() { policyDistDir = oldDist }()

	primary, router, state := newTestHive(t, fluxbeesdk.PrimaryHiveID)
	changed := "CONFIG_CHANGED"
	send := func(origin string) {
		msg, err := fluxbeesdk.BuildMessageEnvelope(
			"33333333-3333-3333-3333-333333333333",
			fluxbeesdk.UnicastDestination(primary.nodeName),
			16, "trace-changed", "system", &changed, nil,
			map[string]any{"subsystem": "opa", "action": "compile_apply", "version": 9,
				"config": map[string]any{"rego": testRego}},
		)
		if err != nil {
			t.Fatalf("build: %v", err)
		}
		if origin != "" {
			msg.Routing.SrcL2Name = &origin
		}
		primary.handleMessage(msg)
	}
	on(state, func() {
		if err := ensureDirs(); err != nil {
			t.Fatalf("dirs: %v", err)
		}
		for _, origin := range []string{"SY.orchestrator@ingress1", "SY.admin@worker1", ""} {
			send(origin)
			if current, _ := currentPolicy(); current.Hash != "" {
				t.Fatalf("%q: a policy was installed from a refused origin", origin)
			}
		}
		if len(router.sent) != 3 {
			t.Fatalf("want 3 refusals, got %d messages", len(router.sent))
		}
		for _, msg := range router.sent {
			var payload map[string]any
			_ = json.Unmarshal(msg.Payload, &payload)
			if payload["error_code"] != "UNAUTHORIZED" {
				t.Fatalf("want UNAUTHORIZED, got %+v", payload)
			}
		}
		send(fluxbeesdk.PrimaryAdminNode)
		if current, _ := currentPolicy(); current.Hash == "" {
			t.Fatal("the primary admin's compile_apply was not applied")
		}
	})
}

// A fresh notice whose policy has not arrived makes the hive re-check every second, not every 5 s.
func TestAWaitingNoticeShortensTheNextCheck(t *testing.T) {
	replica, _, _ := newTestHive(t, "worker1")
	now := time.Now()
	if got := replica.nextSyncCheck(now); got != policySyncInterval {
		t.Fatalf("no notice: want %v, got %v", policySyncInterval, got)
	}
	replica.pendingSyncs = []pendingSync{{src: "admin", version: 3, hash: "sha256:x", received: now.Add(-2 * time.Second)}}
	if got := replica.nextSyncCheck(now); got != policyNoticeRecheck {
		t.Fatalf("fresh notice: want %v, got %v", policyNoticeRecheck, got)
	}
	if got := replica.nextSyncCheck(now.Add(policyNoticeRecheckFor)); got != policySyncInterval {
		t.Fatalf("old notice: want %v, got %v", policySyncInterval, got)
	}
}

// A notice for a policy that never arrives here (superseded before it synced) is not kept forever.
func TestUnsatisfiedNoticeIsDroppedAfterItsTTL(t *testing.T) {
	replica, router, state := newTestHive(t, "worker1")
	on(state, func() {
		if err := ensureDirs(); err != nil {
			t.Fatalf("dirs: %v", err)
		}
		replica.pendingSyncs = []pendingSync{
			{src: "admin", version: 1, hash: "sha256:superseded", received: time.Now().Add(-policyNoticeTTL - time.Second)},
			{src: "admin", version: 2, hash: "sha256:in-flight", received: time.Now()},
		}
		replica.policyMu.Lock()
		replica.answerPendingSyncs()
		replica.policyMu.Unlock()
	})
	if len(replica.pendingSyncs) != 1 || replica.pendingSyncs[0].hash != "sha256:in-flight" {
		t.Fatalf("pending after the sweep: %+v", replica.pendingSyncs)
	}
	if len(router.sent) != 0 {
		t.Fatalf("nothing is installed, nothing should be answered: %+v", router.sent)
	}
}

// A publish that fails after the apply leaves the motherbee ahead of every other hive; the
// motherbee's own sync check publishes again once the cause is gone.
func TestPrimaryRepublishesAfterAFailedPublish(t *testing.T) {
	oldDist := policyDistDir
	defer func() { policyDistDir = oldDist }()
	base := t.TempDir()
	blocker := filepath.Join(base, "not-a-dir")
	if err := os.WriteFile(blocker, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	policyDistDir = filepath.Join(blocker, "opa") // a file is in the way: publishing fails

	primary, router, state := newTestHive(t, fluxbeesdk.PrimaryHiveID)
	on(state, func() {
		if err := ensureDirs(); err != nil {
			t.Fatalf("dirs: %v", err)
		}
		if ok, err := primary.handleOpaAction("admin", "compile_apply", 4, &OpaConfigPayload{Rego: testRego}, false, true); ok || err == nil {
			t.Fatalf("the publish should have failed: ok=%v err=%v", ok, err)
		}
		current, _ := currentPolicy()
		if current.Hash == "" {
			t.Fatal("the motherbee should run the policy it applied")
		}
		if !sentErrorCode(t, router, "PUBLISH_FAILED") {
			t.Fatalf("no PUBLISH_FAILED answer: %+v", router.sent)
		}

		policyDistDir = filepath.Join(base, "opa") // the cause is gone
		primary.syncOnce()
		manifest, err := readPolicyManifest()
		if err != nil || manifest.Hash != current.Hash {
			t.Fatalf("not re-published: manifest %+v err=%v, want %s", manifest, err, current.Hash)
		}
	})
}

// One write path for the one global policy (SY.admin's /opa/policy*): CONFIG_SET cannot install,
// stage or publish a policy, whatever the operation.
func TestConfigSetCannotWriteThePolicy(t *testing.T) {
	oldDist := policyDistDir
	policyDistDir = t.TempDir()
	defer func() { policyDistDir = oldDist }()

	primary, router, state := newTestHive(t, fluxbeesdk.PrimaryHiveID)
	ops := []string{"compile_apply", "apply", "rollback", "clear", "compile", "check"}
	on(state, func() {
		if err := ensureDirs(); err != nil {
			t.Fatalf("dirs: %v", err)
		}
		for _, op := range ops {
			req, err := fluxbeesdk.BuildNodeConfigSetMessage(
				"22222222-2222-2222-2222-222222222222",
				primary.nodeName,
				fluxbeesdk.NodeConfigSetPayload{
					NodeName:      primary.nodeName,
					SchemaVersion: 1,
					ConfigVersion: 3,
					ApplyMode:     fluxbeesdk.NodeConfigApplyModeReplace,
					Config:        map[string]any{"operation": op, "rego": testRego},
				},
				fluxbeesdk.NodeConfigEnvelopeOptions{},
				"trace-config-set-"+op,
			)
			if err != nil {
				t.Fatalf("build config set: %v", err)
			}
			origin := fluxbeesdk.PrimaryAdminNode
			req.Routing.SrcL2Name = &origin
			primary.handleNodeConfigSet(req)
		}
		if current, _ := currentPolicy(); current.Hash != "" {
			t.Fatal("CONFIG_SET installed a policy")
		}
		if staged, _ := readMetadata(filepath.Join(stateDir, "staged", "metadata.json")); staged.Version != 0 || staged.Hash != "" {
			t.Fatalf("CONFIG_SET staged a policy: %+v", staged)
		}
		if _, err := readPolicyManifest(); err == nil {
			t.Fatal("CONFIG_SET published a policy")
		}
	})
	if len(router.sent) != len(ops) {
		t.Fatalf("want %d refusals, got %d messages", len(ops), len(router.sent))
	}
	for _, msg := range router.sent {
		var payload struct {
			OK    bool `json:"ok"`
			Error struct {
				Code string `json:"code"`
			} `json:"error"`
		}
		_ = json.Unmarshal(msg.Payload, &payload)
		if payload.OK || payload.Error.Code != "UNSUPPORTED_OPERATION" {
			t.Fatalf("want UNSUPPORTED_OPERATION, got %s", string(msg.Payload))
		}
	}
}

func sentErrorCode(t *testing.T, router *stubRouterTransport, code string) bool {
	t.Helper()
	for _, msg := range router.sent {
		var payload map[string]any
		if err := json.Unmarshal(msg.Payload, &payload); err != nil {
			continue
		}
		if payload["error_code"] == code {
			return true
		}
	}
	return false
}

func sentSyncOK(t *testing.T, router *stubRouterTransport, hash string) bool {
	t.Helper()
	for _, msg := range router.sent {
		if derefString(msg.Meta.Msg) != "CONFIG_RESPONSE" {
			continue
		}
		var payload map[string]any
		if err := json.Unmarshal(msg.Payload, &payload); err != nil {
			continue
		}
		if payload["action"] == "sync" && payload["status"] == "ok" && payload["hash"] == hash {
			return true
		}
	}
	return false
}
