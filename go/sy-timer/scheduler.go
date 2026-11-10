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

	mu     sync.Mutex
	items  timerHeap
	wakeCh chan struct{}
	lockMu sync.Mutex
	locks  map[string]*sync.Mutex

	sentMu sync.Mutex
	// sent holds each fire sent in the last deliveryAnswerWindow, by the trace_id of its
	// TIMER_FIRED, for the router's UNREACHABLE to name it (undelivered).
	sent map[string]*fireAttempt
}

// Delivery (FINDINGS A-115). A fire is saved before it is sent, so a stop between the two loses it
// instead of sending it twice. A fire the router answers UNREACHABLE because its target is not
// connected, or its link is busy (transientUnreachable), is sent again while it is still on time:
// up to an hour from its first attempt, less for a fire_if_within one-shot (its window from its
// scheduled time) and for a recurring timer (its next instance); a drop one-shot is not sent again.
// The retries live in memory: a fire SY.timer is retrying when it stops is lost, as is one its
// target does not take in time.
const (
	// deliveryRetryWindow is how long a fire is sent again while its target cannot take it, from
	// its first attempt.
	deliveryRetryWindow = time.Hour
	// deliveryAnswerWindow is how long a sent fire waits for the router's UNREACHABLE: one with no
	// answer by then reached its target.
	deliveryAnswerWindow = 30 * time.Second
	// storageRetryDelay is when a fire whose row could not be saved is tried again; nothing was sent.
	storageRetryDelay = 30 * time.Second
)

// deliveryRetryDelays are the waits before each new attempt of a fire; the last one repeats.
var deliveryRetryDelays = []time.Duration{5 * time.Second, 10 * time.Second, 20 * time.Second, 40 * time.Second, time.Minute}

// afterFireRead, set by tests only, runs in a fire between its read of the timer and its save, with
// the timer's lock held: where a change that did not take the lock was written over (FINDINGS
// A-220's review).
var afterFireRead func(timerUUID string)

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
		wakeCh:  make(chan struct{}, 1),
		locks:   map[string]*sync.Mutex{},
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
				if err := s.cancelUnschedulable(ctx, row, nextErr, "timer_replay_invalid_recurring"); err != nil {
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
// the caller says so. One that is no longer pending (purged since it was read) is left as it is.
func (s *timerScheduler) cancelUnschedulable(ctx context.Context, row timerRow, cause error, event string) error {
	canceledBySYTimer(&row, "its next fire cannot be computed: "+cause.Error())
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

// canceledBySYTimer marks row canceled by SY.timer itself, and says why (canceled_reason).
func canceledBySYTimer(row *timerRow, reason string) {
	row.Status = "canceled"
	row.CanceledReason = sql.NullString{String: reason, Valid: true}
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
// is not sent.
func (s *timerScheduler) fireTimerByUUID(ctx context.Context, timerUUID string, now time.Time) {
	lock := s.timerLock(timerUUID)
	lock.Lock()
	defer lock.Unlock()

	row, err := getTimer(ctx, s.service.db, timerUUID)
	if err != nil {
		return
	}
	if row.Status != "pending" {
		return
	}
	if row.FireAtUTC > now.UTC().UnixMilli() {
		s.push(row.UUID, row.FireAtUTC)
		return
	}
	if afterFireRead != nil {
		afterFireRead(row.UUID)
	}
	actual := time.Now().UTC()
	fire := &fireAttempt{row: *row, attempt: 1, deadline: deliveryDeadline(*row, actual)}
	if row.Kind == "recurring" {
		nextFireAt, _, nextErr := nextRecurringFireAt(*row, actual)
		if nextErr != nil {
			if err := s.cancelUnschedulable(ctx, *row, nextErr, "timer_recurring_next_fire_failed"); err != nil {
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
		logEvent("timer_fire_dropped", map[string]any{
			"timer_uuid": row.UUID,
			"attempt":    fire.attempt,
			"reason":     "the timer was canceled or purged while it fired",
		})
		return
	}
	if row.Kind == "recurring" {
		logEvent("timer_recurring_requeued", map[string]any{
			"timer_uuid":     row.UUID,
			"fire_at_utc_ms": row.FireAtUTC,
			"fire_count":     row.FireCount,
		})
		s.push(row.UUID, row.FireAtUTC)
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

// retryLater queues a fire's next attempt, unless it would no longer be on time: then the fire is
// lost.
func (s *timerScheduler) retryLater(fire *fireAttempt, reason string) {
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
		return
	}
	fields["retry_at_utc_ms"] = next.UnixMilli()
	logEvent("timer_fire_undelivered", fields)
	fire.attempt++
	s.pushEntry(&timerHeapEntry{timerUUID: fire.row.UUID, fireAtUTC: next.UnixMilli(), retry: fire})
}

// retryFire sends a fire again, unless its timer was canceled or purged since.
func (s *timerScheduler) retryFire(ctx context.Context, fire *fireAttempt) {
	lock := s.timerLock(fire.row.UUID)
	lock.Lock()
	defer lock.Unlock()

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
		canceledBySYTimer(&row, missed+", and its missed_policy is drop")
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
			canceledBySYTimer(&row, missed+", and its missed_policy fire_if_within has no missed_within_ms")
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
		canceledBySYTimer(&row, fmt.Sprintf("%s, more than its missed_within_ms (%d ms) ago", missed, row.MissedWithinMS.Int64))
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
		canceledBySYTimer(&row, fmt.Sprintf("%s, and its missed_policy %q is unknown", missed, row.MissedPolicy))
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

func (s *timerScheduler) push(timerUUID string, fireAtUTC int64) {
	s.pushEntry(&timerHeapEntry{timerUUID: timerUUID, fireAtUTC: fireAtUTC})
}

func (s *timerScheduler) pushEntry(entry *timerHeapEntry) {
	s.mu.Lock()
	defer s.mu.Unlock()
	heap.Push(&s.items, entry)
	s.signal()
}

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
	return heap.Pop(&s.items).(*timerHeapEntry)
}

func (s *timerScheduler) timerLock(timerUUID string) *sync.Mutex {
	s.lockMu.Lock()
	defer s.lockMu.Unlock()
	lock, ok := s.locks[timerUUID]
	if !ok {
		lock = &sync.Mutex{}
		s.locks[timerUUID] = lock
	}
	return lock
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

func nullableNow(now time.Time) sql.NullInt64 {
	return sql.NullInt64{Int64: now.UTC().UnixMilli(), Valid: true}
}
