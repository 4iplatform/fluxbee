//go:build linux

package main

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"log"
	"os"
	"strings"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

func TestReplayPendingQueuesMissedOneShotWithFirePolicy(t *testing.T) {
	service, tx := newTestService(t)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-due",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    now.Add(-2 * time.Minute).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{"ticket_id":1234}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-10 * time.Minute).UnixMilli(),
	})

	scheduler := newTimerScheduler(service)
	if err := scheduler.replayPending(context.Background(), now); err != nil {
		t.Fatalf("replay pending: %v", err)
	}
	// The replay queues the fire: it goes when the scheduler runs, not before the node serves.
	if len(tx) != 0 {
		t.Fatalf("the replay sent the fire before the scheduler ran")
	}
	scheduler.processDue(context.Background(), now)

	frame := mustReadSentFrame(t, tx)
	var msg fluxbeesdk.Message
	if err := json.Unmarshal(frame, &msg); err != nil {
		t.Fatalf("decode fired event: %v", err)
	}
	if msg.Meta.Msg == nil || *msg.Meta.Msg != fluxbeesdk.MsgTimerFired {
		t.Fatalf("unexpected fired msg: %+v", msg.Meta)
	}

	row, err := getTimer(context.Background(), service.db, "timer-due")
	if err != nil {
		t.Fatalf("load fired timer: %v", err)
	}
	if row.Status != "fired" || row.FireCount != 1 || !row.LastFiredAtUTC.Valid {
		t.Fatalf("unexpected fired row: %+v", row)
	}
}

func TestReplayPendingRecurringRecomputesNextFireAt(t *testing.T) {
	service, _ := newTestService(t)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-recurring",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "recurring",
		FireAtUTC:    now.Add(-2 * time.Hour).UnixMilli(),
		CronSpec:     nullableStringValue("0 9 * * MON"),
		CronTZ:       nullableStringValue("UTC"),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-24 * time.Hour).UnixMilli(),
	})

	scheduler := newTimerScheduler(service)
	if err := scheduler.replayPending(context.Background(), now); err != nil {
		t.Fatalf("replay pending recurring: %v", err)
	}

	row, err := getTimer(context.Background(), service.db, "timer-recurring")
	if err != nil {
		t.Fatalf("load recurring timer: %v", err)
	}
	if row.FireAtUTC <= now.UnixMilli() {
		t.Fatalf("expected recurring timer to be recomputed into the future: %+v", row)
	}
	if row.Status != "pending" {
		t.Fatalf("expected recurring timer to remain pending: %+v", row)
	}
}

// A recurring timer kept before FINDINGS A-206 with a zone before its fields (it parsed when a space
// followed the zone) is refused at its re-schedule, with no panic: the replay logs it and leaves it
// pending, out of the queue, and the start goes on. Due in the queue, it sends nothing, is logged and
// is not queued again: no loop.
func TestAStoredCronSpecWithAZoneIsLeftOutOfTheQueue(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	now := time.Now().UTC()
	row := timerRow{
		UUID:         "timer-zone-in-cron-spec",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "recurring",
		FireAtUTC:    now.Add(-2 * time.Minute).UnixMilli(),
		CronSpec:     nullableStringValue("CRON_TZ=America/New_York 0 9 * * *"),
		CronTZ:       nullableStringValue("UTC"),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-24 * time.Hour).UnixMilli(),
	}
	insertOwnedTimer(t, service.db, row)
	queued := func() int {
		service.scheduler.mu.Lock()
		defer service.scheduler.mu.Unlock()
		return len(service.scheduler.items)
	}
	var logged bytes.Buffer
	log.SetOutput(&logged)
	defer log.SetOutput(os.Stderr)

	if _, _, err := nextRecurringFireAt(row, now); timerErrorCode(err) != "TIMER_INVALID_CRON" {
		t.Fatalf("want an invalid cron, got %v", err)
	}
	if err := service.scheduler.replayPending(context.Background(), now); err != nil {
		t.Fatalf("the replay stopped at it: %v", err)
	}
	if n := queued(); n != 0 {
		t.Fatalf("the replay queued it (%d entries)", n)
	}
	if !strings.Contains(logged.String(), "timer_replay_invalid_recurring") || !strings.Contains(logged.String(), "the time zone goes in cron_tz") {
		t.Fatalf("the replay did not log it: %s", logged.String())
	}

	service.scheduler.push(row.UUID, row.FireAtUTC)
	service.scheduler.processDue(context.Background(), now)
	if len(tx) != 0 {
		t.Fatalf("sent a fire for it")
	}
	if n := queued(); n != 0 {
		t.Fatalf("queued it again (%d entries)", n)
	}
	if !strings.Contains(logged.String(), "timer_recurring_next_fire_failed") {
		t.Fatalf("its fire did not log it: %s", logged.String())
	}
	stored, err := getTimer(context.Background(), service.db, row.UUID)
	if err != nil {
		t.Fatalf("load timer: %v", err)
	}
	if stored.Status != "pending" || stored.FireAtUTC != row.FireAtUTC || stored.FireCount != 0 {
		t.Fatalf("unexpected row: %+v", stored)
	}
}

func nullableStringValue(value string) sql.NullString {
	return sql.NullString{String: value, Valid: true}
}
