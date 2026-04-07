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
