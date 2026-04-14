//go:build linux

package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"sync"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
)

type stubSender struct {
	uuid     string
	fullName string
	tx       chan []byte
}

func (s *stubSender) Send(msg fluxbeesdk.Message) error {
	data, err := json.Marshal(msg)
	if err != nil {
		return err
	}
	s.tx <- data
	return nil
}

func (s *stubSender) UUID() string {
	return s.uuid
}

func (s *stubSender) FullName() string {
	return s.fullName
}

func newTestService(t *testing.T) (*Service, chan []byte) {
	t.Helper()
	db := openTestTimerDB(t)
	tx := make(chan []byte, 8)
	service := &Service{
		sender:   &stubSender{uuid: "timer-node-uuid", fullName: "SY.timer@motherbee", tx: tx},
		nodeName: "SY.timer@motherbee",
		db:       db,
	}
	return service, tx
}

func TestRespondTimerSchedulePersistsOwnedTimer(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	fireInMS := int64((time.Minute).Milliseconds())
	msg := mustBuildSystemMessage(t, map[string]any{
		"fire_in_ms": fireInMS,
		"payload":    map[string]any{"ticket_id": 1234},
	}, "TIMER_SCHEDULE")

	if err := service.respondTimerSchedule(msg); err != nil {
		t.Fatalf("schedule: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK || response.TimerUUID == nil {
		t.Fatalf("unexpected schedule response: %+v", response)
	}
	row, err := getTimer(context.Background(), service.db, *response.TimerUUID)
	if err != nil {
		t.Fatalf("get stored timer: %v", err)
	}
	if row.OwnerL2Name != "WF.demo@motherbee" || row.TargetL2Name != "WF.demo@motherbee" {
		t.Fatalf("unexpected stored ownership: %+v", row)
	}
	timerUUID, fireAtUTCMS := mustPeekScheduledTimer(t, service.scheduler)
	if timerUUID != *response.TimerUUID || fireAtUTCMS != row.FireAtUTC {
		t.Fatalf("unexpected scheduled heap entry: uuid=%s fire_at=%d row=%+v", timerUUID, fireAtUTCMS, row)
	}
}

func TestRespondTimerGetAllowsReadingForeignTimer(t *testing.T) {
	service, tx := newTestService(t)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-1",
		OwnerL2Name:  "AI.other@motherbee",
		TargetL2Name: "AI.other@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})

	msg := mustBuildSystemMessage(t, map[string]any{"timer_uuid": "timer-1"}, "TIMER_GET")
	if err := service.respondTimerGet(msg); err != nil {
		t.Fatalf("get timer: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK || response.Timer == nil || response.Timer.UUID != "timer-1" {
		t.Fatalf("unexpected get response: %+v", response)
	}
}

func TestRespondTimerGetByUUIDAllowsMissingSourceL2Name(t *testing.T) {
	service, tx := newTestService(t)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-1",
		OwnerL2Name:  "AI.other@motherbee",
		TargetL2Name: "AI.other@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})
	msg, err := fluxbeesdk.BuildSystemMessage(
		"22222222-2222-2222-2222-222222222222",
		fluxbeesdk.UnicastDestination("SY.timer@motherbee"),
		16,
		uuid.NewString(),
		"TIMER_GET",
		map[string]any{"timer_uuid": "timer-1"},
	)
	if err != nil {
		t.Fatalf("build message: %v", err)
	}
	if err := service.respondTimerGet(msg); err != nil {
		t.Fatalf("get timer by uuid without src_l2_name: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK || response.Verb != "TIMER_GET" {
		t.Fatalf("unexpected get response: %+v", response)
	}
}

func TestRespondTimerScheduleReturnsExistingPendingTimerByClientRef(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	fireInMS := int64(time.Minute.Milliseconds())
	msg := mustBuildSystemMessage(t, map[string]any{
		"fire_in_ms": fireInMS,
		"client_ref": "wf:demo::timeout",
	}, "TIMER_SCHEDULE")

	if err := service.respondTimerSchedule(msg); err != nil {
		t.Fatalf("first schedule: %v", err)
	}
	first := mustReadTimerResponse(t, tx)
	if !first.OK || first.TimerUUID == nil {
		t.Fatalf("unexpected first schedule response: %+v", first)
	}
	if err := service.respondTimerSchedule(msg); err != nil {
		t.Fatalf("second schedule: %v", err)
	}
	second := mustReadTimerResponse(t, tx)
	if !second.OK || second.TimerUUID == nil {
		t.Fatalf("unexpected second schedule response: %+v", second)
	}
	if *first.TimerUUID != *second.TimerUUID {
		t.Fatalf("expected idempotent schedule to reuse timer uuid: first=%s second=%s", *first.TimerUUID, *second.TimerUUID)
	}
	alreadyExisted, ok := second.Extra["already_existed"].(bool)
	if !ok || !alreadyExisted {
		t.Fatalf("expected already_existed=true, got %+v", second.Extra)
	}
}

func TestRespondTimerCancelMarksTimerCanceled(t *testing.T) {
	service, tx := newTestService(t)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-1",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})

	msg := mustBuildSystemMessage(t, map[string]any{"timer_uuid": "timer-1"}, "TIMER_CANCEL")
	if err := service.respondTimerCancel(msg); err != nil {
		t.Fatalf("cancel timer: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK {
		t.Fatalf("unexpected cancel response: %+v", response)
	}
	row, err := getTimer(context.Background(), service.db, "timer-1")
	if err != nil {
		t.Fatalf("get canceled timer: %v", err)
	}
	if row.Status != "canceled" {
		t.Fatalf("expected canceled status, got %+v", row)
	}
}

func TestRespondTimerCancelByClientRefMarksTimerCanceled(t *testing.T) {
	service, tx := newTestService(t)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-1",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		ClientRef:    sql.NullString{String: "wf:demo::timeout", Valid: true},
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})

	msg := mustBuildSystemMessage(t, map[string]any{"client_ref": "wf:demo::timeout"}, "TIMER_CANCEL")
	if err := service.respondTimerCancel(msg); err != nil {
		t.Fatalf("cancel timer by client_ref: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK {
		t.Fatalf("unexpected cancel by client_ref response: %+v", response)
	}
	row, err := getTimer(context.Background(), service.db, "timer-1")
	if err != nil {
		t.Fatalf("get canceled timer: %v", err)
	}
	if row.Status != "canceled" {
		t.Fatalf("expected canceled status, got %+v", row)
	}
}

func TestRespondTimerCancelByClientRefReturnsFoundFalseWhenMissing(t *testing.T) {
	service, tx := newTestService(t)
	msg := mustBuildSystemMessage(t, map[string]any{"client_ref": "wf:demo::missing"}, "TIMER_CANCEL")
	if err := service.respondTimerCancel(msg); err != nil {
		t.Fatalf("cancel missing timer by client_ref: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK {
		t.Fatalf("unexpected cancel missing response: %+v", response)
	}
	found, ok := response.Extra["found"].(bool)
	if !ok || found {
		t.Fatalf("expected found=false, got %+v", response.Extra)
	}
}

func TestRespondTimerRescheduleUpdatesFireAt(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-1",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})
	newFireAt := time.Now().UTC().Add(2 * time.Hour).UnixMilli()
	msg := mustBuildSystemMessage(t, map[string]any{
		"timer_uuid":         "timer-1",
		"new_fire_at_utc_ms": newFireAt,
	}, "TIMER_RESCHEDULE")

	if err := service.respondTimerReschedule(msg); err != nil {
		t.Fatalf("reschedule timer: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK {
		t.Fatalf("unexpected reschedule response: %+v", response)
	}
	row, err := getTimer(context.Background(), service.db, "timer-1")
	if err != nil {
		t.Fatalf("get rescheduled timer: %v", err)
	}
	if row.FireAtUTC != newFireAt {
		t.Fatalf("unexpected fire_at after reschedule: %+v", row)
	}
	timerUUID, fireAtUTCMS := mustPeekScheduledTimer(t, service.scheduler)
	if timerUUID != "timer-1" || fireAtUTCMS != newFireAt {
		t.Fatalf("unexpected rescheduled heap entry: uuid=%s fire_at=%d", timerUUID, fireAtUTCMS)
	}
}

func TestRespondTimerRescheduleByClientRefUpdatesFireAt(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-1",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		ClientRef:    sql.NullString{String: "wf:demo::timeout", Valid: true},
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})
	newFireAt := time.Now().UTC().Add(2 * time.Hour).UnixMilli()
	msg := mustBuildSystemMessage(t, map[string]any{
		"client_ref":         "wf:demo::timeout",
		"new_fire_at_utc_ms": newFireAt,
	}, "TIMER_RESCHEDULE")

	if err := service.respondTimerReschedule(msg); err != nil {
		t.Fatalf("reschedule timer by client_ref: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK {
		t.Fatalf("unexpected reschedule by client_ref response: %+v", response)
	}
	row, err := getTimer(context.Background(), service.db, "timer-1")
	if err != nil {
		t.Fatalf("get rescheduled timer: %v", err)
	}
	if row.FireAtUTC != newFireAt {
		t.Fatalf("unexpected fire_at after reschedule: %+v", row)
	}
}

func TestRespondTimerScheduleRecurringPersistsCronFields(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	msg := mustBuildSystemMessage(t, map[string]any{
		"cron_spec": "0 9 * * MON",
		"cron_tz":   "America/Argentina/Buenos_Aires",
		"payload":   map[string]any{"report": "weekly"},
	}, "TIMER_SCHEDULE_RECURRING")

	if err := service.respondTimerScheduleRecurring(msg); err != nil {
		t.Fatalf("schedule recurring: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK || response.TimerUUID == nil {
		t.Fatalf("unexpected recurring response: %+v", response)
	}
	row, err := getTimer(context.Background(), service.db, *response.TimerUUID)
	if err != nil {
		t.Fatalf("get stored recurring timer: %v", err)
	}
	if row.Kind != "recurring" || !row.CronSpec.Valid || row.CronSpec.String != "0 9 * * MON" {
		t.Fatalf("unexpected recurring row: %+v", row)
	}
	if !row.CronTZ.Valid || row.CronTZ.String != "America/Argentina/Buenos_Aires" {
		t.Fatalf("unexpected cron_tz: %+v", row)
	}
	timerUUID, fireAtUTCMS := mustPeekScheduledTimer(t, service.scheduler)
	if timerUUID != *response.TimerUUID || fireAtUTCMS != row.FireAtUTC {
		t.Fatalf("unexpected recurring heap entry: uuid=%s fire_at=%d row=%+v", timerUUID, fireAtUTCMS, row)
	}
}

func TestRespondTimerRescheduleRejectsRecurringTimer(t *testing.T) {
	service, tx := newTestService(t)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-recurring",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "recurring",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		CronSpec:     sql.NullString{String: "0 9 * * MON", Valid: true},
		CronTZ:       sql.NullString{String: "UTC", Valid: true},
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})

	msg := mustBuildSystemMessage(t, map[string]any{
		"timer_uuid":     "timer-recurring",
		"new_fire_in_ms": int64((2 * time.Hour).Milliseconds()),
	}, "TIMER_RESCHEDULE")

	if err := service.respondTimerReschedule(msg); err != nil {
		t.Fatalf("reschedule recurring timer: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if response.OK || response.Error == nil || response.Error.Code != "TIMER_RECURRING_NOT_RESCHEDULABLE" {
		t.Fatalf("unexpected recurring reschedule response: %+v", response)
	}
}

func TestRespondTimerListReturnsAllTimersByDefault(t *testing.T) {
	service, tx := newTestService(t)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-owned",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-other",
		OwnerL2Name:  "AI.other@motherbee",
		TargetL2Name: "AI.other@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})

	msg := mustBuildSystemMessage(t, map[string]any{"status_filter": "pending", "limit": 10}, "TIMER_LIST")
	if err := service.respondTimerList(msg); err != nil {
		t.Fatalf("list timers: %v", err)
	}
	payloadData := mustReadResponsePayload(t, tx)
	var response fluxbeesdk.TimerResponse
	if err := json.Unmarshal(payloadData, &response); err != nil {
		t.Fatalf("decode timer response: %v", err)
	}
	if !response.OK {
		t.Fatalf("unexpected list response: %+v", response)
	}
	var payload struct {
		OK     bool             `json:"ok"`
		Verb   string           `json:"verb"`
		Count  int              `json:"count"`
		Timers []map[string]any `json:"timers"`
	}
	if err := json.Unmarshal(payloadData, &payload); err != nil {
		t.Fatalf("decode list payload: %v", err)
	}
	if payload.Count != 2 {
		t.Fatalf("unexpected list payload: %+v", payload)
	}
}

func TestRespondTimerGetByClientRefUsesOwnerScopedLookup(t *testing.T) {
	service, tx := newTestService(t)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-owned",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		ClientRef:    sql.NullString{String: "wf:demo::timeout", Valid: true},
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})

	msg := mustBuildSystemMessage(t, map[string]any{"client_ref": "wf:demo::timeout"}, "TIMER_GET")
	if err := service.respondTimerGet(msg); err != nil {
		t.Fatalf("get timer by client_ref: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK {
		t.Fatalf("unexpected get by client_ref response: %+v", response)
	}
}

func TestRespondTimerCancelByClientRefDoesNotAffectOtherOwner(t *testing.T) {
	service, tx := newTestService(t)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-owned",
		OwnerL2Name:  "AI.other@motherbee",
		TargetL2Name: "AI.other@motherbee",
		ClientRef:    sql.NullString{String: "wf:shared::timeout", Valid: true},
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})

	msg := mustBuildSystemMessage(t, map[string]any{"client_ref": "wf:shared::timeout"}, "TIMER_CANCEL")
	if err := service.respondTimerCancel(msg); err != nil {
		t.Fatalf("cancel foreign timer by client_ref: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK {
		t.Fatalf("unexpected foreign cancel response: %+v", response)
	}
	found, ok := response.Extra["found"].(bool)
	if !ok || found {
		t.Fatalf("expected found=false, got %+v", response.Extra)
	}
	row, err := getTimer(context.Background(), service.db, "timer-owned")
	if err != nil {
		t.Fatalf("get foreign timer: %v", err)
	}
	if row.Status != "pending" {
		t.Fatalf("foreign timer should remain pending, got %+v", row)
	}
}

func TestRespondTimerGetByClientRefDoesNotLeakOtherOwnerTimer(t *testing.T) {
	service, tx := newTestService(t)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-owned",
		OwnerL2Name:  "AI.other@motherbee",
		TargetL2Name: "AI.other@motherbee",
		ClientRef:    sql.NullString{String: "wf:shared::timeout", Valid: true},
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})

	msg := mustBuildSystemMessage(t, map[string]any{"client_ref": "wf:shared::timeout"}, "TIMER_GET")
	if err := service.respondTimerGet(msg); err != nil {
		t.Fatalf("get foreign timer by client_ref: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if response.OK || response.Error == nil || response.Error.Code != "TIMER_NOT_FOUND" {
		t.Fatalf("unexpected foreign get response: %+v", response)
	}
}

func TestRespondTimerRescheduleByClientRefDoesNotAffectOtherOwner(t *testing.T) {
	service, tx := newTestService(t)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-owned",
		OwnerL2Name:  "AI.other@motherbee",
		TargetL2Name: "AI.other@motherbee",
		ClientRef:    sql.NullString{String: "wf:shared::timeout", Valid: true},
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})

	msg := mustBuildSystemMessage(t, map[string]any{
		"client_ref":         "wf:shared::timeout",
		"new_fire_at_utc_ms": time.Now().UTC().Add(2 * time.Hour).UnixMilli(),
	}, "TIMER_RESCHEDULE")
	if err := service.respondTimerReschedule(msg); err != nil {
		t.Fatalf("reschedule foreign timer by client_ref: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if response.OK || response.Error == nil || response.Error.Code != "TIMER_NOT_FOUND" {
		t.Fatalf("unexpected foreign reschedule response: %+v", response)
	}
}

