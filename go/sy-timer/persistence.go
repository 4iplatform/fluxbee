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
	timerSchemaVersion      = 4
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
	// CanceledReason says why SY.timer canceled the timer itself (FINDINGS A-220): a one-shot its
	// missed policy dropped at a start, or a recurring timer whose next fire cannot be computed. Its
	// owner's TIMER_CANCEL leaves none.
	CanceledReason sql.NullString
	// CanceledAtUTC is when the timer was canceled, by its owner or by SY.timer (FINDINGS A-238): a
	// canceled timer is kept defaultHistoryRetention from it (gcHistoricalTimers). One canceled before
	// schema 4, or by a SY.timer before it, has none until the next start dates it.
	CanceledAtUTC sql.NullInt64
}

// timerColumns are a timer row's columns, in the order scanTimerRow reads them.
const timerColumns = `uuid, owner_l2_name, target_l2_name, client_ref, kind, fire_at_utc, cron_spec, cron_tz,
       missed_policy, missed_within_ms, payload, metadata, status, created_at_utc, last_fired_at_utc, fire_count,
       canceled_reason, canceled_at_utc`

// rowScanner is a *sql.Row or a *sql.Rows.
type rowScanner interface {
	Scan(dest ...any) error
}

// scanTimerRow reads a row selected with timerColumns.
func scanTimerRow(scanner rowScanner) (timerRow, error) {
	var row timerRow
	err := scanner.Scan(
		&row.UUID, &row.OwnerL2Name, &row.TargetL2Name, &row.ClientRef, &row.Kind, &row.FireAtUTC, &row.CronSpec, &row.CronTZ,
		&row.MissedPolicy, &row.MissedWithinMS, &row.Payload, &row.Metadata, &row.Status, &row.CreatedAtUTC, &row.LastFiredAtUTC, &row.FireCount,
		&row.CanceledReason, &row.CanceledAtUTC,
	)
	return row, err
}

// openTimerDB opens the database at path and makes its schema (ensureTimerSchema), and says what that
// did to the indexes, for the start's log (startupIndexes).
func openTimerDB(path string) (*sql.DB, timerIndexStep, error) {
	db, err := sql.Open("sqlite", timerDSN(path))
	if err != nil {
		return nil, timerIndexStep{}, err
	}
	indexes, err := ensureTimerSchema(db)
	if err != nil {
		_ = db.Close()
		return nil, indexes, err
	}
	return db, indexes, nil
}

// timerDSN is path plus the pragmas every connection needs. A pragma belongs to one connection and
// database/sql opens several (the scheduler writes while requests are served): in the DSN,
// modernc.org/sqlite applies them to each connection it opens. Set once with db.Exec they reached
// one connection only, and the others failed at once with SQLITE_BUSY instead of waiting.
func timerDSN(path string) string {
	return path + "?_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)"
}

