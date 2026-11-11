//go:build linux

package main

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"log"
	"os"
	"strings"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

func TestReplayPendingQueuesMissedOneShotWithFirePolicy(t *testing.T) {
	service, tx := newTestService(t)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-due",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    now.Add(-2 * time.Minute).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{"ticket_id":1234}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-10 * time.Minute).UnixMilli(),
	})

	scheduler := newTimerScheduler(service)
	if err := scheduler.replayPending(context.Background(), now); err != nil {
		t.Fatalf("replay pending: %v", err)
	}
	// The replay queues the fire: it goes when the scheduler runs, not before the node serves.
	if len(tx) != 0 {
		t.Fatalf("the replay sent the fire before the scheduler ran")
	}
	scheduler.processDue(context.Background(), now)

	frame := mustReadSentFrame(t, tx)
	var msg fluxbeesdk.Message
	if err := json.Unmarshal(frame, &msg); err != nil {
		t.Fatalf("decode fired event: %v", err)
	}
	if msg.Meta.Msg == nil || *msg.Meta.Msg != fluxbeesdk.MsgTimerFired {
		t.Fatalf("unexpected fired msg: %+v", msg.Meta)
	}

	row, err := getTimer(context.Background(), service.db, "timer-due")
	if err != nil {
		t.Fatalf("load fired timer: %v", err)
	}
	if row.Status != "fired" || row.FireCount != 1 || !row.LastFiredAtUTC.Valid {
		t.Fatalf("unexpected fired row: %+v", row)
	}
}

func TestReplayPendingRecurringRecomputesNextFireAt(t *testing.T) {
	service, _ := newTestService(t)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-recurring",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "recurring",
		FireAtUTC:    now.Add(-2 * time.Hour).UnixMilli(),
		CronSpec:     nullableStringValue("0 9 * * MON"),
		CronTZ:       nullableStringValue("UTC"),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-24 * time.Hour).UnixMilli(),
	})

	scheduler := newTimerScheduler(service)
	if err := scheduler.replayPending(context.Background(), now); err != nil {
		t.Fatalf("replay pending recurring: %v", err)
	}

	row, err := getTimer(context.Background(), service.db, "timer-recurring")
	if err != nil {
		t.Fatalf("load recurring timer: %v", err)
	}
	if row.FireAtUTC <= now.UnixMilli() {
		t.Fatalf("expected recurring timer to be recomputed into the future: %+v", row)
	}
	if row.Status != "pending" {
		t.Fatalf("expected recurring timer to remain pending: %+v", row)
	}
}

// unschedulableRecurring is a recurring timer of WF.demo kept before FINDINGS A-206, with a zone
// before its fields (it parsed when a space followed the zone): its next fire cannot be computed.
func unschedulableRecurring(timerUUID string, fireAt time.Time) timerRow {
	return timerRow{
		UUID:         timerUUID,
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		ClientRef:    nullableStringValue("weekly-report"),
		Kind:         "recurring",
		FireAtUTC:    fireAt.UnixMilli(),
		CronSpec:     nullableStringValue("CRON_TZ=America/New_York 0 9 * * *"),
		CronTZ:       nullableStringValue("UTC"),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: fireAt.Add(-24 * time.Hour).UnixMilli(),
	}
}

func queuedEntries(s *timerScheduler) int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.items)
}

