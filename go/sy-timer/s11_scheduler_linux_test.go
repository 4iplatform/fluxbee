//go:build linux

package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"path/filepath"
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

// A start gives its count of pending timers in its timer_startup event and its ready line (§11). A
// count that failed is 0, as for a hive with no pending timer: the start says pending=failed instead,
// with the error in the event, and gives no count (FINDINGS A-259). Before, the ready line said
// pending=0, next to a separate error line.
func TestAStartWhoseCountOfPendingTimersFailedSaysSo(t *testing.T) {
	db := openTestTimerDB(t)
	ctx := context.Background()
	now := time.Now().UTC()
	insertOwnedTimer(t, db, timerRow{UUID: "timer-pending", OwnerL2Name: "WF.demo@motherbee", TargetL2Name: "WF.demo@motherbee",
		Kind: "oneshot", FireAtUTC: now.Add(time.Hour).UnixMilli(), MissedPolicy: "fire", Payload: `{}`, Status: "pending",
		CreatedAtUTC: now.UnixMilli()})

	count, err := countPendingTimers(ctx, db)
	fields := map[string]any{}
	if line := startupPending(fields, count, err); err != nil || line != "pending=1" || len(fields) != 1 || fields["pending_count"] != int64(1) {
		t.Fatalf("a count of one pending timer: %q, %v (%v)", line, fields, err)
	}

	_ = db.Close()
	count, err = countPendingTimers(ctx, db)
	fields = map[string]any{}
	if line := startupPending(fields, count, err); err == nil || count != 0 || line != "pending=failed" ||
		len(fields) != 1 || fields["pending_error"] != err.Error() {
		t.Fatalf("a count that failed: %d, %q, %v (%v)", count, line, fields, err)
	}
}

// A start gives what it did to the indexes in its timer_startup event and its ready line, when it
// built or dropped one or could not (startupIndexes): the first start of 1.7 builds the four
// idx_timers_list* over the table it finds and drops the two they replace, and says how long that
// took, indexes_ms, apart from the router's connection and the replay; an index it could not build
// is in indexes_failed with the error, and the start goes on. A start that did none of it, as most,
// says nothing of them (the review of A-326). Before, the ready line said nothing of the indexes, and
// a build that failed stopped the start.
func TestAStartSaysWhatItDidToTheIndexes(t *testing.T) {
	db, err := sql.Open("sqlite", timerDSN(filepath.Join(t.TempDir(), "timers-v3.db")))
	if err != nil {
		t.Fatalf("open sqlite db: %v", err)
	}
	defer db.Close()
	startAsSYTimer14(t, db)
	if _, err := db.Exec(`CREATE TABLE idx_timers_list_owner (taken INTEGER)`); err != nil {
		t.Fatalf("take an index's name: %v", err)
	}
	start := func() (string, map[string]any, timerIndexStep) {
		t.Helper()
		indexes, err := ensureTimerSchema(db)
		if err != nil {
			t.Fatalf("the start: %v", err)
		}
		fields := map[string]any{}
		return startupIndexes(fields, indexes), fields, indexes
	}

	line, fields, indexes := start()
	took := indexes.took.Milliseconds()
	if want := fmt.Sprintf("indexes_built=3 indexes_dropped=0 indexes_ms=%d indexes_failed=1", took); line != want {
		t.Fatalf("a start that could not build an index says %q, want %q", line, want)
	}
	failed, _ := fields["indexes_failed"].(map[string]string)
	if len(fields) != 4 || fmt.Sprint(fields["indexes_built"]) != "[idx_timers_list_owner_status idx_timers_list_status idx_timers_list]" ||
		fmt.Sprint(fields["indexes_dropped"]) != "[]" || fields["indexes_ms"] != took || len(failed) != 1 ||
		!strings.Contains(failed["idx_timers_list_owner"], "already a table named idx_timers_list_owner") {
		t.Fatalf("its timer_startup fields: %v", fields)
	}
	if encoded, err := json.Marshal(fields); err != nil || !strings.Contains(string(encoded), `"indexes_dropped":[]`) {
		t.Fatalf("the fields as the event gives them: %s (%v)", encoded, err)
	}

	if _, err := db.Exec(`DROP TABLE idx_timers_list_owner`); err != nil {
		t.Fatalf("free the name: %v", err)
	}
	line, fields, indexes = start()
	took = indexes.took.Milliseconds()
	if want := fmt.Sprintf("indexes_built=1 indexes_dropped=2 indexes_ms=%d", took); line != want ||
		len(fields) != 3 || fmt.Sprint(fields["indexes_built"]) != "[idx_timers_list_owner]" ||
		fmt.Sprint(fields["indexes_dropped"]) != "[idx_timers_owner idx_timers_fire_at]" || fields["indexes_ms"] != took {
		t.Fatalf("the next start says %q, %v; want %q", line, fields, want)
	}

	if line, fields, _ = start(); line != "" || len(fields) != 0 {
		t.Fatalf("a start that did nothing to the indexes says %q, %v", line, fields)
	}
}
