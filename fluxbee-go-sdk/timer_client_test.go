package sdk

import (
	"context"
	"encoding/json"
	"testing"
	"time"
)

func TestNewTimerClientDerivesLocalTimerNodeName(t *testing.T) {
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: make(chan []byte, 1), state: &connectionState{connected: true}},
		&NodeReceiver{rx: make(chan receivedMessage, 1), state: &connectionState{connected: true}},
		TimerClientConfig{},
	)
	if err != nil {
		t.Fatalf("new timer client: %v", err)
	}
	if client.timerNode != "SY.timer@motherbee" {
		t.Fatalf("unexpected timer node: %q", client.timerNode)
	}
}

func TestTimerClientNowUsesTimerResponse(t *testing.T) {
	tx := make(chan []byte, 1)
	rx := make(chan receivedMessage, 1)
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: tx, state: &connectionState{connected: true}},
		&NodeReceiver{rx: rx, state: &connectionState{connected: true}},
		TimerClientConfig{TimeRetrySchedule: []time.Duration{time.Millisecond}},
	)
	if err != nil {
		t.Fatalf("new timer client: %v", err)
	}

	go func() {
		frame := <-tx
		var request Message
		if err := json.Unmarshal(frame, &request); err != nil {
			t.Errorf("unmarshal request: %v", err)
			return
		}
		if stringValue(request.Meta.Msg) != "TIMER_NOW" {
			t.Errorf("unexpected request verb: %q", stringValue(request.Meta.Msg))
			return
		}
		response, err := BuildSystemResponse(
			&request,
			"timer-uuid",
			MsgTimerResponse,
			map[string]any{
				"ok":          true,
				"verb":        "TIMER_NOW",
				"now_utc_ms":  int64(1775577600123),
				"now_utc_iso": "2026-04-08T15:20:00.123Z",
			},
			SystemEnvelopeOptions{},
		)
		if err != nil {
			t.Errorf("build response: %v", err)
			return
		}
		rx <- receivedMessage{msg: response}
	}()

	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()

	now, err := client.Now(ctx)
	if err != nil {
		t.Fatalf("timer now: %v", err)
	}
	if now.NowUTCMS != 1775577600123 {
		t.Fatalf("unexpected now response: %+v", now)
	}
}

func TestTimerClientNowReturnsTypedUnreachableErrorAfterRetries(t *testing.T) {
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: make(chan []byte, 4), state: &connectionState{connected: true}},
		&NodeReceiver{rx: make(chan receivedMessage), state: &connectionState{connected: true}},
		TimerClientConfig{TimeRetrySchedule: []time.Duration{time.Millisecond, time.Millisecond, time.Millisecond}},
	)
	if err != nil {
		t.Fatalf("new timer client: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Millisecond)
	defer cancel()

	_, err = client.Now(ctx)
	if err == nil {
		t.Fatalf("expected unreachable error")
	}
	respErr, ok := err.(*SystemResponseError)
	if !ok || respErr.Code != "TIMER_UNREACHABLE" {
		t.Fatalf("unexpected error type: %T %+v", err, err)
	}
}

func TestParseFiredEventParsesPayload(t *testing.T) {
	payload, err := json.Marshal(map[string]any{
		"timer_uuid":               "timer-1",
		"owner_l2_name":            "WF.demo@motherbee",
		"kind":                     "oneshot",
		"scheduled_fire_at_utc_ms": int64(1775577600000),
		"actual_fire_at_utc_ms":    int64(1775577600123),
		"fire_count":               int64(1),
		"is_last_fire":             true,
	})
	if err != nil {
		t.Fatalf("marshal fired payload: %v", err)
	}
	event, err := ParseFiredEvent(Message{
		Meta:    Meta{MsgType: SYSTEMKind, Msg: stringPtr(MsgTimerFired)},
		Payload: payload,
	})
	if err != nil {
		t.Fatalf("parse fired event: %v", err)
	}
	if event.TimerUUID != "timer-1" || event.OwnerL2Name != "WF.demo@motherbee" {
		t.Fatalf("unexpected fired event: %+v", event)
	}
}
