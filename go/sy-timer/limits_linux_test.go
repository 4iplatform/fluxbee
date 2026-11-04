//go:build linux

package main

import (
	"strings"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// A node schedules for itself: a target_l2_name that names another node is refused, unless a SY node
// asks (A-115).
func TestTimerTargetIsTheRequesterUnlessASYNodeAsks(t *testing.T) {
	for _, tc := range []struct {
		name      string
		verb      string
		requester string
		target    string
		code      string
	}{
		{name: "no target", verb: "TIMER_SCHEDULE", requester: "WF.demo@motherbee"},
		{name: "itself", verb: "TIMER_SCHEDULE", requester: "WF.demo@motherbee", target: "WF.demo@motherbee"},
		{name: "another node", verb: "TIMER_SCHEDULE", requester: "WF.demo@motherbee", target: "AI.other@motherbee", code: "TIMER_FORBIDDEN"},
		{name: "another node, recurring", verb: "TIMER_SCHEDULE_RECURRING", requester: "WF.demo@motherbee", target: "AI.other@motherbee", code: "TIMER_FORBIDDEN"},
		{name: "a SY node for another node", verb: "TIMER_SCHEDULE", requester: "SY.orchestrator@motherbee", target: "AI.other@motherbee"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			service, tx := newTestService(t)
			service.scheduler = newTimerScheduler(service)
			request := map[string]any{"fire_in_ms": time.Minute.Milliseconds()}
			if tc.verb == "TIMER_SCHEDULE_RECURRING" {
				request = map[string]any{"cron_spec": "0 9 * * MON"}
			}
			if tc.target != "" {
				request["target_l2_name"] = tc.target
			}
			if err := service.handleMessage(mustBuildSystemMessageAs(t, request, tc.verb, tc.requester)); err != nil {
				t.Fatalf("schedule: %v", err)
			}
			response := mustReadTimerResponse(t, tx)
			if tc.code == "" && !response.OK {
				t.Fatalf("refused: %+v", response.Error)
			}
			if tc.code != "" && (response.OK || response.Error == nil || response.Error.Code != tc.code) {
				t.Fatalf("want %s, got %+v", tc.code, response)
			}
		})
	}
}

// A one-shot fires 60 s to 366 days ahead, scheduled or rescheduled (A-115).
func TestOneShotFireAtHasAMaximum(t *testing.T) {
	day := int64(24 * time.Hour / time.Millisecond)
	for _, tc := range []struct {
		name string
		in   int64
		code string
	}{
		{name: "a year", in: 366 * day},
		{name: "a year and a day", in: 367 * day, code: "TIMER_ABOVE_MAXIMUM"},
		{name: "past what an instant holds", in: 1 << 62, code: "TIMER_ABOVE_MAXIMUM"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err := scheduleFireAt(timerScheduleRequest{FireInMS: &tc.in})
			if tc.code == "" && err != nil {
				t.Fatalf("refused: %v", err)
			}
			if tc.code != "" && (err == nil || timerErrorCode(err) != tc.code) {
				t.Fatalf("want %s, got %v", tc.code, err)
			}
			at := time.Now().UTC().UnixMilli() + tc.in
			_, err = rescheduleFireAt(timerRescheduleRequest{NewFireAtUTCMS: &at})
			if (tc.code == "") != (err == nil) || (err != nil && timerErrorCode(err) != tc.code) {
				t.Fatalf("reschedule: want %q, got %v", tc.code, err)
			}
		})
	}
}

// A payload or metadata over 32 KiB, and a timer past its owner's or the hive's pending ones, are
// refused TIMER_LIMIT_EXCEEDED; an existing timer's client_ref is still answered (A-115).
func TestScheduleIsCapped(t *testing.T) {
	big := strings.Repeat("x", maxTimerDataBytes)
	t.Run("payload", func(t *testing.T) {
		service, tx := newTestService(t)
		service.scheduler = newTimerScheduler(service)
		msg := mustBuildSystemMessage(t, map[string]any{"fire_in_ms": time.Minute.Milliseconds(), "payload": map[string]any{"text": big}}, "TIMER_SCHEDULE")
		if err := service.respondTimerSchedule(msg); err != nil {
			t.Fatalf("schedule: %v", err)
		}
		if response := mustReadTimerResponse(t, tx); response.OK || response.Error == nil || response.Error.Code != "TIMER_LIMIT_EXCEEDED" {
			t.Fatalf("want TIMER_LIMIT_EXCEEDED, got %+v", response)
		}
	})
	t.Run("metadata", func(t *testing.T) {
		service, tx := newTestService(t)
		service.scheduler = newTimerScheduler(service)
		msg := mustBuildSystemMessage(t, map[string]any{"cron_spec": "0 9 * * MON", "metadata": map[string]any{"why": big}}, "TIMER_SCHEDULE_RECURRING")
		if err := service.respondTimerScheduleRecurring(msg); err != nil {
			t.Fatalf("schedule: %v", err)
		}
		if response := mustReadTimerResponse(t, tx); response.OK || response.Error == nil || response.Error.Code != "TIMER_LIMIT_EXCEEDED" {
			t.Fatalf("want TIMER_LIMIT_EXCEEDED, got %+v", response)
		}
	})
	for _, tc := range []struct {
		name            string
		perOwner, total int
		other           bool
	}{
		{name: "per owner", perOwner: 1, total: 100},
		{name: "per hive", perOwner: 100, total: 1, other: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			savedOwner, savedTotal := maxPendingTimersPerOwner, maxPendingTimers
			maxPendingTimersPerOwner, maxPendingTimers = tc.perOwner, tc.total
			t.Cleanup(func() { maxPendingTimersPerOwner, maxPendingTimers = savedOwner, savedTotal })
			service, tx := newTestService(t)
			service.scheduler = newTimerScheduler(service)
			schedule := func(requester, clientRef string) *fluxbeesdk.TimerResponse {
				request := map[string]any{"fire_in_ms": time.Minute.Milliseconds(), "client_ref": clientRef}
				if err := service.respondTimerSchedule(mustBuildSystemMessageAs(t, request, "TIMER_SCHEDULE", requester)); err != nil {
					t.Fatalf("schedule: %v", err)
				}
				return mustReadTimerResponse(t, tx)
			}
			if first := schedule("WF.demo@motherbee", "a"); !first.OK {
				t.Fatalf("first refused: %+v", first.Error)
			}
			second := "WF.demo@motherbee"
			if tc.other {
				second = "WF.other@motherbee"
			}
			if refused := schedule(second, "b"); refused.OK || refused.Error == nil || refused.Error.Code != "TIMER_LIMIT_EXCEEDED" {
				t.Fatalf("want TIMER_LIMIT_EXCEEDED, got %+v", refused)
			}
			if again := schedule("WF.demo@motherbee", "a"); !again.OK || again.AlreadyExisted == nil || !*again.AlreadyExisted {
				t.Fatalf("an existing timer's client_ref must still be answered: %+v", again)
			}
		})
	}
}
