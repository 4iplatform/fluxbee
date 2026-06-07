#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HIVE_ID="${HIVE_ID:-}"
CONFIG_DIR="${CONFIG_DIR:-/etc/fluxbee}"
ROUTER_SOCKET="${ROUTER_SOCKET:-/var/run/fluxbee/routers}"
WF_NODE_NAME="${WF_NODE_NAME:-}"
WF_E2E_RESTART="${WF_E2E_RESTART:-0}"

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

if [[ -z "$WF_NODE_NAME" ]]; then
  WF_NODE_NAME="WF.invoice@${HIVE_ID}"
fi

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

cat >"$TMP_DIR/go.mod" <<EOF
module fluxbee-wf-invoice-runtime-e2e

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
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

type clientNode struct {
	name       string
	dispatcher *sdk.RouterDispatcher
	sender     *sdk.NodeSender
	events     <-chan sdk.Message
}

type wfInstanceResponse struct {
	OK       bool `json:"ok"`
	Instance struct {
		InstanceID   string `json:"InstanceID"`
		WorkflowType string `json:"WorkflowType"`
		Status       string `json:"Status"`
		CurrentState string `json:"CurrentState"`
		StateJSON    string `json:"StateJSON"`
	} `json:"instance"`
	Error  string `json:"error,omitempty"`
	Detail string `json:"detail,omitempty"`
}

func main() {
	var hiveID string
	var configDir string
	var routerSocket string
	var wfNode string
	var restart bool
	flag.StringVar(&hiveID, "hive", "", "Fluxbee hive id")
	flag.StringVar(&configDir, "config-dir", "/etc/fluxbee", "Fluxbee config directory")
	flag.StringVar(&routerSocket, "router-socket", "/var/run/fluxbee/routers", "Fluxbee router socket or socket directory")
	flag.StringVar(&wfNode, "wf-node", "", "WF node name, e.g. WF.invoice@motherbee")
	flag.BoolVar(&restart, "restart", false, "Restart WF systemd unit during recovery check")
	flag.Parse()

	if strings.TrimSpace(hiveID) == "" {
		fatalf("hive id is required")
	}
	if strings.TrimSpace(wfNode) == "" {
		wfNode = "WF.invoice@" + hiveID
	}
	runID := fmt.Sprintf("%d", time.Now().UnixNano())

	fmt.Printf("WF.invoice runtime E2E: hive=%s wf_node=%s restart=%t\n", hiveID, wfNode, restart)

	trigger := connectNode(configDir, routerSocket, fmt.Sprintf("IO.wf.invoice.e2e.%s@%s", runID, hiveID))
	defer trigger.close()
	quickbooks := connectNode(configDir, routerSocket, "IO.quickbooks@"+hiveID)
	defer quickbooks.close()
	validator := connectNode(configDir, routerSocket, "IO.validator@"+hiveID)
	defer validator.close()
	email := connectNode(configDir, routerSocket, "IO.email@"+hiveID)
	defer email.close()
	notifications := connectNode(configDir, routerSocket, "IO.notifications@"+hiveID)
	defer notifications.close()

	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()

	step("1/5", "verify WF node responds to WF_HELP")
	if _, err := rpc(ctx, trigger, wfNode, "WF_HELP", map[string]any{}, "", "WF_HELP_RESPONSE"); err != nil {
		fatalf("WF_HELP failed for %s: %v", wfNode, err)
	}

	step("2/5", "complete invoice workflow through real SY.timer")
	completedID := startInvoiceAndReadFirstRequest(ctx, trigger, quickbooks, wfNode, "cust-complete-"+runID)
	if err := sendEvent(ctx, quickbooks.sender, wfNode, "INVOICE_CREATE_RESPONSE", map[string]any{"ok": true, "invoice_id": "inv-" + runID}, completedID); err != nil {
		fatalf("send INVOICE_CREATE_RESPONSE: %v", err)
	}
	expectRequestAndReply(ctx, validator, wfNode, "INVOICE_VALIDATE_REQUEST", "INVOICE_VALIDATE_RESPONSE", map[string]any{"valid": true})
	expectRequestAndReply(ctx, email, wfNode, "INVOICE_SEND_REQUEST", "INVOICE_SEND_RESPONSE", map[string]any{"delivered": true})
	assertInstance(ctx, trigger, wfNode, completedID, "completed", "completed")

	step("3/5", "cancel invoice workflow mid-flow")
	cancelID := startInvoiceAndReadFirstRequest(ctx, trigger, quickbooks, wfNode, "cust-cancel-"+runID)
	if _, err := rpc(ctx, trigger, wfNode, "WF_CANCEL_INSTANCE", map[string]any{
		"instance_id": cancelID,
		"reason":      "e2e cancel",
	}, "", "WF_CANCEL_INSTANCE_RESPONSE"); err != nil {
		fatalf("WF_CANCEL_INSTANCE failed: %v", err)
	}
	assertInstance(ctx, trigger, wfNode, cancelID, "cancelled", "cancelled")
	drainOptional(ctx, notifications.events, 150*time.Millisecond)

	step("4/5", "restart recovery check")
	recoveryID := startInvoiceAndReadFirstRequest(ctx, trigger, quickbooks, wfNode, "cust-recovery-"+runID)
	if restart {
		restartWFUnit(wfNode, hiveID)
		waitWFHelp(ctx, trigger, wfNode)
	} else {
		fmt.Println("  restart skipped; set WF_E2E_RESTART=1 to restart the WF systemd unit")
	}
	sendEvent(ctx, quickbooks.sender, wfNode, "INVOICE_CREATE_RESPONSE", map[string]any{"ok": true, "invoice_id": "inv-recovery-" + runID}, recoveryID)
	expectRequestAndReply(ctx, validator, wfNode, "INVOICE_VALIDATE_REQUEST", "INVOICE_VALIDATE_RESPONSE", map[string]any{"valid": true})
	expectRequestAndReply(ctx, email, wfNode, "INVOICE_SEND_REQUEST", "INVOICE_SEND_RESPONSE", map[string]any{"delivered": true})
	assertInstance(ctx, trigger, wfNode, recoveryID, "completed", "completed")

	step("5/5", "summary")
	fmt.Println("status=ok")
	fmt.Printf("hive_id=%s\n", hiveID)
	fmt.Printf("wf_node=%s\n", wfNode)
	fmt.Printf("completed_instance=%s\n", completedID)
	fmt.Printf("cancelled_instance=%s\n", cancelID)
	fmt.Printf("recovered_instance=%s\n", recoveryID)
	fmt.Println("WF.invoice runtime E2E passed.")
}

