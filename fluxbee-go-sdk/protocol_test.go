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
	if msg.Routing.SrcL2Name != nil {
		t.Fatalf("outbound message must not set src_l2_name: %+v", msg.Routing)
	}
}

func TestRoutingSrcL2NameRoundtrip(t *testing.T) {
	raw := []byte(`{
		"routing": {
			"src": "src-uuid-1",
			"src_l2_name": "WF.demo@motherbee",
			"dst": "dst-uuid-1",
			"ttl": 16,
			"trace_id": "trace-1"
		},
		"meta": { "type": "system", "msg": "HELLO" },
		"payload": {}
	}`)

	var msg Message
	if err := json.Unmarshal(raw, &msg); err != nil {
		t.Fatalf("unmarshal message: %v", err)
	}
	if msg.Routing.SrcL2Name == nil || *msg.Routing.SrcL2Name != "WF.demo@motherbee" {
		t.Fatalf("unexpected src_l2_name: %+v", msg.Routing)
	}
}

// L2-LOOKUP-14: SourceL2Name() helper

func TestSourceL2NameReturnsStampedValue(t *testing.T) {
	raw := []byte(`{
		"routing": {
			"src": "abc-uuid",
			"src_l2_name": "AI.chat@motherbee",
			"dst": "dst-uuid",
			"ttl": 16,
			"trace_id": "trace-x"
		},
		"meta": { "type": "system", "msg": "ECHO" },
		"payload": {}
	}`)
	var msg Message
	if err := json.Unmarshal(raw, &msg); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if got := msg.SourceL2Name(); got != "AI.chat@motherbee" {
		t.Fatalf("expected AI.chat@motherbee, got %q", got)
	}
}

func TestSourceL2NameReturnsEmptyWhenAbsent(t *testing.T) {
	msg, err := BuildSystemBroadcast("abc-uuid", "trace-x", MSGEcho, map[string]any{}, 1)
	if err != nil {
		t.Fatalf("build: %v", err)
	}
	if got := msg.SourceL2Name(); got != "" {
		t.Fatalf("expected empty string, got %q", got)
	}
}

// L2-LOOKUP-17: wire compatibility — src_l2_name absent in outbound, present in received

func TestSerializeRoundtripWithoutSrcL2Name(t *testing.T) {
	msg, err := BuildSystemMessage("uuid-sender", UnicastDestination("uuid-dst"), 8, "trace-rt", MSGEcho, map[string]any{"key": "value"})
	if err != nil {
		t.Fatalf("build: %v", err)
	}
	encoded, err := json.Marshal(msg)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	var raw map[string]any
	if err := json.Unmarshal(encoded, &raw); err != nil {
		t.Fatalf("unmarshal to map: %v", err)
	}
	routing, _ := raw["routing"].(map[string]any)
	if _, exists := routing["src_l2_name"]; exists {
		t.Fatalf("src_l2_name must be absent in outbound message JSON")
	}

	var decoded Message
	if err := json.Unmarshal(encoded, &decoded); err != nil {
		t.Fatalf("decode roundtrip: %v", err)
	}
	if decoded.Routing.SrcL2Name != nil {
		t.Fatalf("roundtrip: expected nil SrcL2Name, got %q", *decoded.Routing.SrcL2Name)
	}
}

func TestSerializeRoundtripWithSrcL2Name(t *testing.T) {
	raw := []byte(`{
		"routing": {
			"src": "uuid-sender",
			"src_l2_name": "IO.webchat@hivename",
			"dst": "uuid-dst",
			"ttl": 8,
			"trace_id": "trace-rt2"
		},
		"meta": { "type": "system", "msg": "ECHO" },
		"payload": {}
	}`)
	var msg Message
	if err := json.Unmarshal(raw, &msg); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if msg.SourceL2Name() != "IO.webchat@hivename" {
		t.Fatalf("unexpected SourceL2Name: %q", msg.SourceL2Name())
	}
	reEncoded, err := json.Marshal(msg)
	if err != nil {
		t.Fatalf("re-marshal: %v", err)
	}
	var reRaw map[string]any
	if err := json.Unmarshal(reEncoded, &reRaw); err != nil {
		t.Fatalf("re-unmarshal: %v", err)
	}
	routing, _ := reRaw["routing"].(map[string]any)
	if routing["src_l2_name"] != "IO.webchat@hivename" {
		t.Fatalf("src_l2_name not preserved in re-serialization: %+v", routing)
	}
}

func TestMessagesWithoutSrcL2NameFieldDeserializeCleanly(t *testing.T) {
	// Control messages from the router itself (UNREACHABLE, ANNOUNCE) never carry
	// src_l2_name — they must parse without errors and return empty from SourceL2Name.
	raw := []byte(`{
		"routing": {
			"src": "uuid-router",
			"dst": "uuid-dst",
			"ttl": 16,
			"trace_id": "trace-old"
		},
		"meta": { "type": "system", "msg": "UNREACHABLE" },
		"payload": { "original_dst": "uuid-x", "reason": "NODE_NOT_FOUND" }
	}`)
	var msg Message
	if err := json.Unmarshal(raw, &msg); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if msg.Routing.SrcL2Name != nil {
		t.Fatalf("expected nil SrcL2Name for router control message")
	}
	if msg.SourceL2Name() != "" {
		t.Fatalf("expected empty SourceL2Name, got %q", msg.SourceL2Name())
	}
}
