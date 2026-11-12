//go:build linux

package main

import (
	"container/heap"
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"sync"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
)

type timerHeapEntry struct {
	timerUUID string
	fireAtUTC int64
	// retry is set on an entry that sends a fire again: its target did not take it (retryLater).
	retry *fireAttempt
	index int
}

type timerHeap []*timerHeapEntry

func (h timerHeap) Len() int           { return len(h) }
func (h timerHeap) Less(i, j int) bool { return h[i].fireAtUTC < h[j].fireAtUTC }
func (h timerHeap) Swap(i, j int) {
	h[i], h[j] = h[j], h[i]
	h[i].index = i
	h[j].index = j
}
func (h *timerHeap) Push(x any) {
	item := x.(*timerHeapEntry)
	item.index = len(*h)
	*h = append(*h, item)
}
func (h *timerHeap) Pop() any {
	old := *h
	n := len(old)
	item := old[n-1]
	old[n-1] = nil
	item.index = -1
	*h = old[:n-1]
	return item
}

type timerScheduler struct {
	service *Service

	mu    sync.Mutex
	items timerHeap
	// queued is each pending timer's entry in items, by its uuid: the next fire it waits for, one per
	// timer. TIMER_RESCHEDULE moves it, and TIMER_CANCEL and TIMER_PURGE_OWNER take it out, so items
	// holds the pending timers only, and the retries of fires on their way, which are not here
	// (retryLater): each leaves at its time, within the retry window (FINDINGS A-259). Before, nothing
	// took an entry out: a canceled, moved or purged timer's stayed until its old fire time, up to 366
	// days ahead, and was dropped then. A fire whose read or save of its timer fails queues it again 30
	// s later (fireTimerByUUID): a timer canceled or purged after its entry left the heap and before
	// that failed read stays queued until then, when its fire finds it gone or no longer pending.
	queued map[string]*timerHeapEntry
	wakeCh chan struct{}
	// lockMu guards locks: the timers being worked on, each with its lock (lockTimer).
	lockMu sync.Mutex
	locks  map[string]*timerLockEntry

	sentMu sync.Mutex
	// sent holds each fire sent in the last deliveryAnswerWindow, by the trace_id of its
	// TIMER_FIRED, for the router's UNREACHABLE (undelivered) or its target's NOT_TAKEN (notTaken)
	// to name it.
	sent map[string]*fireAttempt
}

// Delivery (FINDINGS A-115). A fire is saved before it is sent, so a stop between the two loses it
// instead of sending it twice. A fire the router answers UNREACHABLE because its target is not
// connected, or its link is busy (transientUnreachable), or its target answers NOT_TAKEN retryable,
// its queue full (notTaken, FINDINGS A-329), is sent again while it is still on time:
// up to an hour from its first attempt, less for a fire_if_within one-shot (its window from its
// scheduled time) and for a recurring timer (its next instance); a drop one-shot is not sent again.
// The retries live in memory: a fire SY.timer is retrying when it stops is lost, as is one its
// target does not take in time.
const (
	// deliveryRetryWindow is how long a fire is sent again while its target cannot take it, from
	// its first attempt.
	deliveryRetryWindow = time.Hour
	// deliveryAnswerWindow is how long a sent fire waits for the router's UNREACHABLE or its
	// target's NOT_TAKEN: one with no answer by then reached its target.
	deliveryAnswerWindow = 30 * time.Second
	// storageRetryDelay is when a fire whose row could not be saved is tried again; nothing was sent.
	storageRetryDelay = 30 * time.Second
)

// deliveryRetryDelays are the waits before each new attempt of a fire; the last one repeats.
var deliveryRetryDelays = []time.Duration{5 * time.Second, 10 * time.Second, 20 * time.Second, 40 * time.Second, time.Minute}

// afterFireRead, set by tests only, runs in a fire right after its read of a pending timer, before its
// save, with the timer's lock held: where a change that did not take the lock was written over
// (FINDINGS A-220's review), and where a purge, which takes none, lands (FINDINGS A-259).
var afterFireRead func(timerUUID string)

