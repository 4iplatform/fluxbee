//go:build linux

package main

import (
	"context"
	"database/sql"
	"testing"
	"time"
)

// The scheduler's heap holds the pending timers only, one entry each, and the retries of fires on
// their way (FINDINGS A-259). Before, nothing took an entry out: a canceled, moved or purged timer's
// stayed until its old fire time, up to 366 days, and a node that looped a schedule and its cancel
// grew the heap without a bound the pending caps could see.

// queuedTimers checks the scheduler's heap and its index of timers, and returns each queued timer's
// fire time by uuid, and how many retries the heap holds: every entry is at its index, no entry fires
// before its parent, each timer's entry is the one its index names, and a retry is no timer's entry.
func queuedTimers(t *testing.T, s *timerScheduler) (timers map[string]int64, retries int) {
	t.Helper()
	s.mu.Lock()
	defer s.mu.Unlock()
	timers = map[string]int64{}
	for i, entry := range s.items {
		if entry.index != i {
			t.Fatalf("the entry at %d of the heap says it is at %d", i, entry.index)
		}
		if parent := (i - 1) / 2; i > 0 && s.items[parent].fireAtUTC > entry.fireAtUTC {
			t.Fatalf("the entry at %d of the heap fires before its parent", i)
		}
		if entry.retry != nil {
			if s.queued[entry.timerUUID] == entry {
				t.Fatalf("a retry of %s is in the index of timers", entry.timerUUID)
			}
			retries++
			continue
		}
		if _, twice := timers[entry.timerUUID]; twice {
			t.Fatalf("%s is in the heap twice", entry.timerUUID)
		}
		if s.queued[entry.timerUUID] != entry {
			t.Fatalf("%s is in the heap, and its index names another entry", entry.timerUUID)
		}
		timers[entry.timerUUID] = entry.fireAtUTC
	}
	if len(s.queued) != len(timers) {
		t.Fatalf("the index names %d timers, and the heap holds %d", len(s.queued), len(timers))
	}
	return timers, retries
}

// A TIMER_CANCEL takes its timer out of the heap, by uuid or by client_ref, and a TIMER_RESCHEDULE
// moves its entry: a node that loops a timer a year ahead and its cancel leaves nothing behind.
// Before, each cancel left its entry, and each reschedule added one.
func TestACanceledOrMovedTimerKeepsNoEntryOrOne(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	answer := func(verb string, request map[string]any) *string {
		t.Helper()
		if err := service.handleMessage(mustBuildSystemMessage(t, request, verb)); err != nil {
			t.Fatalf("%s: %v", verb, err)
		}
		response := mustReadTimerResponse(t, tx)
		if !response.OK {
			t.Fatalf("%s: %+v", verb, response.Error)
		}
		return response.TimerUUID
	}
	schedule := func(request map[string]any) string {
		t.Helper()
		timerUUID := answer("TIMER_SCHEDULE", request)
		if timerUUID == nil {
			t.Fatal("TIMER_SCHEDULE answered no timer_uuid")
		}
		return *timerUUID
	}
	yearAhead := (366 * 24 * time.Hour).Milliseconds()
	moved := schedule(map[string]any{"fire_in_ms": time.Hour.Milliseconds()})
	byUUID := schedule(map[string]any{"fire_in_ms": yearAhead})
	schedule(map[string]any{"fire_in_ms": yearAhead, "client_ref": "wf:demo::sla"})
	kept := schedule(map[string]any{"fire_in_ms": (2 * time.Hour).Milliseconds()})

	answer("TIMER_RESCHEDULE", map[string]any{"timer_uuid": moved, "new_fire_in_ms": (4 * time.Hour).Milliseconds()})
	movedTo := time.Now().UTC().Add(3 * time.Hour).UnixMilli()
	answer("TIMER_RESCHEDULE", map[string]any{"timer_uuid": moved, "new_fire_at_utc_ms": movedTo})
	answer("TIMER_CANCEL", map[string]any{"timer_uuid": byUUID})
	answer("TIMER_CANCEL", map[string]any{"client_ref": "wf:demo::sla"})
	for i := 0; i < 20; i++ {
		answer("TIMER_CANCEL", map[string]any{"timer_uuid": schedule(map[string]any{"fire_in_ms": yearAhead})})
	}

	timers, retries := queuedTimers(t, service.scheduler)
	stored, err := getTimer(context.Background(), service.db, kept)
	if err != nil {
		t.Fatalf("load %s: %v", kept, err)
	}
	if len(timers) != 2 || timers[moved] != movedTo || timers[kept] != stored.FireAtUTC || retries != 0 {
		t.Fatalf("the heap holds %v and %d retries, want %s at %d and %s at %d only", timers, retries, moved, movedTo, kept, stored.FireAtUTC)
	}
}