func TestScheduleIdempotencyAllowsReuseAfterCancel(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	fireInMS := int64(time.Minute.Milliseconds())

	// First schedule: creates the timer.
	msg := mustBuildSystemMessage(t, map[string]any{
		"fire_in_ms": fireInMS,
		"client_ref": "wf:demo::step-timeout",
	}, "TIMER_SCHEDULE")
	if err := service.respondTimerSchedule(msg); err != nil {
		t.Fatalf("first schedule: %v", err)
	}
	first := mustReadTimerResponse(t, tx)
	if !first.OK || first.TimerUUID == nil {
		t.Fatalf("unexpected first schedule response: %+v", first)
	}

	// Cancel by client_ref.
	cancelMsg := mustBuildSystemMessage(t, map[string]any{"client_ref": "wf:demo::step-timeout"}, "TIMER_CANCEL")
	if err := service.respondTimerCancel(cancelMsg); err != nil {
		t.Fatalf("cancel by client_ref: %v", err)
	}
	if resp := mustReadTimerResponse(t, tx); !resp.OK {
		t.Fatalf("unexpected cancel response: %+v", resp)
	}

	// Second schedule with same client_ref after cancel: must create a new timer, not return the old one.
	if err := service.respondTimerSchedule(msg); err != nil {
		t.Fatalf("second schedule after cancel: %v", err)
	}
	second := mustReadTimerResponse(t, tx)
	if !second.OK || second.TimerUUID == nil {
		t.Fatalf("unexpected second schedule response: %+v", second)
	}
	if *first.TimerUUID == *second.TimerUUID {
		t.Fatalf("expected new timer uuid after cancel, got same: %s", *first.TimerUUID)
	}
	if alreadyExisted, _ := second.Extra["already_existed"].(bool); alreadyExisted {
		t.Fatalf("expected already_existed=false after cancel reuse, got true")
	}
}