// ensureTimerSchema makes the timers table, its indexes (ensureTimerIndexes) and its columns, and
// says what it did to the indexes. Each step works on a table it made before, and on a table any
// SY.timer before it made.
func ensureTimerSchema(db *sql.DB) (timerIndexStep, error) {
	var userVersion int
	if err := db.QueryRow(`PRAGMA user_version;`).Scan(&userVersion); err != nil {
		return timerIndexStep{}, err
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
    canceled_reason     TEXT,
    canceled_at_utc     INTEGER
);`); err != nil {
		return timerIndexStep{}, err
	}
	indexes, err := ensureTimerIndexes(db)
	if err != nil {
		return indexes, err
	}
	// Unlike the indexes above, this one is a rule: a requester's client_ref names one pending timer.
	if userVersion < 2 {
		if _, err := db.Exec(`
CREATE UNIQUE INDEX IF NOT EXISTS idx_timers_owner_clientref_pending
ON timers(owner_l2_name, client_ref)
WHERE client_ref IS NOT NULL AND status='pending';
`); err != nil {
			return indexes, fmt.Errorf("create unique pending client_ref index: %w", err)
		}
	}
	// Version 3 (FINDINGS A-220) adds canceled_reason, version 4 (FINDINGS A-238) canceled_at_utc.
	for _, column := range []struct{ name, definition string }{
		{name: "canceled_reason", definition: "TEXT"},
		{name: "canceled_at_utc", definition: "INTEGER"},
	} {
		if err := addTimerColumn(db, column.name, column.definition); err != nil {
			return indexes, err
		}
	}
	if _, err := db.Exec(fmt.Sprintf("PRAGMA user_version=%d;", timerSchemaVersion)); err != nil {
		return indexes, err
	}
	return indexes, nil
}

// timerIndexes are the indexes a start builds when the timers table lacks them (ensureTimerIndexes).
// Each only speeds reads up: every statement reads the same timers without it, more slowly.
// idx_timers_status is the one the hive's count of pending timers reads, which each new timer's cap
// check runs (checkPendingLimits): its entries are narrower than idx_timers_list_status's, so the
// count reads less, and the start's history GC reads it too. A TIMER_LIST page reads one of the four
// idx_timers_list*, in the list's order (fire time, then uuid), from its cursor on: one owner's
// timers in one status, one owner's in any, the hive's in one, the hive's in any (timerListQuery,
// since 1.7, FINDINGS A-326). The owner's count of pending timers and TIMER_PURGE_OWNER read them
// too (deleteTimersByOwner). An older SY.timer started on the table leaves the four as they are, and
// its lists read them too.
var timerIndexes = []struct{ name, columns string }{
	{name: "idx_timers_status", columns: "status"},
	{name: "idx_timers_list_owner_status", columns: "owner_l2_name, status, fire_at_utc, uuid"},
	{name: "idx_timers_list_owner", columns: "owner_l2_name, fire_at_utc, uuid"},
	{name: "idx_timers_list_status", columns: "status, fire_at_utc, uuid"},
	{name: "idx_timers_list", columns: "fire_at_utc, uuid"},
}

// retiredTimerIndexes are two indexes of SY.timer before 1.7 that no statement reads once the table
// has the four idx_timers_list*: idx_timers_owner, which TIMER_PURGE_OWNER's delete of an owner's
// timers read and idx_timers_list_owner serves as well, and idx_timers_fire_at, the pending timers by
// fire time, which idx_timers_list_status holds too. Kept, they cost every write of a timer, and a
// purge most: it deletes each of the owner's timers from every index (the review of A-326). An older
// SY.timer started on the table builds them again, and the next start of 1.7 drops them again.
var retiredTimerIndexes = []string{"idx_timers_owner", "idx_timers_fire_at"}

// timerIndexStep is what a start did to the timers table's indexes (ensureTimerIndexes), which its
// timer_startup event and its ready line give (startupIndexes): the indexes it built, the table
// lacking them; the retired ones it dropped; each one it could not build or drop, with the error;
// and how long that took.
type timerIndexStep struct {
	built   []string
	dropped []string
	failed  map[string]string
	took    time.Duration
}

// ensureTimerIndexes builds the indexes of timerIndexes the timers table lacks, and then, only once
// it has every one of them, drops the retired ones it still has (retiredTimerIndexes): a start that
// could not build an index leaves those as they were, for the reads they serve. An index it cannot
// build or drop goes in the step's failed, with the error, and the start goes on: without the index,
// whose reads are slower, or with a retired one, whose writes are; the next start tries again. Before,
// a CREATE INDEX that failed stopped the start, and systemd started SY.timer again every 5 s; the
// first start of 1.7 builds four indexes over the whole table, which a disk short of room can refuse
// (the review of A-326). The error it returns is one reading which indexes the table has.
func ensureTimerIndexes(db *sql.DB) (timerIndexStep, error) {
	started := time.Now()
	step := timerIndexStep{built: []string{}, dropped: []string{}, failed: map[string]string{}}
	has, err := timerIndexNames(db)
	if err != nil {
		return step, err
	}
	for _, index := range timerIndexes {
		if has[index.name] {
			continue
		}
		if _, err := db.Exec(`CREATE INDEX IF NOT EXISTS ` + index.name + ` ON timers(` + index.columns + `);`); err != nil {
			step.failed[index.name] = err.Error()
			continue
		}
		step.built = append(step.built, index.name)
	}
	if len(step.failed) == 0 {
		for _, name := range retiredTimerIndexes {
			if !has[name] {
				continue
			}
			if _, err := db.Exec(`DROP INDEX IF EXISTS ` + name + `;`); err != nil {
				step.failed[name] = err.Error()
				continue
			}
			step.dropped = append(step.dropped, name)
		}
	}
	step.took = time.Since(started)
	return step, nil
}

// timerIndexNames are the names of the timers table's indexes, as SQLite keeps them.
func timerIndexNames(db *sql.DB) (map[string]bool, error) {
	rows, err := db.Query(`SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='timers'`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	names := map[string]bool{}
	for rows.Next() {
		var name string
		if err := rows.Scan(&name); err != nil {
			return nil, err
		}
		names[name] = true
	}
	return names, rows.Err()
}

// addTimerColumn adds a nullable column to a timers table that lacks it, whatever user_version says:
// a SY.timer before the column writes its own user_version back over a table that has it. That
// SY.timer names every column it reads and writes, so it keeps working on the newer table, and its
// rows get no value in the column.
func addTimerColumn(db *sql.DB, name, definition string) error {
	var has int
	if err := db.QueryRow(`SELECT COUNT(*) FROM pragma_table_info('timers') WHERE name=?;`, name).Scan(&has); err != nil {
		return err
	}
	if has > 0 {
		return nil
	}
	if _, err := db.Exec(`ALTER TABLE timers ADD COLUMN ` + name + ` ` + definition + `;`); err != nil {
		return fmt.Errorf("add %s: %w", name, err)
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
INSERT INTO timers (`+timerColumns+`) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
`, row.UUID, row.OwnerL2Name, row.TargetL2Name, nullableString(row.ClientRef), row.Kind, row.FireAtUTC,
		nullableString(row.CronSpec), nullableString(row.CronTZ), row.MissedPolicy, nullableInt64(row.MissedWithinMS),
		row.Payload, nullableString(row.Metadata), row.Status, row.CreatedAtUTC, nullableInt64(row.LastFiredAtUTC), row.FireCount,
		nullableString(row.CanceledReason), nullableInt64(row.CanceledAtUTC))
	return err
}

