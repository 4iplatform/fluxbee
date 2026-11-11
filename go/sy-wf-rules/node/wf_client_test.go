package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// routerFrameLimit is the router's frame limit, the SDK's: a node's answer over it cannot go.
const routerFrameLimit = 128 * 1024

// wfEngine is how a WF node answers a WF_LIST_INSTANCES, given the request's payload.
type wfEngine func(request map[string]any) map[string]any

// countingEngine answers as wf.engine 0.1.136 does with running instances running: a count-only
// request with how many and no rows; any other as before (listingEngine).
func countingEngine(running int) wfEngine {
	return func(request map[string]any) map[string]any {
		if request["count_only"] == true {
			return map[string]any{"ok": true, "count": running}
		}
		return listingEngine(running)(request)
	}
}

// listingEngine answers as wf.engine before 0.1.136 does with running instances running, and no
// other: it does not know count_only, lists the instances of the status asked for, newest first, as
// many as the limit says (0, every one; less than 0, 50), about 340 B each, and counts the rows it
// lists. A list over the router's frame is refused by the engine (wfNodeAnswer).
func listingEngine(running int) wfEngine {
	return func(request map[string]any) map[string]any {
		listed := running
		if status, _ := request["status"].(string); status != "" && status != "running" {
			listed = 0
		}
		limit, _ := request["limit"].(float64)
		if limit < 0 {
			limit = 50
		}
		if limit > 0 && int(limit) < listed {
			listed = int(limit)
		}
		instances := make([]map[string]any, 0, listed)
		for i := running - 1; i >= running-listed; i-- {
			instances = append(instances, map[string]any{
				"instance_id":      fmt.Sprintf("wfi:00000000-0000-4000-8000-%012d", i),
				"thread_id":        fmt.Sprintf("conv-running-%04d", i),
				"workflow_type":    "invoice",
				"definition_hash":  strings.Repeat("ab", 32),
				"status":           "running",
				"current_state":    "collecting_data",
				"created_at_ms":    1_776_000_000_000 + i,
				"updated_at_ms":    1_776_000_000_000 + i,
				"terminated_at_ms": nil,
			})
		}
		return map[string]any{"ok": true, "count": len(instances), "instances": instances}
	}
}

// wfNodeAnswer is the WF_LIST_INSTANCES_RESPONSE a WF node sends request with payload: one over the
// router's frame cannot go, and the engine answers INTERNAL_ERROR instead, as its replyOrError does
// (FINDINGS A-284).
func wfNodeAnswer(request *fluxbeesdk.Message, payload map[string]any) (fluxbeesdk.Message, error) {
	answer, err := fluxbeesdk.BuildSystemResponse(request, "wf-uuid", "WF_LIST_INSTANCES_RESPONSE", payload, fluxbeesdk.SystemEnvelopeOptions{})
	if err != nil {
		return answer, err
	}
	raw, err := fluxbeesdk.MarshalMessage(answer)
	if err != nil || len(raw) <= routerFrameLimit {
		return answer, err
	}
	tooLarge := &fluxbeesdk.MessageTooLargeError{Size: len(raw), Max: routerFrameLimit}
	return fluxbeesdk.BuildSystemResponse(request, "wf-uuid", "WF_LIST_INSTANCES_RESPONSE", map[string]any{
		"ok": false, "error": "INTERNAL_ERROR", "detail": "WF_LIST_INSTANCES_RESPONSE not sent: " + tooLarge.Error(),
	}, fluxbeesdk.SystemEnvelopeOptions{})
}

// wfNodeOnTestRouter connects SY.wf-rules@motherbee, through the SDK and its route profile, to a
// router of the test's that plays WF.invoice@motherbee running engine: it answers each
// WF_LIST_INSTANCES on its trace_id, with the node's name stamped, and hands the request's payload
// to the channel it returns first.
func wfNodeOnTestRouter(t *testing.T, engine wfEngine) (wfNodeClient, <-chan map[string]any) {
	t.Helper()
	// A socket's path holds about 100 bytes, which a test's own temporary directory may pass.
	dir, err := os.MkdirTemp("", "wfr")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	socket := filepath.Join(dir, "router.sock")
	listener, err := (&net.ListenConfig{}).Listen(context.Background(), "unix", socket)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	requests := make(chan map[string]any, 16)
	go serveAsWFNode(listener, engine, requests)

	configDir := t.TempDir()
	if err := os.WriteFile(filepath.Join(configDir, "hive.yaml"), []byte("hive_id: motherbee\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	profile, err := routeProfile()
	if err != nil {
		t.Fatal(err)
	}
	dispatcher, err := fluxbeesdk.ConnectWithRetry(fluxbeesdk.NodeConfig{
		Name: "SY.wf-rules", RouterSocket: socket, UUIDPersistenceDir: t.TempDir(), UUIDMode: fluxbeesdk.NodeUuidEphemeral,
		ConfigDir: configDir, Version: "test",
	}, 0, profile)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = dispatcher.Close() })
	return newWFNodeClient(dispatcher), requests
}

