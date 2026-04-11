//go:build linux

package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

func TestReplayPendingDropCancelsMissedOneShot(t *testing.T) {
	service, _ := newTestService(t)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-drop",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    now.Add(-2 * time.Minute).UnixMilli(),
		MissedPolicy: "drop",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-10 * time.Minute).UnixMilli(),
	})

	scheduler := newTimerScheduler(service)
	if err := scheduler.replayPending(context.Background(), now); err != nil {
		t.Fatalf("replay pending drop: %v", err)
	}

	row, err := getTimer(context.Background(), service.db, "timer-drop")
	if err != nil {
		t.Fatalf("load dropped timer: %v", err)
	}
	if row.Status != "canceled" {
		t.Fatalf("expected dropped timer to be canceled: %+v", row)
	}
}

func TestReplayPendingFireIfWithinCancelsOutsideWindow(t *testing.T) {
	service, _ := newTestService(t)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-outside-window",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    now.Add(-10 * time.Minute).UnixMilli(),
		MissedPolicy: "fire_if_within",
		MissedWithinMS: sql.NullInt64{
			Int64: int64((time.Minute).Milliseconds()),
			Valid: true,
		},
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-20 * time.Minute).UnixMilli(),
	})

	scheduler := newTimerScheduler(service)
	if err := scheduler.replayPending(context.Background(), now); err != nil {
		t.Fatalf("replay fire_if_within outside window: %v", err)
	}

	row, err := getTimer(context.Background(), service.db, "timer-outside-window")
	if err != nil {
		t.Fatalf("load timer: %v", err)
	}
	if row.Status != "canceled" {
		t.Fatalf("expected timer to be canceled outside window: %+v", row)
	}
}

func TestReplayPendingFireIfWithinFiresInsideWindow(t *testing.T) {
	service, tx := newTestService(t)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-inside-window",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "oneshot",
		FireAtUTC:    now.Add(-30 * time.Second).UnixMilli(),
		MissedPolicy: "fire_if_within",
		MissedWithinMS: sql.NullInt64{
			Int64: int64(time.Minute.Milliseconds()),
			Valid: true,
		},
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-2 * time.Minute).UnixMilli(),
	})

	scheduler := newTimerScheduler(service)
	if err := scheduler.replayPending(context.Background(), now); err != nil {
		t.Fatalf("replay fire_if_within inside window: %v", err)
	}

	frame := <-tx
	var msg fluxbeesdk.Message
	if err := json.Unmarshal(frame, &msg); err != nil {
		t.Fatalf("decode fired event: %v", err)
	}
	if msg.Meta.Msg == nil || *msg.Meta.Msg != fluxbeesdk.MsgTimerFired {
		t.Fatalf("unexpected fired message: %+v", msg.Meta)
	}
}

func TestProcessDueRecurringFiresAndRequeues(t *testing.T) {
	service, tx := newTestService(t)
	now := time.Now().UTC()
	insertOwnedTimer(t, service.db, timerRow{
		UUID:         "timer-recurring-due",
		OwnerL2Name:  "WF.demo@motherbee",
		TargetL2Name: "WF.demo@motherbee",
		Kind:         "recurring",
		FireAtUTC:    now.Add(-2 * time.Minute).UnixMilli(),
		CronSpec:     sql.NullString{String: "0 9 * * MON", Valid: true},
		CronTZ:       sql.NullString{String: "UTC", Valid: true},
		MissedPolicy: "fire",
		Payload:      `{}`,
		Status:       "pending",
		CreatedAtUTC: now.Add(-24 * time.Hour).UnixMilli(),
	})

	scheduler := newTimerScheduler(service)
	scheduler.processDue(context.Background(), now)

	frame := <-tx
	var msg fluxbeesdk.Message
	if err := json.Unmarshal(frame, &msg); err != nil {
		t.Fatalf("decode fired event: %v", err)
	}
	if msg.Meta.Msg == nil || *msg.Meta.Msg != fluxbeesdk.MsgTimerFired {
		t.Fatalf("unexpected fired message: %+v", msg.Meta)
	}

	row, err := getTimer(context.Background(), service.db, "timer-recurring-due")
	if err != nil {
		t.Fatalf("load recurring timer: %v", err)
	}
	if row.Status != "pending" || row.FireCount != 1 || row.FireAtUTC <= now.UnixMilli() {
		t.Fatalf("expected recurring timer to be requeued: %+v", row)
	}
}