func updateTimer(ctx context.Context, db *sql.DB, row timerRow) error {
	_, err := saveTimer(ctx, db, row, "")
	return err
}

// updateTimerIfStatus saves row only while the stored row still has status, the one it was read
// with, and reports whether it did. A row changed or deleted since (a purge, which takes no timer
// lock) is left as it is.
func updateTimerIfStatus(ctx context.Context, db *sql.DB, row timerRow, status string) (bool, error) {
	return saveTimer(ctx, db, row, status)
}

// saveTimer writes every column of row; with whileStatus set, only while the stored row has it. It
// reports whether it wrote the row.
func saveTimer(ctx context.Context, db *sql.DB, row timerRow, whileStatus string) (bool, error) {
	if err := validateTimerRow(row); err != nil {
		return false, err
	}
	query := `
UPDATE timers
SET owner_l2_name=?, target_l2_name=?, client_ref=?, kind=?, fire_at_utc=?, cron_spec=?, cron_tz=?,
    missed_policy=?, missed_within_ms=?, payload=?, metadata=?, status=?, created_at_utc=?, last_fired_at_utc=?, fire_count=?,
    canceled_reason=?, canceled_at_utc=?
WHERE uuid=?`
	args := []any{row.OwnerL2Name, row.TargetL2Name, nullableString(row.ClientRef), row.Kind, row.FireAtUTC,
		nullableString(row.CronSpec), nullableString(row.CronTZ), row.MissedPolicy, nullableInt64(row.MissedWithinMS),
		row.Payload, nullableString(row.Metadata), row.Status, row.CreatedAtUTC, nullableInt64(row.LastFiredAtUTC),
		row.FireCount, nullableString(row.CanceledReason), nullableInt64(row.CanceledAtUTC), row.UUID}
	if whileStatus != "" {
		query += " AND status=?"
		args = append(args, whileStatus)
	}
	result, err := db.ExecContext(ctx, query, args...)
	if err != nil {
		return false, err
	}
	written, err := result.RowsAffected()
	if err != nil {
		return false, err
	}
	return written == 1, nil
}

