package node

import (
	"context"
	"database/sql"
	"path/filepath"
	"testing"
	"time"

	_ "modernc.org/sqlite"
)

// WF-TEST-5 (internal event queue migration item).
//
// Validates that upgrading a database from schema v1 (no `wf_internal_events`
// table) to the current `wfSchemaVersion` adds the queue table and its index
// without touching pre-existing rows, and that after the migration the
// internal-event CRUD path works against rows enqueued for instances created
// before the migration.
//
// Builds a v1 DB by hand (only the tables that existed in v1), seeds an
// instance row, then calls `ensureWFSchema` to drive the migration in
// production code.
func TestEnsureWFSchemaMigratesV1ToCurrentAddsInternalEventQueue(t *testing.T) {
	dbPath := filepath.Join(t.TempDir(), "wf-legacy.db")
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open legacy db: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })

	// --- Build the v1 schema manually (no wf_internal_events). ---
	v1Stmts := []string{
		`CREATE TABLE wf_definitions (
    workflow_type  TEXT PRIMARY KEY,
    definition_json TEXT NOT NULL,
    hash           TEXT NOT NULL,
    loaded_at_ms   INTEGER NOT NULL
);`,
		`CREATE TABLE wf_instances (
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
		`CREATE TABLE wf_instance_log (
    log_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id TEXT NOT NULL,
    logged_at_ms INTEGER NOT NULL,
    action_type  TEXT NOT NULL,
    summary      TEXT NOT NULL,
    ok           INTEGER NOT NULL DEFAULT 1,
    error_detail TEXT NOT NULL DEFAULT ''
);`,
		`CREATE TABLE wf_instance_timers (
    instance_id    TEXT NOT NULL,
    timer_key      TEXT NOT NULL,
    scheduled_at_ms INTEGER NOT NULL,
    fire_at_ms     INTEGER NOT NULL,
    PRIMARY KEY (instance_id, timer_key)
);`,
		`PRAGMA user_version=1;`,
	}
	for _, stmt := range v1Stmts {
		if _, err := db.Exec(stmt); err != nil {
			t.Fatalf("v1 schema stmt failed: %v\n%s", err, stmt)
		}
	}

	// Seed a v1 instance row that must survive the migration.
	now := time.Now().UnixMilli()
	if _, err := db.Exec(`
INSERT INTO wf_instances
    (instance_id, workflow_type, status, current_state, input_json, state_json, current_trace_id, created_at_ms, updated_at_ms)
VALUES (?, ?, 'running', 'start', '{}', '{}', '', ?, ?);`,
		"legacy-instance-1", "wf.invoice", now, now); err != nil {
		t.Fatalf("seed legacy instance: %v", err)
	}

	// Sanity: the queue table must not exist yet in v1.
	if exists := tableExists(t, db, "wf_internal_events"); exists {
		t.Fatalf("v1 fixture leaked wf_internal_events — fixture is wrong")
	}

	// --- Drive the migration. ---
	if err := ensureWFSchema(db); err != nil {
		t.Fatalf("ensureWFSchema: %v", err)
	}

	// Post-migration assertions.
	var version int
	if err := db.QueryRow(`PRAGMA user_version;`).Scan(&version); err != nil {
		t.Fatalf("read user_version: %v", err)
	}
	if version != wfSchemaVersion {
		t.Fatalf("expected user_version=%d after migration, got %d", wfSchemaVersion, version)
	}

	// Queue table and its index must now exist.
	if !tableExists(t, db, "wf_internal_events") {
		t.Fatalf("migration did not create wf_internal_events")
	}
	if !indexExists(t, db, "idx_wfie_instance") {
		t.Fatalf("migration did not create idx_wfie_instance")
	}

	// Pre-migration data must be intact.
	var preserved string
	if err := db.QueryRow(`SELECT workflow_type FROM wf_instances WHERE instance_id = ?`,
		"legacy-instance-1").Scan(&preserved); err != nil {
		t.Fatalf("read preserved legacy instance: %v", err)
	}
	if preserved != "wf.invoice" {
		t.Fatalf("expected legacy instance to survive migration, got workflow_type=%q", preserved)
	}

	// After migration, the queue CRUD must work for the pre-existing instance.
	store := &Store{db: db}
	ctx := context.Background()
	enqueueAt := time.Now().UnixMilli()
	events := []InternalEventRow{
		{
			InstanceID:  "legacy-instance-1",
			MsgType:     "internal",
			MsgName:     "post_migration",
			PayloadJSON: `{"reason":"check_migration_path"}`,
			TraceID:     "trace-post-migration",
			CreatedAtMS: enqueueAt,
		},
	}
	if err := store.EnqueueInternalEvents(ctx, events); err != nil {
		t.Fatalf("EnqueueInternalEvents on legacy instance failed: %v", err)
	}
	listed, err := store.ListInternalEvents(ctx, "legacy-instance-1")
	if err != nil {
		t.Fatalf("ListInternalEvents: %v", err)
	}
	if len(listed) != 1 {
		t.Fatalf("expected 1 queued event after migration, got %d (%+v)", len(listed), listed)
	}
	if listed[0].MsgName != "post_migration" {
		t.Fatalf("unexpected event content: %+v", listed[0])
	}

	// Idempotent re-migration is a no-op.
	if err := ensureWFSchema(db); err != nil {
		t.Fatalf("idempotent re-migration failed: %v", err)
	}
	listed, err = store.ListInternalEvents(ctx, "legacy-instance-1")
	if err != nil {
		t.Fatalf("ListInternalEvents after re-migration: %v", err)
	}
	if len(listed) != 1 {
		t.Fatalf("re-migration must not drop queued events, got %d", len(listed))
	}
}

// WF-TEST-6 (crash mid-consumption transition item).
//
// Validates the recovery contract from spec §11.4 / wf_v1_tasks WF-TEST-6:
// when the runtime begins a consumption transition (picks an internal event,
// computes new state and possibly enqueues new events) but crashes BEFORE
// `CommitInstanceMutation` runs, the on-disk state must still be the
// pre-transition state — instance row untouched, queued event still present,
// no spurious new events inserted. The transition can then be retried on
// restart.
//
// We simulate the crash by:
//   1. Persisting the store to a file path (not `:memory:`)
//   2. Picking the event with `GetNextInternalEvent` (no DB write)
//   3. Computing the next instance row + planned mutation in-memory
//   4. NOT calling `CommitInstanceMutation` — closing the store as if the
//      process died
//   5. Reopening the store and asserting the pre-commit invariants
//   6. Retrying the commit with the same mutation and verifying it succeeds
//      exactly once.
func TestCrashDuringConsumptionTransitionLeavesQueueAndInstanceInPreCommitState(t *testing.T) {
	dbPath := filepath.Join(t.TempDir(), "wf-crash.db")

	store, err := OpenStore(dbPath)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	ctx := context.Background()

	inst := WFInstanceRow{
		InstanceID:     "wfi:crash",
		WorkflowType:   "invoice",
		Status:         "running",
		CurrentState:   "collecting_data",
		InputJSON:      `{}`,
		StateJSON:      `{"step":1}`,
		CurrentTraceID: "",
		CreatedAtMS:    1_000,
		UpdatedAtMS:    1_000,
	}
	if err := store.CreateInstance(ctx, inst); err != nil {
		t.Fatalf("CreateInstance: %v", err)
	}
	if err := store.EnqueueInternalEvents(ctx, []InternalEventRow{{
		InstanceID:  inst.InstanceID,
		MsgType:     "internal",
		MsgName:     "EV1",
		PayloadJSON: `{"value":"trigger"}`,
		TraceID:     "trace-ev1",
		CreatedAtMS: 1_500,
	}}); err != nil {
		t.Fatalf("EnqueueInternalEvents: %v", err)
	}

	// Begin a consumption transition: pick the event but do NOT commit yet.
	picked, ok, err := store.GetNextInternalEvent(ctx, inst.InstanceID)
	if err != nil {
		t.Fatalf("GetNextInternalEvent: %v", err)
	}
	if !ok {
		t.Fatalf("expected a queued event to be available")
	}
	if picked.MsgName != "EV1" {
		t.Fatalf("expected EV1, got %+v", picked)
	}

	// In-memory representation of the new instance + planned mutation
	// (state changes the runtime would compute but hasn't committed).
	plannedInst := inst
	plannedInst.CurrentState = "validated"
	plannedInst.StateJSON = `{"step":2}`
	plannedInst.CurrentTraceID = picked.TraceID
	plannedInst.UpdatedAtMS = 2_000
	plannedMutation := InstanceCommitMutation{
		ConsumedInternalEventID: &picked.EventID,
		EnqueueInternalEvents: []InternalEventRow{{
			InstanceID:  inst.InstanceID,
			MsgType:     "internal",
			MsgName:     "EV2",
			PayloadJSON: `{"value":"derived_from_ev1"}`,
			TraceID:     "trace-ev2",
			CreatedAtMS: 2_000,
		}},
	}

	// --- "Crash" before commit. ---
	if err := store.Close(); err != nil {
		t.Fatalf("close store (simulated crash): %v", err)
	}

	// --- Reopen the store after restart. ---
	store, err = OpenStore(dbPath)
	if err != nil {
		t.Fatalf("reopen store after crash: %v", err)
	}
	t.Cleanup(func() { _ = store.Close() })

	// Invariant 1: the instance row must be in its pre-commit state.
	gotInst, err := store.GetInstance(ctx, inst.InstanceID)
	if err != nil {
		t.Fatalf("GetInstance after restart: %v", err)
	}
	if gotInst.CurrentState != "collecting_data" {
		t.Fatalf("expected pre-commit state collecting_data, got %q", gotInst.CurrentState)
	}
	if gotInst.StateJSON != `{"step":1}` {
		t.Fatalf("expected pre-commit state_json, got %q", gotInst.StateJSON)
	}
	if gotInst.CurrentTraceID != "" {
		t.Fatalf("expected empty current_trace_id pre-commit, got %q", gotInst.CurrentTraceID)
	}
	if gotInst.UpdatedAtMS != 1_000 {
		t.Fatalf("expected pre-commit updated_at_ms=1000, got %d", gotInst.UpdatedAtMS)
	}

	// Invariant 2: the queued event must still be present (NOT consumed),
	// and no spurious EV2 must have been inserted.
	pending, err := store.ListInternalEvents(ctx, inst.InstanceID)
	if err != nil {
		t.Fatalf("ListInternalEvents after restart: %v", err)
	}
	if len(pending) != 1 {
		t.Fatalf("expected exactly 1 queued event (EV1) after crash, got %d: %+v", len(pending), pending)
	}
	if pending[0].MsgName != "EV1" {
		t.Fatalf("expected EV1 to remain queued, got %+v", pending[0])
	}
	if pending[0].EventID != picked.EventID {
		t.Fatalf("expected EV1 event_id to be stable across restart; got %d, want %d", pending[0].EventID, picked.EventID)
	}

	// Invariant 3: GetNextInternalEvent on restart returns the same EV1, so
	// the transition is retriable from the same starting point.
	repicked, ok, err := store.GetNextInternalEvent(ctx, inst.InstanceID)
	if err != nil {
		t.Fatalf("GetNextInternalEvent post-restart: %v", err)
	}
	if !ok || repicked.EventID != picked.EventID {
		t.Fatalf("expected EV1 to be re-picked on restart, got ok=%v event=%+v", ok, repicked)
	}

	// Retry the commit with the same mutation — recovery completes the
	// transition exactly once.
	if err := store.CommitInstanceMutation(ctx, plannedInst, plannedMutation); err != nil {
		t.Fatalf("CommitInstanceMutation on retry: %v", err)
	}

	finalInst, err := store.GetInstance(ctx, inst.InstanceID)
	if err != nil {
		t.Fatalf("GetInstance after retry commit: %v", err)
	}
	if finalInst.CurrentState != "validated" {
		t.Fatalf("expected post-commit state validated, got %q", finalInst.CurrentState)
	}
	if finalInst.CurrentTraceID != picked.TraceID {
		t.Fatalf("expected committed trace_id=%q, got %q", picked.TraceID, finalInst.CurrentTraceID)
	}
	finalPending, err := store.ListInternalEvents(ctx, inst.InstanceID)
	if err != nil {
		t.Fatalf("ListInternalEvents post-commit: %v", err)
	}
	if len(finalPending) != 1 || finalPending[0].MsgName != "EV2" {
		t.Fatalf("expected exactly EV2 queued after commit, got %+v", finalPending)
	}
}

func tableExists(t *testing.T, db *sql.DB, name string) bool {
	t.Helper()
	var found string
	err := db.QueryRow(`SELECT name FROM sqlite_master WHERE type='table' AND name = ?;`, name).Scan(&found)
	if err == sql.ErrNoRows {
		return false
	}
	if err != nil {
		t.Fatalf("inspect sqlite_master for table %q: %v", name, err)
	}
	return found == name
}

func indexExists(t *testing.T, db *sql.DB, name string) bool {
	t.Helper()
	var found string
	err := db.QueryRow(`SELECT name FROM sqlite_master WHERE type='index' AND name = ?;`, name).Scan(&found)
	if err == sql.ErrNoRows {
		return false
	}
	if err != nil {
		t.Fatalf("inspect sqlite_master for index %q: %v", name, err)
	}
	return found == name
}
