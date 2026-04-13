package sdk

import "testing"

func TestBuildAndParseNodeConfigGet(t *testing.T) {
	reqID := "req-1"
	requestedBy := "archi"
	msg, err := BuildNodeConfigGetMessage(
		"src-1",
		"SY.opa.rules@motherbee",
		NodeConfigGetPayload{
			NodeName:    "SY.opa.rules@motherbee",
			RequestID:   &reqID,
			RequestedBy: &requestedBy,
		},
		NodeConfigEnvelopeOptions{},
		"trace-1",
	)
	if err != nil {
		t.Fatalf("build config get: %v", err)
	}
	if !IsNodeConfigGetMessage(&msg) {
		t.Fatalf("expected config get message")
	}
	parsed, err := ParseNodeConfigRequest(&msg)
	if err != nil {
		t.Fatalf("parse config get: %v", err)
	}
	if parsed.Get == nil || parsed.Get.NodeName != "SY.opa.rules@motherbee" {
		t.Fatalf("unexpected parsed get payload: %+v", parsed)
	}
}

func TestBuildNodeConfigResponseKeepsTrace(t *testing.T) {
	msg, err := BuildNodeConfigGetMessage(
		"caller-1",
		"SY.opa.rules@motherbee",
		NodeConfigGetPayload{NodeName: "SY.opa.rules@motherbee"},
		NodeConfigEnvelopeOptions{},
		"trace-2",
	)
	if err != nil {
		t.Fatalf("build incoming: %v", err)
	}
	resp, err := BuildNodeConfigResponseMessage(&msg, "runtime-1", map[string]any{
		"ok":        true,
		"node_name": "SY.opa.rules@motherbee",
	})
	if err != nil {
		t.Fatalf("build response: %v", err)
	}
	if !IsNodeConfigResponseMessage(&resp) {
		t.Fatalf("expected config response")
	}
	if resp.Routing.TraceID != "trace-2" {
		t.Fatalf("trace mismatch: %s", resp.Routing.TraceID)
	}
	if !resp.Routing.Dst.IsUnicast() || resp.Routing.Dst.Value() != "caller-1" {
		t.Fatalf("unexpected destination")
	}
}
