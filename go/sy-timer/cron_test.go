package main

import (
	"strings"
	"testing"
	"time"
)

// What robfig/cron reads before a cron_spec's fields never reaches the parser (FINDINGS A-206): a zone
// with no space after it panicked the parser, and SY.timer with it, and a zone with a space took over
// cron_tz. A descriptor is not in v1. Each is an invalid cron, which says where the zone goes.
func TestCronSpecRefusesAZoneOrADescriptor(t *testing.T) {
	for _, tc := range []struct {
		name string
		spec string
		says string
	}{
		{name: "a zone and four fields split by tabs", spec: "TZ=UTC\t0\t9\t*\t*", says: "the time zone goes in cron_tz"},
		{name: "a zone alone", spec: "TZ=UTC", says: "the time zone goes in cron_tz"},
		{name: "a CRON_TZ zone alone", spec: "CRON_TZ=America/New_York", says: "the time zone goes in cron_tz"},
		{name: "a CRON_TZ zone and five fields", spec: "CRON_TZ=America/New_York 0 9 * * *", says: "the time zone goes in cron_tz"},
		{name: "a descriptor", spec: "@daily", says: "not supported in v1"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var err error
			func() {
				defer func() {
					if r := recover(); r != nil {
						t.Fatalf("%q panicked: %v", tc.spec, r)
					}
				}()
				_, err = parseCronSpec(tc.spec)
			}()
			if err == nil || !strings.HasPrefix(err.Error(), "invalid cron: ") || !strings.Contains(err.Error(), tc.says) {
				t.Fatalf("%q: want an invalid cron that says %q, got %v", tc.spec, tc.says, err)
			}
		})
	}
}

// A plain cron_spec still parses, its 5 fields split by spaces or by tabs, and its schedule runs in
// the zone of the time it is asked from: cron_tz's.
func TestCronSpecReadsItsFieldsInTheZoneAskedFrom(t *testing.T) {
	newYork, err := time.LoadLocation("America/New_York")
	if err != nil {
		t.Fatalf("load zone: %v", err)
	}
	now := time.Date(2026, 11, 9, 12, 0, 0, 0, time.UTC)
	want := time.Date(2026, 11, 9, 9, 0, 0, 0, newYork)
	for _, spec := range []string{"0 9 * * *", "0\t9\t*\t*\t*"} {
		schedule, err := parseCronSpec(spec)
		if err != nil {
			t.Fatalf("%q: %v", spec, err)
		}
		if next := schedule.Next(now.In(newYork)); !next.Equal(want) {
			t.Fatalf("%q: next fire at %s, want %s", spec, next, want)
		}
	}
}
