package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
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

	items, err := listTimers(ctx, db, "WF.demo@motherbee", "pending", 10, nil)
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

func TestScheduleIdempotencyAllowsReuseAfterFire(t *testing.T) {
	db := openTestTimerDB(t)
	ctx := context.Background()
	now := time.Now().UTC().UnixMilli()

	// Insert a fired timer with a known client_ref.
	if err := insertTimer(ctx, db, timerRow{
		UUID:           "timer-fired",
		OwnerL2Name:    "WF.demo@motherbee",
		TargetL2Name:   "WF.demo@motherbee",
		ClientRef:      sql.NullString{String: "wf:demo::fired-key", Valid: true},
		Kind:           "oneshot",
		FireAtUTC:      now - 60_000,
		MissedPolicy:   "fire",
		Payload:        `{}`,
		Status:         "fired",
		CreatedAtUTC:   now - 120_000,
		FireCount:      1,
		LastFiredAtUTC: sql.NullInt64{Int64: now - 60_000, Valid: true},
	}); err != nil {
		t.Fatalf("insert fired timer: %v", err)
	}

	// A new pending timer with the same client_ref must be insertable (fired != pending).
	if err := insertTimer(ctx, db, timerRow{
		UUID:         "timer-new",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		ClientRef:    sql.NullString{String: "wf:demo::fired-key", Valid: true},
		Kind:         "oneshot",
		FireAtUTC:    now + 60_000,
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now,
		FireCount:    0,
	}); err != nil {
		t.Fatalf("insert new timer with reused client_ref after fire: %v", err)
	}

	// The lookup must return the new pending timer, not the fired one.
	row, err := findPendingTimerByClientRef(ctx, db, "WF.demo@motherbee", "wf:demo::fired-key")
	if err != nil {
		t.Fatalf("find pending timer by client_ref after fire reuse: %v", err)
	}
	if row.UUID != "timer-new" {
		t.Fatalf("expected new timer, got %+v", row)
	}
}

func TestEnsureTimerSchemaMigratesLegacyV1WithUniqueClientRefs(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "timers-legacy.db")
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatalf("open legacy sqlite db: %v", err)
	}
	defer db.Close()
	createLegacyV1TimerSchema(t, db)
	if _, err := db.Exec(`
INSERT INTO timers (
	uuid, owner_l2_name, target_l2_name, client_ref, kind, fire_at_utc, cron_spec, cron_tz,
	missed_policy, missed_within_ms, payload, metadata, status, created_at_utc, last_fired_at_utc, fire_count
) VALUES
	('timer-1', 'WF.demo@motherbee', 'WF.demo@motherbee', 'wf:demo::timeout-1', 'oneshot', 1775577600000, NULL, NULL, 'fire', NULL, '{}', NULL, 'pending', 1775574000000, NULL, 0),
	('timer-2', 'WF.demo@motherbee', 'WF.demo@motherbee', 'wf:demo::timeout-2', 'oneshot', 1775578600000, NULL, NULL, 'fire', NULL, '{}', NULL, 'pending', 1775575000000, NULL, 0)
`); err != nil {
		t.Fatalf("insert legacy rows: %v", err)
	}

	if err := ensureTimerSchema(db); err != nil {
		t.Fatalf("migrate legacy v1 db with unique client_refs: %v", err)
	}

	var userVersion int
	if err := db.QueryRow(`PRAGMA user_version;`).Scan(&userVersion); err != nil {
		t.Fatalf("read user_version: %v", err)
	}
	if userVersion != timerSchemaVersion {
		t.Fatalf("unexpected migrated schema version: %d", userVersion)
	}
	row, err := findPendingTimerByClientRef(context.Background(), db, "WF.demo@motherbee", "wf:demo::timeout-1")
	if err != nil {
		t.Fatalf("find migrated timer by client_ref: %v", err)
	}
	if row.UUID != "timer-1" {
		t.Fatalf("unexpected migrated timer: %+v", row)
	}
}

func TestEnsureTimerSchemaFailsLegacyV1WithDuplicateClientRefs(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "timers-legacy-dup.db")
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatalf("open legacy sqlite db: %v", err)
	}
	defer db.Close()
	createLegacyV1TimerSchema(t, db)
	if _, err := db.Exec(`
INSERT INTO timers (
	uuid, owner_l2_name, target_l2_name, client_ref, kind, fire_at_utc, cron_spec, cron_tz,
	missed_policy, missed_within_ms, payload, metadata, status, created_at_utc, last_fired_at_utc, fire_count
) VALUES
	('timer-1', 'WF.demo@motherbee', 'WF.demo@motherbee', 'wf:demo::timeout', 'oneshot', 1775577600000, NULL, NULL, 'fire', NULL, '{}', NULL, 'pending', 1775574000000, NULL, 0),
	('timer-2', 'WF.demo@motherbee', 'WF.demo@motherbee', 'wf:demo::timeout', 'oneshot', 1775578600000, NULL, NULL, 'fire', NULL, '{}', NULL, 'pending', 1775575000000, NULL, 0)
`); err != nil {
		t.Fatalf("insert duplicate legacy rows: %v", err)
	}

	err = ensureTimerSchema(db)
	if err == nil {
		t.Fatalf("expected duplicate client_ref migration to fail")
	}
	if !isPendingClientRefUniqueViolation(err) {
		t.Fatalf("expected pending client_ref unique violation, got %v", err)
	}
}

