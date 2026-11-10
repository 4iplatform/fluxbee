//go:build linux

package main

import (
	"context"
	"encoding/json"
	"testing"
	"time"
)

func TestScheduleFireAtRejectsMixedAbsoluteAndRelative(t *testing.T) {
	fireAt := time.Now().UTC().Add(2 * time.Hour).UnixMilli()
	fireIn := int64((2 * time.Hour).Milliseconds())
	_, err := scheduleFireAt(timerScheduleRequest{
		FireAtUTCMS: &fireAt,
		FireInMS:    &fireIn,
	})
	if err == nil {
		t.Fatalf("expected mixed absolute/relative request to fail")
	}
}

func TestScheduleFireAtRejectsBelowMinimumRelative(t *testing.T) {
	fireIn := int64((30 * time.Second).Milliseconds())
	_, err := scheduleFireAt(timerScheduleRequest{FireInMS: &fireIn})
	if err == nil || timerErrorCode(err) != "TIMER_BELOW_MINIMUM" {
		t.Fatalf("expected below-minimum error, got %v", err)
	}
}

func TestScheduleRecurringFireAtRejectsInvalidCron(t *testing.T) {
	_, _, err := scheduleRecurringFireAt(timerScheduleRecurringRequest{CronSpec: "invalid cron"})
	if err == nil || timerErrorCode(err) != "TIMER_INVALID_CRON" {
		t.Fatalf("expected invalid cron error, got %v", err)
	}
}

// TIMER_SCHEDULE_RECURRING answers a cron_spec that starts with a zone or a descriptor with
// TIMER_INVALID_CRON, and keeps nothing (FINDINGS A-206). Both SDKs count 5 fields split by any
// whitespace: a zone and four fields split by tabs passed them, and panicked SY.timer.
func TestScheduleRecurringRefusesAZoneOrADescriptorInCronSpec(t *testing.T) {
	for _, tc := range []struct {
		name string
		spec string
	}{
		{name: "a zone and four fields split by tabs", spec: "TZ=UTC\t0\t9\t*\t*"},
		{name: "a zone alone", spec: "TZ=UTC"},
		{name: "a CRON_TZ zone and five fields", spec: "CRON_TZ=America/New_York 0 9 * * *"},
		{name: "a descriptor", spec: "@daily"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			service, tx := newTestService(t)
			service.scheduler = newTimerScheduler(service)
			msg := mustBuildSystemMessage(t, map[string]any{"cron_spec": tc.spec}, "TIMER_SCHEDULE_RECURRING")
			if err := service.handleMessage(msg); err != nil {
				t.Fatalf("schedule recurring: %v", err)
			}
			response := mustReadTimerResponse(t, tx)
			if response.OK || response.Error == nil || response.Error.Code != "TIMER_INVALID_CRON" {
				t.Fatalf("want TIMER_INVALID_CRON, got %+v (error %+v)", response, response.Error)
			}
			rows, err := listPendingTimers(context.Background(), service.db)
			if err != nil || len(rows) != 0 {
				t.Fatalf("kept %d timers (%v)", len(rows), err)
			}
		})
	}
}