func serveAsWFNode(listener net.Listener, engine wfEngine, requests chan<- map[string]any) {
	conn, err := listener.Accept()
	if err != nil {
		return
	}
	defer conn.Close()
	if _, err := fluxbeesdk.ReadFrame(conn); err != nil { // SY.wf-rules' HELLO
		return
	}
	announce, err := fluxbeesdk.BuildAnnounce("router-uuid", "wf-rules-uuid", "trace-announce", fluxbeesdk.NodeAnnouncePayload{
		UUID: "wf-rules-uuid", Name: "SY.wf-rules@motherbee", Status: "ok", VpnID: 1, RouterName: "RT.test@motherbee",
	})
	if err != nil {
		return
	}
	write := func(msg fluxbeesdk.Message) bool {
		raw, err := fluxbeesdk.MarshalMessage(msg)
		return err == nil && fluxbeesdk.WriteFrame(conn, raw) == nil
	}
	if !write(announce) {
		return
	}
	wfNode := "WF.invoice@motherbee"
	for {
		frame, err := fluxbeesdk.ReadFrame(conn)
		if err != nil {
			return
		}
		var request fluxbeesdk.Message
		if err := json.Unmarshal(frame, &request); err != nil {
			return
		}
		if stringValue(request.Meta.Msg) != "WF_LIST_INSTANCES" { // SY.wf-rules' WITHDRAW as it closes
			continue
		}
		var payload map[string]any
		if err := json.Unmarshal(request.Payload, &payload); err != nil {
			return
		}
		requests <- payload
		answer, err := wfNodeAnswer(&request, engine(payload))
		if err != nil {
			return
		}
		answer.Routing.SrcL2Name = &wfNode
		if !write(answer) {
			return
		}
	}
}

// SY.wf-rules counts a WF node's running instances with WF_LIST_INSTANCES {status: running, limit:
// 0, count_only: true}, through the SDK and a router of the test's playing the node. An engine of
// 0.1.136 counts the 1000 running instances spec §21 allows a node in one answer; the whole list
// SY.wf-rules asked for before passed the frame past about 350 of them, and the count failed exactly
// for a busy workflow (FINDINGS A-284). During a deploy SY.wf-rules may ask an engine before 0.1.136,
// which ignores count_only and lists every running instance, limit 0: its count is the whole count,
// and past the frame its INTERNAL_ERROR is a refusal, never a count.
func TestSYWfRulesCountsTheRunningInstancesWithACountOnlyList(t *testing.T) {
	for _, tc := range []struct {
		name    string
		engine  wfEngine
		count   int
		refusal string // the node's refusal of the count, if it refuses it
	}{
		{"0.1.136 running 1000", countingEngine(1000), 1000, ""},
		{"0.1.136 running none", countingEngine(0), 0, ""},
		{"before 0.1.136 running 3", listingEngine(3), 3, ""},
		{"before 0.1.136 running 1000", listingEngine(1000), 0, "INTERNAL_ERROR"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			client, requests := wfNodeOnTestRouter(t, tc.engine)
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			count, err := client.CountRunningInstances(ctx, "WF.invoice@motherbee")
			var refusal WfRulesError
			switch {
			case tc.refusal == "" && (err != nil || count != tc.count):
				t.Fatalf("count = %d, %v; want %d", count, err, tc.count)
			case tc.refusal != "" && (!errors.As(err, &refusal) || refusal.Code != tc.refusal || !strings.Contains(refusal.Detail, "message too large")):
				t.Fatalf("count = %d, %v; want the node's refusal %s", count, err, tc.refusal)
			}
			request := <-requests
			if want := map[string]any{"status": "running", "limit": float64(0), "count_only": true}; fmt.Sprint(request) != fmt.Sprint(want) {
				t.Fatalf("WF_LIST_INSTANCES %v, want %v", request, want)
			}
		})
	}
}