// A recurring timer whose next fire cannot be computed is canceled at the replay, with its reason,
// and logged once: the next start does not see it (FINDINGS A-220; it stayed pending and was logged
// at every start). Its owner reads the reason with TIMER_GET and TIMER_LIST, it no longer counts as
// pending, and its client_ref schedules a new timer, with the new spec, instead of answering the
// dead one with already_existed.
func TestAStoredCronSpecThatCannotBeScheduledIsCanceledAtTheReplay(t *testing.T) {
	const owner = "WF.demo@motherbee"
	// One pending timer per owner: the new one goes only if the canceled one no longer counts.
	savedOwner := maxPendingTimersPerOwner
	maxPendingTimersPerOwner = 1
	t.Cleanup(func() { maxPendingTimersPerOwner = savedOwner })
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	now := time.Now().UTC()
	row := unschedulableRecurring("timer-zone-in-cron-spec", now.Add(-2*time.Minute))
	insertOwnedTimer(t, service.db, row)
	var logged bytes.Buffer
	log.SetOutput(&logged)
	defer log.SetOutput(os.Stderr)

	for start := 1; start <= 2; start++ {
		if err := service.scheduler.replayPending(context.Background(), now); err != nil {
			t.Fatalf("start %d: the replay stopped at it: %v", start, err)
		}
	}
	if n := queuedEntries(service.scheduler); n != 0 {
		t.Fatalf("the replay queued it (%d entries)", n)
	}
	if n := strings.Count(logged.String(), `"event":"timer_replay_invalid_recurring"`); n != 1 {
		t.Fatalf("two starts logged it %d times, want once: %s", n, logged.String())
	}
	stored, err := getTimer(context.Background(), service.db, row.UUID)
	if err != nil {
		t.Fatalf("load timer: %v", err)
	}
	// Dated at the first replay, which canceled it: kept 7 days from it (FINDINGS A-238).
	if stored.Status != "canceled" || stored.FireAtUTC != row.FireAtUTC || stored.FireCount != 0 || stored.CanceledAtUTC != nullableNow(now) {
		t.Fatalf("unexpected row: %+v", stored)
	}
	for _, says := range []string{"its next fire cannot be computed", "the time zone goes in cron_tz"} {
		if !strings.Contains(stored.CanceledReason.String, says) {
			t.Fatalf("canceled_reason %q does not say %q", stored.CanceledReason.String, says)
		}
	}
	if pending, err := countPendingTimersByOwner(context.Background(), service.db, owner); err != nil || pending != 0 {
		t.Fatalf("its owner has %d pending timers (%v), want none", pending, err)
	}

	type readTimer struct {
		UUID           string  `json:"uuid"`
		Status         string  `json:"status"`
		CanceledReason *string `json:"canceled_reason"`
	}
	var got struct {
		OK    bool      `json:"ok"`
		Timer readTimer `json:"timer"`
	}
	if err := service.handleMessage(mustBuildSystemMessageAs(t, map[string]any{"timer_uuid": row.UUID}, "TIMER_GET", owner)); err != nil {
		t.Fatalf("get: %v", err)
	}
	if err := json.Unmarshal(mustReadResponsePayload(t, tx), &got); err != nil {
		t.Fatalf("decode get: %v", err)
	}
	if !got.OK || got.Timer.Status != "canceled" || got.Timer.CanceledReason == nil || *got.Timer.CanceledReason != stored.CanceledReason.String {
		t.Fatalf("TIMER_GET: %+v", got)
	}
	var listed struct {
		OK     bool        `json:"ok"`
		Timers []readTimer `json:"timers"`
	}
	if err := service.handleMessage(mustBuildSystemMessageAs(t, map[string]any{"status_filter": "canceled"}, "TIMER_LIST", owner)); err != nil {
		t.Fatalf("list: %v", err)
	}
	if err := json.Unmarshal(mustReadResponsePayload(t, tx), &listed); err != nil {
		t.Fatalf("decode list: %v", err)
	}
	if !listed.OK || len(listed.Timers) != 1 || listed.Timers[0].UUID != row.UUID || listed.Timers[0].CanceledReason == nil {
		t.Fatalf("TIMER_LIST: %+v", listed)
	}

	request := map[string]any{"cron_spec": "0 9 * * MON", "cron_tz": "America/New_York", "client_ref": "weekly-report"}
	if err := service.handleMessage(mustBuildSystemMessageAs(t, request, "TIMER_SCHEDULE_RECURRING", owner)); err != nil {
		t.Fatalf("schedule: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK || response.TimerUUID == nil || *response.TimerUUID == row.UUID || response.AlreadyExisted == nil || *response.AlreadyExisted {
		t.Fatalf("want a new timer, got %+v (error %+v)", response, response.Error)
	}
	fresh, err := findPendingTimerByClientRef(context.Background(), service.db, owner, "weekly-report")
	if err != nil || fresh.UUID != *response.TimerUUID || fresh.CronSpec.String != "0 9 * * MON" || fresh.CronTZ.String != "America/New_York" {
		t.Fatalf("the client_ref's pending timer: %+v (%v)", fresh, err)
	}
}

// Due in the queue, a recurring timer whose next fire cannot be computed is canceled the same way: no
// fire is sent, it is not queued again, and it is logged once (FINDINGS A-220). A row that cannot be
// saved stays pending, and is tried again in 30 s.
func TestARecurringTimerThatCannotBeScheduledAtItsFireIsCanceled(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	now := time.Now().UTC()
	row := unschedulableRecurring("timer-unschedulable-at-fire", now.Add(-time.Second))
	insertOwnedTimer(t, service.db, row)
	var logged bytes.Buffer
	log.SetOutput(&logged)
	defer log.SetOutput(os.Stderr)

	if _, err := service.db.Exec(`CREATE TRIGGER refuse_updates BEFORE UPDATE ON timers BEGIN SELECT RAISE(ABORT, 'disk full'); END;`); err != nil {
		t.Fatalf("create trigger: %v", err)
	}
	service.scheduler.push(row.UUID, row.FireAtUTC)
	service.scheduler.processDue(context.Background(), now)
	if stored, err := getTimer(context.Background(), service.db, row.UUID); err != nil || stored.Status != "pending" || stored.CanceledReason.Valid || stored.CanceledAtUTC.Valid {
		t.Fatalf("a row that was not saved: %+v (%v)", stored, err)
	}
	if _, retryAt := mustPeekScheduledTimer(t, service.scheduler); retryAt < now.Add(storageRetryDelay).UnixMilli() {
		t.Fatalf("tried again at %d, want 30 s after %d", retryAt, now.UnixMilli())
	}
	if !strings.Contains(logged.String(), `"event":"timer_fire_persist_failed"`) || strings.Contains(logged.String(), `"event":"timer_recurring_next_fire_failed"`) {
		t.Fatalf("a row that was not saved must be logged as not saved, not as canceled: %s", logged.String())
	}

	if _, err := service.db.Exec(`DROP TRIGGER refuse_updates;`); err != nil {
		t.Fatalf("drop trigger: %v", err)
	}
	before := time.Now().UTC().UnixMilli()
	service.scheduler.processDue(context.Background(), time.Now().UTC().Add(storageRetryDelay+time.Second))
	after := time.Now().UTC().UnixMilli()
	if len(tx) != 0 {
		t.Fatalf("sent a fire for it")
	}
	if n := queuedEntries(service.scheduler); n != 0 {
		t.Fatalf("queued it again (%d entries)", n)
	}
	stored, err := getTimer(context.Background(), service.db, row.UUID)
	if err != nil {
		t.Fatalf("load timer: %v", err)
	}
	if stored.Status != "canceled" || !strings.Contains(stored.CanceledReason.String, "its next fire cannot be computed") || stored.FireCount != 0 || stored.LastFiredAtUTC.Valid {
		t.Fatalf("unexpected row: %+v", stored)
	}
	// Dated at the fire that canceled it: kept 7 days from it (FINDINGS A-238).
	if at := stored.CanceledAtUTC; !at.Valid || at.Int64 < before || at.Int64 > after {
		t.Fatalf("canceled at %+v, want the fire's time, between %d and %d", at, before, after)
	}
	if n := strings.Count(logged.String(), `"event":"timer_recurring_next_fire_failed"`); n != 1 {
		t.Fatalf("logged %d times, want once: %s", n, logged.String())
	}
}

// A cancel or a reschedule that lands while a fire holds its timer, between the fire's read of the
// row and its save, waits for the fire and then sees what it wrote: the change wins, or loses
// cleanly, never both (FINDINGS A-220's review). Before, it took no lock and the fire's save wrote
// over it: a recurring timer its owner was told is canceled kept firing, a canceled one-shot fired
// anyway, and a rescheduled one fired at its old time.
func TestAChangeRacingAFireWinsOrLosesCleanly(t *testing.T) {
	within := func(t *testing.T, done <-chan struct{}, what string) {
		t.Helper()
		select {
		case <-done:
		case <-time.After(5 * time.Second):
			t.Fatalf("%s: not within 5 s", what)
		}
	}
	for _, tc := range []struct {
		name string
		kind string
		verb string
	}{
		{name: "a cancel racing a recurring timer's fire", kind: "recurring", verb: "TIMER_CANCEL"},
		{name: "a cancel racing a one-shot's fire", kind: "oneshot", verb: "TIMER_CANCEL"},
		{name: "a reschedule racing a one-shot's fire", kind: "oneshot", verb: "TIMER_RESCHEDULE"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			service, tx := newTestService(t)
			service.scheduler = newTimerScheduler(service)
			ctx := context.Background()
			now := time.Now().UTC()
			row := timerRow{
				UUID:         "timer-raced",
				OwnerL2Name:  "WF.demo@motherbee",
				TargetL2Name: "WF.demo@motherbee",
				Kind:         tc.kind,
				FireAtUTC:    now.Add(-time.Second).UnixMilli(),
				MissedPolicy: "fire",
				Payload:      `{}`,
				Status:       "pending",
				CreatedAtUTC: now.Add(-time.Minute).UnixMilli(),
			}
			if tc.kind == "recurring" {
				row.CronSpec = nullableStringValue("0 9 * * MON")
				row.CronTZ = nullableStringValue("UTC")
			}
			insertOwnedTimer(t, service.db, row)
			request := map[string]any{"timer_uuid": row.UUID}
			if tc.verb == "TIMER_RESCHEDULE" {
				request["new_fire_in_ms"] = time.Hour.Milliseconds()
			}
			change := mustBuildSystemMessage(t, request, tc.verb)

			// The fire reads the timer and waits there, holding it, while the change lands.
			read, proceed, fired := make(chan struct{}), make(chan struct{}), make(chan struct{})
			afterFireRead = func(string) {
				close(read)
				<-proceed
			}
			t.Cleanup(func() { afterFireRead = nil })
			go func() {
				defer close(fired)
				service.scheduler.push(row.UUID, row.FireAtUTC)
				service.scheduler.processDue(ctx, now)
			}()
			within(t, read, "the fire's read")
			changed, answered := make(chan error, 1), make(chan struct{})
			go func() {
				defer close(answered)
				changed <- service.handleMessage(change)
			}()
			time.Sleep(200 * time.Millisecond) // a change that does not wait for the fire is done by now
			close(proceed)
			within(t, fired, "the fire")
			within(t, answered, tc.verb)
			if err := <-changed; err != nil {
				t.Fatalf("%s: %v", tc.verb, err)
			}

			fires := 0
			var response *fluxbeesdk.TimerResponse
			for len(tx) > 0 {
				var msg fluxbeesdk.Message
				if err := json.Unmarshal(<-tx, &msg); err != nil {
					t.Fatalf("decode a sent message: %v", err)
				}
				switch stringValue(msg.Meta.Msg) {
				case fluxbeesdk.MsgTimerFired:
					fires++
				case fluxbeesdk.MsgTimerResponse:
					response = &fluxbeesdk.TimerResponse{}
					if err := json.Unmarshal(msg.Payload, response); err != nil {
						t.Fatalf("decode the answer: %v", err)
					}
				}
			}
			if response == nil {
				t.Fatalf("%s got no answer", tc.verb)
			}
			stored, err := getTimer(ctx, service.db, row.UUID)
			if err != nil {
				t.Fatalf("load timer: %v", err)
			}
			switch {
			case response.OK && tc.verb == "TIMER_CANCEL":
				// The cancel won, or came after a recurring timer's fire: canceled, it fires no more;
				// a one-shot it canceled did not fire.
				if stored.Status != "canceled" || (tc.kind == "oneshot" && fires != 0) {
					t.Fatalf("answered canceled, with %d fires: %+v", fires, stored)
				}
			case response.OK:
				// The reschedule won: the one-shot did not fire at its old time, and waits for its new one.
				if fires != 0 || stored.Status != "pending" || stored.FireAtUTC <= now.UnixMilli() {
					t.Fatalf("answered rescheduled, with %d fires: %+v", fires, stored)
				}
			default:
				// The fire won: a one-shot that fired is neither canceled nor rescheduled.
				if tc.kind != "oneshot" || response.Error == nil || response.Error.Code != "TIMER_ALREADY_FIRED" || fires != 1 || stored.Status != "fired" {
					t.Fatalf("refused %+v, with %d fires: %+v", response.Error, fires, stored)
				}
			}
		})
	}
}

