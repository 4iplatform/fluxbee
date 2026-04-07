package sdk

import (
	"encoding/json"
	"testing"
)

func TestDestinationJSONRoundtrip(t *testing.T) {
	cases := []Destination{
		UnicastDestination("SY.admin@motherbee"),
		BroadcastDestination(),
		ResolveDestination(),
	}

	for _, tc := range cases {
		data, err := json.Marshal(tc)
		if err != nil {
			t.Fatalf("marshal destination: %v", err)
		}
		var out Destination
		if err := json.Unmarshal(data, &out); err != nil {
			t.Fatalf("unmarshal destination: %v", err)
		}
		if tc.IsUnicast() != out.IsUnicast() || tc.IsBroadcast() != out.IsBroadcast() || tc.IsResolve() != out.IsResolve() || tc.Value() != out.Value() {
			t.Fatalf("destination mismatch: in=%+v out=%+v", tc, out)
		}
	}
}

func TestBuildHelloUsesSystemEnvelope(t *testing.T) {
	msg, err := BuildHello("node-uuid", "trace-1", NodeHelloPayload{
		UUID:    "node-uuid",
		Name:    "SY.opa.rules@motherbee",
		Version: "1.0",
	})
	if err != nil {
		t.Fatalf("build hello: %v", err)
	}
	if msg.Meta.MsgType != SYSTEMKind {
		t.Fatalf("unexpected msg type: %s", msg.Meta.MsgType)
	}
	if stringValue(msg.Meta.Msg) != MSGHello {
		t.Fatalf("unexpected msg: %s", stringValue(msg.Meta.Msg))
	}
	if !msg.Routing.Dst.IsResolve() {
		t.Fatalf("expected resolve destination")
	}
}

func TestBuildCommandResponseUsesStandardEnvelope(t *testing.T) {
	msg, err := BuildCommandResponse("src-1", "dst-1", "trace-1", "compile_policy", map[string]any{
		"status": "ok",
	})
	if err != nil {
		t.Fatalf("build command response: %v", err)
	}
	if msg.Meta.MsgType != "command_response" {
		t.Fatalf("unexpected msg type: %q", msg.Meta.MsgType)
	}
	if stringValue(msg.Meta.Action) != "compile_policy" {
		t.Fatalf("unexpected action: %q", stringValue(msg.Meta.Action))
	}
	if !msg.Routing.Dst.IsUnicast() || msg.Routing.Dst.Value() != "dst-1" {
		t.Fatalf("unexpected destination: %+v", msg.Routing.Dst)
	}
}

func TestBuildSystemBroadcastUsesBroadcastDestination(t *testing.T) {
	msg, err := BuildSystemBroadcast("src-1", "trace-1", MSGOPAReload, map[string]any{
		"version": 1,
	}, 2)
	if err != nil {
		t.Fatalf("build system broadcast: %v", err)
	}
	if msg.Meta.MsgType != SYSTEMKind || stringValue(msg.Meta.Msg) != MSGOPAReload {
		t.Fatalf("unexpected system envelope: %+v", msg.Meta)
	}
	if !msg.Routing.Dst.IsBroadcast() {
		t.Fatalf("expected broadcast destination")
	}
	if msg.Routing.TTL != 2 {
		t.Fatalf("unexpected ttl: %d", msg.Routing.TTL)
	}
}
