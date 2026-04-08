//go:build linux

package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	fluxbeesdk "github.com/4iplatform/json-router/sy-opa-rules/sdk"
	"github.com/google/uuid"
)

type stubRouterTransport struct {
	sent []fluxbeesdk.Message
	last string
}

func (s *stubRouterTransport) SendSDK(msg fluxbeesdk.Message) {
	s.sent = append(s.sent, msg)
}

func (s *stubRouterTransport) LastPeer() string {
	return s.last
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
		hiveID:     "motherbee",
		nodeUUID:   uuid.MustParse("11111111-1111-1111-1111-111111111111"),
		nodeName:   "SY.opa.rules@motherbee",
		routerConn: router,
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
		hiveID:     "motherbee",
		nodeUUID:   uuid.MustParse("11111111-1111-1111-1111-111111111111"),
		nodeName:   "SY.opa.rules@motherbee",
		routerConn: router,
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
		hiveID:     "motherbee",
		nodeUUID:   uuid.MustParse("11111111-1111-1111-1111-111111111111"),
		nodeName:   "SY.opa.rules@motherbee",
		routerConn: router,
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
		hiveID:     "motherbee",
		nodeUUID:   uuid.MustParse("11111111-1111-1111-1111-111111111111"),
		nodeName:   "SY.opa.rules@motherbee",
		routerConn: router,
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
		hiveID:     "motherbee",
		nodeUUID:   uuid.MustParse("11111111-1111-1111-1111-111111111111"),
		nodeName:   "SY.opa.rules@motherbee",
		routerConn: router,
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

func TestHandleCommandCompilePolicyInvalidPayloadReturnsCommandResponseError(t *testing.T) {
	router := &stubRouterTransport{}
	service := &Service{
		hiveID:     "motherbee",
		nodeUUID:   uuid.MustParse("11111111-1111-1111-1111-111111111111"),
		nodeName:   "SY.opa.rules@motherbee",
		routerConn: router,
	}

	request, err := fluxbeesdk.BuildMessageEnvelope(
		"22222222-2222-2222-2222-222222222222",
		fluxbeesdk.UnicastDestination(service.nodeName),
		16,
		"trace-command",
		"command",
		nil,
		func() *string {
			value := "compile_policy"
			return &value
		}(),
		map[string]any{
			"rego": 17,
		},
	)
	if err != nil {
		t.Fatalf("build command request: %v", err)
	}

	service.handleCommand(request)

	if len(router.sent) != 1 {
		t.Fatalf("expected 1 command response, got %d", len(router.sent))
	}
	resp := router.sent[0]
	if resp.Meta.MsgType != "command_response" || derefString(resp.Meta.Action) != "compile_policy" {
		t.Fatalf("unexpected command response envelope: %+v", resp.Meta)
	}
	var payload map[string]any
	if err := json.Unmarshal(resp.Payload, &payload); err != nil {
		t.Fatalf("decode command response payload: %v", err)
	}
	if payload["status"] != "error" || payload["error_code"] != "COMPILE_ERROR" {
		t.Fatalf("unexpected command error payload: %+v", payload)
	}
}
