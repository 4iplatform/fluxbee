package node

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	wfcel "github.com/4iplatform/json-router/pkg/wfcel"
	_ "modernc.org/sqlite"
)

const (
	// wfSchemaVersion 3 (0.1.90): a definition is kept by its hash for as long as a running instance
	// uses it, each instance records the hash it was born with, and an inbound event is journaled
	// until its transition commits (wf-orchestration-v1 §5.1).
	wfSchemaVersion = 3
	maxLogEntries   = 100
)

// WFInstanceRow is the persistent representation of a workflow instance.
type WFInstanceRow struct {
	InstanceID   string
	WorkflowType string
	// DefinitionHash is the hash of the definition the instance was born with, and runs on until it
	// ends (a redeploy does not move it).
	DefinitionHash string
	Status         string // running, cancelling, completed, cancelled, failed
	CurrentState   string
	InputJSON      string // JSON-encoded input map
	StateJSON      string // JSON-encoded state variable map
	CurrentTraceID string // kept empty: the inbound journal replays an event with its own trace_id
	CreatedAtMS    int64
	UpdatedAtMS    int64
	TerminatedAtMS sql.NullInt64
}

// WFLogEntry is a single entry in the per-instance audit log.
type WFLogEntry struct {
	LogID       int64
	InstanceID  string
	LoggedAtMS  int64
	ActionType  string
	Summary     string
	OK          bool
	ErrorDetail string
}

// TimerRow tracks a timer that a WF instance expects to exist in SY.timer.
type TimerRow struct {
	InstanceID    string
	TimerKey      string
	ScheduledAtMS int64
	FireAtMS      int64
}

// InternalEventRow is a queued synthetic event emitted by an instance and not yet consumed.
type InternalEventRow struct {
	EventID     int64
	InstanceID  string
	MsgType     string
	MsgName     string
	PayloadJSON string
	TraceID     string
	CreatedAtMS int64
	// Attempts counts the starts that ran it again (a leftover of a crash); live processing does not
	// count. At maxReplayAttempts its instance fails (causeReplayExhausted).
	Attempts int
}

// Inbound journal kinds: an event that creates an instance, or one an instance takes.
const (
	inboundCreate = "create"
	inboundEvent  = "event"
)

// InboundEventRow is an inbound event the engine took and has not finished: saved before it is
// processed and deleted in the commit of its transition, so a crash in between leaves it for
// recovery to process again (at least once, wf-orchestration-v1 §5.1).
type InboundEventRow struct {
	EventID      int64
	InstanceID   string // the instance it goes to, or the one it creates
	Kind         string // inboundCreate | inboundEvent
	MessageJSON  string // the message as received
	ReceivedAtMS int64
	// Attempts counts the starts that ran it again; live processing does not count. At
	// maxReplayAttempts it is not run again (causeReplayExhausted).
	Attempts int
}

type InstanceCommitMutation struct {
	ConsumedInternalEventID *int64
	ConsumedInboundEventID  *int64
	EnqueueInternalEvents   []InternalEventRow
	ClearInternalEvents     bool
}

// Store is the SQLite-backed persistence layer for a single WF node.
type Store struct {
	db *sql.DB
}

// OpenStore opens (or creates) the SQLite database at path and applies the schema.
func OpenStore(path string) (*Store, error) {
	if path != ":memory:" {
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			return nil, fmt.Errorf("create wf db dir for %q: %w", path, err)
		}
	}
	// A pragma belongs to one connection and database/sql opens several: in the DSN,
	// modernc.org/sqlite applies them to each connection it opens. Set once with db.Exec they
	// reached one connection only, and the others failed at once with SQLITE_BUSY instead of
	// waiting (as in sy-timer). synchronous stays SQLite's default, FULL, as in sy-timer: a commit is
	// on disk when it returns, so at least once holds across a host crash too.
	dsn := path + "?_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)"
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, fmt.Errorf("open wf db %q: %w", path, err)
	}
	if err := ensureWFSchema(db); err != nil {
		_ = db.Close()
		return nil, err
	}
	return &Store{db: db}, nil
}