// Schema version 3 (FINDINGS A-220) adds canceled_reason to a version 2 table and keeps its rows. A
// table that has it under user_version 2, which a SY.timer before version 3 writes back over it, is
// left as it is.
func TestEnsureTimerSchemaAddsCanceledReasonToAVersion2Table(t *testing.T) {
	db, err := sql.Open("sqlite", timerDSN(filepath.Join(t.TempDir(), "timers-v2.db")))
	if err != nil {
		t.Fatalf("open sqlite db: %v", err)
	}
	defer db.Close()
	ctx := context.Background()
	createLegacyV1TimerSchema(t, db)
	for _, stmt := range []string{
		`CREATE UNIQUE INDEX idx_timers_owner_clientref_pending ON timers(owner_l2_name, client_ref) WHERE client_ref IS NOT NULL AND status='pending';`,
		`PRAGMA user_version=2;`,
		`INSERT INTO timers (
	uuid, owner_l2_name, target_l2_name, client_ref, kind, fire_at_utc, cron_spec, cron_tz,
	missed_policy, missed_within_ms, payload, metadata, status, created_at_utc, last_fired_at_utc, fire_count
) VALUES ('timer-1', 'WF.demo@motherbee', 'WF.demo@motherbee', 'weekly-report', 'recurring', 1775577600000, '0 9 * * MON', 'UTC', 'fire', NULL, '{}', NULL, 'pending', 1775574000000, NULL, 0);`,
	} {
		if _, err := db.Exec(stmt); err != nil {
			t.Fatalf("version 2 table: %v", err)
		}
	}

	for pass := 1; pass <= 2; pass++ {
		if err := ensureTimerSchema(db); err != nil {
			t.Fatalf("pass %d: %v", pass, err)
		}
		var version int
		if err := db.QueryRow(`PRAGMA user_version;`).Scan(&version); err != nil || version != timerSchemaVersion {
			t.Fatalf("pass %d: user_version %d (%v), want %d", pass, version, err, timerSchemaVersion)
		}
		row, err := getTimer(ctx, db, "timer-1")
		if err != nil || row.Status != "pending" || row.ClientRef.String != "weekly-report" || row.CanceledReason.Valid {
			t.Fatalf("pass %d: the version 2 row: %+v (%v)", pass, row, err)
		}
		// What a SY.timer before version 3 writes back.
		if _, err := db.Exec(`PRAGMA user_version=2;`); err != nil {
			t.Fatalf("user_version 2: %v", err)
		}
	}

	row, err := getTimer(ctx, db, "timer-1")
	if err != nil {
		t.Fatalf("get timer: %v", err)
	}
	row.Status = "canceled"
	row.CanceledReason = sql.NullString{String: "its next fire cannot be computed: invalid cron", Valid: true}
	if err := updateTimer(ctx, db, *row); err != nil {
		t.Fatalf("update timer: %v", err)
	}
	if saved, err := getTimer(ctx, db, "timer-1"); err != nil || saved.Status != "canceled" || saved.CanceledReason != row.CanceledReason {
		t.Fatalf("the canceled timer read back: %+v (%v)", saved, err)
	}
}

// TIMER_GET and TIMER_LIST show a timer with the SDK's fields, and canceled_reason when SY.timer
// canceled it itself, left out otherwise (FINDINGS A-220); the SDK's TimerInfo reads it back
// (FINDINGS A-239).
func TestTimerInfoShowsACanceledReasonOnlyWhenThereIsOne(t *testing.T) {
	row := timerRow{
		UUID:         "timer-1",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "recurring",
		FireAtUTC:    1775577600000,
		CronSpec:     sql.NullString{String: "0 9 * * MON", Valid: true},
		MissedPolicy: "fire",
		Payload:      `{"report":"weekly"}`,
		Status:       "canceled",
		CreatedAtUTC: 1775574000000,
	}
	for _, reason := range []string{"", "its next fire cannot be computed: invalid cron"} {
		row.CanceledReason = sql.NullString{String: reason, Valid: reason != ""}
		info, err := timerRowToInfo(row)
		if err != nil {
			t.Fatalf("timer row to info: %v", err)
		}
		encoded, err := json.Marshal(info)
		if err != nil {
			t.Fatalf("encode: %v", err)
		}
		var fields map[string]any
		if err := json.Unmarshal(encoded, &fields); err != nil {
			t.Fatalf("decode: %v", err)
		}
		if fields["uuid"] != "timer-1" || fields["status"] != "canceled" || fields["cron_spec"] != "0 9 * * MON" || fields["payload"] == nil {
			t.Fatalf("the SDK's fields: %s", encoded)
		}
		if got, has := fields["canceled_reason"]; has != (reason != "") || (has && got != reason) {
			t.Fatalf("reason %q: %s", reason, encoded)
		}
		var decoded fluxbeesdk.TimerInfo
		if err := json.Unmarshal(encoded, &decoded); err != nil {
			t.Fatalf("decode as the SDK does: %v", err)
		}
		if got := decoded.CanceledReason; (got != nil) != (reason != "") || (got != nil && *got != reason) {
			t.Fatalf("reason %q, the SDK read %v", reason, got)
		}
	}
}