// afterFireSave, set by tests only, runs in a fire right after its save, before its send, with the
// timer's lock held: where a purge lands after a recurring timer's next fire is saved (FINDINGS A-259).
var afterFireSave func(timerUUID string)

// transientUnreachable are the router's UNREACHABLE reasons for a target that is down, restarting or
// behind a link that is down, and LINK_BUSY: its link queue is full, a refusal that clears as the
// target reads (the router from 0.1.129, FINDINGS A-222). A fire refused for another reason
// (VPN_BLOCKED, OPA_*) is not sent again.
var transientUnreachable = map[string]bool{
	"NODE_NOT_FOUND":      true,
	"PEER_UNAVAILABLE":    true,
	"WAN_UNAVAILABLE":     true,
	"GATEWAY_UNAVAILABLE": true,
	"LINK_BUSY":           true,
}

// timerLockEntry is a timer's lock, and how many hold it or wait for it (refs, under the scheduler's
// lockMu): the map keeps the entry while refs is above zero (lockTimer, FINDINGS A-244).
type timerLockEntry struct {
	mu   sync.Mutex
	refs int
}

// fireAttempt is a fire on its way: the row as it was when it fired (the fire's scheduled time and
// fire_count), which attempt this is, and until when it is sent again.
type fireAttempt struct {
	row      timerRow
	attempt  int
	deadline time.Time
	sentAt   time.Time
}

func newTimerScheduler(service *Service) *timerScheduler {
	return &timerScheduler{
		service: service,
		items:   timerHeap{},
		queued:  map[string]*timerHeapEntry{},
		wakeCh:  make(chan struct{}, 1),
		locks:   map[string]*timerLockEntry{},
		sent:    map[string]*fireAttempt{},
	}
}

func (s *timerScheduler) start() {
	go s.run()
}

func (s *timerScheduler) signal() {
	select {
	case s.wakeCh <- struct{}{}:
	default:
	}
}

func (s *timerScheduler) replayPending(ctx context.Context, now time.Time) error {
	rows, err := listPendingTimers(ctx, s.service.db)
	if err != nil {
		return err
	}
	for _, row := range rows {
		if row.Kind == "recurring" {
			nextFireAt, _, nextErr := nextRecurringFireAt(row, now)
			if nextErr != nil {
				if err := s.cancelUnschedulable(ctx, row, nextErr, "timer_replay_invalid_recurring", now); err != nil {
					return err
				}
				continue
			}
			row.FireAtUTC = nextFireAt
			if err := updateTimer(ctx, s.service.db, row); err != nil {
				return err
			}
			logEvent("timer_replay_loaded", map[string]any{
				"timer_uuid":     row.UUID,
				"kind":           row.Kind,
				"fire_at_utc_ms": row.FireAtUTC,
			})
			s.push(row.UUID, row.FireAtUTC)
			continue
		}
		if row.FireAtUTC > now.UTC().UnixMilli() {
			logEvent("timer_replay_loaded", map[string]any{
				"timer_uuid":     row.UUID,
				"kind":           row.Kind,
				"fire_at_utc_ms": row.FireAtUTC,
			})
			s.push(row.UUID, row.FireAtUTC)
			continue
		}
		if err := s.handleMissedTimer(ctx, row, now); err != nil {
			return err
		}
	}
	return nil
}

