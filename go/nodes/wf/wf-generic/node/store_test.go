package node

import (
	"context"
	"os"
	"path/filepath"
	"testing"
)

func openTestStore(t *testing.T) *Store {
	t.Helper()
	s, err := OpenStore(":memory:")
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	t.Cleanup(func() { _ = s.Close() })
	return s
}

func TestOpenStoreCreatesParentDirectory(t *testing.T) {
	base := t.TempDir()
	path := filepath.Join(base, "nested", "wf", "wf.db")

	if _, err := os.Stat(filepath.Dir(path)); !os.IsNotExist(err) {
		t.Fatalf("expected parent dir to be absent before OpenStore, err=%v", err)
	}

	store, err := OpenStore(path)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	t.Cleanup(func() { _ = store.Close() })

	if _, err := os.Stat(filepath.Dir(path)); err != nil {
		t.Fatalf("expected parent dir to exist after OpenStore: %v", err)
	}
	if _, err := os.Stat(path); err != nil {
		t.Fatalf("expected db file to exist after OpenStore: %v", err)
	}
}

// --- Instance roundtrip ---

func TestStoreInstanceRoundtrip(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()

	inst := WFInstanceRow{
		InstanceID:   "wfi:test-001",
		WorkflowType: "invoice",
		Status:       "running",
		CurrentState: "collecting_data",
		InputJSON:    `{"customer_id":"cust-1"}`,
		StateJSON:    `{}`,
		CreatedAtMS:  1776100000000,
		UpdatedAtMS:  1776100000000,
	}
	if err := s.CreateInstance(ctx, inst); err != nil {
		t.Fatalf("CreateInstance: %v", err)
	}

	got, err := s.GetInstance(ctx, inst.InstanceID)
	if err != nil {
		t.Fatalf("GetInstance: %v", err)
	}
	if got.InstanceID != inst.InstanceID {
		t.Errorf("InstanceID: got %q, want %q", got.InstanceID, inst.InstanceID)
	}
	if got.CurrentState != "collecting_data" {
		t.Errorf("CurrentState: got %q", got.CurrentState)
	}
}

func TestStoreUpdateInstance(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()

	inst := WFInstanceRow{
		InstanceID:   "wfi:test-002",
		WorkflowType: "invoice",
		Status:       "running",
		CurrentState: "collecting_data",
		InputJSON:    `{}`,
		StateJSON:    `{}`,
		CreatedAtMS:  1776100000000,
		UpdatedAtMS:  1776100000000,
	}
	if err := s.CreateInstance(ctx, inst); err != nil {
		t.Fatalf("CreateInstance: %v", err)
	}

	inst.Status = "completed"
	inst.CurrentState = "completed"
	inst.StateJSON = `{"validated_at":1776100001000}`
	inst.UpdatedAtMS = 1776100001000
	inst.TerminatedAtMS = newNullInt64(1776100001000)

	if err := s.UpdateInstance(ctx, inst); err != nil {
		t.Fatalf("UpdateInstance: %v", err)
	}

	got, err := s.GetInstance(ctx, inst.InstanceID)
	if err != nil {
		t.Fatalf("GetInstance: %v", err)
	}
	if got.Status != "completed" {
		t.Errorf("Status: got %q, want completed", got.Status)
	}
	if !got.TerminatedAtMS.Valid {
		t.Error("TerminatedAtMS should be set")
	}
}

func TestStoreLoadRunningInstances(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()

	base := WFInstanceRow{
		WorkflowType: "invoice",
		InputJSON:    `{}`,
		StateJSON:    `{}`,
		CreatedAtMS:  1776100000000,
		UpdatedAtMS:  1776100000000,
	}

	statuses := []struct {
		id     string
		status string
	}{
		{"wfi:r1", "running"},
		{"wfi:r2", "cancelling"},
		{"wfi:r3", "completed"},
	}
	for _, tc := range statuses {
		r := base
		r.InstanceID = tc.id
		r.Status = tc.status
		r.CurrentState = "s"
		if err := s.CreateInstance(ctx, r); err != nil {
			t.Fatalf("CreateInstance %s: %v", tc.id, err)
		}
	}

	running, err := s.LoadRunningInstances(ctx)
	if err != nil {
		t.Fatalf("LoadRunningInstances: %v", err)
	}
	if len(running) != 2 {
		t.Fatalf("expected 2 running instances, got %d", len(running))
	}
}

func TestStoreGetInstanceNotFound(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()
	_, err := s.GetInstance(ctx, "wfi:nonexistent")
	if err == nil {
		t.Fatal("expected error for nonexistent instance")
	}
}

// --- Log cap ---

