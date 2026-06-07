#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HIVE_ID="${HIVE_ID:-}"
CONFIG_DIR="${CONFIG_DIR:-/etc/fluxbee}"
ROUTER_SOCKET="${ROUTER_SOCKET:-/var/run/fluxbee/routers}"
NODE_NAME="${NODE_NAME:-}"

if [[ -z "$HIVE_ID" ]]; then
  if [[ ! -f "$CONFIG_DIR/hive.yaml" ]]; then
    echo "FAIL: HIVE_ID is empty and $CONFIG_DIR/hive.yaml is missing" >&2
    exit 1
  fi
  HIVE_ID="$(awk -F': *' '/^hive_id:/ {print $2; exit}' "$CONFIG_DIR/hive.yaml" | tr -d '"')"
fi

if [[ -z "$HIVE_ID" ]]; then
  echo "FAIL: could not resolve HIVE_ID" >&2
  exit 1
fi

if [[ -z "$NODE_NAME" ]]; then
  NODE_NAME="WF.timer.e2e.$(date +%s)-$$@${HIVE_ID}"
fi

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

cat >"$TMP_DIR/go.mod" <<EOF
module fluxbee-sy-timer-client-ref-e2e

go 1.21

require (
	github.com/4iplatform/json-router/fluxbee-go-sdk v0.0.0
	github.com/google/uuid v1.6.0
	gopkg.in/yaml.v3 v3.0.1
)

replace github.com/4iplatform/json-router/fluxbee-go-sdk => ${ROOT_DIR}/go/fluxbee-go-sdk
EOF

cat >"$TMP_DIR/main.go" <<'EOF'
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"strings"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