// cancelUnschedulable closes a recurring timer whose next fire cannot be computed, at the replay or
// at a fire (FINDINGS A-220): canceled, with its reason, so its client_ref is free for a new timer
// and it no longer counts toward its owner's and the hive's pending timers. Its owner reads the
// reason (TIMER_GET, TIMER_LIST). It is logged once, under event: a canceled timer is not replayed
// again. The fire due when it is closed is not sent. A row that cannot be saved is not logged here:
// the caller says so. One that is no longer pending (purged since it was read) is left as it is. at
// is when it is canceled: the replay's now, or the fire's.
func (s *timerScheduler) cancelUnschedulable(ctx context.Context, row timerRow, cause error, event string, at time.Time) error {
	canceledBySYTimer(&row, "its next fire cannot be computed: "+cause.Error(), at)
	saved, err := updateTimerIfStatus(ctx, s.service.db, row, "pending")
	if err != nil || !saved {
		return err
	}
	fields := map[string]any{
		"timer_uuid":      row.UUID,
		"owner_l2_name":   row.OwnerL2Name,
		"outcome":         "canceled",
		"canceled_reason": row.CanceledReason.String,
	}
	if row.ClientRef.Valid {
		fields["client_ref"] = row.ClientRef.String
	}
	logEvent(event, fields)
	log.Printf("recurring timer %s of %s canceled: %s", row.UUID, row.OwnerL2Name, row.CanceledReason.String)
	return nil
}

// canceledBySYTimer marks row canceled by SY.timer itself at at, and says why (canceled_reason). Its
// history is kept 7 days from at (canceled_at_utc, gcHistoricalTimers; FINDINGS A-238).
func canceledBySYTimer(row *timerRow, reason string, at time.Time) {
	row.Status = "canceled"
	row.CanceledReason = sql.NullString{String: reason, Valid: true}
	row.CanceledAtUTC = nullableNow(at)
}

func (s *timerScheduler) run() {
	for {
		wait := s.nextWaitDuration(time.Now().UTC())
		if wait < 0 {
			wait = 0
		}
		timer := time.NewTimer(wait)
		select {
		case <-s.wakeCh:
			if !timer.Stop() {
				select {
				case <-timer.C:
				default:
				}
			}
			continue
		case <-timer.C:
			s.processDue(context.Background(), time.Now().UTC())
		}
	}
}

func (s *timerScheduler) nextWaitDuration(now time.Time) time.Duration {
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.items) == 0 {
		return time.Hour
	}
	next := s.items[0]
	d := time.Until(time.UnixMilli(next.fireAtUTC).UTC())
	if d < 0 {
		return 0
	}
	return d
}

func (s *timerScheduler) processDue(ctx context.Context, now time.Time) {
	for {
		entry := s.popDue(now.UTC().UnixMilli())
		if entry == nil {
			return
		}
		if entry.retry != nil {
			s.retryFire(ctx, entry.retry)
			continue
		}
		s.fireTimerByUUID(ctx, entry.timerUUID, now)
	}
}