// A refused schedule or reschedule answers the code its error carries, not one its text names
// (FINDINGS A-220): the text quotes the spec or the zone the requester sent. Each case
// timerErrorCode covers, through the function that refuses it.
func TestScheduleErrorCodesAreCarriedByType(t *testing.T) {
	day := int64(24 * time.Hour / time.Millisecond)
	ms := func(value int64) *int64 { return &value }
	oneShot := func(at, in *int64) error {
		_, err := scheduleFireAt(timerScheduleRequest{FireAtUTCMS: at, FireInMS: in})
		return err
	}
	reschedule := func(at, in *int64) error {
		_, err := rescheduleFireAt(timerRescheduleRequest{NewFireAtUTCMS: at, NewFireInMS: in})
		return err
	}
	recurring := func(spec, tz string) error {
		_, _, err := scheduleRecurringFireAt(timerScheduleRecurringRequest{CronSpec: spec, CronTZ: tz})
		return err
	}
	for _, tc := range []struct {
		name string
		err  error
		code string
	}{
		{name: "neither fire_at nor fire_in", err: oneShot(nil, nil), code: "TIMER_INVALID_PAYLOAD"},
		{name: "both new_fire_at and new_fire_in", err: reschedule(ms(1), ms(1)), code: "TIMER_INVALID_PAYLOAD"},
		{name: "30 s ahead", err: oneShot(nil, ms(30_000)), code: "TIMER_BELOW_MINIMUM"},
		{name: "rescheduled 30 s ahead", err: reschedule(nil, ms(30_000)), code: "TIMER_BELOW_MINIMUM"},
		{name: "367 days ahead", err: oneShot(nil, ms(367*day)), code: "TIMER_ABOVE_MAXIMUM"},
		{name: "no cron_spec", err: recurring("  ", ""), code: "TIMER_INVALID_PAYLOAD"},
		{name: "a zone that is none", err: recurring("0 9 * * *", "Mars/Olympus"), code: "TIMER_INVALID_TIMEZONE"},
		{name: "a zone that quotes the minimum", err: recurring("0 9 * * *", "minimum timer duration"), code: "TIMER_INVALID_TIMEZONE"},
		{name: "a zone that quotes an invalid cron", err: recurring("0 9 * * *", "invalid cron"), code: "TIMER_INVALID_TIMEZONE"},
		{name: "a spec that is no cron", err: recurring("every monday", ""), code: "TIMER_INVALID_CRON"},
		{name: "a spec that quotes an invalid timezone", err: recurring("invalid timezone 0 9 * * *", ""), code: "TIMER_INVALID_CRON"},
		{name: "a spec that quotes the minimum", err: recurring("minimum timer duration 0 9 * *", ""), code: "TIMER_INVALID_CRON"},
		{name: "a spec that quotes the maximum", err: recurring("maximum timer delay 0 9 * *", ""), code: "TIMER_INVALID_CRON"},
		{name: "a spec with no future fire", err: recurring("0 0 30 2 *", ""), code: "TIMER_INVALID_CRON"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := timerErrorCode(tc.err); got != tc.code {
				t.Fatalf("%v: code %s, want %s", tc.err, got, tc.code)
			}
		})
	}
}

// TIMER_SCHEDULE_RECURRING answers a junk cron_spec with TIMER_INVALID_CRON whatever its fields say
// (FINDINGS A-220): one that repeated "invalid timezone" came back TIMER_INVALID_TIMEZONE.
func TestScheduleRecurringAnswersAJunkSpecWithInvalidCron(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	msg := mustBuildSystemMessage(t, map[string]any{"cron_spec": "invalid timezone 0 9 * * *"}, "TIMER_SCHEDULE_RECURRING")
	if err := service.handleMessage(msg); err != nil {
		t.Fatalf("schedule recurring: %v", err)
	}
	if response := mustReadTimerResponse(t, tx); response.OK || response.Error == nil || response.Error.Code != "TIMER_INVALID_CRON" {
		t.Fatalf("want TIMER_INVALID_CRON, got %+v (error %+v)", response, response.Error)
	}
}

// A cancel or a reschedule of another owner's timer is TIMER_NOT_OWNER, a code its error carries
// (FINDINGS A-220), not one read from its text.
func TestCancelOrRescheduleOfAnotherOwnersTimerIsNotOwner(t *testing.T) {
	for _, tc := range []struct {
		verb    string
		request map[string]any
	}{
		{verb: "TIMER_CANCEL", request: map[string]any{"timer_uuid": "timer-1"}},
		{verb: "TIMER_RESCHEDULE", request: map[string]any{"timer_uuid": "timer-1", "new_fire_in_ms": time.Hour.Milliseconds()}},
	} {
		t.Run(tc.verb, func(t *testing.T) {
			service, tx := newTestService(t)
			insertOwnedTimer(t, service.db, timerRow{
				UUID:         "timer-1",
				OwnerL2Name:  "AI.other@motherbee",
				TargetL2Name: "AI.other@motherbee",
				Kind:         "oneshot",
				FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
				MissedPolicy: "fire",
				Payload:      `{}`,
				Status:       "pending",
				CreatedAtUTC: time.Now().UTC().UnixMilli(),
			})
			if err := service.handleMessage(mustBuildSystemMessageAs(t, tc.request, tc.verb, "WF.demo@motherbee")); err != nil {
				t.Fatalf("%s: %v", tc.verb, err)
			}
			if response := mustReadTimerResponse(t, tx); response.OK || response.Error == nil || response.Error.Code != "TIMER_NOT_OWNER" {
				t.Fatalf("want TIMER_NOT_OWNER, got %+v (error %+v)", response, response.Error)
			}
		})
	}
}

