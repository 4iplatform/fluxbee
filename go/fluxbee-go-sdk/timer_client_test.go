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

func TestNewTimerClientNormalizesInvalidRetrySchedule(t *testing.T) {
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: make(chan []byte, 1), state: &connectionState{connected: true}},
		&NodeReceiver{rx: make(chan receivedMessage, 1), state: &connectionState{connected: true}},
		TimerClientConfig{TimeRetrySchedule: []time.Duration{0, -time.Second}},
	)
	if err != nil {
		t.Fatalf("new timer client: %v", err)
	}
	if len(client.timeRetrySchedule) != len(defaultTimeRetrySchedule) {
		t.Fatalf("unexpected retry schedule length: %v", client.timeRetrySchedule)
	}
	for i, want := range defaultTimeRetrySchedule {
		if client.timeRetrySchedule[i] != want {
			t.Fatalf("unexpected retry schedule[%d]: got=%s want=%s", i, client.timeRetrySchedule[i], want)
		}
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

func TestTimerClientConvertUsesTimerResponse(t *testing.T) {
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
		if stringValue(request.Meta.Msg) != "TIMER_CONVERT" {
			t.Errorf("unexpected request verb: %q", stringValue(request.Meta.Msg))
			return
		}
		response, err := BuildSystemResponse(
			&request,
			"timer-uuid",
			MsgTimerResponse,
			map[string]any{
				"ok":                true,
				"verb":              "TIMER_CONVERT",
				"instant_utc_ms":    int64(1775577600000),
				"to_tz":             "Europe/Madrid",
				"local_iso":         "2026-04-08T17:20:00+02:00",
				"tz_offset_seconds": 7200,
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

	result, err := client.Convert(ctx, 1775577600000, "Europe/Madrid")
	if err != nil {
		t.Fatalf("timer convert: %v", err)
	}
	if result.ToTZ != "Europe/Madrid" || result.TZOffsetSeconds != 7200 {
		t.Fatalf("unexpected convert response: %+v", result)
	}
}

func TestTimerClientParseUsesTimerResponse(t *testing.T) {
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
		if stringValue(request.Meta.Msg) != "TIMER_PARSE" {
			t.Errorf("unexpected request verb: %q", stringValue(request.Meta.Msg))
			return
		}
		response, err := BuildSystemResponse(
			&request,
			"timer-uuid",
			MsgTimerResponse,
			map[string]any{
				"ok":               true,
				"verb":             "TIMER_PARSE",
				"instant_utc_ms":   int64(1775575200000),
				"resolved_iso_utc": "2026-04-08T15:20:00Z",
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

	result, err := client.Parse(ctx, "2026-04-08 17:20:00", "2006-01-02 15:04:05", "Europe/Madrid")
	if err != nil {
		t.Fatalf("timer parse: %v", err)
	}
	if result.InstantUTCMS != 1775575200000 || result.ResolvedISOUTC != "2026-04-08T15:20:00Z" {
		t.Fatalf("unexpected parse response: %+v", result)
	}
}

func TestTimerClientFormatUsesTimerResponse(t *testing.T) {
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
		if stringValue(request.Meta.Msg) != "TIMER_FORMAT" {
			t.Errorf("unexpected request verb: %q", stringValue(request.Meta.Msg))
			return
		}
		response, err := BuildSystemResponse(
			&request,
			"timer-uuid",
			MsgTimerResponse,
			map[string]any{
				"ok":        true,
				"verb":      "TIMER_FORMAT",
				"formatted": "Wed 08 Apr 2026 12:20 -03",
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

	result, err := client.Format(ctx, 1775577600000, "Mon 02 Jan 2006 15:04 MST", "America/Argentina/Buenos_Aires")
	if err != nil {
		t.Fatalf("timer format: %v", err)
	}
	if result.Formatted != "Wed 08 Apr 2026 12:20 -03" {
		t.Fatalf("unexpected format response: %+v", result)
	}
}

func TestTimerClientHelpUsesTimerResponse(t *testing.T) {
	tx := make(chan []byte, 1)
	rx := make(chan receivedMessage, 1)
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: tx, state: &connectionState{connected: true}},
		&NodeReceiver{rx: rx, state: &connectionState{connected: true}},
		TimerClientConfig{},
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
		if stringValue(request.Meta.Msg) != MsgTimerHelp {
			t.Errorf("unexpected request verb: %q", stringValue(request.Meta.Msg))
			return
		}
		response, err := BuildSystemResponse(
			&request,
			"timer-uuid",
			MsgTimerResponse,
			map[string]any{
				"ok":          true,
				"verb":        MsgTimerHelp,
				"node_family": "SY",
				"node_kind":   "SY.timer",
				"version":     "1.0",
				"description": "timer help",
			},
			SystemEnvelopeOptions{},
		)
		if err != nil {
			t.Errorf("build response: %v", err)
			return
		}
		rx <- receivedMessage{msg: response}
	}()

	help, err := client.Help(context.Background())
	if err != nil {
		t.Fatalf("help: %v", err)
	}
	if help.NodeKind != "SY.timer" || help.Verb != MsgTimerHelp {
		t.Fatalf("unexpected help payload: %+v", help)
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

func TestTimerClientNowInRejectsEmptyTimezone(t *testing.T) {
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: make(chan []byte, 1), state: &connectionState{connected: true}},
		&NodeReceiver{rx: make(chan receivedMessage, 1), state: &connectionState{connected: true}},
		TimerClientConfig{},
	)
	if err != nil {
		t.Fatalf("new timer client: %v", err)
	}
	if _, err := client.NowIn(context.Background(), "   "); err == nil {
		t.Fatalf("expected timezone validation error")
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

func TestScheduleInRejectsBelowMinimumClientSide(t *testing.T) {
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: make(chan []byte, 1), state: &connectionState{connected: true}},
		&NodeReceiver{rx: make(chan receivedMessage, 1), state: &connectionState{connected: true}},
		TimerClientConfig{},
	)
	if err != nil {
		t.Fatalf("new timer client: %v", err)
	}
	_, err = client.ScheduleIn(context.Background(), 30*time.Second, ScheduleOptions{})
	if err == nil {
		t.Fatalf("expected below-minimum error")
	}
	respErr, ok := err.(*SystemResponseError)
	if !ok || respErr.Code != "TIMER_BELOW_MINIMUM" {
		t.Fatalf("unexpected error: %T %+v", err, err)
	}
}

func TestScheduleRejectsMissedWithinWithoutFireIfWithin(t *testing.T) {
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: make(chan []byte, 1), state: &connectionState{connected: true}},
		&NodeReceiver{rx: make(chan receivedMessage, 1), state: &connectionState{connected: true}},
		TimerClientConfig{},
	)
	if err != nil {
		t.Fatalf("new timer client: %v", err)
	}
	within := int64(5000)
	_, err = client.ScheduleIn(context.Background(), time.Minute, ScheduleOptions{
		MissedPolicy:   MissedPolicyFire,
		MissedWithinMS: &within,
	})
	if err == nil {
		t.Fatalf("expected missed_within validation error")
	}
}

func TestScheduleRecurringRejectsInvalidCronShape(t *testing.T) {
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: make(chan []byte, 1), state: &connectionState{connected: true}},
		&NodeReceiver{rx: make(chan receivedMessage, 1), state: &connectionState{connected: true}},
		TimerClientConfig{},
	)
	if err != nil {
		t.Fatalf("new timer client: %v", err)
	}
	_, err = client.ScheduleRecurring(context.Background(), RecurringOptions{CronSpec: "* * * * * *"})
	if err == nil {
		t.Fatalf("expected cron validation error")
	}
}

func TestScheduleUsesTimerResponseTimerUUID(t *testing.T) {
	tx := make(chan []byte, 1)
	rx := make(chan receivedMessage, 1)
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: tx, state: &connectionState{connected: true}},
		&NodeReceiver{rx: rx, state: &connectionState{connected: true}},
		TimerClientConfig{},
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
		if stringValue(request.Meta.Msg) != "TIMER_SCHEDULE" {
			t.Errorf("unexpected request verb: %q", stringValue(request.Meta.Msg))
			return
		}
		response, err := BuildSystemResponse(
			&request,
			"timer-uuid",
			MsgTimerResponse,
			map[string]any{
				"ok":             true,
				"verb":           "TIMER_SCHEDULE",
				"timer_uuid":     "timer-1",
				"fire_at_utc_ms": int64(1775577600000),
			},
			SystemEnvelopeOptions{},
		)
		if err != nil {
			t.Errorf("build response: %v", err)
			return
		}
		rx <- receivedMessage{msg: response}
	}()

	id, err := client.ScheduleIn(context.Background(), time.Minute, ScheduleOptions{ClientRef: "demo"})
	if err != nil {
		t.Fatalf("schedule in: %v", err)
	}
	if id != TimerID("timer-1") {
		t.Fatalf("unexpected timer id: %q", id)
	}
}

