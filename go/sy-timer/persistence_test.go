package main

import (
	"context"
	"database/sql"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func openTestTimerDB(t *testing.T) *sql.DB {
	t.Helper()
	dir := t.TempDir()
	db, err := openTimerDB(filepath.Join(dir, "timers.db"))
	if err != nil {
		t.Fatalf("open timer db: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return db
}

func TestEnsureTimerSchemaCreatesTableAndIndexes(t *testing.T) {
	db := openTestTimerDB(t)

	var userVersion int
	if err := db.QueryRow(`PRAGMA user_version;`).Scan(&userVersion); err != nil {
		t.Fatalf("read user_version: %v", err)
	}
	if userVersion != timerSchemaVersion {
		t.Fatalf("unexpected schema version: %d", userVersion)
	}

	var count int
	if err := db.QueryRow(`SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='timers'`).Scan(&count); err != nil {
		t.Fatalf("check table: %v", err)
	}
	if count != 1 {
		t.Fatalf("timers table missing")
	}

	if err := db.QueryRow(`SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_timers_owner_clientref_pending'`).Scan(&count); err != nil {
		t.Fatalf("check unique pending client_ref index: %v", err)
	}
	if count != 1 {
		t.Fatalf("pending client_ref index missing")
	}
}

func TestInsertGetAndListTimer(t *testing.T) {
	db := openTestTimerDB(t)
	ctx := context.Background()
	row := timerRow{
		UUID:         "timer-1",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    1775577600000,
		MissedPolicy: "fire",
		Payload:      `{"ticket_id":1234}`,
		Status:       "pending",
		CreatedAtUTC: 1775574000000,
		FireCount:    0,
	}
	if err := insertTimer(ctx, db, row); err != nil {
		t.Fatalf("insert timer: %v", err)
	}

	got, err := getTimer(ctx, db, "timer-1")
	if err != nil {
		t.Fatalf("get timer: %v", err)
	}
	if got.UUID != "timer-1" || got.OwnerL2Name != "WF.demo@motherbee" {
		t.Fatalf("unexpected timer row: %+v", got)
	}

	items, err := listTimers(ctx, db, "WF.demo@motherbee", "pending", 10)
	if err != nil {
		t.Fatalf("list timers: %v", err)
	}
	if len(items) != 1 || items[0].UUID != "timer-1" {
		t.Fatalf("unexpected listed timers: %+v", items)
	}
}

func TestFindPendingTimerByClientRefReturnsOnlyActiveTimer(t *testing.T) {
	db := openTestTimerDB(t)
	ctx := context.Background()
	now := time.Now().UTC().UnixMilli()
	if err := insertTimer(ctx, db, timerRow{
		UUID:         "timer-fired",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		ClientRef:    sql.NullString{String: "wf:demo::timeout", Valid: true},
		Kind:         "oneshot",
		FireAtUTC:    now - 60_000,
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "fired",
		CreatedAtUTC: now - 120_000,
		FireCount:    1,
	}); err != nil {
		t.Fatalf("insert fired timer: %v", err)
	}
	if err := insertTimer(ctx, db, timerRow{
		UUID:         "timer-pending",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		ClientRef:    sql.NullString{String: "wf:demo::timeout", Valid: true},
		Kind:         "oneshot",
		FireAtUTC:    now + 60_000,
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now,
		FireCount:    0,
	}); err != nil {
		t.Fatalf("insert pending timer: %v", err)
	}

	row, err := findPendingTimerByClientRef(ctx, db, "WF.demo@motherbee", "wf:demo::timeout")
	if err != nil {
		t.Fatalf("find pending timer by client_ref: %v", err)
	}
	if row.UUID != "timer-pending" {
		t.Fatalf("unexpected pending timer: %+v", row)
	}
}

func TestPendingClientRefUniqueIndexRejectsDuplicateActiveTimers(t *testing.T) {
	db := openTestTimerDB(t)
	ctx := context.Background()
	row := timerRow{
		UUID:         "timer-1",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		ClientRef:    sql.NullString{String: "wf:demo::timeout", Valid: true},
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
		FireCount:    0,
	}
	if err := insertTimer(ctx, db, row); err != nil {
		t.Fatalf("insert first timer: %v", err)
	}
	row.UUID = "timer-2"
	row.CreatedAtUTC++
	if err := insertTimer(ctx, db, row); !isPendingClientRefUniqueViolation(err) {
		t.Fatalf("expected pending client_ref unique violation, got %v", err)
	}
}

func TestTimerRowToInfoPreservesJsonBlobs(t *testing.T) {
	info, err := timerRowToInfo(timerRow{
		UUID:         "timer-1",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    1775577600000,
		MissedPolicy: "fire_if_within",
		MissedWithinMS: sql.NullInt64{
			Int64: 600000,
			Valid: true,
		},
		Payload:      `{"ticket_id":1234}`,
		Metadata:     sql.NullString{String: `{"reason":"demo"}`, Valid: true},
		Status:       "pending",
		CreatedAtUTC: 1775574000000,
		FireCount:    0,
	})
	if err != nil {
		t.Fatalf("timer row to info: %v", err)
	}
	if string(info.Payload) != `{"ticket_id":1234}` {
		t.Fatalf("unexpected payload: %s", string(info.Payload))
	}
	if string(info.Metadata) != `{"reason":"demo"}` {
		t.Fatalf("unexpected metadata: %s", string(info.Metadata))
	}
}

func TestGCHistoricalTimersDeletesOldHistoricalRows(t *testing.T) {
	db := openTestTimerDB(t)
	ctx := context.Background()
	now := time.UnixMilli(1776000000000).UTC()

	oldHistorical := timerRow{
		UUID:         "timer-old",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    1775000000000,
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "fired",
		CreatedAtUTC: 1775000000000,
		LastFiredAtUTC: sql.NullInt64{
			Int64: 1775000000000,
			Valid: true,
		},
		FireCount: 1,
	}
	newPending := timerRow{
		UUID:         "timer-pending",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    1777000000000,
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: 1777000000000,
		FireCount:    0,
	}
	if err := insertTimer(ctx, db, oldHistorical); err != nil {
		t.Fatalf("insert old historical timer: %v", err)
	}
	if err := insertTimer(ctx, db, newPending); err != nil {
		t.Fatalf("insert pending timer: %v", err)
	}

	deleted, err := gcHistoricalTimers(ctx, db, now, defaultHistoryRetention)
	if err != nil {
		t.Fatalf("gc historical timers: %v", err)
	}
	if deleted != 1 {
		t.Fatalf("expected 1 deleted row, got %d", deleted)
	}

	if _, err := getTimer(ctx, db, "timer-old"); err == nil {
		t.Fatalf("expected old timer to be deleted")
	}
	if _, err := getTimer(ctx, db, "timer-pending"); err != nil {
		t.Fatalf("expected pending timer to remain: %v", err)
	}
}

func TestManagedNodeInstanceDirUsesSYKindRoot(t *testing.T) {
	got, err := managedNodeInstanceDir("SY.timer@motherbee")
	if err != nil {
		t.Fatalf("managed node instance dir: %v", err)
	}
	want := filepath.Join(nodeRootDir, "SY", "SY.timer@motherbee")
	if got != want {
		t.Fatalf("unexpected managed dir: got=%s want=%s", got, want)
	}
}

func TestOpenTimerDBCreatesFile(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "timers.db")
	db, err := openTimerDB(path)
	if err != nil {
		t.Fatalf("open timer db: %v", err)
	}
	_ = db.Close()
	if _, err := os.Stat(path); err != nil {
		t.Fatalf("expected db file to exist: %v", err)
	}
}
