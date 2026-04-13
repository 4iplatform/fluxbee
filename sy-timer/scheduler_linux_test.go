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

func TestReplayPendingFiresMissedOneShotWithFirePolicy(t *testing.T) {
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

	frame := <-tx
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

func nullableStringValue(value string) sql.NullString {
	return sql.NullString{String: value, Valid: true}
}
