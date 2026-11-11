//go:build linux

package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"strings"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

func TestReplayPendingDropCancelsMissedOneShot(t *testing.T) {
	service, _ := newTestService(t)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-drop",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    now.Add(-2 * time.Minute).UnixMilli(),
		MissedPolicy: "drop",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-10 * time.Minute).UnixMilli(),
	})

	scheduler := newTimerScheduler(service)
	if err := scheduler.replayPending(context.Background(), now); err != nil {
		t.Fatalf("replay pending drop: %v", err)
	}

	row, err := getTimer(context.Background(), service.db, "timer-drop")
	if err != nil {
		t.Fatalf("load dropped timer: %v", err)
	}
	if row.Status != "canceled" || !strings.Contains(row.CanceledReason.String, "its missed_policy is drop") {
		t.Fatalf("expected dropped timer to be canceled, saying why: %+v", row)
	}
	// Dated at the replay: kept 7 days from it (FINDINGS A-238).
	if row.CanceledAtUTC != nullableNow(now) {
		t.Fatalf("canceled at %+v, want the replay's %d", row.CanceledAtUTC, now.UnixMilli())
	}
}

func TestReplayPendingFireIfWithinCancelsOutsideWindow(t *testing.T) {
	service, _ := newTestService(t)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-outside-window",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    now.Add(-10 * time.Minute).UnixMilli(),
		MissedPolicy: "fire_if_within",
		MissedWithinMS: sql.NullInt64{
			Int64: int64((time.Minute).Milliseconds()),
			Valid: true,
		},
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-20 * time.Minute).UnixMilli(),
	})

	scheduler := newTimerScheduler(service)
	if err := scheduler.replayPending(context.Background(), now); err != nil {
		t.Fatalf("replay fire_if_within outside window: %v", err)
	}

	row, err := getTimer(context.Background(), service.db, "timer-outside-window")
	if err != nil {
		t.Fatalf("load timer: %v", err)
	}
	if row.Status != "canceled" || !strings.Contains(row.CanceledReason.String, "more than its missed_within_ms (60000 ms) ago") {
		t.Fatalf("expected timer to be canceled outside window, saying why: %+v", row)
	}
	if row.CanceledAtUTC != nullableNow(now) {
		t.Fatalf("canceled at %+v, want the replay's %d", row.CanceledAtUTC, now.UnixMilli())
	}
}

func TestReplayPendingFireIfWithinFiresInsideWindow(t *testing.T) {
	service, tx := newTestService(t)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-inside-window",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    now.Add(-30 * time.Second).UnixMilli(),
		MissedPolicy: "fire_if_within",
		MissedWithinMS: sql.NullInt64{
			Int64: int64(time.Minute.Milliseconds()),
			Valid: true,
		},
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-2 * time.Minute).UnixMilli(),
	})

	scheduler := newTimerScheduler(service)
	if err := scheduler.replayPending(context.Background(), now); err != nil {
		t.Fatalf("replay fire_if_within inside window: %v", err)
	}
	scheduler.processDue(context.Background(), now)

	frame := mustReadSentFrame(t, tx)
	var msg fluxbeesdk.Message
	if err := json.Unmarshal(frame, &msg); err != nil {
		t.Fatalf("decode fired event: %v", err)
	}
	if msg.Meta.Msg == nil || *msg.Meta.Msg != fluxbeesdk.MsgTimerFired {
		t.Fatalf("unexpected fired message: %+v", msg.Meta)
	}
}

func TestProcessDueRecurringFiresAndRequeues(t *testing.T) {
	service, tx := newTestService(t)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-recurring-due",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "recurring",
		FireAtUTC:    now.Add(-2 * time.Minute).UnixMilli(),
		CronSpec:     sql.NullString{String: "0 9 * * MON", Valid: true},
		CronTZ:       sql.NullString{String: "UTC", Valid: true},
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-24 * time.Hour).UnixMilli(),
	})

	scheduler := newTimerScheduler(service)
	// processDue works off the scheduler's queue, which scheduling (or a replay) fills; this row was
	// written straight to the DB, so it is queued here.
	scheduler.push("timer-recurring-due", now.Add(-2*time.Minute).UnixMilli())
	scheduler.processDue(context.Background(), now)

	frame := mustReadSentFrame(t, tx)
	var msg fluxbeesdk.Message
	if err := json.Unmarshal(frame, &msg); err != nil {
		t.Fatalf("decode fired event: %v", err)
	}
	if msg.Meta.Msg == nil || *msg.Meta.Msg != fluxbeesdk.MsgTimerFired {
		t.Fatalf("unexpected fired message: %+v", msg.Meta)
	}

	row, err := getTimer(context.Background(), service.db, "timer-recurring-due")
	if err != nil {
		t.Fatalf("load recurring timer: %v", err)
	}
	if row.Status != "pending" || row.FireCount != 1 || row.FireAtUTC <= now.UnixMilli() {
		t.Fatalf("expected recurring timer to be requeued: %+v", row)
	}
}

// A start gives its history GC's counts in its timer_startup event and its ready line (§11). A GC
// that failed returns counts of 0, as one that found nothing to delete or to date: the start says
// gc=failed instead, with the error in the event, and gives no counts (FINDINGS A-238's review).
func TestAStartWhoseHistoryGCFailedSaysSo(t *testing.T) {
	db := openTestTimerDB(t)
	ctx := context.Background()
	now := time.Now().UTC()
	insertOwnedTimer(t, db, timerRow{UUID: "timer-canceled-before-1.5", OwnerL2Name: "WF.demo@motherbee", TargetL2Name: "WF.demo@motherbee",
		Kind: "oneshot", FireAtUTC: now.Add(time.Hour).UnixMilli(), MissedPolicy: "fire", Payload: `{}`, Status: "canceled",
		CreatedAtUTC: now.Add(-time.Hour).UnixMilli()})

	deleted, stamped, err := gcHistoricalTimers(ctx, db, now, defaultHistoryRetention)
	fields := map[string]any{}
	if line := startupGC(fields, deleted, stamped, err); err != nil || line != "gc_deleted=0 gc_stamped=1" ||
		len(fields) != 2 || fields["gc_deleted"] != int64(0) || fields["gc_stamped"] != int64(1) {
		t.Fatalf("a GC that dated a timer: %q, %v (%v)", line, fields, err)
	}

	_ = db.Close()
	deleted, stamped, err = gcHistoricalTimers(ctx, db, now, defaultHistoryRetention)
	fields = map[string]any{}
	if line := startupGC(fields, deleted, stamped, err); err == nil || deleted != 0 || stamped != 0 || line != "gc=failed" ||
		len(fields) != 1 || fields["gc_error"] != err.Error() {
		t.Fatalf("a GC that failed: deleted %d, dated %d, %q, %v (%v)", deleted, stamped, line, fields, err)
	}
}