// A TIMER_PURGE_OWNER takes the owner's pending timers out of the heap, and only the owner's; its
// answer still counts every timer it deleted, its history included.
func TestAPurgedOwnersTimersLeaveTheHeap(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	ask := func(verb, sender string, request map[string]any) string {
		t.Helper()
		if err := service.handleMessage(mustBuildSystemMessageAs(t, request, verb, sender)); err != nil {
			t.Fatalf("%s: %v", verb, err)
		}
		response := mustReadTimerResponse(t, tx)
		if !response.OK || response.TimerUUID == nil {
			t.Fatalf("%s: %+v (error %+v)", verb, response, response.Error)
		}
		return *response.TimerUUID
	}
	ask("TIMER_SCHEDULE", "WF.demo@motherbee", map[string]any{"fire_in_ms": time.Hour.Milliseconds()})
	ask("TIMER_SCHEDULE_RECURRING", "WF.demo@motherbee", map[string]any{"cron_spec": "0 9 * * MON"})
	other := ask("TIMER_SCHEDULE", "AI.other@motherbee", map[string]any{"fire_in_ms": time.Hour.Milliseconds()})
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{UUID: "timer-fired", OwnerL2Name: "WF.demo@motherbee", TargetL2Name: "WF.demo@motherbee",
		Kind: "oneshot", FireAtUTC: now.Add(-time.Hour).UnixMilli(), MissedPolicy: "fire", Payload: `{}`, Status: "fired",
		CreatedAtUTC: now.Add(-2 * time.Hour).UnixMilli(), LastFiredAtUTC: sql.NullInt64{Int64: now.Add(-time.Hour).UnixMilli(), Valid: true}, FireCount: 1})
	if timers, _ := queuedTimers(t, service.scheduler); len(timers) != 3 {
		t.Fatalf("the heap holds %v, want the three pending timers", timers)
	}

	purge := mustBuildSystemMessageAs(t, map[string]any{"owner_l2_name": "WF.demo@motherbee"}, "TIMER_PURGE_OWNER", "SY.orchestrator@motherbee")
	if err := service.handleMessage(purge); err != nil {
		t.Fatalf("purge: %v", err)
	}
	if response := mustReadTimerResponse(t, tx); !response.OK || response.DeletedCount == nil || *response.DeletedCount != 3 {
		t.Fatalf("purge: %+v (error %+v), want 3 deleted", response, response.Error)
	}
	if timers, retries := queuedTimers(t, service.scheduler); len(timers) != 1 || timers[other] == 0 || retries != 0 {
		t.Fatalf("after the purge the heap holds %v and %d retries, want %s only", timers, retries, other)
	}
}

