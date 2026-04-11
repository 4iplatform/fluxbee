package sdk

import (
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestTimerWireScheduleRequestMatchesGoldenFixture(t *testing.T) {
	msg, err := BuildSystemRequest(
		"src-uuid-1",
		"SY.timer@motherbee",
		"TIMER_SCHEDULE",
		map[string]any{
			"fire_in_ms":      int64(120000),
			"target_l2_name":  "WF.demo@motherbee",
			"client_ref":      "demo-timeout",
			"payload":         map[string]any{"kind": "timeout"},
			"missed_policy":   "fire_if_within",
			"missed_within_ms": int64(600000),
			"metadata":        map[string]any{"note": "created by fixture"},
		},
		"trace-schedule-1",
		SystemEnvelopeOptions{},
	)
	if err != nil {
		t.Fatalf("build request: %v", err)
	}

	assertJSONMatchesFixture(t, fixturePath("request_schedule_in.json"), mustMarshalJSON(t, msg))
}

func TestTimerWireListResponseParsesFromGoldenFixture(t *testing.T) {
	msg := mustLoadFixtureMessage(t, "response_timer_list_ok.json")

	timerResp, err := ParseTimerResponse(&msg)
	if err != nil {
		t.Fatalf("parse timer response: %v", err)
	}
	if !timerResp.OK || timerResp.Verb != "TIMER_LIST" {
		t.Fatalf("unexpected timer response: %+v", timerResp)
	}

	var payload timerListResponse
	if err := ParseSystemResponse(&msg, MsgTimerResponse, &payload); err != nil {
		t.Fatalf("parse list payload: %v", err)
	}
	if payload.Count != 1 || len(payload.Timers) != 1 {
		t.Fatalf("unexpected list payload: %+v", payload)
	}
	if payload.Timers[0].UUID != "timer-1" || payload.Timers[0].OwnerL2Name != "WF.demo@motherbee" {
		t.Fatalf("unexpected timer info: %+v", payload.Timers[0])
	}
}

func TestTimerWireErrorResponseParsesFromGoldenFixture(t *testing.T) {
	msg := mustLoadFixtureMessage(t, "response_timer_error.json")

	timerResp, err := ParseTimerResponse(&msg)
	if err != nil {
		t.Fatalf("parse timer response: %v", err)
	}
	respErr := timerResp.ResponseError()
	if respErr == nil {
		t.Fatalf("expected typed response error")
	}
	if respErr.Code != "TIMER_NOT_FOUND" || respErr.Verb != "TIMER_CANCEL" {
		t.Fatalf("unexpected response error: %+v", respErr)
	}
}

func TestTimerWireFiredEventParsesFromGoldenFixture(t *testing.T) {
	msg := mustLoadFixtureMessage(t, "event_timer_fired.json")

	event, err := ParseFiredEvent(msg)
	if err != nil {
		t.Fatalf("parse fired event: %v", err)
	}
	if event.TimerUUID != "timer-1" || !event.IsLastFire {
		t.Fatalf("unexpected fired event: %+v", event)
	}
}

func TestTimerWireGoldenFixturesUseRustProtocolEnvelopeFields(t *testing.T) {
	for _, name := range []string{
		"request_schedule_in.json",
		"response_timer_list_ok.json",
		"response_timer_error.json",
		"event_timer_fired.json",
	} {
		raw := mustReadFile(t, fixturePath(name))
		var decoded map[string]any
		if err := json.Unmarshal(raw, &decoded); err != nil {
			t.Fatalf("unmarshal %s: %v", name, err)
		}

		meta, ok := decoded["meta"].(map[string]any)
		if !ok {
			t.Fatalf("%s missing meta object", name)
		}
		if _, exists := meta["type"]; !exists {
			t.Fatalf("%s missing meta.type", name)
		}
		if _, exists := meta["msg_type"]; exists {
			t.Fatalf("%s unexpectedly uses legacy msg_type field", name)
		}

		routing, ok := decoded["routing"].(map[string]any)
		if !ok {
			t.Fatalf("%s missing routing object", name)
		}
		if _, exists := routing["trace_id"]; !exists {
			t.Fatalf("%s missing routing.trace_id", name)
		}
	}
}

func fixturePath(name string) string {
	return filepath.Join("testdata", "timer_wire", name)
}

func mustLoadFixtureMessage(t *testing.T, name string) Message {
	t.Helper()
	raw := mustReadFile(t, fixturePath(name))
	var msg Message
	if err := json.Unmarshal(raw, &msg); err != nil {
		t.Fatalf("unmarshal fixture %s: %v", name, err)
	}
	return msg
}

func mustReadFile(t *testing.T, path string) []byte {
	t.Helper()
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	return raw
}

func mustMarshalJSON(t *testing.T, value any) []byte {
	t.Helper()
	raw, err := json.Marshal(value)
	if err != nil {
		t.Fatalf("marshal json: %v", err)
	}
	return raw
}

func assertJSONMatchesFixture(t *testing.T, path string, actual []byte) {
	t.Helper()
	expectedRaw := mustReadFile(t, path)

	var expected any
	if err := json.Unmarshal(expectedRaw, &expected); err != nil {
		t.Fatalf("unmarshal expected fixture %s: %v", path, err)
	}
	var got any
	if err := json.Unmarshal(actual, &got); err != nil {
		t.Fatalf("unmarshal actual json: %v", err)
	}
	if !reflect.DeepEqual(expected, got) {
		t.Fatalf("json mismatch for %s\nexpected=%s\nactual=%s", path, string(expectedRaw), string(actual))
	}
}