func connectNode(configDir, routerSocket, name string) *clientNode {
	profile, err := sdk.NewOperationalRouteProfile().
		BroadcastChannel("events").
		PostPendingRule(sdk.RouteAny{}, sdk.RouteBroadcast{Channel: "events"}).
		Build()
	if err != nil {
		fatalf("build route profile for %s: %v", name, err)
	}
	dispatcher, err := sdk.ConnectWithRetry(sdk.NodeConfig{
		Name:         name,
		RouterSocket: routerSocket,
		UUIDMode:     sdk.NodeUuidEphemeral,
		ConfigDir:    configDir,
		Version:      "wf-invoice-runtime-e2e",
	}, 250*time.Millisecond, profile)
	if err != nil {
		fatalf("connect %s: %v", name, err)
	}
	events, err := dispatcher.Subscribe("events")
	if err != nil {
		fatalf("subscribe events for %s: %v", name, err)
	}
	return &clientNode{
		name:       name,
		dispatcher: dispatcher,
		sender:     dispatcher.SenderSnapshot(),
		events:     events,
	}
}

func (n *clientNode) close() {
	if n != nil && n.dispatcher != nil {
		_ = n.dispatcher.Close()
	}
}

func expectRequestAndReply(ctx context.Context, node *clientNode, wfNode, requestMsg, replyMsg string, payload map[string]any) {
	msg := awaitMsg(ctx, node.events, requestMsg)
	threadID := ""
	if msg.Meta.ThreadID != nil {
		threadID = *msg.Meta.ThreadID
	}
	if threadID == "" {
		fatalf("%s missing thread_id", requestMsg)
	}
	if err := sendEvent(ctx, node.sender, wfNode, replyMsg, payload, threadID); err != nil {
		fatalf("send %s: %v", replyMsg, err)
	}
}

func sendEvent(ctx context.Context, sender *sdk.NodeSender, dst, msgName string, payload any, threadID string) error {
	msg, err := sdk.BuildSystemRequest(sender.UUID(), dst, msgName, payload, traceID(msgName), sdk.SystemEnvelopeOptions{})
	if err != nil {
		return err
	}
	if strings.TrimSpace(threadID) != "" {
		threadCopy := threadID
		msg.Meta.ThreadID = &threadCopy
	}
	if err := sender.Send(msg); err != nil {
		return err
	}
	select {
	case <-ctx.Done():
		return ctx.Err()
	default:
		return nil
	}
}