func main() {
	var hiveID string
	var configDir string
	var routerSocket string
	var nodeName string
	flag.StringVar(&hiveID, "hive", "", "Fluxbee hive id")
	flag.StringVar(&configDir, "config-dir", "/etc/fluxbee", "Fluxbee config directory")
	flag.StringVar(&routerSocket, "router-socket", "/var/run/fluxbee/routers", "Fluxbee router socket or socket directory")
	flag.StringVar(&nodeName, "node", "", "E2E node L2 name")
	flag.Parse()

	if strings.TrimSpace(hiveID) == "" {
		fatalf("hive id is required")
	}
	if strings.TrimSpace(nodeName) == "" {
		nodeName = fmt.Sprintf("WF.timer.e2e.%d@%s", time.Now().Unix(), hiveID)
	}
	timerNode := "SY.timer@" + hiveID
	clientRefPrefix := fmt.Sprintf("sytimer-e2e:%d", time.Now().UnixNano())

	fmt.Printf("SY.timer client_ref E2E: hive=%s node=%s timer=%s\n", hiveID, nodeName, timerNode)

	profile, err := sdk.NewOperationalRouteProfile().Build()
	if err != nil {
		fatalf("build route profile: %v", err)
	}
	dispatcher, err := sdk.ConnectWithRetry(sdk.NodeConfig{
		Name:         nodeName,
		RouterSocket: routerSocket,
		UUIDMode:     sdk.NodeUuidEphemeral,
		ConfigDir:    configDir,
		Version:      "sy-timer-client-ref-e2e",
	}, 250*time.Millisecond, profile)
	if err != nil {
		fatalf("connect to router: %v", err)
	}
	defer func() { _ = dispatcher.Close() }()
	sender := dispatcher.SenderSnapshot()

	timer, err := sdk.NewTimerClient(dispatcher, sdk.TimerClientConfig{TimerNode: timerNode})
	if err != nil {
		fatalf("new timer client: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	step("1/4", "schedule then cancel by client_ref")
	cancelRef := clientRefPrefix + ":cancel"
	cancelID, err := timer.ScheduleIn(ctx, 10*time.Minute, sdk.ScheduleOptions{
		ClientRef:    cancelRef,
		TargetL2Name: sender.FullName(),
		Payload: map[string]any{
			"purpose": "cancel-by-client-ref",
		},
	})
	if err != nil {
		fatalf("schedule cancel fixture: %v", err)
	}
	if err := timer.CancelByClientRef(ctx, cancelRef); err != nil {
		fatalf("cancel by client_ref: %v", err)
	}
	fmt.Printf("  canceled timer_uuid=%s client_ref=%s\n", cancelID, cancelRef)

	step("2/4", "schedule then reschedule by client_ref")
	rescheduleRef := clientRefPrefix + ":reschedule"
	rescheduleID, err := timer.ScheduleIn(ctx, 10*time.Minute, sdk.ScheduleOptions{
		ClientRef:    rescheduleRef,
		TargetL2Name: sender.FullName(),
		Payload: map[string]any{
			"purpose": "reschedule-by-client-ref",
		},
	})
	if err != nil {
		fatalf("schedule reschedule fixture: %v", err)
	}
	newFireAt := time.Now().UTC().Add(12 * time.Minute)
	if err := timer.RescheduleByClientRef(ctx, rescheduleRef, newFireAt); err != nil {
		fatalf("reschedule by client_ref: %v", err)
	}
	info, err := timer.GetByClientRef(ctx, rescheduleRef)
	if err != nil {
		fatalf("get rescheduled timer by client_ref: %v", err)
	}
	if info.UUID != string(rescheduleID) {
		fatalf("rescheduled timer uuid mismatch: got=%s want=%s", info.UUID, rescheduleID)
	}
	if absMillis(info.FireAtUTCMS-newFireAt.UnixMilli()) > 3000 {
		fatalf("rescheduled fire_at mismatch: got=%d want around=%d", info.FireAtUTCMS, newFireAt.UnixMilli())
	}
	fmt.Printf("  rescheduled timer_uuid=%s client_ref=%s\n", rescheduleID, rescheduleRef)

	step("3/4", "idempotent schedule by same client_ref")
	idempotentRef := clientRefPrefix + ":idempotent"
	firstID, err := timer.ScheduleIn(ctx, 15*time.Minute, sdk.ScheduleOptions{
		ClientRef:    idempotentRef,
		TargetL2Name: sender.FullName(),
		Payload: map[string]any{
			"purpose": "idempotent-schedule",
		},
	})
	if err != nil {
		fatalf("first idempotent schedule: %v", err)
	}
	secondID, err := timer.ScheduleIn(ctx, 15*time.Minute, sdk.ScheduleOptions{
		ClientRef:    idempotentRef,
		TargetL2Name: sender.FullName(),
		Payload: map[string]any{
			"purpose": "idempotent-schedule-duplicate",
		},
	})
	if err != nil {
		fatalf("second idempotent schedule: %v", err)
	}
	if firstID != secondID {
		fatalf("idempotent schedule created duplicate uuid: first=%s second=%s", firstID, secondID)
	}
	pending, err := timer.ListMine(ctx, sdk.ListFilter{StatusFilter: "pending", Limit: 1000})
	if err != nil {
		fatalf("list own pending timers: %v", err)
	}
	matches := 0
	for _, item := range pending {
		if item.ClientRef != nil && *item.ClientRef == idempotentRef {
			matches++
		}
	}
	if matches != 1 {
		fatalf("expected exactly one pending timer for idempotent client_ref, got=%d", matches)
	}
	fmt.Printf("  idempotent timer_uuid=%s client_ref=%s\n", firstID, idempotentRef)

	step("4/4", "cleanup pending E2E timers")
	for _, ref := range []string{rescheduleRef, idempotentRef} {
		if err := timer.CancelByClientRef(ctx, ref); err != nil {
			fatalf("cleanup cancel %s: %v", ref, err)
		}
	}

	fmt.Println("status=ok")
	fmt.Printf("hive_id=%s\n", hiveID)
	fmt.Printf("node_name=%s\n", sender.FullName())
	fmt.Println("SY.timer client_ref E2E passed.")
}

func step(n, msg string) {
	fmt.Printf("Step %s: %s\n", n, msg)
}

func absMillis(v int64) int64 {
	if v < 0 {
		return -v
	}
	return v
}

func fatalf(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "FAIL: "+format+"\n", args...)
	os.Exit(1)
}
EOF

(
  cd "$TMP_DIR"
  go mod tidy >/dev/null
  go run . \
    -hive "$HIVE_ID" \
    -config-dir "$CONFIG_DIR" \
    -router-socket "$ROUTER_SOCKET" \
    -node "$NODE_NAME"
)