// TIMER_GET and TIMER_LIST send a timer as exactly these bytes: the SDK's TimerInfo, every field in
// its order and under its tag, an optional one left out when the row has none; the payload always
// goes, {} for a timer scheduled without one (FINDINGS A-239's review). testdata/timer_wire only
// decodes, so a change to TimerInfo's fields, tags or order, or to what timerRowToInfo fills, fails
// here, not at a client.
func TestATimerGoesOnTheWireByteForByte(t *testing.T) {
	for _, tc := range []struct {
		name string
		row  timerRow
		want string
	}{
		{
			name: "every optional field",
			row: timerRow{
				UUID:           "timer-1",
				OwnerL2Name:    "WF.demo@motherbee",
				TargetL2Name:   "WF.demo@motherbee",
				ClientRef:      sql.NullString{String: "weekly-report", Valid: true},
				Kind:           "recurring",
				FireAtUTC:      1775577600000,
				CronSpec:       sql.NullString{String: "0 9 * * MON", Valid: true},
				CronTZ:         sql.NullString{String: "America/Argentina/Buenos_Aires", Valid: true},
				MissedPolicy:   "fire_if_within",
				MissedWithinMS: sql.NullInt64{Int64: 600000, Valid: true},
				Payload:        `{"report":"weekly"}`,
				Metadata:       sql.NullString{String: `{"created_by":"WF.demo"}`, Valid: true},
				Status:         "canceled",
				CreatedAtUTC:   1775574000000,
				LastFiredAtUTC: sql.NullInt64{Int64: 1775576000000, Valid: true},
				FireCount:      3,
				CanceledReason: sql.NullString{String: "its next fire cannot be computed: invalid cron", Valid: true},
			},
			want: `{"uuid":"timer-1","owner_l2_name":"WF.demo@motherbee","target_l2_name":"WF.demo@motherbee",` +
				`"client_ref":"weekly-report","kind":"recurring","fire_at_utc_ms":1775577600000,` +
				`"cron_spec":"0 9 * * MON","cron_tz":"America/Argentina/Buenos_Aires","missed_policy":"fire_if_within",` +
				`"missed_within_ms":600000,"status":"canceled","created_at_utc_ms":1775574000000,"fire_count":3,` +
				`"last_fired_at_utc_ms":1775576000000,"payload":{"report":"weekly"},"metadata":{"created_by":"WF.demo"},` +
				`"canceled_reason":"its next fire cannot be computed: invalid cron"}`,
		},
		{
			name: "none",
			row: timerRow{
				UUID:         "timer-2",
				OwnerL2Name:  "WF.demo@motherbee",
				TargetL2Name: "WF.demo@motherbee",
				Kind:         "oneshot",
				FireAtUTC:    1775577600000,
				MissedPolicy: "fire",
				Payload:      `{}`,
				Status:       "pending",
				CreatedAtUTC: 1775574000000,
			},
			want: `{"uuid":"timer-2","owner_l2_name":"WF.demo@motherbee","target_l2_name":"WF.demo@motherbee",` +
				`"kind":"oneshot","fire_at_utc_ms":1775577600000,"missed_policy":"fire","status":"pending",` +
				`"created_at_utc_ms":1775574000000,"fire_count":0,"payload":{}}`,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			info, err := timerRowToInfo(tc.row)
			if err != nil {
				t.Fatalf("timer row to info: %v", err)
			}
			encoded, err := json.Marshal(info)
			if err != nil {
				t.Fatalf("encode: %v", err)
			}
			if string(encoded) != tc.want {
				t.Fatalf("the timer on the wire:\n got %s\nwant %s", encoded, tc.want)
			}
		})
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

	deleted, stamped, err := gcHistoricalTimers(ctx, db, now, defaultHistoryRetention)
	if err != nil {
		t.Fatalf("gc historical timers: %v", err)
	}
	if deleted != 1 || stamped != 0 {
		t.Fatalf("expected 1 deleted row and none dated, got %d and %d", deleted, stamped)
	}

	if _, err := getTimer(ctx, db, "timer-old"); err == nil {
		t.Fatalf("expected old timer to be deleted")
	}
	if _, err := getTimer(ctx, db, "timer-pending"); err != nil {
		t.Fatalf("expected pending timer to remain: %v", err)
	}
}