func rpc(ctx context.Context, node *clientNode, dst, msgName string, payload any, threadID, responseMsg string) (sdk.Message, error) {
	msg, err := sdk.BuildSystemRequest(node.sender.UUID(), dst, msgName, payload, traceID(msgName), sdk.SystemEnvelopeOptions{})
	if err != nil {
		return sdk.Message{}, err
	}
	if strings.TrimSpace(threadID) != "" {
		threadCopy := threadID
		msg.Meta.ThreadID = &threadCopy
	}
	matcher := sdk.PendingMatcher{
		Success: []sdk.RouteMatch{sdk.RouteExact{MsgType: sdk.SYSTEMKind, Msg: responseMsg}},
		TerminalError: []sdk.RouteMatch{
			sdk.RouteExact{MsgType: sdk.SYSTEMKind, Msg: sdk.MSGUnreachable},
			sdk.RouteExact{MsgType: sdk.SYSTEMKind, Msg: sdk.MSGTTLExceeded},
		},
		InvalidResponse: []sdk.RouteMatch{sdk.RouteAnyMsgOfType{MsgType: sdk.SYSTEMKind}},
	}
	return node.dispatcher.SendWithMatcher(ctx, msg, matcher, sdk.RpcRequestLabels{
		Target:      dst,
		RequestMsg:  msgName,
		ResponseMsg: responseMsg,
	}, 5*time.Second)
}

func awaitMsg(ctx context.Context, events <-chan sdk.Message, msgName string) sdk.Message {
	if events == nil {
		fatalf("events channel is nil")
	}
	for {
		select {
		case <-ctx.Done():
			fatalf("waiting for %s: %v", msgName, ctx.Err())
		case msg, ok := <-events:
			if !ok {
				fatalf("events channel closed while waiting for %s", msgName)
			}
			if msgName == "" || (msg.Meta.Msg != nil && *msg.Meta.Msg == msgName) {
				return msg
			}
		}
	}
}

func startInvoiceAndReadFirstRequest(ctx context.Context, trigger, quickbooks *clientNode, wfNode, customerID string) string {
	if err := sendEvent(ctx, trigger.sender, wfNode, "INVOICE_START", map[string]any{
		"customer_id":  customerID,
		"amount_cents": 12345,
		"currency":     "USD",
	}, ""); err != nil {
		fatalf("start invoice: %v", err)
	}
	msg := awaitMsg(ctx, quickbooks.events, "INVOICE_CREATE_REQUEST")
	if msg.Meta.ThreadID == nil || *msg.Meta.ThreadID == "" {
		fatalf("INVOICE_CREATE_REQUEST missing thread_id")
	}
	return *msg.Meta.ThreadID
}

func assertInstance(ctx context.Context, trigger *clientNode, wfNode, instanceID, wantStatus, wantState string) {
	resp, err := rpc(ctx, trigger, wfNode, "WF_GET_INSTANCE", map[string]any{
		"instance_id": instanceID,
		"log_limit":   10,
	}, "", "WF_GET_INSTANCE_RESPONSE")
	if err != nil {
		fatalf("WF_GET_INSTANCE %s: %v", instanceID, err)
	}
	var payload wfInstanceResponse
	if err := json.Unmarshal(resp.Payload, &payload); err != nil {
		fatalf("parse WF_GET_INSTANCE response: %v", err)
	}
	if !payload.OK {
		fatalf("WF_GET_INSTANCE not ok: error=%s detail=%s", payload.Error, payload.Detail)
	}
	if payload.Instance.Status != wantStatus || payload.Instance.CurrentState != wantState {
		fatalf("instance %s state mismatch: status=%s state=%s want status=%s state=%s", instanceID, payload.Instance.Status, payload.Instance.CurrentState, wantStatus, wantState)
	}
}

func waitWFHelp(ctx context.Context, trigger *clientNode, wfNode string) {
	deadline := time.Now().Add(20 * time.Second)
	var lastErr error
	for time.Now().Before(deadline) {
		attemptCtx, cancel := context.WithTimeout(ctx, 2*time.Second)
		_, err := rpc(attemptCtx, trigger, wfNode, "WF_HELP", map[string]any{}, "", "WF_HELP_RESPONSE")
		cancel()
		if err == nil {
			return
		}
		lastErr = err
		time.Sleep(500 * time.Millisecond)
	}
	fatalf("WF node did not respond after restart: %v", lastErr)
}

func restartWFUnit(wfNode, hiveID string) {
	base := strings.TrimSuffix(wfNode, "@"+hiveID)
	unit := fmt.Sprintf("fluxbee-node-%s-%s.service", base, hiveID)
	fmt.Printf("  restarting %s\n", unit)
	cmd := exec.Command("sudo", "systemctl", "restart", unit)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		fatalf("restart %s: %v", unit, err)
	}
}

func drainOptional(ctx context.Context, events <-chan sdk.Message, d time.Duration) {
	attemptCtx, cancel := context.WithTimeout(ctx, d)
	defer cancel()
	select {
	case <-events:
	case <-attemptCtx.Done():
	}
}

func traceID(prefix string) string {
	return fmt.Sprintf("e2e-%s-%d", strings.ToLower(prefix), time.Now().UnixNano())
}

func step(n, msg string) {
	fmt.Printf("Step %s: %s\n", n, msg)
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
    -wf-node "$WF_NODE_NAME" \
    -restart="$WF_E2E_RESTART"
)
