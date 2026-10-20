//go:build linux

package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
)

type stubRouterTransport struct {
	sent []fluxbeesdk.Message
}

func (s *stubRouterTransport) SendSDK(msg fluxbeesdk.Message) {
	s.sent = append(s.sent, msg)
}

func TestHandleNodeConfigGetReturnsContractAndSnapshots(t *testing.T) {
	oldStateDir := stateDir
	stateDir = t.TempDir()
	defer func() { stateDir = oldStateDir }()

	for _, dir := range []string{"current", "staged", "backup"} {
		if err := os.MkdirAll(filepath.Join(stateDir, dir), 0o755); err != nil {
			t.Fatalf("mkdir %s: %v", dir, err)
		}
	}
	if err := writeMetadata(filepath.Join(stateDir, "current", "metadata.json"), PolicyMetadata{
		Version:    7,
		Hash:       "sha256:current",
		Entrypoint: "router/target",
	}); err != nil {
		t.Fatalf("write current metadata: %v", err)
	}

	router := &stubRouterTransport{}
	service := &Service{
		hiveID:   "motherbee",
		nodeUUID: uuid.MustParse("11111111-1111-1111-1111-111111111111"),
		nodeName: "SY.opa.rules@motherbee",
		testSend: router.SendSDK,
	}

	req, err := fluxbeesdk.BuildNodeConfigGetMessage(
		"22222222-2222-2222-2222-222222222222",
		service.nodeName,
		fluxbeesdk.NodeConfigGetPayload{NodeName: service.nodeName},
		fluxbeesdk.NodeConfigEnvelopeOptions{},
		"trace-config-get",
	)
	if err != nil {
		t.Fatalf("build config get: %v", err)
	}

	service.handleNodeConfigGet(req)

	if len(router.sent) != 1 {
		t.Fatalf("expected 1 response, got %d", len(router.sent))
	}
	parsed, err := fluxbeesdk.ParseNodeConfigResponse(&router.sent[0])
	if err != nil {
		t.Fatalf("parse config response: %v", err)
	}
	if !parsed.OK || parsed.NodeName != service.nodeName {
		t.Fatalf("unexpected response header: %+v", parsed)
	}
	if parsed.Contract == nil || parsed.EffectiveConfig == nil {
		t.Fatalf("expected contract and effective_config in response: %+v", parsed)
	}
}

func TestHandleNodeConfigSetRejectsNonObjectConfig(t *testing.T) {
	router := &stubRouterTransport{}
	service := &Service{
		hiveID:   "motherbee",
		nodeUUID: uuid.MustParse("11111111-1111-1111-1111-111111111111"),
		nodeName: "SY.opa.rules@motherbee",
		testSend: router.SendSDK,
	}

	req, err := fluxbeesdk.BuildNodeConfigSetMessage(
		"22222222-2222-2222-2222-222222222222",
		service.nodeName,
		fluxbeesdk.NodeConfigSetPayload{
			NodeName:      service.nodeName,
			SchemaVersion: 1,
			ConfigVersion: 1,
			ApplyMode:     fluxbeesdk.NodeConfigApplyModeReplace,
			Config:        "not-an-object",
		},
		fluxbeesdk.NodeConfigEnvelopeOptions{},
		"trace-config-set",
	)
	if err != nil {
		t.Fatalf("build config set: %v", err)
	}

	service.handleNodeConfigSet(req)

	if len(router.sent) != 1 {
		t.Fatalf("expected 1 response, got %d", len(router.sent))
	}
	var payload struct {
		OK    bool `json:"ok"`
		Error struct {
			Code string `json:"code"`
		} `json:"error"`
	}
	if err := json.Unmarshal(router.sent[0].Payload, &payload); err != nil {
		t.Fatalf("decode response payload: %v", err)
	}
	if payload.OK {
		t.Fatalf("expected error response, got ok")
	}
	if payload.Error.Code != "INVALID_CONFIG_SET" {
		t.Fatalf("unexpected error code: %q", payload.Error.Code)
	}
}

func TestBroadcastOpaReloadUsesSystemBroadcastEnvelope(t *testing.T) {
	router := &stubRouterTransport{}
	service := &Service{
		hiveID:   "motherbee",
		nodeUUID: uuid.MustParse("11111111-1111-1111-1111-111111111111"),
		nodeName: "SY.opa.rules@motherbee",
		testSend: router.SendSDK,
	}

	service.broadcastOpaReload(9, "sha256:test")

	if len(router.sent) != 1 {
		t.Fatalf("expected 1 broadcast, got %d", len(router.sent))
	}
	msg := router.sent[0]
	if msg.Meta.MsgType != fluxbeesdk.SYSTEMKind || derefString(msg.Meta.Msg) != fluxbeesdk.MSGOPAReload {
		t.Fatalf("unexpected message envelope: %+v", msg.Meta)
	}
	if !msg.Routing.Dst.IsBroadcast() {
		t.Fatalf("expected broadcast destination")
	}
}