func TestStoreLogCapEnforced(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()

	inst := WFInstanceRow{
		InstanceID:   "wfi:logtest",
		WorkflowType: "invoice",
		Status:       "running",
		CurrentState: "s",
		InputJSON:    `{}`,
		StateJSON:    `{}`,
		CreatedAtMS:  1776100000000,
		UpdatedAtMS:  1776100000000,
	}
	if err := s.CreateInstance(ctx, inst); err != nil {
		t.Fatalf("CreateInstance: %v", err)
	}

	// Insert 105 entries
	for i := 0; i < 105; i++ {
		entry := WFLogEntry{
			InstanceID: "wfi:logtest",
			LoggedAtMS: 1776100000000 + int64(i),
			ActionType: "send_message",
			Summary:    "msg",
			OK:         true,
		}
		if err := s.AppendLog(ctx, entry); err != nil {
			t.Fatalf("AppendLog #%d: %v", i, err)
		}
	}

	logs, err := s.GetRecentLog(ctx, "wfi:logtest", 200)
	if err != nil {
		t.Fatalf("GetRecentLog: %v", err)
	}
	if len(logs) != maxLogEntries {
		t.Fatalf("expected %d log entries after cap, got %d", maxLogEntries, len(logs))
	}
	// Most recent entry should be the last inserted (logged_at_ms = 1776100000104)
	if logs[0].LoggedAtMS != 1776100000104 {
		t.Errorf("newest entry: got logged_at_ms=%d, want 1776100000104", logs[0].LoggedAtMS)
	}
}

// --- GC ---

func TestStoreGCDeletesOldTerminated(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()

	base := WFInstanceRow{
		WorkflowType: "invoice",
		Status:       "completed",
		CurrentState: "completed",
		InputJSON:    `{}`,
		StateJSON:    `{}`,
		CreatedAtMS:  1000,
		UpdatedAtMS:  1000,
	}

	// Old terminated instance
	old := base
	old.InstanceID = "wfi:old"
	old.TerminatedAtMS = newNullInt64(1000)
	if err := s.CreateInstance(ctx, old); err != nil {
		t.Fatalf("CreateInstance old: %v", err)
	}

	// New terminated instance
	newInst := base
	newInst.InstanceID = "wfi:new"
	newInst.TerminatedAtMS = newNullInt64(9000)
	if err := s.CreateInstance(ctx, newInst); err != nil {
		t.Fatalf("CreateInstance new: %v", err)
	}

	// GC with cutoff=5000 — should delete old, keep new
	n, err := s.DeleteTerminatedBefore(ctx, 5000)
	if err != nil {
		t.Fatalf("DeleteTerminatedBefore: %v", err)
	}
	if n != 1 {
		t.Fatalf("expected 1 deleted, got %d", n)
	}

	_, err = s.GetInstance(ctx, "wfi:old")
	if err == nil {
		t.Error("wfi:old should be deleted")
	}
	_, err = s.GetInstance(ctx, "wfi:new")
	if err != nil {
		t.Errorf("wfi:new should still exist: %v", err)
	}
}

// --- Timer index ---

func TestStoreTimerIndexCRUD(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()

	if err := s.RegisterTimer(ctx, "wfi:t1", "invoice_timeout", 1000, 2000); err != nil {
		t.Fatalf("RegisterTimer: %v", err)
	}

	timers, err := s.ListTimersForInstance(ctx, "wfi:t1")
	if err != nil {
		t.Fatalf("ListTimersForInstance: %v", err)
	}
	if len(timers) != 1 || timers[0].TimerKey != "invoice_timeout" {
		t.Fatalf("unexpected timers: %v", timers)
	}

	if err := s.DeleteTimer(ctx, "wfi:t1", "invoice_timeout"); err != nil {
		t.Fatalf("DeleteTimer: %v", err)
	}

	timers, err = s.ListTimersForInstance(ctx, "wfi:t1")
	if err != nil {
		t.Fatalf("ListTimersForInstance after delete: %v", err)
	}
	if len(timers) != 0 {
		t.Fatalf("expected 0 timers after delete, got %d", len(timers))
	}
}

func TestStoreListAllExpectedTimers(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()

	if err := s.RegisterTimer(ctx, "wfi:a", "t1", 1000, 2000); err != nil {
		t.Fatalf("RegisterTimer: %v", err)
	}
	if err := s.RegisterTimer(ctx, "wfi:b", "t2", 1000, 3000); err != nil {
		t.Fatalf("RegisterTimer: %v", err)
	}

	all, err := s.ListAllExpectedTimers(ctx)
	if err != nil {
		t.Fatalf("ListAllExpectedTimers: %v", err)
	}
	if len(all) != 2 {
		t.Fatalf("expected 2 timers, got %d", len(all))
	}
}

func TestStoreTimerUpsertUpdatesFireAt(t *testing.T) {
	s := openTestStore(t)
	ctx := context.Background()

	if err := s.RegisterTimer(ctx, "wfi:u", "tk", 1000, 2000); err != nil {
		t.Fatalf("first RegisterTimer: %v", err)
	}
	// Reschedule: same key, new fire_at
	if err := s.RegisterTimer(ctx, "wfi:u", "tk", 1000, 5000); err != nil {
		t.Fatalf("second RegisterTimer: %v", err)
	}

	timers, _ := s.ListTimersForInstance(ctx, "wfi:u")
	if len(timers) != 1 || timers[0].FireAtMS != 5000 {
		t.Fatalf("expected fire_at_ms=5000, got %v", timers)
	}
}