// A canceled timer is kept 7 days from its cancellation, not from its creation or its last fire
// (FINDINGS A-238): one canceled yesterday whose creation, or last fire, is weeks old stays, with its
// canceled_reason. A fired one goes 7 days after its last fire, as before, and a pending one never.
// One canceled with no date (before schema 4, or by a SY.timer before it) is dated at the start, and
// goes 7 days after it. Before, the GC aged every one by its last fire or its creation.
func TestGCHistoricalTimersAgesACanceledTimerByItsCancellation(t *testing.T) {
	db := openTestTimerDB(t)
	ctx := context.Background()
	now := time.UnixMilli(1776000000000).UTC()
	day := 24 * time.Hour
	ago := func(days int) int64 { return now.Add(-time.Duration(days) * day).UnixMilli() }
	at := func(days int) sql.NullInt64 { return sql.NullInt64{Int64: ago(days), Valid: true} }
	timer := func(timerUUID, kind, status string, createdDaysAgo int) timerRow {
		row := timerRow{
			UUID:         timerUUID,
			OwnerL2Name:  "WF.demo@motherbee",
			TargetL2Name: "WF.demo@motherbee",
			Kind:         kind,
			FireAtUTC:    ago(createdDaysAgo - 10),
			MissedPolicy: "fire",
			Payload:      `{}`,
			Status:       status,
			CreatedAtUTC: ago(createdDaysAgo),
		}
		if kind == "recurring" {
			row.CronSpec = sql.NullString{String: "0 9 * * MON", Valid: true}
		}
		return row
	}
	canceledYesterday := timer("canceled-yesterday", "oneshot", "canceled", 30)
	canceledYesterday.CanceledAtUTC = at(1)
	canceledLastWeek := timer("canceled-8-days-ago", "oneshot", "canceled", 30)
	canceledLastWeek.CanceledAtUTC = at(8)
	firedLastWeek := timer("fired-8-days-ago", "oneshot", "fired", 30)
	firedLastWeek.LastFiredAtUTC, firedLastWeek.FireCount = at(8), 1
	firedYesterday := timer("fired-yesterday", "oneshot", "fired", 30)
	firedYesterday.LastFiredAtUTC, firedYesterday.FireCount = at(1), 1
	recurringCanceledYesterday := timer("recurring-canceled-yesterday", "recurring", "canceled", 60)
	recurringCanceledYesterday.LastFiredAtUTC, recurringCanceledYesterday.FireCount = at(20), 6
	recurringCanceledYesterday.CanceledReason = sql.NullString{String: "its next fire cannot be computed: invalid cron", Valid: true}
	recurringCanceledYesterday.CanceledAtUTC = at(1)
	canceledUndated := timer("canceled-before-schema-4", "oneshot", "canceled", 30)
	pending := timer("pending", "oneshot", "pending", 30)
	pending.FireAtUTC = now.Add(day).UnixMilli()
	for _, row := range []timerRow{canceledYesterday, canceledLastWeek, firedLastWeek, firedYesterday, recurringCanceledYesterday, canceledUndated, pending} {
		if err := insertTimer(ctx, db, row); err != nil {
			t.Fatalf("insert %s: %v", row.UUID, err)
		}
	}
	kept := func(when string, want ...string) {
		t.Helper()
		var uuids []string
		rows, err := listTimers(ctx, db, "", "all", maxListLimit, nil)
		if err != nil {
			t.Fatalf("%s: list: %v", when, err)
		}
		for _, row := range rows {
			uuids = append(uuids, row.UUID)
		}
		sort.Strings(uuids)
		sort.Strings(want)
		if strings.Join(uuids, ",") != strings.Join(want, ",") {
			t.Fatalf("%s: kept %v, want %v", when, uuids, want)
		}
	}

	deleted, stamped, err := gcHistoricalTimers(ctx, db, now, defaultHistoryRetention)
	if err != nil || deleted != 2 || stamped != 1 {
		t.Fatalf("the first start: deleted %d, dated %d (%v), want 2 and 1", deleted, stamped, err)
	}
	kept("the first start", "canceled-yesterday", "fired-yesterday", "recurring-canceled-yesterday", "canceled-before-schema-4", "pending")
	if row, err := getTimer(ctx, db, "canceled-before-schema-4"); err != nil || row.CanceledAtUTC != nullableNow(now) {
		t.Fatalf("the undated canceled timer: %+v (%v), want it dated %d", row, err, now.UnixMilli())
	}
	if row, err := getTimer(ctx, db, "recurring-canceled-yesterday"); err != nil || row.CanceledReason != recurringCanceledYesterday.CanceledReason || row.CanceledAtUTC != at(1) {
		t.Fatalf("the recurring timer SY.timer canceled: %+v (%v)", row, err)
	}

	deleted, stamped, err = gcHistoricalTimers(ctx, db, now.Add(8*day), defaultHistoryRetention)
	if err != nil || deleted != 4 || stamped != 0 {
		t.Fatalf("a start 8 days later: deleted %d, dated %d (%v), want 4 and 0", deleted, stamped, err)
	}
	kept("a start 8 days later", "pending")
}