func TestHandleQueryGetStatusPreservesQueryResponseShape(t *testing.T) {
	oldStateDir := stateDir
	oldRouterSockDir := routerSockDir
	stateDir = t.TempDir()
	routerSockDir = t.TempDir()
	defer func() {
		stateDir = oldStateDir
		routerSockDir = oldRouterSockDir
	}()

	for _, dir := range []string{"current", "staged"} {
		if err := os.MkdirAll(filepath.Join(stateDir, dir), 0o755); err != nil {
			t.Fatalf("mkdir %s: %v", dir, err)
		}
	}
	if err := writeMetadata(filepath.Join(stateDir, "current", "metadata.json"), PolicyMetadata{
		Version: 4,
		Hash:    "sha256:current",
	}); err != nil {
		t.Fatalf("write current metadata: %v", err)
	}
	if err := writeMetadata(filepath.Join(stateDir, "staged", "metadata.json"), PolicyMetadata{
		Version: 5,
		Hash:    "sha256:staged",
	}); err != nil {
		t.Fatalf("write staged metadata: %v", err)
	}

	router := &stubRouterTransport{}
	service := &Service{
		hiveID:   "motherbee",
		nodeUUID: uuid.MustParse("11111111-1111-1111-1111-111111111111"),
		nodeName: "SY.opa.rules@motherbee",
		testSend: router.SendSDK,
	}

	request, err := fluxbeesdk.BuildMessageEnvelope(
		"22222222-2222-2222-2222-222222222222",
		fluxbeesdk.UnicastDestination(service.nodeName),
		16,
		"trace-query",
		"query",
		nil,
		func() *string {
			value := "get_status"
			return &value
		}(),
		map[string]any{},
	)
	if err != nil {
		t.Fatalf("build query request: %v", err)
	}

	service.handleQuery(request)

	if len(router.sent) != 1 {
		t.Fatalf("expected 1 query response, got %d", len(router.sent))
	}
	resp := router.sent[0]
	if resp.Meta.MsgType != "query_response" || derefString(resp.Meta.Action) != "get_status" {
		t.Fatalf("unexpected query response envelope: %+v", resp.Meta)
	}
	var payload map[string]any
	if err := json.Unmarshal(resp.Payload, &payload); err != nil {
		t.Fatalf("decode query response payload: %v", err)
	}
	if payload["status"] != "ok" {
		t.Fatalf("unexpected status payload: %+v", payload)
	}
}

func TestHandleQueryGetPolicyPreservesQueryResponseShape(t *testing.T) {
	oldStateDir := stateDir
	stateDir = t.TempDir()
	defer func() { stateDir = oldStateDir }()

	if err := os.MkdirAll(filepath.Join(stateDir, "current"), 0o755); err != nil {
		t.Fatalf("mkdir current: %v", err)
	}
	if err := writeMetadata(filepath.Join(stateDir, "current", "metadata.json"), PolicyMetadata{
		Version:    6,
		Hash:       "sha256:policy",
		Entrypoint: "router/target",
	}); err != nil {
		t.Fatalf("write metadata: %v", err)
	}
	if err := os.WriteFile(filepath.Join(stateDir, "current", "policy.rego"), []byte("package router\ndefault allow := true\n"), 0o644); err != nil {
		t.Fatalf("write rego: %v", err)
	}

	router := &stubRouterTransport{}
	service := &Service{
		hiveID:   "motherbee",
		nodeUUID: uuid.MustParse("11111111-1111-1111-1111-111111111111"),
		nodeName: "SY.opa.rules@motherbee",
		testSend: router.SendSDK,
	}

	request, err := fluxbeesdk.BuildMessageEnvelope(
		"22222222-2222-2222-2222-222222222222",
		fluxbeesdk.UnicastDestination(service.nodeName),
		16,
		"trace-policy",
		"query",
		nil,
		func() *string {
			value := "get_policy"
			return &value
		}(),
		map[string]any{},
	)
	if err != nil {
		t.Fatalf("build query request: %v", err)
	}

	service.handleQuery(request)

	if len(router.sent) != 1 {
		t.Fatalf("expected 1 query response, got %d", len(router.sent))
	}
	resp := router.sent[0]
	if resp.Meta.MsgType != "query_response" || derefString(resp.Meta.Action) != "get_policy" {
		t.Fatalf("unexpected query response envelope: %+v", resp.Meta)
	}
	var payload map[string]any
	if err := json.Unmarshal(resp.Payload, &payload); err != nil {
		t.Fatalf("decode query response payload: %v", err)
	}
	if payload["status"] != "ok" || payload["rego"] == nil {
		t.Fatalf("unexpected policy payload: %+v", payload)
	}
}