// Close closes the underlying database.
func (s *Store) Close() error {
	return s.db.Close()
}

// wfSchemaStatements create every table and index at its current shape.
var wfSchemaStatements = []string{
	`CREATE TABLE IF NOT EXISTS wf_definitions (
    hash            TEXT PRIMARY KEY,
    workflow_type   TEXT NOT NULL,
    definition_json TEXT NOT NULL,
    loaded_at_ms    INTEGER NOT NULL
);`,
	`CREATE TABLE IF NOT EXISTS wf_instances (
    instance_id       TEXT PRIMARY KEY,
    workflow_type     TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'running'
                      CHECK(status IN ('running','cancelling','completed','cancelled','failed')),
    current_state     TEXT NOT NULL,
    input_json        TEXT NOT NULL DEFAULT '{}',
    state_json        TEXT NOT NULL DEFAULT '{}',
    current_trace_id  TEXT NOT NULL DEFAULT '',
    created_at_ms     INTEGER NOT NULL,
    updated_at_ms     INTEGER NOT NULL,
    terminated_at_ms  INTEGER,
    definition_hash   TEXT NOT NULL DEFAULT ''
);`,
	`CREATE INDEX IF NOT EXISTS idx_wfi_status ON wf_instances(status);`,
	`CREATE INDEX IF NOT EXISTS idx_wfi_created ON wf_instances(created_at_ms);`,
	`CREATE INDEX IF NOT EXISTS idx_wfi_definition ON wf_instances(definition_hash);`,
	`CREATE TABLE IF NOT EXISTS wf_instance_log (
    log_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id TEXT NOT NULL,
    logged_at_ms INTEGER NOT NULL,
    action_type  TEXT NOT NULL,
    summary      TEXT NOT NULL,
    ok           INTEGER NOT NULL DEFAULT 1,
    error_detail TEXT NOT NULL DEFAULT ''
);`,
	`CREATE INDEX IF NOT EXISTS idx_wfl_instance ON wf_instance_log(instance_id, log_id DESC);`,
	`CREATE TABLE IF NOT EXISTS wf_instance_timers (
    instance_id    TEXT NOT NULL,
    timer_key      TEXT NOT NULL,
    scheduled_at_ms INTEGER NOT NULL,
    fire_at_ms     INTEGER NOT NULL,
    PRIMARY KEY (instance_id, timer_key)
);`,
	`CREATE TABLE IF NOT EXISTS wf_internal_events (
    event_id       INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id    TEXT NOT NULL,
    msg_type       TEXT NOT NULL,
    msg_name       TEXT NOT NULL,
    payload_json   TEXT NOT NULL,
    trace_id       TEXT NOT NULL,
    created_at_ms  INTEGER NOT NULL,
    attempts       INTEGER NOT NULL DEFAULT 0
);`,
	`CREATE INDEX IF NOT EXISTS idx_wfie_instance ON wf_internal_events(instance_id, event_id ASC);`,
	`CREATE TABLE IF NOT EXISTS wf_inbound_events (
    event_id        INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id     TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK(kind IN ('create','event')),
    message_json    TEXT NOT NULL,
    received_at_ms  INTEGER NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0
);`,
}

func ensureWFSchema(db *sql.DB) error {
	var userVersion int
	if err := db.QueryRow(`PRAGMA user_version;`).Scan(&userVersion); err != nil {
		return err
	}
	if userVersion >= wfSchemaVersion {
		return nil
	}
	tx, err := db.Begin()
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback() }()
	if userVersion >= 1 && userVersion < 3 {
		if err := migrateToDefinitionsByHash(tx); err != nil {
			return fmt.Errorf("migrate wf schema to v3: %w", err)
		}
	}
	for _, stmt := range wfSchemaStatements {
		if _, err := tx.Exec(stmt); err != nil {
			return fmt.Errorf("create wf schema: %w", err)
		}
	}
	if _, err := tx.Exec(fmt.Sprintf(`PRAGMA user_version=%d;`, wfSchemaVersion)); err != nil {
		return err
	}
	return tx.Commit()
}