// A timer purged while its fire holds it is not written back, and its fire is not sent:
// TIMER_PURGE_OWNER takes no timer lock, and the fire saves its row only while it is still pending
// (FINDINGS A-220's review).
func TestTheFireOfATimerPurgedDuringItIsNotSent(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	ctx := context.Background()
	now := time.Now().UTC()
	insertDueTimer(t, service, "timer-purged", "fire")
	read, proceed, fired := make(chan struct{}), make(chan struct{}), make(chan struct{})
	afterFireRead = func(string) {
		close(read)
		<-proceed
	}
	t.Cleanup(func() { afterFireRead = nil })
	go func() {
		defer close(fired)
		service.scheduler.push("timer-purged", now.UnixMilli())
		service.scheduler.processDue(ctx, now)
	}()
	select {
	case <-read:
	case <-time.After(5 * time.Second):
		t.Fatal("the fire did not read the timer within 5 s")
	}
	purge := mustBuildSystemMessageAs(t, map[string]any{"owner_l2_name": "WF.demo@motherbee"}, "TIMER_PURGE_OWNER", "SY.orchestrator@motherbee")
	if err := service.handleMessage(purge); err != nil {
		t.Fatalf("purge: %v", err)
	}
	if response := mustReadTimerResponse(t, tx); !response.OK || response.DeletedCount == nil || *response.DeletedCount != 1 {
		t.Fatalf("purge: %+v (error %+v)", response, response.Error)
	}
	close(proceed)
	select {
	case <-fired:
	case <-time.After(5 * time.Second):
		t.Fatal("the fire did not end within 5 s")
	}
	if len(tx) != 0 {
		t.Fatalf("sent the fire of a purged timer")
	}
	if _, err := getTimer(ctx, service.db, "timer-purged"); err != sql.ErrNoRows {
		t.Fatalf("the purged timer is back: %v", err)
	}
}

func nullableStringValue(value string) sql.NullString {
	return sql.NullString{String: value, Valid: true}
}