func TestScheduleRecurringUsesTimerResponseTimerUUID(t *testing.T) {
	tx := make(chan []byte, 1)
	rx := make(chan receivedMessage, 1)
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: tx, state: &connectionState{connected: true}},
		&NodeReceiver{rx: rx, state: &connectionState{connected: true}},
		TimerClientConfig{},
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
		if stringValue(request.Meta.Msg) != "TIMER_SCHEDULE_RECURRING" {
			t.Errorf("unexpected request verb: %q", stringValue(request.Meta.Msg))
			return
		}
		response, err := BuildSystemResponse(
			&request,
			"timer-uuid",
			MsgTimerResponse,
			map[string]any{
				"ok":             true,
				"verb":           "TIMER_SCHEDULE_RECURRING",
				"timer_uuid":     "timer-r1",
				"fire_at_utc_ms": int64(1775577600000),
			},
			SystemEnvelopeOptions{},
		)
		if err != nil {
			t.Errorf("build response: %v", err)
			return
		}
		rx <- receivedMessage{msg: response}
	}()

	id, err := client.ScheduleRecurring(context.Background(), RecurringOptions{CronSpec: "0 9 * * MON"})
	if err != nil {
		t.Fatalf("schedule recurring: %v", err)
	}
	if id != TimerID("timer-r1") {
		t.Fatalf("unexpected timer id: %q", id)
	}
}