func TestConcurrentScheduleWithSameClientRefCreatesExactlyOneTimer(t *testing.T) {
	service, _ := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	fireInMS := int64(time.Minute.Milliseconds())

	const goroutines = 20
	results := make(chan *fluxbeesdk.TimerResponse, goroutines)
	txAll := make(chan []byte, goroutines)

	// Replace the shared tx channel with one large enough to receive all responses.
	service.sender = &stubSender{
		uuid:     "timer-node-uuid",
		fullName: "SY.timer@motherbee",
		tx:       txAll,
	}

	var wg sync.WaitGroup
	for i := 0; i < goroutines; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			msg := mustBuildSystemMessage(t, map[string]any{
				"fire_in_ms": fireInMS,
				"client_ref": "wf:demo::concurrent-key",
			}, "TIMER_SCHEDULE")
			if err := service.respondTimerSchedule(msg); err != nil {
				t.Errorf("concurrent schedule: %v", err)
			}
		}()
	}
	wg.Wait()

	// Collect all responses.
	close(txAll)
	for frame := range txAll {
		var msg fluxbeesdk.Message
		if err := json.Unmarshal(frame, &msg); err != nil {
			t.Fatalf("decode sent message: %v", err)
		}
		var resp fluxbeesdk.TimerResponse
		if err := json.Unmarshal(msg.Payload, &resp); err != nil {
			t.Fatalf("decode timer response: %v", err)
		}
		results <- &resp
	}
	close(results)

	var uuids []string
	for resp := range results {
		if !resp.OK || resp.TimerUUID == nil {
			t.Fatalf("unexpected response: %+v", resp)
		}
		uuids = append(uuids, *resp.TimerUUID)
	}
	if len(uuids) != goroutines {
		t.Fatalf("expected %d responses, got %d", goroutines, len(uuids))
	}

	// All responses must carry the same timer UUID — exactly one timer was created.
	first := uuids[0]
	for _, u := range uuids[1:] {
		if u != first {
			t.Fatalf("concurrent schedules produced different timer UUIDs: %s vs %s", first, u)
		}
	}

	// Exactly one pending row must exist in the database.
	rows, err := listTimers(context.Background(), service.db, "WF.demo@motherbee", "pending", 100)
	if err != nil {
		t.Fatalf("list pending timers: %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("expected exactly 1 pending timer after concurrent schedules, got %d", len(rows))
	}
}

