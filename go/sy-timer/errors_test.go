package main

import (
	"database/sql"
	"errors"
	"fmt"
	"testing"
)

// quotesEveryCode is a text that names the words of every code: what a requester's spec or zone can
// put in an error's text.
const quotesEveryCode = "invalid timezone, invalid cron, minimum timer duration, maximum timer delay, timer does not belong to requester, routing.src_l2_name missing"

// An error's code is the one it carries, through any wrapping; its text never decides it (FINDINGS
// A-220).
func TestTimerErrorCodeIsCarriedByTypeNotByText(t *testing.T) {
	for _, tc := range []struct {
		name string
		err  error
		code string
	}{
		{name: "no error", code: "TIMER_INTERNAL"},
		{name: "below the minimum", err: timerErrorf("TIMER_BELOW_MINIMUM", "minimum timer duration is 60 seconds"), code: "TIMER_BELOW_MINIMUM"},
		{name: "above the maximum", err: timerErrorf("TIMER_ABOVE_MAXIMUM", "maximum timer delay is 366 days"), code: "TIMER_ABOVE_MAXIMUM"},
		{name: "a zone whose text quotes the others", err: timerErrorf("TIMER_INVALID_TIMEZONE", "invalid timezone: %w", errors.New("unknown time zone "+quotesEveryCode)), code: "TIMER_INVALID_TIMEZONE"},
		{name: "a cron whose text quotes the others", err: timerErrorf("TIMER_INVALID_CRON", "invalid cron: %s", quotesEveryCode), code: "TIMER_INVALID_CRON"},
		{name: "a carried code, wrapped", err: fmt.Errorf("schedule: %w", timerErrorf("TIMER_INVALID_CRON", "invalid cron: no future fire time")), code: "TIMER_INVALID_CRON"},
		{name: "a payload's", err: timerErrorf("TIMER_INVALID_PAYLOAD", "cron_spec must be non-empty"), code: "TIMER_INVALID_PAYLOAD"},
		{name: "none carried, whatever the text says", err: errors.New(quotesEveryCode), code: "TIMER_INVALID_PAYLOAD"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := timerErrorCode(tc.err); got != tc.code {
				t.Fatalf("%v: code %s, want %s", tc.err, got, tc.code)
			}
		})
	}
}

// A cancel's or a reschedule's code when its timer cannot be resolved: none found, the code its
// error carries, or the database's, whatever the text says (FINDINGS A-220).
func TestTimerStorageOrInternalCodeIsCarriedByType(t *testing.T) {
	for _, tc := range []struct {
		name string
		err  error
		code string
	}{
		{name: "no timer", err: sql.ErrNoRows, code: "TIMER_NOT_FOUND"},
		{name: "no timer, wrapped", err: fmt.Errorf("get timer: %w", sql.ErrNoRows), code: "TIMER_NOT_FOUND"},
		{name: "another owner's", err: timerErrorf("TIMER_NOT_OWNER", "timer does not belong to requester"), code: "TIMER_NOT_OWNER"},
		{name: "no routing.src_l2_name", err: timerErrorf("TIMER_INTERNAL", "routing.src_l2_name missing from incoming message (src=x)"), code: "TIMER_INTERNAL"},
		{name: "the database's, whatever the text says", err: errors.New(quotesEveryCode), code: "TIMER_STORAGE_ERROR"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := timerStorageOrInternalCode(tc.err); got != tc.code {
				t.Fatalf("%v: code %s, want %s", tc.err, got, tc.code)
			}
		})
	}
}