// What the count shows the operator: get_status and list_workflows show the 1000 running instances
// of a busy workflow, and a delete without force is refused INSTANCES_ACTIVE with that count; both
// showed no count before (instances_error INTERNAL_ERROR), and the delete answered INSTANCES_UNKNOWN
// (FINDINGS A-284). An engine before 0.1.136 running as many, during a deploy, still refuses the
// count: the delete is refused INSTANCES_UNKNOWN, naming the refusal, and nothing is purged.
func TestABusyWorkflowShowsItsRunningInstancesAndADeleteWithoutForceKeepsThem(t *testing.T) {
	for _, tc := range []struct {
		name   string
		engine wfEngine
		shown  *int   // active_instances, nil when the count is refused
		error  string // instances_error
		code   string // the delete without force's refusal
		says   string
	}{
		{"0.1.136", countingEngine(1000), intPtr(1000), "", "INSTANCES_ACTIVE", "wf node still has 1000 running instances"},
		{"before 0.1.136", listingEngine(1000), nil, "INTERNAL_ERROR: WF_LIST_INSTANCES_RESPONSE not sent: message too large", "INSTANCES_UNKNOWN", "the wf node answered INTERNAL_ERROR: WF_LIST_INSTANCES_RESPONSE not sent: message too large"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			svc := newTestService(t)
			applyWorkflowForDelete(t, svc)
			killed := false
			svc.nodes = &fakeNodeClient{getNodeConfigFunc: deployedWFNodeConfig, killNodeFunc: purgingKill(t, &killed)}
			svc.wfNodes, _ = wfNodeOnTestRouter(t, tc.engine)

			status, err := svc.GetWorkflowStatus(GetStatusRequest{WorkflowName: "invoice"})
			if err != nil || fmt.Sprint(formatOptionalInt(status.WFNode.ActiveInstances)) != fmt.Sprint(formatOptionalInt(tc.shown)) ||
				!strings.HasPrefix(status.WFNode.InstancesError, tc.error) || status.WFNode.Timeout {
				t.Fatalf("get_status: active_instances %v, instances_error %q, wf_node_timeout %v, %v",
					formatOptionalInt(status.WFNode.ActiveInstances), status.WFNode.InstancesError, status.WFNode.Timeout, err)
			}
			listed, err := svc.ListWorkflowStatuses()
			if err != nil || len(listed) != 1 || fmt.Sprint(formatOptionalInt(listed[0].ActiveInstances)) != fmt.Sprint(formatOptionalInt(tc.shown)) {
				t.Fatalf("list_workflows: %+v, %v", listed, err)
			}

			_, err = svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice"})
			refusedDelete(t, svc, err, killed, tc.code, tc.says)
		})
	}
}

// A count that is no whole number from 0 is no count: get_status shows none, and a delete without
// force is refused INSTANCES_UNKNOWN, naming it, and purges nothing. A count of -1, or one past what
// an int holds, was read as a node with no running instances, and the delete purged them.
func TestACountThatIsNoWholeNumberFromZeroIsNoCount(t *testing.T) {
	for _, count := range []any{-1, 1e300, 2.5, "3", nil} {
		t.Run(fmt.Sprint(count), func(t *testing.T) {
			svc := newTestService(t)
			applyWorkflowForDelete(t, svc)
			killed := false
			svc.nodes = &fakeNodeClient{getNodeConfigFunc: deployedWFNodeConfig, killNodeFunc: purgingKill(t, &killed)}
			svc.wfNodes, _ = wfNodeOnTestRouter(t, func(map[string]any) map[string]any { return map[string]any{"ok": true, "count": count} })

			status, err := svc.GetWorkflowStatus(GetStatusRequest{WorkflowName: "invoice"})
			if err != nil || status.WFNode.ActiveInstances != nil {
				t.Fatalf("get_status: active_instances %v, %v", formatOptionalInt(status.WFNode.ActiveInstances), err)
			}
			_, err = svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice"})
			refusedDelete(t, svc, err, killed, "INSTANCES_UNKNOWN",
				"the wf node's instance count (WF_LIST_INSTANCES) failed: wf node returned invalid count", "delete with force to remove it anyway")
		})
	}
}