// migrateToDefinitionsByHash takes a v1 or v2 store to v3: its one definition per workflow type is
// kept by its hash, and each running instance records the hash of that definition, the one it ran on
// (the engine before v3 moved every running instance onto the definition it stored last); the
// internal-event queue counts its replays (attempts).
func migrateToDefinitionsByHash(tx *sql.Tx) error {
	stmts := []string{
		`ALTER TABLE wf_definitions RENAME TO wf_definitions_by_type;`,
		`CREATE TABLE wf_definitions (
    hash            TEXT PRIMARY KEY,
    workflow_type   TEXT NOT NULL,
    definition_json TEXT NOT NULL,
    loaded_at_ms    INTEGER NOT NULL
);`,
		`INSERT OR IGNORE INTO wf_definitions (hash, workflow_type, definition_json, loaded_at_ms)
SELECT hash, workflow_type, definition_json, loaded_at_ms FROM wf_definitions_by_type;`,
		`ALTER TABLE wf_instances ADD COLUMN definition_hash TEXT NOT NULL DEFAULT '';`,
		`UPDATE wf_instances SET definition_hash = COALESCE(
    (SELECT d.hash FROM wf_definitions_by_type d WHERE d.workflow_type = wf_instances.workflow_type), '')
WHERE status IN ('running', 'cancelling');`,
		`DROP TABLE wf_definitions_by_type;`,
	}
	for _, stmt := range stmts {
		if _, err := tx.Exec(stmt); err != nil {
			return err
		}
	}
	// A v2 store has the internal-event queue (v1 has not: it is created at its v3 shape).
	var queues int
	if err := tx.QueryRow(`SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'wf_internal_events';`).Scan(&queues); err != nil {
		return err
	}
	if queues > 0 {
		if _, err := tx.Exec(`ALTER TABLE wf_internal_events ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;`); err != nil {
			return err
		}
	}
	return nil
}

// --- Definitions ---

// DefinitionRow is a definition kept by its hash.
type DefinitionRow struct {
	Hash           string
	WorkflowType   string
	DefinitionJSON string
	LoadedAtMS     int64
}

// PutDefinition keeps a definition by its hash; loading the same one again refreshes loaded_at_ms.
func (s *Store) PutDefinition(ctx context.Context, row DefinitionRow) error {
	_, err := s.db.ExecContext(ctx, `
INSERT INTO wf_definitions (hash, workflow_type, definition_json, loaded_at_ms)
VALUES (?, ?, ?, ?)
ON CONFLICT(hash) DO UPDATE SET loaded_at_ms = excluded.loaded_at_ms;`,
		row.Hash, row.WorkflowType, row.DefinitionJSON, row.LoadedAtMS)
	return err
}

// GetDefinition returns the definition kept under hash, if any.
func (s *Store) GetDefinition(ctx context.Context, hash string) (DefinitionRow, bool, error) {
	var r DefinitionRow
	err := s.db.QueryRowContext(ctx, `
SELECT hash, workflow_type, definition_json, loaded_at_ms FROM wf_definitions WHERE hash = ?;`, hash).Scan(
		&r.Hash, &r.WorkflowType, &r.DefinitionJSON, &r.LoadedAtMS)
	if errors.Is(err, sql.ErrNoRows) {
		return r, false, nil
	}
	return r, err == nil, err
}

// PruneDefinitions drops every kept definition but current and those a running or cancelling
// instance still uses. It returns how many it dropped.
func (s *Store) PruneDefinitions(ctx context.Context, current string) (int64, error) {
	result, err := s.db.ExecContext(ctx, `
DELETE FROM wf_definitions
WHERE hash != ? AND hash NOT IN (
    SELECT definition_hash FROM wf_instances WHERE status IN ('running', 'cancelling')
);`, current)
	if err != nil {
		return 0, err
	}
	return result.RowsAffected()
}

// --- Instances ---

// errInstanceNotFound is GetInstance's error for an instance the store does not have.
var errInstanceNotFound = errors.New("not found")

const instanceColumns = `instance_id, workflow_type, definition_hash, status, current_state, input_json, state_json,
       current_trace_id, created_at_ms, updated_at_ms, terminated_at_ms`