// fireTimerByUUID fires a due timer. It holds the timer's lock from its read of the row to its save
// and its send; TIMER_CANCEL and TIMER_RESCHEDULE take the same lock (lockOwnedTimerForMutation), so
// neither lands between them to be written over (FINDINGS A-220's review). The save is made only
// while the row is still pending: a purge, which takes no lock, is not written over, and its fire
// is not sent. Nor is the purged timer left in the heap (FINDINGS A-259): the fire queues the timer
// again (a recurring timer's next fire, a timer moved later) before its save, or before a second
// read, so a purge after its first read takes that entry out after it is queued, or the save or the
// read finds the timer gone and the fire takes the entry out itself. A first read that fails (the
// database, not a timer gone) queues the timer again in 30 s, as a save that fails does: the entry
// that brought the fire here was the timer's one (popDue), and for all the fire knows the timer is
// still pending.
func (s *timerScheduler) fireTimerByUUID(ctx context.Context, timerUUID string, now time.Time) {
	unlock := s.lockTimer(timerUUID)
	defer unlock()

	row, err := getTimer(ctx, s.service.db, timerUUID)
	if errors.Is(err, sql.ErrNoRows) {
		return
	}
	if err != nil {
		// Before, this returned with no line, and the timer, with no entry left, did not fire until
		// the next start (the review of A-259).
		retryAt := time.Now().UTC().Add(storageRetryDelay)
		logEvent("timer_fire_read_failed", map[string]any{
			"timer_uuid":      timerUUID,
			"error":           err.Error(),
			"retry_at_utc_ms": retryAt.UnixMilli(),
		})
		log.Printf("fire of %s not sent: its row could not be read: %v", timerUUID, err)
		s.push(timerUUID, retryAt.UnixMilli())
		return
	}
	if row.Status != "pending" {
		return
	}
	if afterFireRead != nil {
		afterFireRead(row.UUID)
	}
	if row.FireAtUTC > now.UTC().UnixMilli() {
		// Moved later (TIMER_RESCHEDULE) after this entry left the heap: the move queued the timer at
		// its new time, and this keeps it there. Read again after it, for a purge since the read above.
		s.push(row.UUID, row.FireAtUTC)
		if _, err := getTimer(ctx, s.service.db, row.UUID); errors.Is(err, sql.ErrNoRows) {
			s.remove(row.UUID)
		}
		return
	}
	actual := time.Now().UTC()
	fire := &fireAttempt{row: *row, attempt: 1, deadline: deliveryDeadline(*row, actual)}
	if row.Kind == "recurring" {
		nextFireAt, _, nextErr := nextRecurringFireAt(*row, actual)
		if nextErr != nil {
			if err := s.cancelUnschedulable(ctx, *row, nextErr, "timer_recurring_next_fire_failed", actual); err != nil {
				logEvent("timer_fire_persist_failed", map[string]any{
					"timer_uuid": row.UUID,
					"error":      err.Error(),
				})
				log.Printf("recurring timer %s not canceled: its row was not saved: %v", row.UUID, err)
				s.push(row.UUID, actual.Add(storageRetryDelay).UnixMilli())
			}
			return
		}
		row.FireAtUTC = nextFireAt
		if next := time.UnixMilli(nextFireAt).UTC(); next.Before(fire.deadline) {
			fire.deadline = next
		}
		// Queued before the save, which says whether the timer is still there (above).
		s.push(row.UUID, nextFireAt)
	} else {
		row.Status = "fired"
	}
	row.FireCount++
	row.LastFiredAtUTC = nullableNow(actual)
	saved, err := updateTimerIfStatus(ctx, s.service.db, *row, "pending")
	if err != nil {
		logEvent("timer_fire_persist_failed", map[string]any{
			"timer_uuid": row.UUID,
			"error":      err.Error(),
		})
		log.Printf("fire of %s not sent: its row was not saved: %v", row.UUID, err)
		s.push(row.UUID, actual.Add(storageRetryDelay).UnixMilli())
		return
	}
	if !saved {
		s.remove(row.UUID)
		logEvent("timer_fire_dropped", map[string]any{
			"timer_uuid": row.UUID,
			"attempt":    fire.attempt,
			"reason":     "the timer was canceled or purged while it fired",
		})
		return
	}
	if afterFireSave != nil {
		afterFireSave(row.UUID)
	}
	if row.Kind == "recurring" {
		logEvent("timer_recurring_requeued", map[string]any{
			"timer_uuid":     row.UUID,
			"fire_at_utc_ms": row.FireAtUTC,
			"fire_count":     row.FireCount,
		})
	}
	s.deliver(fire)
}

// deliveryDeadline is until when a fire of row, first sent at first, is sent again: an hour, or a
// fire_if_within one-shot's window from its scheduled time if that ends sooner; a drop one-shot is
// not sent again. The fire cuts a recurring timer's at its next instance.
func deliveryDeadline(row timerRow, first time.Time) time.Time {
	deadline := first.Add(deliveryRetryWindow)
	if row.Kind == "recurring" {
		return deadline
	}
	switch row.MissedPolicy {
	case "drop":
		return first
	case "fire_if_within":
		if row.MissedWithinMS.Valid {
			if within := time.UnixMilli(row.FireAtUTC + row.MissedWithinMS.Int64).UTC(); within.Before(deadline) {
				return within
			}
		}
	}
	return deadline
}