func TestCancelReturnsNilOnSuccessfulTimerResponse(t *testing.T) {
	tx := make(chan []byte, 1)
	rx := make(chan receivedMessage, 1)
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: tx, state: &connectionState{connected: true}},
		&NodeReceiver{rx: rx, state: &connectionState{connected: true}},
		TimerClientConfig{},
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
		if stringValue(request.Meta.Msg) != "TIMER_CANCEL" {
			t.Errorf("unexpected request verb: %q", stringValue(request.Meta.Msg))
			return
		}
		response, err := BuildSystemResponse(
			&request,
			"timer-uuid",
			MsgTimerResponse,
			map[string]any{
				"ok":         true,
				"verb":       "TIMER_CANCEL",
				"timer_uuid": "timer-1",
				"status":     "canceled",
			},
			SystemEnvelopeOptions{},
		)
		if err != nil {
			t.Errorf("build response: %v", err)
			return
		}
		rx <- receivedMessage{msg: response}
	}()

	if err := client.Cancel(context.Background(), TimerID("timer-1")); err != nil {
		t.Fatalf("cancel: %v", err)
	}
}

func TestCancelRejectsEmptyTimerID(t *testing.T) {
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: make(chan []byte, 1), state: &connectionState{connected: true}},
		&NodeReceiver{rx: make(chan receivedMessage, 1), state: &connectionState{connected: true}},
		TimerClientConfig{},
	)
	if err != nil {
		t.Fatalf("new timer client: %v", err)
	}
	if err := client.Cancel(context.Background(), TimerID("   ")); err == nil {
		t.Fatalf("expected empty timer id validation error")
	}
}

func TestRescheduleReturnsNilOnSuccessfulTimerResponse(t *testing.T) {
	tx := make(chan []byte, 1)
	rx := make(chan receivedMessage, 1)
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: tx, state: &connectionState{connected: true}},
		&NodeReceiver{rx: rx, state: &connectionState{connected: true}},
		TimerClientConfig{},
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
		if stringValue(request.Meta.Msg) != "TIMER_RESCHEDULE" {
			t.Errorf("unexpected request verb: %q", stringValue(request.Meta.Msg))
			return
		}
		response, err := BuildSystemResponse(
			&request,
			"timer-uuid",
			MsgTimerResponse,
			map[string]any{
				"ok":             true,
				"verb":           "TIMER_RESCHEDULE",
				"timer_uuid":     "timer-1",
				"fire_at_utc_ms": int64(1775577600000),
			},
			SystemEnvelopeOptions{},
		)
		if err != nil {
			t.Errorf("build response: %v", err)
			return
		}
		rx <- receivedMessage{msg: response}
	}()

	if err := client.Reschedule(context.Background(), TimerID("timer-1"), time.Now().UTC().Add(2*time.Hour)); err != nil {
		t.Fatalf("reschedule: %v", err)
	}
}