// CreateInstance inserts a new workflow instance row.
func (s *Store) CreateInstance(ctx context.Context, inst WFInstanceRow) error {
	_, err := s.db.ExecContext(ctx, `
INSERT INTO wf_instances (`+instanceColumns+`)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);`, instanceValues(inst)...)
	return err
}

func instanceValues(inst WFInstanceRow) []any {
	return []any{
		inst.InstanceID, inst.WorkflowType, inst.DefinitionHash, inst.Status, inst.CurrentState,
		inst.InputJSON, inst.StateJSON, inst.CurrentTraceID,
		inst.CreatedAtMS, inst.UpdatedAtMS, inst.TerminatedAtMS,
	}
}

// GetInstance retrieves a single instance by ID.
func (s *Store) GetInstance(ctx context.Context, instanceID string) (WFInstanceRow, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT `+instanceColumns+` FROM wf_instances WHERE instance_id = ?;`, instanceID)
	if err != nil {
		return WFInstanceRow{}, err
	}
	defer rows.Close()
	found, err := scanInstances(rows)
	if err != nil {
		return WFInstanceRow{}, err
	}
	if len(found) == 0 {
		return WFInstanceRow{}, fmt.Errorf("instance %q %w", instanceID, errInstanceNotFound)
	}
	return found[0], nil
}

// CommitInstanceMutation atomically writes the instance row (an instance's first commit creates it,
// so a creation is one commit: its row, its internal events and its inbound event) with the
// consumption of the event its transition took and the internal-event queue changes.
func (s *Store) CommitInstanceMutation(ctx context.Context, inst WFInstanceRow, mutation InstanceCommitMutation) error {
	if mutation.ClearInternalEvents && len(mutation.EnqueueInternalEvents) > 0 {
		return fmt.Errorf("cannot enqueue internal events while clear_internal_events is set")
	}

	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback() }()

	if _, err := tx.ExecContext(ctx, `
INSERT INTO wf_instances (`+instanceColumns+`)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(instance_id) DO UPDATE SET
    status           = excluded.status,
    current_state    = excluded.current_state,
    state_json       = excluded.state_json,
    current_trace_id = excluded.current_trace_id,
    updated_at_ms    = excluded.updated_at_ms,
    terminated_at_ms = excluded.terminated_at_ms;`, instanceValues(inst)...); err != nil {
		return err
	}

	if mutation.ClearInternalEvents {
		if _, err := tx.ExecContext(ctx, `
DELETE FROM wf_internal_events WHERE instance_id = ?;`, inst.InstanceID); err != nil {
			return err
		}
	} else if mutation.ConsumedInternalEventID != nil {
		if _, err := tx.ExecContext(ctx, `
DELETE FROM wf_internal_events WHERE instance_id = ? AND event_id = ?;`,
			inst.InstanceID, *mutation.ConsumedInternalEventID); err != nil {
			return err
		}
	}
	if mutation.ConsumedInboundEventID != nil {
		// Every row of the instance up to the consumed one goes: a row an earlier failed commit left
		// came before this state, which already moved on, and must not run again at the next start.
		if _, err := tx.ExecContext(ctx, `
DELETE FROM wf_inbound_events WHERE instance_id = ? AND event_id <= ?;`,
			inst.InstanceID, *mutation.ConsumedInboundEventID); err != nil {
			return err
		}
	}

	for _, ev := range mutation.EnqueueInternalEvents {
		if _, err := tx.ExecContext(ctx, `
INSERT INTO wf_internal_events (instance_id, msg_type, msg_name, payload_json, trace_id, created_at_ms)
VALUES (?, ?, ?, ?, ?, ?);`,
			ev.InstanceID, ev.MsgType, ev.MsgName, ev.PayloadJSON, ev.TraceID, ev.CreatedAtMS); err != nil {
			return err
		}
	}

	if err := tx.Commit(); err != nil {
		return err
	}
	return nil
}

// FailInstance ends an instance as failed with no transition: what the engine does when it cannot
// run one (its definition is gone, or the transition and the failure handling both panicked). Its
// pending internal and inbound events go with it; its timers are the caller's.
func (s *Store) FailInstance(ctx context.Context, instanceID string, nowMS int64) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback() }()
	stmts := []struct {
		query string
		args  []any
	}{
		{`UPDATE wf_instances SET status = 'failed', current_trace_id = '', updated_at_ms = ?, terminated_at_ms = ?
WHERE instance_id = ?;`, []any{nowMS, nowMS, instanceID}},
		{`DELETE FROM wf_internal_events WHERE instance_id = ?;`, []any{instanceID}},
		{`DELETE FROM wf_inbound_events WHERE instance_id = ?;`, []any{instanceID}},
	}
	for _, stmt := range stmts {
		if _, err := tx.ExecContext(ctx, stmt.query, stmt.args...); err != nil {
			return err
		}
	}
	return tx.Commit()
}

// LoadRunningInstances returns all instances with status 'running' or 'cancelling'.
func (s *Store) LoadRunningInstances(ctx context.Context) ([]WFInstanceRow, error) {
	rows, err := s.db.QueryContext(ctx, `
SELECT `+instanceColumns+`
FROM wf_instances WHERE status IN ('running', 'cancelling');`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanInstances(rows)
}

// ListInstances returns instances with optional filtering.
func (s *Store) ListInstances(ctx context.Context, statusFilter string, limit int, createdAfterMS int64) ([]WFInstanceRow, error) {
	query := `
SELECT ` + instanceColumns + `
FROM wf_instances WHERE 1=1`
	args := []any{}
	if statusFilter != "" {
		query += ` AND status = ?`
		args = append(args, statusFilter)
	}
	if createdAfterMS > 0 {
		query += ` AND created_at_ms > ?`
		args = append(args, createdAfterMS)
	}
	query += ` ORDER BY created_at_ms DESC`
	if limit > 0 {
		query += ` LIMIT ?`
		args = append(args, limit)
	}
	rows, err := s.db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanInstances(rows)
}

func scanInstances(rows *sql.Rows) ([]WFInstanceRow, error) {
	var out []WFInstanceRow
	for rows.Next() {
		var r WFInstanceRow
		if err := rows.Scan(
			&r.InstanceID, &r.WorkflowType, &r.DefinitionHash, &r.Status, &r.CurrentState,
			&r.InputJSON, &r.StateJSON, &r.CurrentTraceID,
			&r.CreatedAtMS, &r.UpdatedAtMS, &r.TerminatedAtMS,
		); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

// --- Inbound journal ---

// JournalInbound saves an inbound event before the engine processes it, and returns its id. For a
// TIMER_FIRED, firedTimerKey names the fired timer: its index row goes in the same commit, so once
// the fire is taken a restart cannot fire it again (recovery sees no timer to recover).
func (s *Store) JournalInbound(ctx context.Context, row InboundEventRow, firedTimerKey string) (int64, error) {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return 0, err
	}
	defer func() { _ = tx.Rollback() }()
	result, err := tx.ExecContext(ctx, `
INSERT INTO wf_inbound_events (instance_id, kind, message_json, received_at_ms)
VALUES (?, ?, ?, ?);`, row.InstanceID, row.Kind, row.MessageJSON, row.ReceivedAtMS)
	if err != nil {
		return 0, err
	}
	id, err := result.LastInsertId()
	if err != nil {
		return 0, err
	}
	if firedTimerKey != "" {
		if _, err := tx.ExecContext(ctx, `
DELETE FROM wf_instance_timers WHERE instance_id = ? AND timer_key = ?;`, row.InstanceID, firedTimerKey); err != nil {
			return 0, err
		}
	}
	return id, tx.Commit()
}

// ListInboundEvents returns the inbound events not finished, oldest first.
func (s *Store) ListInboundEvents(ctx context.Context) ([]InboundEventRow, error) {
	rows, err := s.db.QueryContext(ctx, `
SELECT event_id, instance_id, kind, message_json, received_at_ms, attempts
FROM wf_inbound_events ORDER BY event_id ASC;`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []InboundEventRow
	for rows.Next() {
		var r InboundEventRow
		if err := rows.Scan(&r.EventID, &r.InstanceID, &r.Kind, &r.MessageJSON, &r.ReceivedAtMS, &r.Attempts); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

// CountInboundAttempt records, in its own commit, that a start runs the journaled event again.
func (s *Store) CountInboundAttempt(ctx context.Context, eventID int64) error {
	_, err := s.db.ExecContext(ctx, `UPDATE wf_inbound_events SET attempts = attempts + 1 WHERE event_id = ?;`, eventID)
	return err
}

// CountInternalAttempt records, in its own commit, that a start runs the queued internal event again.
func (s *Store) CountInternalAttempt(ctx context.Context, eventID int64) error {
	_, err := s.db.ExecContext(ctx, `UPDATE wf_internal_events SET attempts = attempts + 1 WHERE event_id = ?;`, eventID)
	return err
}

// DeleteInboundEvent drops an inbound event that no transition takes (one recovery cannot deliver).
func (s *Store) DeleteInboundEvent(ctx context.Context, eventID int64) error {
	_, err := s.db.ExecContext(ctx, `DELETE FROM wf_inbound_events WHERE event_id = ?;`, eventID)
	return err
}

// --- Instance Log ---

// AppendLog inserts a log entry and enforces the 100-entry cap (FIFO).
// Must be called under the per-instance mutex.
func (s *Store) AppendLog(ctx context.Context, entry WFLogEntry) error {
	okInt := 0
	if entry.OK {
		okInt = 1
	}
	if _, err := s.db.ExecContext(ctx, `
INSERT INTO wf_instance_log (instance_id, logged_at_ms, action_type, summary, ok, error_detail)
VALUES (?, ?, ?, ?, ?, ?);`,
		entry.InstanceID, entry.LoggedAtMS, entry.ActionType, entry.Summary, okInt, entry.ErrorDetail); err != nil {
		return err
	}
	// Trim oldest entries if cap exceeded
	_, err := s.db.ExecContext(ctx, `
DELETE FROM wf_instance_log
WHERE instance_id = ? AND log_id NOT IN (
    SELECT log_id FROM wf_instance_log
    WHERE instance_id = ?
    ORDER BY log_id DESC
    LIMIT ?
);`, entry.InstanceID, entry.InstanceID, maxLogEntries)
	return err
}

// GetRecentLog returns the most recent log entries for an instance, newest first.
func (s *Store) GetRecentLog(ctx context.Context, instanceID string, limit int) ([]WFLogEntry, error) {
	if limit <= 0 {
		limit = maxLogEntries
	}
	rows, err := s.db.QueryContext(ctx, `
SELECT log_id, instance_id, logged_at_ms, action_type, summary, ok, error_detail
FROM wf_instance_log
WHERE instance_id = ?
ORDER BY log_id DESC
LIMIT ?;`, instanceID, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []WFLogEntry
	for rows.Next() {
		var e WFLogEntry
		var okInt int
		if err := rows.Scan(&e.LogID, &e.InstanceID, &e.LoggedAtMS, &e.ActionType, &e.Summary, &okInt, &e.ErrorDetail); err != nil {
			return nil, err
		}
		e.OK = okInt != 0
		out = append(out, e)
	}
	return out, rows.Err()
}

// --- Internal Event Queue ---

// EnqueueInternalEvents appends durable internal events for an instance in FIFO order.
func (s *Store) EnqueueInternalEvents(ctx context.Context, events []InternalEventRow) error {
	if len(events) == 0 {
		return nil
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback() }()
	for _, ev := range events {
		if _, err := tx.ExecContext(ctx, `
INSERT INTO wf_internal_events (instance_id, msg_type, msg_name, payload_json, trace_id, created_at_ms)
VALUES (?, ?, ?, ?, ?, ?);`,
			ev.InstanceID, ev.MsgType, ev.MsgName, ev.PayloadJSON, ev.TraceID, ev.CreatedAtMS); err != nil {
			return err
		}
	}
	return tx.Commit()
}

// GetNextInternalEvent returns the oldest pending internal event for an instance.
func (s *Store) GetNextInternalEvent(ctx context.Context, instanceID string) (InternalEventRow, bool, error) {
	rows, err := s.queryInternalEvents(ctx, `
SELECT event_id, instance_id, msg_type, msg_name, payload_json, trace_id, created_at_ms, attempts
FROM wf_internal_events
WHERE instance_id = ?
ORDER BY event_id ASC
LIMIT 1;`, instanceID)
	if err != nil {
		return InternalEventRow{}, false, err
	}
	if len(rows) == 0 {
		return InternalEventRow{}, false, nil
	}
	return rows[0], true, nil
}

// ListInternalEvents returns all pending internal events for an instance in FIFO order.
func (s *Store) ListInternalEvents(ctx context.Context, instanceID string) ([]InternalEventRow, error) {
	return s.queryInternalEvents(ctx, `
SELECT event_id, instance_id, msg_type, msg_name, payload_json, trace_id, created_at_ms, attempts
FROM wf_internal_events
WHERE instance_id = ?
ORDER BY event_id ASC;`, instanceID)
}

// ListAllInternalEvents returns all pending internal events across instances.
func (s *Store) ListAllInternalEvents(ctx context.Context) ([]InternalEventRow, error) {
	return s.queryInternalEvents(ctx, `
SELECT event_id, instance_id, msg_type, msg_name, payload_json, trace_id, created_at_ms, attempts
FROM wf_internal_events
ORDER BY instance_id ASC, event_id ASC;`)
}

// DeleteInternalEvent removes a queued internal event by id.
func (s *Store) DeleteInternalEvent(ctx context.Context, instanceID string, eventID int64) error {
	_, err := s.db.ExecContext(ctx, `
DELETE FROM wf_internal_events WHERE instance_id = ? AND event_id = ?;`,
		instanceID, eventID)
	return err
}

func (s *Store) queryInternalEvents(ctx context.Context, query string, args ...any) ([]InternalEventRow, error) {
	rows, err := s.db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []InternalEventRow
	for rows.Next() {
		var r InternalEventRow
		if err := rows.Scan(
			&r.EventID,
			&r.InstanceID,
			&r.MsgType,
			&r.MsgName,
			&r.PayloadJSON,
			&r.TraceID,
			&r.CreatedAtMS,
			&r.Attempts,
		); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

// --- Timer Index ---

// RegisterTimer records a timer that this WF instance has scheduled with SY.timer.
func (s *Store) RegisterTimer(ctx context.Context, instanceID, timerKey string, scheduledAtMS, fireAtMS int64) error {
	_, err := s.db.ExecContext(ctx, `
INSERT INTO wf_instance_timers (instance_id, timer_key, scheduled_at_ms, fire_at_ms)
VALUES (?, ?, ?, ?)
ON CONFLICT(instance_id, timer_key) DO UPDATE SET
    scheduled_at_ms = excluded.scheduled_at_ms,
    fire_at_ms      = excluded.fire_at_ms;`,
		instanceID, timerKey, scheduledAtMS, fireAtMS)
	return err
}

// DeleteTimer removes a timer entry from the local index.
func (s *Store) DeleteTimer(ctx context.Context, instanceID, timerKey string) error {
	_, err := s.db.ExecContext(ctx, `
DELETE FROM wf_instance_timers WHERE instance_id = ? AND timer_key = ?;`,
		instanceID, timerKey)
	return err
}

// ListTimersForInstance returns all timer entries for a given instance (used at termination).
func (s *Store) ListTimersForInstance(ctx context.Context, instanceID string) ([]TimerRow, error) {
	return s.queryTimers(ctx, `
SELECT instance_id, timer_key, scheduled_at_ms, fire_at_ms
FROM wf_instance_timers WHERE instance_id = ?;`, instanceID)
}

// GetTimerForInstance returns the timer row for a specific instance/timer_key.
func (s *Store) GetTimerForInstance(ctx context.Context, instanceID, timerKey string) (TimerRow, bool, error) {
	rows, err := s.queryTimers(ctx, `
SELECT instance_id, timer_key, scheduled_at_ms, fire_at_ms
FROM wf_instance_timers
WHERE instance_id = ? AND timer_key = ?;`, instanceID, timerKey)
	if err != nil {
		return TimerRow{}, false, err
	}
	if len(rows) == 0 {
		return TimerRow{}, false, nil
	}
	return rows[0], true, nil
}

// ListAllExpectedTimers returns all timer entries across all instances, in the order they fire (used
// at boot recovery, which fires the missed ones in that order).
func (s *Store) ListAllExpectedTimers(ctx context.Context) ([]TimerRow, error) {
	return s.queryTimers(ctx, `
SELECT instance_id, timer_key, scheduled_at_ms, fire_at_ms FROM wf_instance_timers
ORDER BY fire_at_ms, instance_id, timer_key;`)
}

func (s *Store) queryTimers(ctx context.Context, query string, args ...any) ([]TimerRow, error) {
	rows, err := s.db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []TimerRow
	for rows.Next() {
		var r TimerRow
		if err := rows.Scan(&r.InstanceID, &r.TimerKey, &r.ScheduledAtMS, &r.FireAtMS); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

// --- GC ---

// DeleteTerminatedBefore deletes instances (and their logs and timers) terminated before cutoffMS.
// Returns the number of instances deleted.
func (s *Store) DeleteTerminatedBefore(ctx context.Context, cutoffMS int64) (int, error) {
	rows, err := s.db.QueryContext(ctx, `
SELECT instance_id FROM wf_instances
WHERE terminated_at_ms IS NOT NULL AND terminated_at_ms < ?;`, cutoffMS)
	if err != nil {
		return 0, err
	}
	var ids []string
	for rows.Next() {
		var id string
		if err := rows.Scan(&id); err != nil {
			rows.Close()
			return 0, err
		}
		ids = append(ids, id)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return 0, err
	}
	for _, id := range ids {
		if _, err := s.db.ExecContext(ctx, `DELETE FROM wf_instance_log WHERE instance_id = ?;`, id); err != nil {
			return 0, err
		}
		if _, err := s.db.ExecContext(ctx, `DELETE FROM wf_instance_timers WHERE instance_id = ?;`, id); err != nil {
			return 0, err
		}
		if _, err := s.db.ExecContext(ctx, `DELETE FROM wf_internal_events WHERE instance_id = ?;`, id); err != nil {
			return 0, err
		}
		if _, err := s.db.ExecContext(ctx, `DELETE FROM wf_inbound_events WHERE instance_id = ?;`, id); err != nil {
			return 0, err
		}
		if _, err := s.db.ExecContext(ctx, `DELETE FROM wf_instances WHERE instance_id = ?;`, id); err != nil {
			return 0, err
		}
	}
	return len(ids), nil
}

// --- Helpers ---

// newNullInt64 creates a valid sql.NullInt64.
func newNullInt64(v int64) sql.NullInt64 {
	return sql.NullInt64{Int64: v, Valid: true}
}

// marshalJSON writes a value the instance holds (its input, its state, a payload) as JSON that
// unmarshalJSON reads back with the same types (wfcel.EncodeJSON: a double keeps its decimal point).
func marshalJSON(v any) (string, error) {
	if v == nil {
		return "{}", nil
	}
	b, err := wfcel.EncodeJSON(v)
	if err != nil {
		return "", err
	}
	return string(b), nil
}

// unmarshalJSON reads a stored map back as the engine holds every value, live and after a restart
// (wfcel.DecodeJSON): an integer is an int64 and any other number a float64. Before 0.1.90 a value
// was a float64 live and an int64 restored, so a guard could work after a restart and fail before it
// (FINDINGS A-118): cel-go compares and equates an int with a double, but fails arithmetic that
// mixes them and `%` on a double.
func unmarshalJSON(s string) (map[string]any, error) {
	if s == "" || s == "{}" {
		return map[string]any{}, nil
	}
	return wfcel.DecodeJSONObject([]byte(s))
}

// nowMS returns the current time in Unix milliseconds using the given clock.
func nowMS(clock ClockFunc) int64 {
	if clock == nil {
		return time.Now().UnixMilli()
	}
	return clock().UnixMilli()
}