func TestValidateMissedPolicyRawRejectsMissingWindow(t *testing.T) {
	err := validateMissedPolicyRaw("fire_if_within", nil)
	if err == nil {
		t.Fatalf("expected fire_if_within without window to fail")
	}
}

func TestRespondTimerNowInReturnsTimezoneProjection(t *testing.T) {
	service, tx := newTestService(t)
	msg := mustBuildSystemMessage(t, map[string]any{"tz": "America/Argentina/Buenos_Aires"}, "TIMER_NOW_IN")
	if err := service.respondTimerNowIn(msg); err != nil {
		t.Fatalf("now in: %v", err)
	}
	var payload struct {
		OK          bool   `json:"ok"`
		Verb        string `json:"verb"`
		TZ          string `json:"tz"`
		NowLocalISO string `json:"now_local_iso"`
	}
	if err := json.Unmarshal(mustReadResponsePayload(t, tx), &payload); err != nil {
		t.Fatalf("decode payload: %v", err)
	}
	if !payload.OK || payload.Verb != "TIMER_NOW_IN" || payload.TZ != "America/Argentina/Buenos_Aires" || payload.NowLocalISO == "" {
		t.Fatalf("unexpected payload: %+v", payload)
	}
}

func TestRespondTimerConvertReturnsConvertedInstant(t *testing.T) {
	service, tx := newTestService(t)
	msg := mustBuildSystemMessage(t, map[string]any{
		"instant_utc_ms": time.Date(2026, 4, 8, 15, 20, 0, 0, time.UTC).UnixMilli(),
		"to_tz":          "Europe/Madrid",
	}, "TIMER_CONVERT")
	if err := service.respondTimerConvert(msg); err != nil {
		t.Fatalf("convert: %v", err)
	}
	var payload struct {
		OK       bool   `json:"ok"`
		Verb     string `json:"verb"`
		ToTZ     string `json:"to_tz"`
		LocalISO string `json:"local_iso"`
	}
	if err := json.Unmarshal(mustReadResponsePayload(t, tx), &payload); err != nil {
		t.Fatalf("decode payload: %v", err)
	}
	if !payload.OK || payload.Verb != "TIMER_CONVERT" || payload.ToTZ != "Europe/Madrid" || payload.LocalISO == "" {
		t.Fatalf("unexpected payload: %+v", payload)
	}
}

func TestRespondTimerParseReturnsResolvedUTC(t *testing.T) {
	service, tx := newTestService(t)
	msg := mustBuildSystemMessage(t, map[string]any{
		"input":  "2026-04-08 17:20:00",
		"layout": "2006-01-02 15:04:05",
		"tz":     "Europe/Madrid",
	}, "TIMER_PARSE")
	if err := service.respondTimerParse(msg); err != nil {
		t.Fatalf("parse: %v", err)
	}
	var payload struct {
		OK             bool   `json:"ok"`
		Verb           string `json:"verb"`
		ResolvedISOUTC string `json:"resolved_iso_utc"`
	}
	if err := json.Unmarshal(mustReadResponsePayload(t, tx), &payload); err != nil {
		t.Fatalf("decode payload: %v", err)
	}
	if !payload.OK || payload.Verb != "TIMER_PARSE" || payload.ResolvedISOUTC == "" {
		t.Fatalf("unexpected payload: %+v", payload)
	}
}

func TestRespondTimerFormatReturnsFormattedString(t *testing.T) {
	service, tx := newTestService(t)
	msg := mustBuildSystemMessage(t, map[string]any{
		"instant_utc_ms": time.Date(2026, 4, 8, 15, 20, 0, 0, time.UTC).UnixMilli(),
		"layout":         "Mon 02 Jan 2006 15:04 MST",
		"tz":             "America/Argentina/Buenos_Aires",
	}, "TIMER_FORMAT")
	if err := service.respondTimerFormat(msg); err != nil {
		t.Fatalf("format: %v", err)
	}
	var payload struct {
		OK        bool   `json:"ok"`
		Verb      string `json:"verb"`
		Formatted string `json:"formatted"`
	}
	if err := json.Unmarshal(mustReadResponsePayload(t, tx), &payload); err != nil {
		t.Fatalf("decode payload: %v", err)
	}
	if !payload.OK || payload.Verb != "TIMER_FORMAT" || payload.Formatted == "" {
		t.Fatalf("unexpected payload: %+v", payload)
	}
}
