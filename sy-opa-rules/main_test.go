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