func TestListRejectsInvalidFilters(t *testing.T) {
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: make(chan []byte, 1), state: &connectionState{connected: true}},
		&NodeReceiver{rx: make(chan receivedMessage, 1), state: &connectionState{connected: true}},
		TimerClientConfig{},
	)
	if err != nil {
		t.Fatalf("new timer client: %v", err)
	}
	if _, err := client.List(context.Background(), ListFilter{StatusFilter: "weird"}); err == nil {
		t.Fatalf("expected invalid status filter error")
	}
	if _, err := client.List(context.Background(), ListFilter{Limit: MaxTimerListLimit + 1}); err == nil {
		t.Fatalf("expected limit validation error")
	}
}

func TestTimeFormattingOpsRejectEmptyFields(t *testing.T) {
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: make(chan []byte, 1), state: &connectionState{connected: true}},
		&NodeReceiver{rx: make(chan receivedMessage, 1), state: &connectionState{connected: true}},
		TimerClientConfig{},
	)
	if err != nil {
		t.Fatalf("new timer client: %v", err)
	}
	if _, err := client.Convert(context.Background(), 1775577600000, " "); err == nil {
		t.Fatalf("expected convert validation error")
	}
	if _, err := client.Parse(context.Background(), "", "2006-01-02", "UTC"); err == nil {
		t.Fatalf("expected parse validation error")
	}
	if _, err := client.Format(context.Background(), 1775577600000, "", "UTC"); err == nil {
		t.Fatalf("expected format validation error")
	}
}

func TestGetParsesTimerInfo(t *testing.T) {
	tx := make(chan []byte, 1)
	rx := make(chan receivedMessage, 1)
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: tx, state: &connectionState{connected: true}},
		&NodeReceiver{rx: rx, state: &connectionState{connected: true}},
		TimerClientConfig{},
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
		response, err := BuildSystemResponse(
			&request,
			"timer-uuid",
			MsgTimerResponse,
			map[string]any{
				"ok":   true,
				"verb": "TIMER_GET",
				"timer": map[string]any{
					"uuid":              "timer-1",
					"owner_l2_name":     "WF.demo@motherbee",
					"target_l2_name":    "WF.demo@motherbee",
					"kind":              "oneshot",
					"fire_at_utc_ms":    int64(1775577600000),
					"missed_policy":     "fire",
					"status":            "pending",
					"created_at_utc_ms": int64(1775574000000),
					"fire_count":        int64(0),
					"payload":           map[string]any{"ticket_id": 1234},
					"metadata":          map[string]any{"created_by_reasoning": "..."},
				},
			},
			SystemEnvelopeOptions{},
		)
		if err != nil {
			t.Errorf("build response: %v", err)
			return
		}
		rx <- receivedMessage{msg: response}
	}()

	info, err := client.Get(context.Background(), TimerID("timer-1"))
	if err != nil {
		t.Fatalf("get timer: %v", err)
	}
	if info.UUID != "timer-1" || info.OwnerL2Name != "WF.demo@motherbee" {
		t.Fatalf("unexpected timer info: %+v", info)
	}
}

func TestListMineParsesTimers(t *testing.T) {
	tx := make(chan []byte, 1)
	rx := make(chan receivedMessage, 1)
	client, err := NewTimerClient(
		&NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: tx, state: &connectionState{connected: true}},
		&NodeReceiver{rx: rx, state: &connectionState{connected: true}},
		TimerClientConfig{},
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
		response, err := BuildSystemResponse(
			&request,
			"timer-uuid",
			MsgTimerResponse,
			map[string]any{
				"ok":    true,
				"verb":  "TIMER_LIST",
				"count": 1,
				"timers": []map[string]any{
					{
						"uuid":              "timer-1",
						"owner_l2_name":     "WF.demo@motherbee",
						"target_l2_name":    "WF.demo@motherbee",
						"kind":              "oneshot",
						"fire_at_utc_ms":    int64(1775577600000),
						"missed_policy":     "fire",
						"status":            "pending",
						"created_at_utc_ms": int64(1775574000000),
						"fire_count":        int64(0),
					},
				},
			},
			SystemEnvelopeOptions{},
		)
		if err != nil {
			t.Errorf("build response: %v", err)
			return
		}
		rx <- receivedMessage{msg: response}
	}()

	items, err := client.ListMine(context.Background(), ListFilter{StatusFilter: "pending", Limit: 100})
	if err != nil {
		t.Fatalf("list mine: %v", err)
	}
	if len(items) != 1 || items[0].UUID != "timer-1" {
		t.Fatalf("unexpected timers: %+v", items)
	}
}
