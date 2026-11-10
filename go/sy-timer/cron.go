package main

import (
	"fmt"
	"strings"

	"github.com/robfig/cron/v3"
)

// cronParser reads the 5 fields of a cron_spec: minute, hour, day of month, month and day of week.
var cronParser = cron.NewParser(cron.Minute | cron.Hour | cron.Dom | cron.Month | cron.Dow)

// parseCronSpec reads a cron_spec, at its request and at each re-schedule: its schedule runs in the
// zone of the time it is asked from, cron_tz's. What robfig/cron reads before the fields is refused
// before the parser sees it (FINDINGS A-206): a TZ= or CRON_TZ= zone, which would take over cron_tz,
// and panics the parser when no space follows it; and a descriptor (@daily and the rest), not in v1.
// Its errors start with "invalid cron", which timerErrorCode answers with TIMER_INVALID_CRON.
func parseCronSpec(spec string) (cron.Schedule, error) {
	for _, prefix := range []string{"TZ=", "CRON_TZ="} {
		if strings.HasPrefix(spec, prefix) {
			return nil, fmt.Errorf("invalid cron: cron_spec starts with %s: the time zone goes in cron_tz, cron_spec is the 5 fields only", prefix)
		}
	}
	if strings.HasPrefix(spec, "@") {
		return nil, fmt.Errorf("invalid cron: cron_spec starts with @: descriptors (@daily and the rest) are not supported in v1, cron_spec is the 5 fields only")
	}
	schedule, err := cronParser.Parse(spec)
	if err != nil {
		return nil, fmt.Errorf("invalid cron: %w", err)
	}
	return schedule, nil
}
