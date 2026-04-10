//go:build linux

package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"os"
	"path/filepath"
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
	dir := t.TempDir()

	motherbeeUUID := "11111111-1111-1111-1111-111111111111"
	if err := osWriteFile(filepath.Join(dir, "WF.demo.uuid"), []byte(motherbeeUUID)); err != nil {
		t.Fatalf("write wf uuid: %v", err)
	}

	db := openTestTimerDB(t)
	tx := make(chan []byte, 8)
	service := &Service{
		sender:             &stubSender{uuid: "timer-node-uuid", fullName: "SY.timer@motherbee", tx: tx},
		nodeName:           "SY.timer@motherbee",
		db:                 db,
		uuidPersistenceDir: dir,
	}
	return service, tx
}

func TestRespondTimerSchedulePersistsOwnedTimer(t *testing.T) {
	service, tx := newTestService(t)
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
}

func TestRespondTimerGetRejectsForeignOwner(t *testing.T) {
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
	if response.OK || response.Error == nil || response.Error.Code != "TIMER_NOT_OWNER" {
		t.Fatalf("unexpected get response: %+v", response)
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

func TestRespondTimerRescheduleUpdatesFireAt(t *testing.T) {
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
}

func TestRespondTimerScheduleRecurringPersistsCronFields(t *testing.T) {
	service, tx := newTestService(t)
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

func TestRespondTimerListOnlyReturnsOwnerTimers(t *testing.T) {
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
	if payload.Count != 1 || payload.Timers[0]["uuid"] != "timer-owned" {
		t.Fatalf("unexpected list payload: %+v", payload)
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

func osWriteFile(path string, data []byte) error {
	return os.WriteFile(path, data, 0o644)
}