// timerSchemaVersionOfSYTimer14 is the user_version SY.timer 1.4 writes: schema 3 (FINDINGS A-220).
const timerSchemaVersionOfSYTimer14 = 3

// ensureTimerSchemaOfSYTimer14 is SY.timer 1.4's schema step, the first thing its start does, copied
// from its code (6c6b084f, go/sy-timer/persistence.go) with only its timerSchemaVersion renamed. On a
// database it never opened it creates its table; on a newer table every statement is IF NOT EXISTS
// or skipped, and it writes its user_version back.
func ensureTimerSchemaOfSYTimer14(db *sql.DB) error {
	var userVersion int
	if err := db.QueryRow(`PRAGMA user_version;`).Scan(&userVersion); err != nil {
		return err
	}
	if _, err := db.Exec(`
CREATE TABLE IF NOT EXISTS timers (
    uuid                TEXT PRIMARY KEY,
    owner_l2_name       TEXT NOT NULL,
    target_l2_name      TEXT NOT NULL,
    client_ref          TEXT,
    kind                TEXT NOT NULL CHECK(kind IN ('oneshot','recurring')),
    fire_at_utc         INTEGER NOT NULL,
    cron_spec           TEXT,
    cron_tz             TEXT,
    missed_policy       TEXT NOT NULL DEFAULT 'fire'
                        CHECK(missed_policy IN ('fire','drop','fire_if_within')),
    missed_within_ms    INTEGER,
    payload             TEXT NOT NULL,
    metadata            TEXT,
    status              TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending','fired','canceled')),
    created_at_utc      INTEGER NOT NULL,
    last_fired_at_utc   INTEGER,
    fire_count          INTEGER NOT NULL DEFAULT 0,
    canceled_reason     TEXT
);`); err != nil {
		return err
	}
	indexes := []string{
		`CREATE INDEX IF NOT EXISTS idx_timers_owner ON timers(owner_l2_name);`,
		`CREATE INDEX IF NOT EXISTS idx_timers_fire_at ON timers(fire_at_utc) WHERE status='pending';`,
		`CREATE INDEX IF NOT EXISTS idx_timers_status ON timers(status);`,
	}
	for _, stmt := range indexes {
		if _, err := db.Exec(stmt); err != nil {
			return err
		}
	}
	if userVersion < 2 {
		if _, err := db.Exec(`
CREATE UNIQUE INDEX IF NOT EXISTS idx_timers_owner_clientref_pending
ON timers(owner_l2_name, client_ref)
WHERE client_ref IS NOT NULL AND status='pending';
`); err != nil {
			return fmt.Errorf("create unique pending client_ref index: %w", err)
		}
	}
	// Version 3 (FINDINGS A-220): canceled_reason. Added when the table lacks it, whatever
	// user_version says: a SY.timer before it writes user_version 2 over a table that has it.
	var hasCanceledReason int
	if err := db.QueryRow(`SELECT COUNT(*) FROM pragma_table_info('timers') WHERE name='canceled_reason';`).Scan(&hasCanceledReason); err != nil {
		return err
	}
	if hasCanceledReason == 0 {
		if _, err := db.Exec(`ALTER TABLE timers ADD COLUMN canceled_reason TEXT;`); err != nil {
			return fmt.Errorf("add canceled_reason: %w", err)
		}
	}
	if _, err := db.Exec(fmt.Sprintf("PRAGMA user_version=%d;", timerSchemaVersionOfSYTimer14)); err != nil {
		return err
	}
	return nil
}

// startAsSYTimer14 runs SY.timer 1.4's schema step on db, as its start does.
func startAsSYTimer14(t *testing.T, db *sql.DB) {
	t.Helper()
	if err := ensureTimerSchemaOfSYTimer14(db); err != nil {
		t.Fatalf("SY.timer 1.4's schema step: %v", err)
	}
}

// timersSchema is the timers table and its indexes as SQLite keeps them, one per line.
func timersSchema(t *testing.T, db *sql.DB) string {
	t.Helper()
	rows, err := db.Query(`SELECT type, name, COALESCE(sql, '') FROM sqlite_master WHERE tbl_name='timers' ORDER BY type, name`)
	if err != nil {
		t.Fatalf("read the schema: %v", err)
	}
	defer rows.Close()
	var schema []string
	for rows.Next() {
		var kind, name, definition string
		if err := rows.Scan(&kind, &name, &definition); err != nil {
			t.Fatalf("read the schema: %v", err)
		}
		schema = append(schema, kind+" "+name+": "+definition)
	}
	if err := rows.Err(); err != nil {
		t.Fatalf("read the schema: %v", err)
	}
	return strings.Join(schema, "\n")
}