// A check is a read (A-76): CONFIG_CHANGED "check", as SY.admin sends it, compiles in a scratch
// directory and is answered at once with a CONFIG_RESPONSE that echoes its check_id and has no
// version. Nothing under the state dir changes, staged/ included (a check between a compile and
// its apply), last_error stays as it was, and the SHM region is never touched: these services have
// none.
func TestACheckIsAReadThatEchoesItsCheckID(t *testing.T) {
	oldStateDir := stateDir
	stateDir = t.TempDir()
	defer func() { stateDir = oldStateDir }()
	staged := PolicyMetadata{Version: 5, Hash: "sha256:staged", Entrypoint: defaultEntrypoint}
	if err := writePolicyFiles(filepath.Join(stateDir, "staged"), []byte("staged wasm"), staged, "package staged\n"); err != nil {
		t.Fatalf("stage a policy: %v", err)
	}
	before := snapshotFiles(t, stateDir)

	const adminUUID = "33333333-3333-3333-3333-333333333333"
	cases := []struct {
		name   string
		hiveID string
		origin string
		rego   string
		status string
		code   string
	}{
		{name: "valid rego", hiveID: fluxbeesdk.PrimaryHiveID, origin: fluxbeesdk.PrimaryAdminNode, rego: testRego, status: "ok"},
		{name: "invalid rego", hiveID: fluxbeesdk.PrimaryHiveID, origin: fluxbeesdk.PrimaryAdminNode, rego: "package router\n\ndefault target =\n", status: "error", code: "COMPILE_ERROR"},
		{name: "rego missing", hiveID: fluxbeesdk.PrimaryHiveID, origin: fluxbeesdk.PrimaryAdminNode, rego: "", status: "error", code: "COMPILE_ERROR"},
		{name: "not the motherbee", hiveID: "worker1", origin: fluxbeesdk.PrimaryAdminNode, rego: testRego, status: "error", code: "NOT_PRIMARY"},
		{name: "not the admin", hiveID: fluxbeesdk.PrimaryHiveID, origin: "AI.rogue@motherbee", rego: testRego, status: "error", code: "UNAUTHORIZED"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			router := &stubRouterTransport{}
			service := &Service{
				hiveID:   tc.hiveID,
				nodeUUID: uuid.New(),
				nodeName: "SY.opa.rules@" + tc.hiveID,
				testSend: router.SendSDK,
			}
			service.setLastError("an earlier failure")
			checkID := uuid.NewString()
			changed := "CONFIG_CHANGED"
			request, err := fluxbeesdk.BuildMessageEnvelope(
				adminUUID,
				fluxbeesdk.UnicastDestination(service.nodeName),
				16, uuid.NewString(), fluxbeesdk.SYSTEMKind, &changed, nil,
				map[string]any{
					"subsystem":  "opa",
					"action":     "check",
					"auto_apply": nil,
					"version":    0,
					"config":     map[string]any{"rego": tc.rego, "entrypoint": defaultEntrypoint, "check_id": checkID},
					"hive":       tc.hiveID,
				},
			)
			if err != nil {
				t.Fatalf("build check: %v", err)
			}
			origin := tc.origin
			request.Routing.SrcL2Name = &origin

			service.handleMessage(request)

			if len(router.sent) != 1 {
				t.Fatalf("want 1 answer, got %d", len(router.sent))
			}
			answer := router.sent[0]
			if derefString(answer.Meta.Msg) != "CONFIG_RESPONSE" || !answer.Routing.Dst.IsUnicast() || answer.Routing.Dst.Value() != adminUUID {
				t.Fatalf("want CONFIG_RESPONSE to the requester, got %s to %+v", derefString(answer.Meta.Msg), answer.Routing.Dst)
			}
			var payload map[string]any
			if err := json.Unmarshal(answer.Payload, &payload); err != nil {
				t.Fatalf("decode answer: %v", err)
			}
			if payload["subsystem"] != "opa" || payload["action"] != "check" || payload["check_id"] != checkID ||
				payload["status"] != tc.status || payload["hive"] != tc.hiveID {
				t.Fatalf("answer: %v", payload)
			}
			if _, ok := payload["version"]; ok {
				t.Fatalf("a check's answer carries no version: %v", payload)
			}
			if tc.status == "ok" {
				if hash, _ := payload["hash"].(string); !strings.HasPrefix(hash, "sha256:") || payload["wasm_size_bytes"] == float64(0) || payload["compile_time_ms"] == nil {
					t.Fatalf("ok answer without the compile result: %v", payload)
				}
			} else if payload["error_code"] != tc.code || payload["error_detail"] == "" {
				t.Fatalf("want %s, got %v", tc.code, payload)
			}
			service.statusMu.Lock()
			lastError := service.syncStatus.lastError
			service.statusMu.Unlock()
			if lastError != "an earlier failure" {
				t.Fatalf("a check changed last_error to %q", lastError)
			}
			if after := snapshotFiles(t, stateDir); !reflect.DeepEqual(after, before) {
				t.Fatalf("a check changed the state dir:\nbefore %v\nafter  %v", before, after)
			}
		})
	}
}

// snapshotFiles maps each file under root (relative path) to the sha256 of its content.
func snapshotFiles(t *testing.T, root string) map[string]string {
	t.Helper()
	files := map[string]string{}
	err := filepath.WalkDir(root, func(path string, entry os.DirEntry, err error) error {
		if err != nil || entry.IsDir() {
			return err
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(root, path)
		if err != nil {
			return err
		}
		sum := sha256.Sum256(data)
		files[rel] = hex.EncodeToString(sum[:])
		return nil
	})
	if err != nil {
		t.Fatalf("walk %s: %v", root, err)
	}
	return files
}
