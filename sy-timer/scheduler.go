//go:build linux

package main

import (
	"container/heap"
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"log"
	"sync"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
	"github.com/robfig/cron/v3"
)

type timerHeapEntry struct {
	timerUUID string
	fireAtUTC int64
	index     int
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
}

func newTimerScheduler(service *Service) *timerScheduler {
	return &timerScheduler{
		service: service,
		items:   timerHeap{},
		wakeCh:  make(chan struct{}, 1),
		locks:   map[string]*sync.Mutex{},
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
			nextFireAt, _, err := nextRecurringFireAt(row, now)
			if err != nil {
				logEvent("timer_replay_invalid_recurring", map[string]any{
					"timer_uuid": row.UUID,
					"error":      err.Error(),
				})
				log.Printf("timer replay skipped invalid recurring timer %s: %v", row.UUID, err)
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
		s.fireTimerByUUID(ctx, entry.timerUUID, now)
	}
}

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
	actual := time.Now().UTC()
	if err := s.emitTimerFired(*row, actual); err != nil {
		logEvent("timer_fire_emit_failed", map[string]any{
			"timer_uuid":      row.UUID,
			"owner_l2_name":   row.OwnerL2Name,
			"target_l2_name":  row.TargetL2Name,
			"kind":            row.Kind,
			"scheduled_fire":  row.FireAtUTC,
			"actual_fire":     actual.UTC().UnixMilli(),
			"error":           err.Error(),
		})
		log.Printf("emit TIMER_FIRED failed for %s: %v", row.UUID, err)
	}
	logEvent("timer_fired", map[string]any{
		"timer_uuid":      row.UUID,
		"owner_l2_name":   row.OwnerL2Name,
		"target_l2_name":  row.TargetL2Name,
		"kind":            row.Kind,
		"scheduled_fire":  row.FireAtUTC,
		"actual_fire":     actual.UTC().UnixMilli(),
		"fire_count":      row.FireCount + 1,
	})
	if row.Kind == "recurring" {
		row.FireCount++
		row.LastFiredAtUTC = nullableNow(actual)
		nextFireAt, _, err := nextRecurringFireAt(*row, actual)
		if err != nil {
			logEvent("timer_recurring_next_fire_failed", map[string]any{
				"timer_uuid": row.UUID,
				"error":      err.Error(),
			})
			log.Printf("failed to compute next recurring fire for %s: %v", row.UUID, err)
			return
		}
		row.FireAtUTC = nextFireAt
		if err := updateTimer(ctx, s.service.db, *row); err != nil {
			logEvent("timer_recurring_persist_failed", map[string]any{
				"timer_uuid": row.UUID,
				"error":      err.Error(),
			})
			log.Printf("failed to persist recurring timer %s: %v", row.UUID, err)
			return
		}
		logEvent("timer_recurring_requeued", map[string]any{
			"timer_uuid":     row.UUID,
			"fire_at_utc_ms": row.FireAtUTC,
			"fire_count":     row.FireCount,
		})
		s.push(row.UUID, row.FireAtUTC)
		return
	}

	row.Status = "fired"
	row.FireCount++
	row.LastFiredAtUTC = nullableNow(actual)
	if err := updateTimer(ctx, s.service.db, *row); err != nil {
		logEvent("timer_fired_persist_failed", map[string]any{
			"timer_uuid": row.UUID,
			"error":      err.Error(),
		})
		log.Printf("failed to persist fired timer %s: %v", row.UUID, err)
	}
}

func (s *timerScheduler) handleMissedTimer(ctx context.Context, row timerRow, now time.Time) error {
	switch row.MissedPolicy {
	case "fire":
		logEvent("timer_missed_policy_applied", map[string]any{
			"timer_uuid":     row.UUID,
			"policy":         row.MissedPolicy,
			"scheduled_fire": row.FireAtUTC,
			"now_utc_ms":     now.UTC().UnixMilli(),
			"outcome":        "fire",
		})
		s.fireTimerByUUID(ctx, row.UUID, now)
	case "drop":
		row.Status = "canceled"
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
			row.Status = "canceled"
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
				"timer_uuid":        row.UUID,
				"policy":            row.MissedPolicy,
				"scheduled_fire":    row.FireAtUTC,
				"now_utc_ms":        now.UTC().UnixMilli(),
				"missed_within_ms":  row.MissedWithinMS.Int64,
				"outcome":           "fire",
			})
			s.fireTimerByUUID(ctx, row.UUID, now)
			return nil
		}
		row.Status = "canceled"
		logEvent("timer_missed_policy_applied", map[string]any{
			"timer_uuid":        row.UUID,
			"policy":            row.MissedPolicy,
			"scheduled_fire":    row.FireAtUTC,
			"now_utc_ms":        now.UTC().UnixMilli(),
			"missed_within_ms":  row.MissedWithinMS.Int64,
			"outcome":           "drop_outside_window",
		})
		return updateTimer(ctx, s.service.db, row)
	default:
		row.Status = "canceled"
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

func (s *timerScheduler) emitTimerFired(row timerRow, actual time.Time) error {
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
		uuid.NewString(),
		fluxbeesdk.MsgTimerFired,
		payload,
	)
	if err != nil {
		return err
	}
	return s.service.sender.Send(msg)
}

func (s *timerScheduler) push(timerUUID string, fireAtUTC int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	heap.Push(&s.items, &timerHeapEntry{timerUUID: timerUUID, fireAtUTC: fireAtUTC})
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
		return 0, "", fmt.Errorf("invalid cron: missing cron_spec")
	}
	cronTZ := "UTC"
	if row.CronTZ.Valid && row.CronTZ.String != "" {
		cronTZ = row.CronTZ.String
	}
	loc, err := loadLocation(cronTZ)
	if err != nil {
		return 0, "", fmt.Errorf("invalid timezone: %w", err)
	}
	parser := cron.NewParser(cron.Minute | cron.Hour | cron.Dom | cron.Month | cron.Dow)
	schedule, err := parser.Parse(row.CronSpec.String)
	if err != nil {
		return 0, "", fmt.Errorf("invalid cron: %w", err)
	}
	first := schedule.Next(now.In(loc))
	if first.IsZero() {
		return 0, "", fmt.Errorf("invalid cron: no future fire time")
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