func TestRespondTimerListFiltersByOwnerWhenRequested(t *testing.T) {
	service, tx := newTestService(t)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-owned",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-other",
		OwnerL2Name:  "AI.other@motherbee",
		TargetL2Name: "AI.other@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})

	msg := mustBuildSystemMessage(t, map[string]any{
		"owner_l2_name": "WF.demo@motherbee",
		"status_filter": "pending",
		"limit":         10,
	}, "TIMER_LIST")
	if err := service.respondTimerList(msg); err != nil {
		t.Fatalf("list timers by owner: %v", err)
	}
	payloadData := mustReadResponsePayload(t, tx)
	var payload struct {
		OK     bool             `json:"ok"`
		Verb   string           `json:"verb"`
		Count  int              `json:"count"`
		Timers []map[string]any `json:"timers"`
	}
	if err := json.Unmarshal(payloadData, &payload); err != nil {
		t.Fatalf("decode list payload: %v", err)
	}
	if payload.Count != 1 || payload.Timers[0]["uuid"] != "timer-owned" {
		t.Fatalf("unexpected filtered list payload: %+v", payload)
	}
}

func TestRespondTimerPurgeOwnerRequiresLocalOrchestrator(t *testing.T) {
	service, tx := newTestService(t)
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-owned",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})

	msg := mustBuildSystemMessage(t, map[string]any{"owner_l2_name": "WF.demo@motherbee"}, "TIMER_PURGE_OWNER")
	if err := service.respondTimerPurgeOwner(msg); err != nil {
		t.Fatalf("purge owner forbidden: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if response.OK || response.Error == nil || response.Error.Code != "TIMER_FORBIDDEN" {
		t.Fatalf("unexpected purge forbidden response: %+v", response)
	}
}

func TestRespondTimerPurgeOwnerDeletesTimersForOwner(t *testing.T) {
	service, tx := newTestService(t)

	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-owned-1",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-owned-2",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(2 * time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-other",
		OwnerL2Name:  "AI.other@motherbee",
		TargetL2Name: "AI.other@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    time.Now().UTC().Add(time.Hour).UnixMilli(),
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: time.Now().UTC().UnixMilli(),
	})

	msg := mustBuildSystemMessageAs(t,
		map[string]any{"owner_l2_name": "WF.demo@motherbee"},
		"TIMER_PURGE_OWNER",
		"SY.orchestrator@motherbee",
	)
	if err := service.respondTimerPurgeOwner(msg); err != nil {
		t.Fatalf("purge owner: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if !response.OK || response.DeletedCount == nil || *response.DeletedCount != 2 {
		t.Fatalf("unexpected purge response: %+v", response)
	}
	if _, err := getTimer(context.Background(), service.db, "timer-owned-1"); err == nil {
		t.Fatalf("expected first owned timer to be deleted")
	}
	if _, err := getTimer(context.Background(), service.db, "timer-owned-2"); err == nil {
		t.Fatalf("expected second owned timer to be deleted")
	}
	if _, err := getTimer(context.Background(), service.db, "timer-other"); err != nil {
		t.Fatalf("expected unrelated timer to remain: %v", err)
	}
}

func insertOwnedTimer(t *testing.T, db *sql.DB, row timerRow) {
	t.Helper()
	if err := insertTimer(context.Background(), db, row); err != nil {
		t.Fatalf("insert timer: %v", err)
	}
}

func mustBuildSystemMessage(t *testing.T, payload any, verb string) fluxbeesdk.Message {
	t.Helper()
	return mustBuildSystemMessageAs(t, payload, verb, "WF.demo@motherbee")
}

func mustBuildSystemMessageAs(t *testing.T, payload any, verb, srcL2Name string) fluxbeesdk.Message {
	t.Helper()
	msg, err := fluxbeesdk.BuildSystemMessage(
		"11111111-1111-1111-1111-111111111111",
		fluxbeesdk.UnicastDestination("SY.timer@motherbee"),
		16,
		uuid.NewString(),
		verb,
		payload,
	)
	if err != nil {
		t.Fatalf("build message: %v", err)
	}
	msg.Routing.SrcL2Name = &srcL2Name
	return msg
}

func mustReadTimerResponse(t *testing.T, tx chan []byte) *fluxbeesdk.TimerResponse {
	t.Helper()
	data := mustReadResponsePayload(t, tx)
	var response fluxbeesdk.TimerResponse
	if err := json.Unmarshal(data, &response); err != nil {
		t.Fatalf("decode timer response: %v", err)
	}
	return &response
}

func mustReadResponsePayload(t *testing.T, tx chan []byte) []byte {
	t.Helper()
	frame := <-tx
	var msg fluxbeesdk.Message
	if err := json.Unmarshal(frame, &msg); err != nil {
		t.Fatalf("decode sent message: %v", err)
	}
	return msg.Payload
}

func mustPeekScheduledTimer(t *testing.T, scheduler *timerScheduler) (string, int64) {
	t.Helper()
	if scheduler == nil {
		t.Fatal("scheduler is nil")
	}
	scheduler.mu.Lock()
	defer scheduler.mu.Unlock()
	if len(scheduler.items) == 0 {
		t.Fatal("scheduler heap is empty")
	}
	entry := scheduler.items[0]
	return entry.timerUUID, entry.fireAtUTC
}
