package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"path/filepath"
	"strings"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	_ "modernc.org/sqlite"
)

const (
	timerSchemaVersion      = 1
	defaultHistoryRetention = 7 * 24 * time.Hour
	defaultListLimit        = 100
	maxListLimit            = 1000
)

type timerRow struct {
	UUID           string
	OwnerL2Name    string
	TargetL2Name   string
	ClientRef      sql.NullString
	Kind           string
	FireAtUTC      int64
	CronSpec       sql.NullString
	CronTZ         sql.NullString
	MissedPolicy   string
	MissedWithinMS sql.NullInt64
	Payload        string
	Metadata       sql.NullString
	Status         string
	CreatedAtUTC   int64
	LastFiredAtUTC sql.NullInt64
	FireCount      int64
}

func openTimerDB(path string) (*sql.DB, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	if _, err := db.Exec("PRAGMA journal_mode=WAL;"); err != nil {
		_ = db.Close()
		return nil, err
	}
	if _, err := db.Exec("PRAGMA busy_timeout=5000;"); err != nil {
		_ = db.Close()
		return nil, err
	}
	if err := ensureTimerSchema(db); err != nil {
		_ = db.Close()
		return nil, err
	}
	return db, nil
}

func ensureTimerSchema(db *sql.DB) error {
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
    fire_count          INTEGER NOT NULL DEFAULT 0
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
	if _, err := db.Exec(fmt.Sprintf("PRAGMA user_version=%d;", timerSchemaVersion)); err != nil {
		return err
	}
	return nil
}

func managedNodeInstanceDir(nodeName string) (string, error) {
	nodeName = strings.TrimSpace(nodeName)
	if nodeName == "" {
		return "", fmt.Errorf("node_name must be non-empty")
	}
	localName, _, ok := strings.Cut(nodeName, "@")
	if !ok || strings.TrimSpace(localName) == "" {
		return "", fmt.Errorf("node_name must include hive suffix")
	}
	kind, _, ok := strings.Cut(localName, ".")
	if !ok || strings.TrimSpace(kind) == "" {
		return "", fmt.Errorf("node_name must include kind prefix")
	}
	return filepath.Join(nodeRootDir, kind, nodeName), nil
}

func insertTimer(ctx context.Context, db *sql.DB, row timerRow) error {
	if err := validateTimerRow(row); err != nil {
		return err
	}
	_, err := db.ExecContext(ctx, `
INSERT INTO timers (
    uuid, owner_l2_name, target_l2_name, client_ref, kind, fire_at_utc, cron_spec, cron_tz,
    missed_policy, missed_within_ms, payload, metadata, status, created_at_utc, last_fired_at_utc, fire_count
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
`, row.UUID, row.OwnerL2Name, row.TargetL2Name, nullableString(row.ClientRef), row.Kind, row.FireAtUTC,
		nullableString(row.CronSpec), nullableString(row.CronTZ), row.MissedPolicy, nullableInt64(row.MissedWithinMS),
		row.Payload, nullableString(row.Metadata), row.Status, row.CreatedAtUTC, nullableInt64(row.LastFiredAtUTC), row.FireCount)
	return err
}

func updateTimer(ctx context.Context, db *sql.DB, row timerRow) error {
	if err := validateTimerRow(row); err != nil {
		return err
	}
	_, err := db.ExecContext(ctx, `
UPDATE timers
SET owner_l2_name=?, target_l2_name=?, client_ref=?, kind=?, fire_at_utc=?, cron_spec=?, cron_tz=?,
    missed_policy=?, missed_within_ms=?, payload=?, metadata=?, status=?, created_at_utc=?, last_fired_at_utc=?, fire_count=?
WHERE uuid=?
`, row.OwnerL2Name, row.TargetL2Name, nullableString(row.ClientRef), row.Kind, row.FireAtUTC,
		nullableString(row.CronSpec), nullableString(row.CronTZ), row.MissedPolicy, nullableInt64(row.MissedWithinMS),
		row.Payload, nullableString(row.Metadata), row.Status, row.CreatedAtUTC, nullableInt64(row.LastFiredAtUTC),
		row.FireCount, row.UUID)
	return err
}

func getTimer(ctx context.Context, db *sql.DB, timerUUID string) (*timerRow, error) {
	var row timerRow
	err := db.QueryRowContext(ctx, `
SELECT uuid, owner_l2_name, target_l2_name, client_ref, kind, fire_at_utc, cron_spec, cron_tz,
       missed_policy, missed_within_ms, payload, metadata, status, created_at_utc, last_fired_at_utc, fire_count
FROM timers WHERE uuid=?
`, timerUUID).Scan(
		&row.UUID, &row.OwnerL2Name, &row.TargetL2Name, &row.ClientRef, &row.Kind, &row.FireAtUTC, &row.CronSpec, &row.CronTZ,
		&row.MissedPolicy, &row.MissedWithinMS, &row.Payload, &row.Metadata, &row.Status, &row.CreatedAtUTC, &row.LastFiredAtUTC, &row.FireCount,
	)
	if err != nil {
		return nil, err
	}
	return &row, nil
}