func getTimer(ctx context.Context, db *sql.DB, timerUUID string) (*timerRow, error) {
	row, err := scanTimerRow(db.QueryRowContext(ctx, `
SELECT `+timerColumns+`
FROM timers WHERE uuid=?
`, timerUUID))
	if err != nil {
		return nil, err
	}
	return &row, nil
}

func findPendingTimerByClientRef(ctx context.Context, db *sql.DB, ownerL2Name, clientRef string) (*timerRow, error) {
	row, err := scanTimerRow(db.QueryRowContext(ctx, `
SELECT `+timerColumns+`
FROM timers
WHERE owner_l2_name=? AND client_ref=? AND status='pending'
ORDER BY created_at_utc DESC
LIMIT 1
`, ownerL2Name, clientRef))
	if err != nil {
		return nil, err
	}
	return &row, nil
}

// timerListCursor is where a TIMER_LIST page ended: the last timer it listed, by fire time and uuid,
// the order a list keeps. The next page starts after it (FINDINGS A-151).
type timerListCursor struct {
	fireAtUTC int64
	uuid      string
}

// listTimers lists a TIMER_LIST page: the timers of an owner, or of every owner for "", in a status
// or in any ("all", or ""), by fire time and then uuid, after a cursor when there is one, each as
// TIMER_GET answers it (timerRowToInfo). The page ends at limit (defaultListLimit for none,
// maxListLimit at most) or where its timers would take more than maxBytes encoded, the frame's share
// (maxTimerListPageBytes, FINDINGS A-151), and says whether more follow its last timer
// (takeTimerPage). An error that carries TIMER_INTERNAL is a timer that cannot be answered; any other
// is the database's.
func listTimers(ctx context.Context, db *sql.DB, ownerL2Name, statusFilter string, limit, maxBytes int, after *timerListCursor) (listed []fluxbeesdk.TimerInfo, more bool, err error) {
	if limit <= 0 {
		limit = defaultListLimit
	}
	if limit > maxListLimit {
		limit = maxListLimit
	}
	query, args := timerListQuery(ownerL2Name, statusFilter, limit, after)
	rows, err := db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, false, err
	}
	defer rows.Close()
	return takeTimerPage(rows, limit, maxBytes)
}

// timerListQuery is the query of a TIMER_LIST page of up to limit timers. It selects one timer past
// the limit, never listed, to know whether more follow; before 1.6 it selected the limit only, and a
// list its limit cut read as the whole list (FINDINGS A-266). Each filter has an index that holds its
// timers in the list's order (idx_timers_list*, ensureTimerSchema), and the cursor is a row value,
// which SQLite searches that index for: a page starts at its place in the index and reads on in
// order, with nothing to sort. Before 1.7 a page of an owner's timers in one status searched
// idx_timers_status, read every timer of the hive in that status, every owner's, and sorted the
// owner's past its cursor; a page of any status read all of the owner's, or the whole table for the
// hive's, and sorted them; and the cursor was a filter on what it read (FINDINGS A-326).
func timerListQuery(ownerL2Name, statusFilter string, limit int, after *timerListCursor) (string, []any) {
	var conditions []string
	var args []any
	if ownerL2Name != "" {
		conditions = append(conditions, "owner_l2_name=?")
		args = append(args, ownerL2Name)
	}
	if statusFilter != "" && statusFilter != "all" {
		conditions = append(conditions, "status=?")
		args = append(args, statusFilter)
	}
	if after != nil {
		conditions = append(conditions, "(fire_at_utc, uuid) > (?, ?)")
		args = append(args, after.fireAtUTC, after.uuid)
	}
	query := `
SELECT ` + timerColumns + `
FROM timers`
	if len(conditions) > 0 {
		query += "\nWHERE " + strings.Join(conditions, " AND ")
	}
	query += "\nORDER BY fire_at_utc ASC, uuid ASC\nLIMIT ?"
	return query, append(args, limit+1)
}

