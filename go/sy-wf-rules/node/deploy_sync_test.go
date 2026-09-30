package node

import (
	"errors"
	"testing"
	"time"
)

func notSynced() error {
	return &orchestratorActionError{Status: "error", Code: "RUNTIME_NOT_AVAILABLE", Message: "version '0.0.2' not available"}
}

func TestUntilRuntimeSyncedRetriesUntilThePackageArrives(t *testing.T) {
	calls := 0
	err := untilRuntimeSynced(10*time.Second, func() error {
		calls++
		if calls < 3 {
			return notSynced()
		}
		return nil
	})
	if err != nil || calls != 3 {
		t.Fatalf("want success on the 3rd call, got err=%v calls=%d", err, calls)
	}
}

func TestUntilRuntimeSyncedDoesNotRetryOtherErrors(t *testing.T) {
	calls := 0
	other := &orchestratorActionError{Status: "error", Code: "FORBIDDEN", Message: "no"}
	err := untilRuntimeSynced(10*time.Second, func() error {
		calls++
		return other
	})
	if !errors.Is(err, other) || calls != 1 {
		t.Fatalf("want the first error back after 1 call, got err=%v calls=%d", err, calls)
	}
}

func TestUntilRuntimeSyncedGivesUpAtTheBudget(t *testing.T) {
	calls := 0
	started := time.Now()
	err := untilRuntimeSynced(1500*time.Millisecond, func() error {
		calls++
		return notSynced()
	})
	if !runtimeNotSyncedYet(err) {
		t.Fatalf("want the not-synced error back, got %v", err)
	}
	if elapsed := time.Since(started); elapsed > 3*time.Second || calls > 2 {
		t.Fatalf("budget not honored: %v elapsed, %d calls", elapsed, calls)
	}
}

func TestRuntimeNotSyncedYetCodes(t *testing.T) {
	for _, code := range []string{"RUNTIME_NOT_AVAILABLE", "RUNTIME_NOT_PRESENT", "BASE_RUNTIME_NOT_AVAILABLE"} {
		if !runtimeNotSyncedYet(&orchestratorActionError{Code: code}) {
			t.Fatalf("%s must count as not synced yet", code)
		}
	}
	if runtimeNotSyncedYet(errors.New("RUNTIME_NOT_AVAILABLE")) || runtimeNotSyncedYet(nil) {
		t.Fatal("only orchestrator refusals count")
	}
}