// deliver sends a fire, and keeps it for the router's UNREACHABLE to name it (undelivered). A fire
// the link does not take is sent again; one over the frame limit cannot go at all, and is lost.
func (s *timerScheduler) deliver(fire *fireAttempt) {
	now := time.Now().UTC()
	traceID := uuid.NewString()
	fields := map[string]any{
		"timer_uuid":     fire.row.UUID,
		"owner_l2_name":  fire.row.OwnerL2Name,
		"target_l2_name": fire.row.TargetL2Name,
		"kind":           fire.row.Kind,
		"scheduled_fire": fire.row.FireAtUTC,
		"actual_fire":    now.UnixMilli(),
		"fire_count":     fire.row.FireCount + 1,
		"attempt":        fire.attempt,
		"trace_id":       traceID,
	}
	// Kept before it goes: the router's UNREACHABLE can come back before Send returns.
	fire.sentAt = now
	s.sentMu.Lock()
	for id, sent := range s.sent {
		if now.Sub(sent.sentAt) > deliveryAnswerWindow {
			delete(s.sent, id)
		}
	}
	s.sent[traceID] = fire
	s.sentMu.Unlock()
	if err := s.emitTimerFired(fire.row, now, traceID); err != nil {
		s.sentMu.Lock()
		delete(s.sent, traceID)
		s.sentMu.Unlock()
		fields["error"] = err.Error()
		logEvent("timer_fire_emit_failed", fields)
		log.Printf("emit TIMER_FIRED failed for %s: %v", fire.row.UUID, err)
		var tooLarge *fluxbeesdk.MessageTooLargeError
		if !errors.As(err, &tooLarge) {
			s.retryLater(fire, "SEND_FAILED")
		}
		return
	}
	logEvent("timer_fired", fields)
}

// undelivered takes the router's UNREACHABLE for a fire sent in the last deliveryAnswerWindow: a
// fire whose target was not there is sent again. It reports whether traceID named a fire.
func (s *timerScheduler) undelivered(traceID, reason string) bool {
	s.sentMu.Lock()
	fire, ok := s.sent[traceID]
	delete(s.sent, traceID)
	s.sentMu.Unlock()
	if !ok {
		return false
	}
	if !transientUnreachable[reason] {
		logEvent("timer_fire_refused", map[string]any{
			"timer_uuid":     fire.row.UUID,
			"target_l2_name": fire.row.TargetL2Name,
			"reason":         reason,
			"attempt":        fire.attempt,
		})
		log.Printf("fire of %s refused by the router (%s): not sent again", fire.row.UUID, reason)
		return true
	}
	s.retryLater(fire, reason)
	return true
}

// notTaken takes the NOT_TAKEN of the target of a fire sent in the last deliveryAnswerWindow
// (FINDINGS A-329): the target's Go SDK did not queue the fire, its queue full (node_busy). A
// retryable one is sent again as a fire whose target was not connected is (retryLater: after 5, 10,
// 20 and 40 s, then each minute, while it is on time; a fire no longer on time, a drop one-shot's
// at once, is lost), and logged at each attempt: a plain line names the next attempt only when one
// is queued. One that is not retryable is not sent again. Only the fire's target, by the name its
// router stamps (from), decides it: another node's NOT_TAKEN on the trace decides nothing, and the
// fire waits for its target's answer. It reports whether traceID named a fire the NOT_TAKEN was
// taken for.
func (s *timerScheduler) notTaken(traceID, from string, notice fluxbeesdk.NotTakenPayload) bool {
	s.sentMu.Lock()
	fire, ok := s.sent[traceID]
	if ok && fire.row.TargetL2Name != from {
		ok = false
	}
	if ok {
		delete(s.sent, traceID)
	}
	s.sentMu.Unlock()
	if !ok {
		return false
	}
	reason := fluxbeesdk.MSGNotTaken + " " + notice.Reason
	if !notice.Retryable {
		logEvent("timer_fire_refused", map[string]any{
			"timer_uuid":     fire.row.UUID,
			"target_l2_name": fire.row.TargetL2Name,
			"reason":         reason,
			"detail":         notice.Detail,
			"attempt":        fire.attempt,
		})
		log.Printf("fire of %s not taken by %s (%s, not retryable): not sent again", fire.row.UUID, from, notice.Reason)
		return true
	}
	// Said once retryLater decides: a fire no longer on time is lost, never "sent again" (the review).
	if next, queued := s.retryLater(fire, reason); queued {
		log.Printf("fire of %s not taken by %s (%s): sent again at %s", fire.row.UUID, from, notice.Reason, next.Format("15:04:05.000Z"))
	}
	return true
}

