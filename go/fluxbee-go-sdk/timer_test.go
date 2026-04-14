package sdk

import (
	"encoding/json"
	"testing"
)

func TestBuildHelpDescriptorReturnsCanonicalPayload(t *testing.T) {
	payload := BuildHelpDescriptor(
		MsgTimerHelp,
		"SY",
		"SY.timer",
		"1.0",
		"System node providing persistent timers.",
		[]HelpOperationDescriptor{{Verb: "TIMER_NOW", Description: "Return current time."}},
		[]HelpErrorDescriptor{{Code: "TIMER_INTERNAL", Description: "Internal error."}},
	)
	if !payload.OK || payload.Verb != MsgTimerHelp || payload.NodeKind != "SY.timer" {
		t.Fatalf("unexpected help payload: %+v", payload)
	}
}

func TestParseTimerResponsePreservesExtraFields(t *testing.T) {
	raw, err := json.Marshal(map[string]any{
		"ok":             true,
		"verb":           "TIMER_SCHEDULE",
		"timer_uuid":     "timer-1",
		"fire_at_utc_ms": int64(1775577600000),
		"note":           "kept",
	})
	if err != nil {
		t.Fatalf("marshal timer response: %v", err)
	}
	msg := Message{
		Meta:    Meta{MsgType: SYSTEMKind, Msg: stringPtr(MsgTimerResponse)},
		Payload: raw,
	}
	parsed, err := ParseTimerResponse(&msg)
	if err != nil {
		t.Fatalf("parse timer response: %v", err)
	}
	if parsed.Verb != "TIMER_SCHEDULE" || parsed.TimerUUID == nil || *parsed.TimerUUID != "timer-1" {
		t.Fatalf("unexpected parsed timer response: %+v", parsed)
	}
	if parsed.Extra["note"] != "kept" {
		t.Fatalf("expected extra field to be preserved: %+v", parsed.Extra)
	}
}

func TestTimerResponseErrorReturnsTypedSystemError(t *testing.T) {
	resp := &TimerResponse{
		OK:   false,
		Verb: "TIMER_CANCEL",
		Error: &TimerError{
			Code:    "TIMER_NOT_FOUND",
			Message: "timer does not exist",
		},
	}
	err := resp.ResponseError()
	if err == nil || err.Code != "TIMER_NOT_FOUND" {
		t.Fatalf("unexpected timer response error: %+v", err)
	}
}
