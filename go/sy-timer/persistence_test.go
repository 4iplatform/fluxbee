package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"os"
	"path/filepath"
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