// timerQueryRows are the rows of a query of timers, read one at a time: a *sql.Rows.
type timerQueryRows interface {
	rowScanner
	Next() bool
	Err() error
}

// takeTimerPage reads a TIMER_LIST page from rows, one timer at a time: up to limit timers, and no
// more than fit in maxBytes, encoded as the page lists them, its first whatever its size. The first
// timer it cannot take says that more follow its last one, and it reads no further: a page reads the
// timers it lists and one more, about 229 of a WF's timers where 228 fit a frame (FINDINGS A-326).
// Before 1.7 a page read limit+1 timers, up to 1001, and dropped those past the frame.
func takeTimerPage(rows timerQueryRows, limit, maxBytes int) (page []fluxbeesdk.TimerInfo, more bool, err error) {
	page = []fluxbeesdk.TimerInfo{}
	pageBytes := 0
	for rows.Next() {
		if len(page) == limit {
			return page, true, nil
		}
		row, err := scanTimerRow(rows)
		if err != nil {
			return nil, false, err
		}
		info, err := timerRowToInfo(row)
		if err != nil {
			return nil, false, &timerError{code: "TIMER_INTERNAL", err: err}
		}
		encoded, err := json.Marshal(info)
		if err != nil {
			return nil, false, &timerError{code: "TIMER_INTERNAL", err: err}
		}
		if len(page) > 0 && pageBytes+len(encoded)+1 > maxBytes {
			return page, true, nil
		}
		pageBytes += len(encoded) + 1
		page = append(page, info)
	}
	if err := rows.Err(); err != nil {
		return nil, false, err
	}
	return page, false, nil
}