// A timer's entry leaves the heap with its fire, and a retry is no timer's entry: a one-shot that
// fired is no longer queued, while its fire waits to be sent again; a recurring timer is queued once,
// at its next fire, beside the retry of its last; canceled, it leaves the heap, and that retry is
// dropped at its time (retryFire), as before.
func TestAFireLeavesOneEntryPerPendingTimer(t *testing.T) {
	fastRetries(t)
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	ctx := context.Background()
	heapHolds := func(when string, wantTimers, wantRetries int) map[string]int64 {
		t.Helper()
		timers, retries := queuedTimers(t, service.scheduler)
		if len(timers) != wantTimers || retries != wantRetries {
			t.Fatalf("%s the heap holds %v and %d retries, want %d timers and %d retries", when, timers, retries, wantTimers, wantRetries)
		}
		return timers
	}

	insertDueTimer(t, service, "timer-oneshot", "fire")
	traceID := fireOnce(t, service, tx, "timer-oneshot")
	heapHolds("after a one-shot's fire", 0, 0)
	if err := service.handleMessage(unreachable(t, traceID, "NODE_NOT_FOUND")); err != nil {
		t.Fatalf("handle UNREACHABLE: %v", err)
	}
	heapHolds("while its fire waits to be sent again", 0, 1)
	service.scheduler.processDue(ctx, time.Now().UTC().Add(time.Second))
	if _, fired := mustReadFire(t, tx); fired.TimerUUID != "timer-oneshot" {
		t.Fatalf("the retry sent %+v", fired)
	}
	heapHolds("after its retry", 0, 0)

	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{UUID: "timer-recurring", OwnerL2Name: "WF.demo@motherbee", TargetL2Name: "WF.demo@motherbee",
		Kind: "recurring", FireAtUTC: now.Add(-time.Second).UnixMilli(), CronSpec: nullableStringValue("0 9 * * MON"),
		CronTZ: nullableStringValue("UTC"), MissedPolicy: "fire", Payload: `{}`, Status: "pending", CreatedAtUTC: now.Add(-time.Hour).UnixMilli()})
	traceID = fireOnce(t, service, tx, "timer-recurring")
	stored, err := getTimer(ctx, service.db, "timer-recurring")
	if err != nil {
		t.Fatalf("load timer: %v", err)
	}
	if timers := heapHolds("after a recurring timer's fire", 1, 0); timers["timer-recurring"] != stored.FireAtUTC {
		t.Fatalf("queued at %d, want its next fire, %d", timers["timer-recurring"], stored.FireAtUTC)
	}
	if err := service.handleMessage(unreachable(t, traceID, "NODE_NOT_FOUND")); err != nil {
		t.Fatalf("handle UNREACHABLE: %v", err)
	}
	heapHolds("while its last fire waits to be sent again", 1, 1)
	if err := service.handleMessage(mustBuildSystemMessage(t, map[string]any{"timer_uuid": "timer-recurring"}, "TIMER_CANCEL")); err != nil {
		t.Fatalf("cancel: %v", err)
	}
	if response := mustReadTimerResponse(t, tx); !response.OK {
		t.Fatalf("cancel: %+v", response.Error)
	}
	heapHolds("after its cancel", 0, 1)
	service.scheduler.processDue(ctx, time.Now().UTC().Add(time.Second))
	if len(tx) != 0 {
		t.Fatal("sent again the fire of a canceled timer")
	}
	heapHolds("after the retry's time", 0, 0)
}

// A TIMER_RESCHEDULE that lands while a fire waits for the timer's lock, after its entry left the
// heap, queues the timer at its new time, and the fire, which finds it moved later, keeps that one
// entry. Before, both queued it, and the timer was in the heap twice.
func TestATimerMovedWhileItsFireWaitsIsQueuedOnce(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	ctx := context.Background()
	insertDueTimer(t, service, "timer-moved", "fire")
	service.scheduler.push("timer-moved", time.Now().UTC().UnixMilli())

	held := service.scheduler.lockTimer("timer-moved")
	fired := make(chan struct{})
	go func() {
		defer close(fired)
		service.scheduler.processDue(ctx, time.Now().UTC())
	}()
	waitForTimerLockRefs(t, service.scheduler, "timer-moved", 2)
	// What TIMER_RESCHEDULE does under the lock (respondTimerReschedule).
	row, err := getTimer(ctx, service.db, "timer-moved")
	if err != nil {
		t.Fatalf("load timer: %v", err)
	}
	row.FireAtUTC = time.Now().UTC().Add(time.Hour).UnixMilli()
	if err := updateTimer(ctx, service.db, *row); err != nil {
		t.Fatalf("move timer: %v", err)
	}
	service.enqueueTimer(row.UUID, row.FireAtUTC)
	held()
	select {
	case <-fired:
	case <-time.After(5 * time.Second):
		t.Fatal("the fire did not end within 5 s")
	}

	if len(tx) != 0 {
		t.Fatal("fired a timer moved later")
	}
	if timers, retries := queuedTimers(t, service.scheduler); len(timers) != 1 || timers["timer-moved"] != row.FireAtUTC || retries != 0 {
		t.Fatalf("the heap holds %v and %d retries, want timer-moved at %d only", timers, retries, row.FireAtUTC)
	}
}