// What SY.timer 1.4 sends its database, copied from its code (6c6b084f, go/sy-timer/persistence.go):
// it names every column it writes and reads, and none is canceled_at_utc.
const (
	insertOfSYTimer14 = `
INSERT INTO timers (uuid, owner_l2_name, target_l2_name, client_ref, kind, fire_at_utc, cron_spec, cron_tz,
       missed_policy, missed_within_ms, payload, metadata, status, created_at_utc, last_fired_at_utc, fire_count,
       canceled_reason) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
`
	updateWhileStatusOfSYTimer14 = `
UPDATE timers
SET owner_l2_name=?, target_l2_name=?, client_ref=?, kind=?, fire_at_utc=?, cron_spec=?, cron_tz=?,
    missed_policy=?, missed_within_ms=?, payload=?, metadata=?, status=?, created_at_utc=?, last_fired_at_utc=?, fire_count=?,
    canceled_reason=?
WHERE uuid=? AND status=?`
	selectOfSYTimer14 = `
SELECT uuid, owner_l2_name, target_l2_name, client_ref, kind, fire_at_utc, cron_spec, cron_tz,
       missed_policy, missed_within_ms, payload, metadata, status, created_at_utc, last_fired_at_utc, fire_count,
       canceled_reason
FROM timers WHERE uuid=?
`
	gcOfSYTimer14 = `
DELETE FROM timers
WHERE status IN ('fired','canceled')
  AND COALESCE(last_fired_at_utc, created_at_utc) < ?
`
)

// writeAsSYTimer14 inserts row as SY.timer 1.4 does.
func writeAsSYTimer14(t *testing.T, db *sql.DB, row timerRow) {
	t.Helper()
	if _, err := db.Exec(insertOfSYTimer14, row.UUID, row.OwnerL2Name, row.TargetL2Name, nullableString(row.ClientRef), row.Kind, row.FireAtUTC,
		nullableString(row.CronSpec), nullableString(row.CronTZ), row.MissedPolicy, nullableInt64(row.MissedWithinMS),
		row.Payload, nullableString(row.Metadata), row.Status, row.CreatedAtUTC, nullableInt64(row.LastFiredAtUTC), row.FireCount,
		nullableString(row.CanceledReason)); err != nil {
		t.Fatalf("SY.timer 1.4's insert of %s: %v", row.UUID, err)
	}
}

// readAsSYTimer14 reads a timer as SY.timer 1.4 does.
func readAsSYTimer14(t *testing.T, db *sql.DB, timerUUID string) timerRow {
	t.Helper()
	var row timerRow
	if err := db.QueryRow(selectOfSYTimer14, timerUUID).Scan(
		&row.UUID, &row.OwnerL2Name, &row.TargetL2Name, &row.ClientRef, &row.Kind, &row.FireAtUTC, &row.CronSpec, &row.CronTZ,
		&row.MissedPolicy, &row.MissedWithinMS, &row.Payload, &row.Metadata, &row.Status, &row.CreatedAtUTC, &row.LastFiredAtUTC, &row.FireCount,
		&row.CanceledReason,
	); err != nil {
		t.Fatalf("SY.timer 1.4's read of %s: %v", timerUUID, err)
	}
	return row
}