func listPendingTimers(ctx context.Context, db *sql.DB) ([]timerRow, error) {
	rows, err := db.QueryContext(ctx, `
SELECT `+timerColumns+`
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

// deleteTimersByOwner deletes every timer of ownerL2Name, in any status, in one transaction, and
// returns how many, and the uuids of the pending ones among them: their entries leave the scheduler's
// heap (TIMER_PURGE_OWNER, FINDINGS A-259). The pending ones are deleted first, and only their uuids
// come back (RETURNING); the rest, the owner's history of up to 7 days, is deleted and counted, not
// read, as before 1.6. One statement that returned every deleted row read that whole history back,
// within the orchestrator's 3 s for the purge (the review of A-259). Each timer deleted leaves the
// table and every index, so each index the table has makes the purge longer (retiredTimerIndexes;
// its times in docs/sy-timer.md §7.7, the review of A-326).
func deleteTimersByOwner(ctx context.Context, db *sql.DB, ownerL2Name string) (deleted int64, pending []string, err error) {
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return 0, nil, err
	}
	defer func() { _ = tx.Rollback() }()
	rows, err := tx.QueryContext(ctx, deleteOwnerPendingTimersSQL, ownerL2Name)
	if err != nil {
		return 0, nil, err
	}
	if pending, err = collectUUIDs(rows); err != nil {
		return 0, nil, err
	}
	result, err := tx.ExecContext(ctx, deleteOwnerTimersSQL, ownerL2Name)
	if err != nil {
		return 0, nil, err
	}
	history, err := result.RowsAffected()
	if err != nil {
		return 0, nil, err
	}
	if err := tx.Commit(); err != nil {
		return 0, nil, err
	}
	return int64(len(pending)) + history, pending, nil
}

// The two statements of TIMER_PURGE_OWNER (deleteTimersByOwner): the owner's pending timers, whose
// uuids come back, then the rest. They find the owner's timers in idx_timers_list_owner_status and
// idx_timers_list_owner (timerIndexes).
const (
	deleteOwnerPendingTimersSQL = `DELETE FROM timers WHERE owner_l2_name=? AND status='pending' RETURNING uuid`
	deleteOwnerTimersSQL        = `DELETE FROM timers WHERE owner_l2_name=?`
)

// collectUUIDs reads the uuids rows returns, and closes them.
func collectUUIDs(rows *sql.Rows) ([]string, error) {
	defer rows.Close()
	var uuids []string
	for rows.Next() {
		var timerUUID string
		if err := rows.Scan(&timerUUID); err != nil {
			return nil, err
		}
		uuids = append(uuids, timerUUID)
	}
	return uuids, rows.Err()
}

func countPendingTimers(ctx context.Context, db *sql.DB) (int64, error) {
	var count int64
	err := db.QueryRowContext(ctx, `SELECT COUNT(*) FROM timers WHERE status='pending'`).Scan(&count)
	return count, err
}

func countPendingTimersByOwner(ctx context.Context, db *sql.DB, ownerL2Name string) (int64, error) {
	var count int64
	err := db.QueryRowContext(ctx, `SELECT COUNT(*) FROM timers WHERE owner_l2_name=? AND status='pending'`, ownerL2Name).Scan(&count)
	return count, err
}

// gcHistoricalTimers deletes the history older than retention, at a start (main): a fired timer by
// its last fire (its creation, for one that never fired), a canceled one by its cancellation
// (FINDINGS A-238). A canceled timer with no cancellation time, canceled before schema 4 or by a
// SY.timer before it, is dated now first, and kept retention from this start: stamped counts them.
// Before, a canceled timer went by its last fire or its creation, so one canceled yesterday whose
// creation was a week old was deleted, with its canceled_reason.
func gcHistoricalTimers(ctx context.Context, db *sql.DB, now time.Time, retention time.Duration) (deleted, stamped int64, err error) {
	if retention <= 0 {
		retention = defaultHistoryRetention
	}
	cutoff := now.UTC().Add(-retention).UnixMilli()
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return 0, 0, err
	}
	defer func() { _ = tx.Rollback() }()
	result, err := tx.ExecContext(ctx, `
UPDATE timers SET canceled_at_utc=?
WHERE status='canceled' AND canceled_at_utc IS NULL
`, now.UTC().UnixMilli())
	if err != nil {
		return 0, 0, err
	}
	if stamped, err = result.RowsAffected(); err != nil {
		return 0, 0, err
	}
	result, err = tx.ExecContext(ctx, `
DELETE FROM timers
WHERE (status='fired' AND COALESCE(last_fired_at_utc, created_at_utc) < ?)
   OR (status='canceled' AND canceled_at_utc < ?)
`, cutoff, cutoff)
	if err != nil {
		return 0, 0, err
	}
	if deleted, err = result.RowsAffected(); err != nil {
		return 0, 0, err
	}
	if err = tx.Commit(); err != nil {
		return 0, 0, err
	}
	return deleted, stamped, nil
}

// timerRowToInfo is row as TIMER_GET and TIMER_LIST answer it: the SDK's TimerInfo, with its
// canceled_reason when SY.timer canceled it itself (FINDINGS A-220), which the SDK's TimerInfo
// keeps since FINDINGS A-239.
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
	if row.CanceledReason.Valid {
		value := row.CanceledReason.String
		info.CanceledReason = &value
	}
	return info, nil
}

func collectTimerRows(rows *sql.Rows) ([]timerRow, error) {
	var out []timerRow
	for rows.Next() {
		row, err := scanTimerRow(rows)
		if err != nil {
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

// nullableNow is the instant now as a timer row keeps it: unix ms, UTC.
func nullableNow(now time.Time) sql.NullInt64 {
	return sql.NullInt64{Int64: now.UTC().UnixMilli(), Valid: true}
}

func isPendingClientRefUniqueViolation(err error) bool {
	if err == nil {
		return false
	}
	message := err.Error()
	return strings.Contains(message, "idx_timers_owner_clientref_pending") ||
		strings.Contains(message, "UNIQUE constraint failed: timers.owner_l2_name, timers.client_ref")
}