// retryLater queues a fire's next attempt, unless it would no longer be on time: then the fire is
// lost. It returns when the next attempt goes, and whether one was queued (false: the fire is lost,
// and logged so).
func (s *timerScheduler) retryLater(fire *fireAttempt, reason string) (time.Time, bool) {
	next := time.Now().UTC().Add(deliveryRetryDelays[min(fire.attempt, len(deliveryRetryDelays))-1])
	fields := map[string]any{
		"timer_uuid":     fire.row.UUID,
		"target_l2_name": fire.row.TargetL2Name,
		"reason":         reason,
		"attempt":        fire.attempt,
	}
	if next.After(fire.deadline) {
		logEvent("timer_fire_lost", fields)
		log.Printf("fire of %s lost: %s did not take it in %d attempts (%s)", fire.row.UUID, fire.row.TargetL2Name, fire.attempt, reason)
		return time.Time{}, false
	}
	fields["retry_at_utc_ms"] = next.UnixMilli()
	logEvent("timer_fire_undelivered", fields)
	fire.attempt++
	s.pushRetry(fire, next.UnixMilli())
	return next, true
}

// retryFire sends a fire again, unless its timer was canceled or purged since.
func (s *timerScheduler) retryFire(ctx context.Context, fire *fireAttempt) {
	unlock := s.lockTimer(fire.row.UUID)
	defer unlock()

	row, err := getTimer(ctx, s.service.db, fire.row.UUID)
	if err != nil && !errors.Is(err, sql.ErrNoRows) {
		s.retryLater(fire, "STORAGE_ERROR")
		return
	}
	if err != nil || row.Status == "canceled" {
		logEvent("timer_fire_dropped", map[string]any{
			"timer_uuid": fire.row.UUID,
			"attempt":    fire.attempt,
			"reason":     "the timer was canceled or purged",
		})
		return
	}
	s.deliver(fire)
}

// handleMissedTimer applies an overdue one-shot's missed_policy at start. One that fires is queued: it
// goes when the scheduler runs, as the node serves requests, and is sent again until its owner is
// connected again (deliver). One that does not is canceled, and its canceled_reason says why.
func (s *timerScheduler) handleMissedTimer(ctx context.Context, row timerRow, now time.Time) error {
	const missed = "missed: its fire time passed while SY.timer was down"
	switch row.MissedPolicy {
	case "fire":
		logEvent("timer_missed_policy_applied", map[string]any{
			"timer_uuid":     row.UUID,
			"policy":         row.MissedPolicy,
			"scheduled_fire": row.FireAtUTC,
			"now_utc_ms":     now.UTC().UnixMilli(),
			"outcome":        "fire",
		})
		s.push(row.UUID, row.FireAtUTC)
	case "drop":
		canceledBySYTimer(&row, missed+", and its missed_policy is drop", now)
		logEvent("timer_missed_policy_applied", map[string]any{
			"timer_uuid":     row.UUID,
			"policy":         row.MissedPolicy,
			"scheduled_fire": row.FireAtUTC,
			"now_utc_ms":     now.UTC().UnixMilli(),
			"outcome":        "drop",
		})
		return updateTimer(ctx, s.service.db, row)
	case "fire_if_within":
		if !row.MissedWithinMS.Valid {
			canceledBySYTimer(&row, missed+", and its missed_policy fire_if_within has no missed_within_ms", now)
			logEvent("timer_missed_policy_applied", map[string]any{
				"timer_uuid":     row.UUID,
				"policy":         row.MissedPolicy,
				"scheduled_fire": row.FireAtUTC,
				"now_utc_ms":     now.UTC().UnixMilli(),
				"outcome":        "drop_missing_window",
			})
			return updateTimer(ctx, s.service.db, row)
		}
		if now.UTC().UnixMilli()-row.FireAtUTC <= row.MissedWithinMS.Int64 {
			logEvent("timer_missed_policy_applied", map[string]any{
				"timer_uuid":       row.UUID,
				"policy":           row.MissedPolicy,
				"scheduled_fire":   row.FireAtUTC,
				"now_utc_ms":       now.UTC().UnixMilli(),
				"missed_within_ms": row.MissedWithinMS.Int64,
				"outcome":          "fire",
			})
			s.push(row.UUID, row.FireAtUTC)
			return nil
		}
		canceledBySYTimer(&row, fmt.Sprintf("%s, more than its missed_within_ms (%d ms) ago", missed, row.MissedWithinMS.Int64), now)
		logEvent("timer_missed_policy_applied", map[string]any{
			"timer_uuid":       row.UUID,
			"policy":           row.MissedPolicy,
			"scheduled_fire":   row.FireAtUTC,
			"now_utc_ms":       now.UTC().UnixMilli(),
			"missed_within_ms": row.MissedWithinMS.Int64,
			"outcome":          "drop_outside_window",
		})
		return updateTimer(ctx, s.service.db, row)
	default:
		canceledBySYTimer(&row, fmt.Sprintf("%s, and its missed_policy %q is unknown", missed, row.MissedPolicy), now)
		logEvent("timer_missed_policy_applied", map[string]any{
			"timer_uuid":     row.UUID,
			"policy":         row.MissedPolicy,
			"scheduled_fire": row.FireAtUTC,
			"now_utc_ms":     now.UTC().UnixMilli(),
			"outcome":        "drop_unknown_policy",
		})
		return updateTimer(ctx, s.service.db, row)
	}
	return nil
}

