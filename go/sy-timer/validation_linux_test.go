//go:build linux

package main

import (
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