// Schema version 4 (FINDINGS A-238) adds canceled_at_utc to SY.timer 1.4's table, and keeps its rows:
// their cancellation has no date, until the next start's GC dates it. A table that has it under
// user_version 3, which SY.timer 1.4's start writes back over it, is left as it is.
func TestEnsureTimerSchemaAddsCanceledAtToAVersion3Table(t *testing.T) {
	db, err := sql.Open("sqlite", timerDSN(filepath.Join(t.TempDir(), "timers-v3.db")))
	if err != nil {
		t.Fatalf("open sqlite db: %v", err)
	}
	defer db.Close()
	ctx := context.Background()
	// SY.timer 1.4 starts on a database it never opened: its table, schema 3.
	startAsSYTimer14(t, db)
	created := time.Now().UTC().Add(-time.Hour).UnixMilli()
	reason := sql.NullString{String: "missed: its fire time passed while SY.timer was down, and its missed_policy is drop", Valid: true}
	for _, row := range []timerRow{
		{UUID: "timer-pending", OwnerL2Name: "WF.demo@motherbee", TargetL2Name: "WF.demo@motherbee", ClientRef: sql.NullString{String: "weekly-report", Valid: true},
			Kind: "oneshot", FireAtUTC: created + time.Hour.Milliseconds()*2, MissedPolicy: "fire", Payload: `{}`, Status: "pending", CreatedAtUTC: created},
		{UUID: "timer-dropped", OwnerL2Name: "WF.demo@motherbee", TargetL2Name: "WF.demo@motherbee",
			Kind: "oneshot", FireAtUTC: created + time.Minute.Milliseconds(), MissedPolicy: "drop", Payload: `{}`, Status: "canceled", CreatedAtUTC: created, CanceledReason: reason},
	} {
		writeAsSYTimer14(t, db, row)
	}

	for pass := 1; pass <= 2; pass++ {
		if err := ensureTimerSchema(db); err != nil {
			t.Fatalf("pass %d: %v", pass, err)
		}
		var version, hasColumn int
		if err := db.QueryRow(`PRAGMA user_version;`).Scan(&version); err != nil || version != timerSchemaVersion {
			t.Fatalf("pass %d: user_version %d (%v), want %d", pass, version, err, timerSchemaVersion)
		}
		if err := db.QueryRow(`SELECT COUNT(*) FROM pragma_table_info('timers') WHERE name='canceled_at_utc';`).Scan(&hasColumn); err != nil || hasColumn != 1 {
			t.Fatalf("pass %d: canceled_at_utc %d times (%v)", pass, hasColumn, err)
		}
		if row, err := getTimer(ctx, db, "timer-pending"); err != nil || row.Status != "pending" || row.ClientRef.String != "weekly-report" || row.CanceledAtUTC.Valid {
			t.Fatalf("pass %d: the pending row: %+v (%v)", pass, row, err)
		}
		if row, err := getTimer(ctx, db, "timer-dropped"); err != nil || row.Status != "canceled" || row.CanceledReason != reason || row.CanceledAtUTC.Valid {
			t.Fatalf("pass %d: the canceled row: %+v (%v)", pass, row, err)
		}
		// SY.timer 1.4 starts on it: it writes user_version 3 back.
		startAsSYTimer14(t, db)
	}
}