func (s *timerScheduler) emitTimerFired(row timerRow, actual time.Time, traceID string) error {
	userPayload := json.RawMessage(row.Payload)
	if len(userPayload) == 0 {
		userPayload = json.RawMessage(`{}`)
	}
	if !json.Valid(userPayload) {
		return fmt.Errorf("invalid payload json for timer %s", row.UUID)
	}

	payload := map[string]any{
		"timer_uuid":               row.UUID,
		"owner_l2_name":            row.OwnerL2Name,
		"kind":                     row.Kind,
		"scheduled_fire_at_utc_ms": row.FireAtUTC,
		"actual_fire_at_utc_ms":    actual.UTC().UnixMilli(),
		"fire_count":               row.FireCount + 1,
		"is_last_fire":             row.Kind != "recurring",
		"user_payload":             userPayload,
	}
	if row.ClientRef.Valid {
		payload["client_ref"] = row.ClientRef.String
	}
	if row.Metadata.Valid {
		var metadata any
		if err := json.Unmarshal([]byte(row.Metadata.String), &metadata); err != nil {
			return err
		}
		payload["metadata"] = metadata
	}

	msg, err := fluxbeesdk.BuildSystemMessage(
		s.service.sender.UUID(),
		fluxbeesdk.UnicastDestination(row.TargetL2Name),
		16,
		traceID,
		fluxbeesdk.MsgTimerFired,
		payload,
	)
	if err != nil {
		return err
	}
	return s.service.sender.Send(msg)
}

// push queues timerUUID's next fire at fireAtUTC. A timer queued already moves there (TIMER_RESCHEDULE,
// a fire that finds its timer moved later): a timer is in the heap once (queued, FINDINGS A-259).
func (s *timerScheduler) push(timerUUID string, fireAtUTC int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if entry, ok := s.queued[timerUUID]; ok {
		entry.fireAtUTC = fireAtUTC
		heap.Fix(&s.items, entry.index)
	} else {
		entry = &timerHeapEntry{timerUUID: timerUUID, fireAtUTC: fireAtUTC}
		heap.Push(&s.items, entry)
		s.queued[timerUUID] = entry
	}
	s.signal()
}