// A purge that lands while a fire holds its timer, after the fire read it, leaves no entry of the timer
// behind: TIMER_PURGE_OWNER takes no timer lock, and the fire queues a recurring timer's next fire
// before its save, which then finds the timer gone, and queues a timer it finds moved later before it
// reads it again; either way it takes the entry out itself. Its fire is not sent.
func TestAPurgeDuringAFireLeavesNoEntry(t *testing.T) {
	for _, tc := range []struct {
		name       string
		kind       string
		movedLater bool
	}{
		{name: "a recurring timer's fire", kind: "recurring"},
		{name: "a fire that finds its timer moved later", kind: "oneshot", movedLater: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			service, tx := newTestService(t)
			service.scheduler = newTimerScheduler(service)
			now := time.Now().UTC()
			row := timerRow{UUID: "timer-purged", OwnerL2Name: "WF.demo@motherbee", TargetL2Name: "WF.demo@motherbee", Kind: tc.kind,
				FireAtUTC: now.Add(-time.Second).UnixMilli(), MissedPolicy: "fire", Payload: `{}`, Status: "pending", CreatedAtUTC: now.Add(-time.Hour).UnixMilli()}
			if tc.kind == "recurring" {
				row.CronSpec, row.CronTZ = nullableStringValue("0 9 * * MON"), nullableStringValue("UTC")
			}
			if tc.movedLater {
				row.FireAtUTC = now.Add(time.Hour).UnixMilli()
			}
			insertOwnedTimer(t, service.db, row)
			// The fire reads the timer, and the purge lands, in the fire's goroutine, with the timer's
			// lock held: the purge takes none.
			purged := false
			afterFireRead = func(string) {
				purge := mustBuildSystemMessageAs(t, map[string]any{"owner_l2_name": "WF.demo@motherbee"}, "TIMER_PURGE_OWNER", "SY.orchestrator@motherbee")
				if err := service.handleMessage(purge); err != nil {
					t.Fatalf("purge: %v", err)
				}
				purged = true
			}
			t.Cleanup(func() { afterFireRead = nil })
			service.scheduler.push(row.UUID, now.Add(-time.Second).UnixMilli())
			service.scheduler.processDue(context.Background(), now)

			if !purged {
				t.Fatal("the fire did not read the timer")
			}
			if response := mustReadTimerResponse(t, tx); !response.OK || response.DeletedCount == nil || *response.DeletedCount != 1 {
				t.Fatalf("purge: %+v (error %+v)", response, response.Error)
			}
			if len(tx) != 0 {
				t.Fatal("sent the fire of a purged timer")
			}
			if timers, retries := queuedTimers(t, service.scheduler); len(timers) != 0 || retries != 0 {
				t.Fatalf("the purged timer left %v and %d retries in the heap", timers, retries)
			}
		})
	}
}

// A purge that lands right after a recurring timer's fire saved its next fire leaves no entry either:
// the fire queued that next fire before its save, and the purge takes the entry out. The fire, saved
// before the purge, is sent, as before. Before, the fire queued it after the save, and a purge between
// the two left it in the heap until that next fire, up to a year ahead.
func TestAPurgeRightAfterARecurringTimersSaveLeavesNoEntry(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{UUID: "timer-purged", OwnerL2Name: "WF.demo@motherbee", TargetL2Name: "WF.demo@motherbee",
		Kind: "recurring", FireAtUTC: now.Add(-time.Second).UnixMilli(), CronSpec: nullableStringValue("0 9 1 1 *"),
		CronTZ: nullableStringValue("UTC"), MissedPolicy: "fire", Payload: `{}`, Status: "pending", CreatedAtUTC: now.Add(-time.Hour).UnixMilli()})
	purged := false
	afterFireSave = func(string) {
		purge := mustBuildSystemMessageAs(t, map[string]any{"owner_l2_name": "WF.demo@motherbee"}, "TIMER_PURGE_OWNER", "SY.orchestrator@motherbee")
		if err := service.handleMessage(purge); err != nil {
			t.Fatalf("purge: %v", err)
		}
		purged = true
	}
	t.Cleanup(func() { afterFireSave = nil })
	service.scheduler.push("timer-purged", now.Add(-time.Second).UnixMilli())
	service.scheduler.processDue(context.Background(), now)

	if !purged {
		t.Fatal("the fire did not save the timer")
	}
	if response := mustReadTimerResponse(t, tx); !response.OK || response.DeletedCount == nil || *response.DeletedCount != 1 {
		t.Fatalf("purge: %+v (error %+v)", response, response.Error)
	}
	if _, fired := mustReadFire(t, tx); fired.TimerUUID != "timer-purged" {
		t.Fatalf("sent %+v", fired)
	}
	if timers, retries := queuedTimers(t, service.scheduler); len(timers) != 0 || retries != 0 {
		t.Fatalf("the purged timer left %v and %d retries in the heap", timers, retries)
	}
}