// Back to SY.timer 1.4 after 1.5 (FINDINGS A-238's rollback): 1.4 starts on the version 4 table, and
// reads, writes and deletes its timers as before. Its start, its own schema step run as it is, writes
// user_version 3 back and changes nothing else: every other statement of its ensureTimerSchema is IF
// NOT EXISTS, or skipped. Its GC ages a canceled timer by its creation again. The date 1.5 gave a
// cancellation survives 1.4's writes, and 1.4's own cancellation has none; 1.5, started again, keeps
// the table and dates that one.
func TestSYTimer14KeepsWorkingOnAVersion4Table(t *testing.T) {
	db := openTestTimerDB(t)
	ctx := context.Background()
	now := time.Now().UTC()
	canceledBy15 := func(timerUUID string, created time.Time) timerRow {
		return timerRow{UUID: timerUUID, OwnerL2Name: "WF.demo@motherbee", TargetL2Name: "WF.demo@motherbee",
			Kind: "oneshot", FireAtUTC: now.Add(time.Hour).UnixMilli(), MissedPolicy: "fire", Payload: `{}`, Status: "canceled",
			CreatedAtUTC: created.UnixMilli(), CanceledAtUTC: nullableNow(now.Add(-time.Hour))}
	}
	scheduledLastMonth := canceledBy15("timer-of-last-month", now.Add(-30*24*time.Hour))
	scheduledToday := canceledBy15("timer-of-today", now.Add(-2*time.Hour))
	for _, row := range []timerRow{scheduledLastMonth, scheduledToday} {
		if err := insertTimer(ctx, db, row); err != nil {
			t.Fatalf("insert %s: %v", row.UUID, err)
		}
	}

	// SY.timer 1.4 starts: its schema step, which leaves the table as it is, then its history GC,
	// which takes the timer canceled an hour ago but scheduled a month ago.
	schemaOf15 := timersSchema(t, db)
	startAsSYTimer14(t, db)
	var version int
	if err := db.QueryRow(`PRAGMA user_version;`).Scan(&version); err != nil || version != timerSchemaVersionOfSYTimer14 {
		t.Fatalf("SY.timer 1.4's start: user_version %d (%v), want %d", version, err, timerSchemaVersionOfSYTimer14)
	}
	if schema := timersSchema(t, db); schema != schemaOf15 {
		t.Fatalf("SY.timer 1.4's start changed the table:\n%s\nwas:\n%s", schema, schemaOf15)
	}
	result, err := db.Exec(gcOfSYTimer14, now.Add(-defaultHistoryRetention).UnixMilli())
	if err != nil {
		t.Fatalf("SY.timer 1.4's gc: %v", err)
	}
	if deleted, _ := result.RowsAffected(); deleted != 1 {
		t.Fatalf("SY.timer 1.4's gc deleted %d timers, want 1", deleted)
	}
	if _, err := getTimer(ctx, db, scheduledLastMonth.UUID); err != sql.ErrNoRows {
		t.Fatalf("the timer scheduled a month ago: %v", err)
	}
	// It schedules a timer, reads it, and cancels it; it cancels 1.5's again.
	scheduledBy14 := timerRow{UUID: "timer-of-1.4", OwnerL2Name: "WF.demo@motherbee", TargetL2Name: "WF.demo@motherbee",
		Kind: "oneshot", FireAtUTC: now.Add(time.Hour).UnixMilli(), MissedPolicy: "fire", Payload: `{}`, Status: "pending", CreatedAtUTC: now.UnixMilli()}
	writeAsSYTimer14(t, db, scheduledBy14)
	for _, timerUUID := range []string{scheduledBy14.UUID, scheduledToday.UUID} {
		row := readAsSYTimer14(t, db, timerUUID)
		status := row.Status
		row.Status = "canceled"
		result, err := db.Exec(updateWhileStatusOfSYTimer14, row.OwnerL2Name, row.TargetL2Name, nullableString(row.ClientRef), row.Kind, row.FireAtUTC,
			nullableString(row.CronSpec), nullableString(row.CronTZ), row.MissedPolicy, nullableInt64(row.MissedWithinMS),
			row.Payload, nullableString(row.Metadata), row.Status, row.CreatedAtUTC, nullableInt64(row.LastFiredAtUTC),
			row.FireCount, nullableString(row.CanceledReason), row.UUID, status)
		if err != nil {
			t.Fatalf("SY.timer 1.4's update of %s: %v", timerUUID, err)
		}
		if written, _ := result.RowsAffected(); written != 1 {
			t.Fatalf("SY.timer 1.4's update of %s wrote %d rows", timerUUID, written)
		}
		if read := readAsSYTimer14(t, db, timerUUID); read.Status != "canceled" {
			t.Fatalf("SY.timer 1.4 read back %+v", read)
		}
	}
	if row, err := getTimer(ctx, db, scheduledToday.UUID); err != nil || row.CanceledAtUTC != scheduledToday.CanceledAtUTC {
		t.Fatalf("the date 1.5 gave: %+v (%v)", row, err)
	}
	if row, err := getTimer(ctx, db, scheduledBy14.UUID); err != nil || row.Status != "canceled" || row.CanceledAtUTC.Valid {
		t.Fatalf("1.4's own cancellation: %+v (%v)", row, err)
	}

	// SY.timer 1.5 starts again.
	if err := ensureTimerSchema(db); err != nil {
		t.Fatalf("ensure schema: %v", err)
	}
	if err := db.QueryRow(`PRAGMA user_version;`).Scan(&version); err != nil || version != timerSchemaVersion {
		t.Fatalf("user_version %d (%v), want %d", version, err, timerSchemaVersion)
	}
	if schema := timersSchema(t, db); schema != schemaOf15 {
		t.Fatalf("SY.timer 1.5's start changed the table:\n%s\nwas:\n%s", schema, schemaOf15)
	}
	restart := now.Add(time.Minute)
	deleted, stamped, err := gcHistoricalTimers(ctx, db, restart, defaultHistoryRetention)
	if err != nil || deleted != 0 || stamped != 1 {
		t.Fatalf("the start: deleted %d, dated %d (%v), want 0 and 1", deleted, stamped, err)
	}
	if row, err := getTimer(ctx, db, scheduledBy14.UUID); err != nil || row.CanceledAtUTC != nullableNow(restart) {
		t.Fatalf("1.4's cancellation, dated at the start: %+v (%v)", row, err)
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

func createLegacyV1TimerSchema(t *testing.T, db *sql.DB) {
	t.Helper()
	if _, err := db.Exec(`
CREATE TABLE timers (
    uuid                TEXT PRIMARY KEY,
    owner_l2_name       TEXT NOT NULL,
    target_l2_name      TEXT NOT NULL,
    client_ref          TEXT,
    kind                TEXT NOT NULL CHECK(kind IN ('oneshot','recurring')),
    fire_at_utc         INTEGER NOT NULL,
    cron_spec           TEXT,
    cron_tz             TEXT,
    missed_policy       TEXT NOT NULL DEFAULT 'fire'
                        CHECK(missed_policy IN ('fire','drop','fire_if_within')),
    missed_within_ms    INTEGER,
    payload             TEXT NOT NULL,
    metadata            TEXT,
    status              TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending','fired','canceled')),
    created_at_utc      INTEGER NOT NULL,
    last_fired_at_utc   INTEGER,
    fire_count          INTEGER NOT NULL DEFAULT 0
);`); err != nil {
		t.Fatalf("create legacy timers table: %v", err)
	}
	for _, stmt := range []string{
		`CREATE INDEX idx_timers_owner ON timers(owner_l2_name);`,
		`CREATE INDEX idx_timers_fire_at ON timers(fire_at_utc) WHERE status='pending';`,
		`CREATE INDEX idx_timers_status ON timers(status);`,
		`PRAGMA user_version=1;`,
	} {
		if _, err := db.Exec(stmt); err != nil {
			t.Fatalf("apply legacy schema statement %q: %v", stmt, err)
		}
	}
}