// pushRetry queues a fire's next attempt at fireAtUTC (retryLater). It is not its timer's entry: a
// recurring timer whose last fire is sent again waits for its next fire too.
func (s *timerScheduler) pushRetry(fire *fireAttempt, fireAtUTC int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	heap.Push(&s.items, &timerHeapEntry{timerUUID: fire.row.UUID, fireAtUTC: fireAtUTC, retry: fire})
	s.signal()
}

// remove takes the entries of timerUUIDs out of the heap, for those that have one: a canceled or
// purged timer leaves it at once, not at its old fire time (FINDINGS A-259). A retry of a fire sent
// already stays: it leaves at its time, dropped because its timer was canceled or purged (retryFire).
func (s *timerScheduler) remove(timerUUIDs ...string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, timerUUID := range timerUUIDs {
		if entry, ok := s.queued[timerUUID]; ok {
			heap.Remove(&s.items, entry.index)
			delete(s.queued, timerUUID)
		}
	}
	s.signal()
}

// popDue takes out the first entry when it is due: a timer's own entry leaves queued with it.
func (s *timerScheduler) popDue(nowUTCMS int64) *timerHeapEntry {
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.items) == 0 {
		return nil
	}
	entry := s.items[0]
	if entry.fireAtUTC > nowUTCMS {
		return nil
	}
	heap.Pop(&s.items)
	if s.queued[entry.timerUUID] == entry {
		delete(s.queued, entry.timerUUID)
	}
	return entry
}

// lockTimer takes a timer's lock and returns what releases it; calling that again does nothing. A
// fire and its retries take it, and TIMER_CANCEL and TIMER_RESCHEDULE (lockOwnedTimerForMutation), so
// neither lands between a fire's read of the row and its save. The map keeps a timer's entry while
// something holds it or waits for it: the last release takes it out, and a later caller makes a new
// one, so two callers never hold different locks for one timer. Before, the map kept an entry for
// every timer that ever fired, was canceled or rescheduled, for the process's life (FINDINGS A-244).
func (s *timerScheduler) lockTimer(timerUUID string) (unlock func()) {
	s.lockMu.Lock()
	entry, ok := s.locks[timerUUID]
	if !ok {
		entry = &timerLockEntry{}
		s.locks[timerUUID] = entry
	}
	entry.refs++
	s.lockMu.Unlock()

	entry.mu.Lock()
	var once sync.Once
	return func() {
		once.Do(func() {
			entry.mu.Unlock()
			s.lockMu.Lock()
			defer s.lockMu.Unlock()
			entry.refs--
			if entry.refs == 0 {
				delete(s.locks, timerUUID)
			}
		})
	}
}

func nextRecurringFireAt(row timerRow, now time.Time) (int64, string, error) {
	if !row.CronSpec.Valid || row.CronSpec.String == "" {
		return 0, "", timerErrorf("TIMER_INVALID_CRON", "invalid cron: missing cron_spec")
	}
	cronTZ := "UTC"
	if row.CronTZ.Valid && row.CronTZ.String != "" {
		cronTZ = row.CronTZ.String
	}
	loc, err := loadLocation(cronTZ)
	if err != nil {
		return 0, "", timerErrorf("TIMER_INVALID_TIMEZONE", "invalid timezone: %w", err)
	}
	// A spec stored before FINDINGS A-206 may start with a zone, which parsed then when a space
	// followed it: it is refused now, and both callers cancel the timer, with this error as its
	// reason (cancelUnschedulable, FINDINGS A-220).
	schedule, err := parseCronSpec(row.CronSpec.String)
	if err != nil {
		return 0, "", err
	}
	first := schedule.Next(now.In(loc))
	if first.IsZero() {
		return 0, "", timerErrorf("TIMER_INVALID_CRON", "invalid cron: no future fire time")
	}
	second := schedule.Next(first.Add(time.Second).In(loc))
	if !second.IsZero() && second.Sub(first) < fluxbeesdk.MinTimerDuration {
		return 0, "", errBelowMinimum()
	}
	return first.UTC().UnixMilli(), cronTZ, nil
}
