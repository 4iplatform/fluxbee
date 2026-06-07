package sdk

import (
	"encoding/json"
	"testing"
)

func TestBuildSystemRequestDefaultsActionToVerb(t *testing.T) {
	msg, err := BuildSystemRequest(
		"src-1",
		"SY.timer@motherbee",
		"TIMER_NOW",
		map[string]any{},
		"trace-1",
		SystemEnvelopeOptions{},
	)
	if err != nil {
		t.Fatalf("build system request: %v", err)
	}
	if msg.Meta.MsgType != SYSTEMKind {
		t.Fatalf("unexpected msg type: %q", msg.Meta.MsgType)
	}
	if stringValue(msg.Meta.Msg) != "TIMER_NOW" {
		t.Fatalf("unexpected msg: %q", stringValue(msg.Meta.Msg))
	}
	if stringValue(msg.Meta.Action) != "TIMER_NOW" {
		t.Fatalf("unexpected action: %q", stringValue(msg.Meta.Action))
	}
	if !msg.Routing.Dst.IsUnicast() || msg.Routing.Dst.Value() != "SY.timer@motherbee" {
		t.Fatalf("unexpected destination: %+v", msg.Routing.Dst)
	}
}

func TestBuildSystemResponseKeepsTraceAndRepliesToSource(t *testing.T) {
	request, err := BuildSystemRequest(
		"src-1",
		"SY.timer@motherbee",
		"TIMER_NOW",
		map[string]any{},
		"trace-1",
		SystemEnvelopeOptions{},
	)
	if err != nil {
		t.Fatalf("build request: %v", err)
	}

	response, err := BuildSystemResponse(
		&request,
		"timer-uuid",
		MsgTimerResponse,
		map[string]any{"ok": true, "verb": "TIMER_NOW"},
		SystemEnvelopeOptions{},
	)
	if err != nil {
		t.Fatalf("build response: %v", err)
	}
	if response.Routing.TraceID != "trace-1" {
		t.Fatalf("unexpected trace: %q", response.Routing.TraceID)
	}
	if !response.Routing.Dst.IsUnicast() || response.Routing.Dst.Value() != "src-1" {
		t.Fatalf("unexpected reply destination: %+v", response.Routing.Dst)
	}
}

func TestParseSystemResponseErrorBuildsTypedError(t *testing.T) {
	payload, err := json.Marshal(SystemActionResponse{
		OK:   false,
		Verb: "TIMER_SCHEDULE",
		Error: &SystemErrorDetail{
			Code:    "TIMER_BELOW_MINIMUM",
			Message: "minimum timer duration is 60 seconds",
		},
	})
	if err != nil {
		t.Fatalf("marshal response: %v", err)
	}
	msg := Message{
		Meta:    Meta{MsgType: SYSTEMKind, Msg: stringPtr(MsgTimerResponse)},
		Payload: payload,
	}
	respErr, err := ParseSystemResponseError(&msg, MsgTimerResponse)
	if err != nil {
		t.Fatalf("parse system response error: %v", err)
	}
	if respErr == nil || respErr.Code != "TIMER_BELOW_MINIMUM" {
		t.Fatalf("unexpected parsed error: %+v", respErr)
	}
}

func stringPtr(value string) *string {
	return &value
}