func listTimers(ctx context.Context, db *sql.DB, ownerL2Name, statusFilter string, limit int) ([]timerRow, error) {
	if limit <= 0 {
		limit = defaultListLimit
	}
	if limit > maxListLimit {
		limit = maxListLimit
	}
	if statusFilter == "" || statusFilter == "all" {
		rows, err := db.QueryContext(ctx, `
SELECT uuid, owner_l2_name, target_l2_name, client_ref, kind, fire_at_utc, cron_spec, cron_tz,
       missed_policy, missed_within_ms, payload, metadata, status, created_at_utc, last_fired_at_utc, fire_count
FROM timers
WHERE owner_l2_name=?
ORDER BY fire_at_utc ASC
LIMIT ?
`, ownerL2Name, limit)
		if err != nil {
			return nil, err
		}
		defer rows.Close()
		return collectTimerRows(rows)
	}

	rows, err := db.QueryContext(ctx, `
SELECT uuid, owner_l2_name, target_l2_name, client_ref, kind, fire_at_utc, cron_spec, cron_tz,
       missed_policy, missed_within_ms, payload, metadata, status, created_at_utc, last_fired_at_utc, fire_count
FROM timers
WHERE owner_l2_name=? AND status=?
ORDER BY fire_at_utc ASC
LIMIT ?
`, ownerL2Name, statusFilter, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return collectTimerRows(rows)
}

func listPendingTimers(ctx context.Context, db *sql.DB) ([]timerRow, error) {
	rows, err := db.QueryContext(ctx, `
SELECT uuid, owner_l2_name, target_l2_name, client_ref, kind, fire_at_utc, cron_spec, cron_tz,
       missed_policy, missed_within_ms, payload, metadata, status, created_at_utc, last_fired_at_utc, fire_count
FROM timers
WHERE status='pending'
ORDER BY fire_at_utc ASC
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return collectTimerRows(rows)
}

func countPendingTimers(ctx context.Context, db *sql.DB) (int64, error) {
	var count int64
	err := db.QueryRowContext(ctx, `SELECT COUNT(*) FROM timers WHERE status='pending'`).Scan(&count)
	return count, err
}

func gcHistoricalTimers(ctx context.Context, db *sql.DB, now time.Time, retention time.Duration) (int64, error) {
	if retention <= 0 {
		retention = defaultHistoryRetention
	}
	cutoff := now.UTC().Add(-retention).UnixMilli()
	result, err := db.ExecContext(ctx, `
DELETE FROM timers
WHERE status IN ('fired','canceled')
  AND COALESCE(last_fired_at_utc, created_at_utc) < ?
`, cutoff)
	if err != nil {
		return 0, err
	}
	return result.RowsAffected()
}

func timerRowToInfo(row timerRow) (fluxbeesdk.TimerInfo, error) {
	info := fluxbeesdk.TimerInfo{
		UUID:           row.UUID,
		OwnerL2Name:    row.OwnerL2Name,
		TargetL2Name:   row.TargetL2Name,
		Kind:           row.Kind,
		FireAtUTCMS:    row.FireAtUTC,
		MissedPolicy:   fluxbeesdk.MissedPolicy(row.MissedPolicy),
		Status:         row.Status,
		CreatedAtUTCMS: row.CreatedAtUTC,
		FireCount:      row.FireCount,
	}
	if row.ClientRef.Valid {
		value := row.ClientRef.String
		info.ClientRef = &value
	}
	if row.CronSpec.Valid {
		value := row.CronSpec.String
		info.CronSpec = &value
	}
	if row.CronTZ.Valid {
		value := row.CronTZ.String
		info.CronTZ = &value
	}
	if row.MissedWithinMS.Valid {
		value := row.MissedWithinMS.Int64
		info.MissedWithinMS = &value
	}
	if row.LastFiredAtUTC.Valid {
		value := row.LastFiredAtUTC.Int64
		info.LastFiredAtUTCMS = &value
	}
	payload, err := jsonRaw(row.Payload)
	if err != nil {
		return fluxbeesdk.TimerInfo{}, err
	}
	info.Payload = payload
	if row.Metadata.Valid {
		metadata, err := jsonRaw(row.Metadata.String)
		if err != nil {
			return fluxbeesdk.TimerInfo{}, err
		}
		info.Metadata = metadata
	}
	return info, nil
}

func collectTimerRows(rows *sql.Rows) ([]timerRow, error) {
	var out []timerRow
	for rows.Next() {
		var row timerRow
		if err := rows.Scan(
			&row.UUID, &row.OwnerL2Name, &row.TargetL2Name, &row.ClientRef, &row.Kind, &row.FireAtUTC, &row.CronSpec, &row.CronTZ,
			&row.MissedPolicy, &row.MissedWithinMS, &row.Payload, &row.Metadata, &row.Status, &row.CreatedAtUTC, &row.LastFiredAtUTC, &row.FireCount,
		); err != nil {
			return nil, err
		}
		out = append(out, row)
	}
	return out, rows.Err()
}

func validateTimerRow(row timerRow) error {
	if row.UUID == "" || row.OwnerL2Name == "" || row.TargetL2Name == "" || row.Kind == "" || row.MissedPolicy == "" || row.Status == "" {
		return fmt.Errorf("timer row missing required fields")
	}
	if row.Payload == "" {
		row.Payload = "{}"
	}
	if _, err := jsonRaw(row.Payload); err != nil {
		return fmt.Errorf("invalid payload json: %w", err)
	}
	if row.Metadata.Valid {
		if _, err := jsonRaw(row.Metadata.String); err != nil {
			return fmt.Errorf("invalid metadata json: %w", err)
		}
	}
	return nil
}

func jsonRaw(value string) (json.RawMessage, error) {
	raw := json.RawMessage(value)
	if !json.Valid(raw) {
		return nil, fmt.Errorf("invalid json")
	}
	return raw, nil
}

func nullableString(value sql.NullString) any {
	if value.Valid {
		return value.String
	}
	return nil
}

func nullableInt64(value sql.NullInt64) any {
	if value.Valid {
		return value.Int64
	}
	return nil
}
