//go:build linux

package main

import (
	"encoding/json"
	"fmt"
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

// A TIMER_LIST longer than a frame answers in pages (FINDINGS A-151): each page fits, a page with
// more after it says where they start (next_cursor), and the pages list every timer once, by fire
// time and then uuid. A cursor that is not one is refused.
func TestTimerListAnswersInPagesThatFitAFrame(t *testing.T) {
	service, tx := newTestService(t)
	const owner = "WF.demo@motherbee"
	const total = 300
	payload := `{"note":"` + strings.Repeat("x", 900) + `"}`
	base := time.Now().UTC().Add(time.Hour).UnixMilli()
	for i := 0; i < total; i++ {
		insertOwnedTimer(t, service.db, timerRow{
			UUID:         fmt.Sprintf("timer-%03d", i),
			OwnerL2Name:  owner,
			TargetL2Name: owner,
			Kind:         "oneshot",
			FireAtUTC:    base + int64(i/2), // two by two at one time: the uuid orders them
			MissedPolicy: "fire",
			Payload:      payload,
			Status:       "pending",
			CreatedAtUTC: base,
		})
	}

	type page struct {
		OK     bool `json:"ok"`
		Count  int  `json:"count"`
		Timers []struct {
			UUID string `json:"uuid"`
		} `json:"timers"`
		NextCursor string `json:"next_cursor"`
		Error      *struct {
			Code string `json:"code"`
		} `json:"error"`
	}
	list := func(cursor string) (page, int) {
		t.Helper()
		request := map[string]any{"limit": 1000}
		if cursor != "" {
			request["cursor"] = cursor
		}
		if err := service.respondTimerList(mustBuildSystemMessageAs(t, request, "TIMER_LIST", owner)); err != nil {
			t.Fatalf("list timers: %v", err)
		}
		raw := mustReadResponsePayload(t, tx)
		var answer page
		if err := json.Unmarshal(raw, &answer); err != nil {
			t.Fatalf("decode a page: %v", err)
		}
		return answer, len(raw)
	}

	var listed []string
	cursor := ""
	for pages := 1; ; pages++ {
		answer, size := list(cursor)
		if !answer.OK || answer.Count != len(answer.Timers) || size > maxTimerListPageBytes+1024 {
			t.Fatalf("page %d: ok=%v count=%d of %d timers, %d bytes", pages, answer.OK, answer.Count, len(answer.Timers), size)
		}
		for _, timer := range answer.Timers {
			listed = append(listed, timer.UUID)
		}
		if answer.NextCursor == "" {
			if pages < 2 {
				t.Fatalf("%d timers of about 1 KiB fit one page", total)
			}
			break
		}
		cursor = answer.NextCursor
	}
	if len(listed) != total {
		t.Fatalf("the pages listed %d timers, want %d", len(listed), total)
	}
	for i, uuid := range listed {
		if want := fmt.Sprintf("timer-%03d", i); uuid != want {
			t.Fatalf("timer %d is %s, want %s: every timer once, in order", i, uuid, want)
		}
	}

	if answer, _ := list("nonsense"); answer.OK || answer.Error == nil || answer.Error.Code != "TIMER_INVALID_PAYLOAD" {
		t.Fatalf("a cursor that is not one: %+v", answer)
	}
}

// A cursor TIMER_LIST cannot read is refused naming the field, never the value it was sent
// (FINDINGS A-232).
func TestATimerListCursorRefusalNamesNoValue(t *testing.T) {
	for _, cursor := range []string{"3f1c0e9a8b7d6c5e", "1730000000000", "notanumber:abc", "731:"} {
		_, err := parseTimerListCursor(cursor)
		if err == nil {
			t.Fatalf("cursor %q: no refusal", cursor)
		}
		if err.Error() != "cursor must be a next_cursor of TIMER_LIST" {
			t.Fatalf("cursor %q: refusal %q", cursor, err.Error())
		}
	}
	if got, err := parseTimerListCursor("1730000000000:4b1e"); err != nil || got == nil {
		t.Fatalf("a next_cursor: %v %v", got, err)
	}
}
