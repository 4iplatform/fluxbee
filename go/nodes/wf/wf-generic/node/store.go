package node

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	_ "modernc.org/sqlite"
)

const (
	wfSchemaVersion = 1
	maxLogEntries   = 100
)

// WFInstanceRow is the persistent representation of a workflow instance.
type WFInstanceRow struct {
	InstanceID     string
	WorkflowType   string
	Status         string // running, cancelling, completed, cancelled, failed
	CurrentState   string
	InputJSON      string // JSON-encoded input map
	StateJSON      string // JSON-encoded state variable map
	CurrentTraceID string // trace_id of the currently-executing event (for crash recovery)
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

// Store is the SQLite-backed persistence layer for a single WF node.
type Store struct {
	db *sql.DB
}

// OpenStore opens (or creates) the SQLite database at path and applies the schema.
func OpenStore(path string) (*Store, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("open wf db %q: %w", path, err)
	}
	if _, err := db.Exec(`PRAGMA journal_mode=WAL;`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("set WAL mode: %w", err)
	}
	if _, err := db.Exec(`PRAGMA synchronous=NORMAL;`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("set synchronous: %w", err)
	}
	if _, err := db.Exec(`PRAGMA busy_timeout=5000;`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("set busy_timeout: %w", err)
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

func ensureWFSchema(db *sql.DB) error {
	var userVersion int
	if err := db.QueryRow(`PRAGMA user_version;`).Scan(&userVersion); err != nil {
		return err
	}
	if userVersion >= wfSchemaVersion {
		return nil
	}

	stmts := []string{
		`CREATE TABLE IF NOT EXISTS wf_definitions (
    workflow_type  TEXT PRIMARY KEY,
    definition_json TEXT NOT NULL,
    hash           TEXT NOT NULL,
    loaded_at_ms   INTEGER NOT NULL
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
    terminated_at_ms  INTEGER
);`,
		`CREATE INDEX IF NOT EXISTS idx_wfi_status ON wf_instances(status);`,
		`CREATE INDEX IF NOT EXISTS idx_wfi_created ON wf_instances(created_at_ms);`,
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
	}

	for _, stmt := range stmts {
		if _, err := db.Exec(stmt); err != nil {
			return fmt.Errorf("create wf schema: %w", err)
		}
	}

	if _, err := db.Exec(fmt.Sprintf(`PRAGMA user_version=%d;`, wfSchemaVersion)); err != nil {
		return err
	}
	return nil
}

// --- Definitions ---

// UpsertDefinition inserts or replaces the stored definition for a workflow type.
func (s *Store) UpsertDefinition(ctx context.Context, workflowType, definitionJSON, hash string, clock ClockFunc) error {
	now := clock().UnixMilli()
	_, err := s.db.ExecContext(ctx, `
INSERT INTO wf_definitions (workflow_type, definition_json, hash, loaded_at_ms)
VALUES (?, ?, ?, ?)
ON CONFLICT(workflow_type) DO UPDATE SET
    definition_json = excluded.definition_json,
    hash            = excluded.hash,
    loaded_at_ms    = excluded.loaded_at_ms;`,
		workflowType, definitionJSON, hash, now)
	return err
}

type DefinitionRow struct {
	WorkflowType   string
	DefinitionJSON string
	Hash           string
	LoadedAtMS     int64
}

// LoadAllDefinitions returns all stored workflow definitions.
func (s *Store) LoadAllDefinitions(ctx context.Context) ([]DefinitionRow, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT workflow_type, definition_json, hash, loaded_at_ms FROM wf_definitions;`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []DefinitionRow
	for rows.Next() {
		var r DefinitionRow
		if err := rows.Scan(&r.WorkflowType, &r.DefinitionJSON, &r.Hash, &r.LoadedAtMS); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

// --- Instances ---

// CreateInstance inserts a new workflow instance row.
func (s *Store) CreateInstance(ctx context.Context, inst WFInstanceRow) error {
	_, err := s.db.ExecContext(ctx, `
INSERT INTO wf_instances
    (instance_id, workflow_type, status, current_state, input_json, state_json, current_trace_id, created_at_ms, updated_at_ms, terminated_at_ms)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?);`,
		inst.InstanceID, inst.WorkflowType, inst.Status, inst.CurrentState,
		inst.InputJSON, inst.StateJSON, inst.CurrentTraceID,
		inst.CreatedAtMS, inst.UpdatedAtMS, inst.TerminatedAtMS)
	return err
}

// GetInstance retrieves a single instance by ID.
func (s *Store) GetInstance(ctx context.Context, instanceID string) (WFInstanceRow, error) {
	var r WFInstanceRow
	err := s.db.QueryRowContext(ctx, `
SELECT instance_id, workflow_type, status, current_state, input_json, state_json, current_trace_id,
       created_at_ms, updated_at_ms, terminated_at_ms
FROM wf_instances WHERE instance_id = ?;`, instanceID).Scan(
		&r.InstanceID, &r.WorkflowType, &r.Status, &r.CurrentState,
		&r.InputJSON, &r.StateJSON, &r.CurrentTraceID,
		&r.CreatedAtMS, &r.UpdatedAtMS, &r.TerminatedAtMS)
	if err == sql.ErrNoRows {
		return r, fmt.Errorf("instance %q not found", instanceID)
	}
	return r, err
}

// UpdateInstance persists changed fields for an existing instance.
func (s *Store) UpdateInstance(ctx context.Context, inst WFInstanceRow) error {
	_, err := s.db.ExecContext(ctx, `
UPDATE wf_instances SET
    status           = ?,
    current_state    = ?,
    state_json       = ?,
    current_trace_id = ?,
    updated_at_ms    = ?,
    terminated_at_ms = ?
WHERE instance_id = ?;`,
		inst.Status, inst.CurrentState, inst.StateJSON, inst.CurrentTraceID,
		inst.UpdatedAtMS, inst.TerminatedAtMS, inst.InstanceID)
	return err
}

// LoadRunningInstances returns all instances with status 'running' or 'cancelling'.
func (s *Store) LoadRunningInstances(ctx context.Context) ([]WFInstanceRow, error) {
	rows, err := s.db.QueryContext(ctx, `
SELECT instance_id, workflow_type, status, current_state, input_json, state_json, current_trace_id,
       created_at_ms, updated_at_ms, terminated_at_ms
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
SELECT instance_id, workflow_type, status, current_state, input_json, state_json, current_trace_id,
       created_at_ms, updated_at_ms, terminated_at_ms
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
			&r.InstanceID, &r.WorkflowType, &r.Status, &r.CurrentState,
			&r.InputJSON, &r.StateJSON, &r.CurrentTraceID,
			&r.CreatedAtMS, &r.UpdatedAtMS, &r.TerminatedAtMS,
		); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, rows.Err()
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

// ListAllExpectedTimers returns all timer entries across all instances (used at boot recovery).
func (s *Store) ListAllExpectedTimers(ctx context.Context) ([]TimerRow, error) {
	return s.queryTimers(ctx, `
SELECT instance_id, timer_key, scheduled_at_ms, fire_at_ms FROM wf_instance_timers;`)
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

// marshalJSON is a convenience wrapper used by callers that need to persist maps.
func marshalJSON(v any) (string, error) {
	if v == nil {
		return "{}", nil
	}
	b, err := json.Marshal(v)
	if err != nil {
		return "", err
	}
	return string(b), nil
}

// unmarshalJSON is a convenience wrapper used by callers that need to restore maps.
func unmarshalJSON(s string) (map[string]any, error) {
	if s == "" || s == "{}" {
		return map[string]any{}, nil
	}
	var m map[string]any
	if err := json.Unmarshal([]byte(s), &m); err != nil {
		return nil, err
	}
	return m, nil
}

// nowMS returns the current time in Unix milliseconds using the given clock.
func nowMS(clock ClockFunc) int64 {
	if clock == nil {
		return time.Now().UnixMilli()
	}
	return clock().UnixMilli()
}
