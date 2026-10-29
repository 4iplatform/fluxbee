package node

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// recordingAdmin stands for SY.admin: it records each ADMIN_COMMAND and answers with reply.
type recordingAdmin struct {
	requests []fluxbeesdk.AdminRpcRequest
	reply    map[string]any
}

func (r *recordingAdmin) send(_ context.Context, request fluxbeesdk.AdminRpcRequest) (fluxbeesdk.Message, error) {
	r.requests = append(r.requests, request)
	raw, err := json.Marshal(r.reply)
	if err != nil {
		return fluxbeesdk.Message{}, err
	}
	msg := fluxbeesdk.MsgAdminCommandResponse
	return fluxbeesdk.Message{Meta: fluxbeesdk.Meta{MsgType: fluxbeesdk.ADMINKind, Msg: &msg}, Payload: raw}, nil
}

func (r *recordingAdmin) last(t *testing.T) (fluxbeesdk.AdminRpcRequest, map[string]any) {
	t.Helper()
	if len(r.requests) == 0 {
		t.Fatal("nothing was sent to the admin")
	}
	request := r.requests[len(r.requests)-1]
	params, _ := request.Params.(map[string]any)
	return request, params
}

// Every node action of SY.wf-rules goes to SY.admin, for its own hive, as the admin's node actions.
func TestEveryNodeActionGoesToTheAdminForThisHive(t *testing.T) {
	admin := &recordingAdmin{reply: map[string]any{"status": "ok", "payload": map[string]any{
		"status":      "ok",
		"config":      map[string]any{"_system": map[string]any{"runtime_version": "3"}},
		"node_status": map[string]any{"lifecycle_state": "RUNNING", "health_state": "HEALTHY"},
	}}}
	client := &adminNodeClient{send: admin.send, hive: "worker1"}
	ctx := context.Background()
	const node = "WF.invoice@worker1"

	cases := []struct {
		action  string
		timeout time.Duration
		call    func() error
		check   func(params map[string]any)
	}{
		{"get_node_config", nodeQueryTimeout, func() error {
			payload, err := client.GetNodeConfig(ctx, node)
			if err == nil && runtimeVersionFromNodeConfig(payload) != "3" {
				t.Fatalf("payload = %v", payload)
			}
			return err
		}, nil},
		{"set_node_config", nodeQueryTimeout, func() error {
			_, err := client.SetNodeConfig(ctx, node, map[string]any{"gc_retention_days": 7}, &managedRuntimeBinding{
				Runtime: "wf.invoice", RuntimeVersion: "4", RequestedRuntimeVersion: "4", RuntimeBase: "wf.engine", PackagePath: "/dist/wf.invoice/4",
			})
			return err
		}, func(params map[string]any) {
			if params["runtime_version"] != "4" || params["package_path"] != "/dist/wf.invoice/4" || params["replace"] != false || params["config"] == nil {
				t.Fatalf("set_node_config params = %v", params)
			}
		}},
		{"run_node", nodeLaunchTimeout, func() error {
			_, err := client.RunNode(ctx, node, "wf.invoice", "4", map[string]any{"tenant_id": "tnt:1"}, "archi")
			return err
		}, func(params map[string]any) {
			if params["runtime"] != "wf.invoice" || params["runtime_version"] != "4" || params["launched_by"] != "archi" {
				t.Fatalf("run_node params = %v", params)
			}
		}},
		{"restart_node", nodeLaunchTimeout, func() error {
			_, err := client.RestartNode(ctx, node)
			return err
		}, nil},
		{"kill_node", nodeDeleteTimeout, func() error {
			_, err := client.KillNode(ctx, node, true, true)
			return err
		}, func(params map[string]any) {
			if params["force"] != true || params["purge_instance"] != true {
				t.Fatalf("kill_node params = %v", params)
			}
		}},
		{"get_node_status", nodeQueryTimeout, func() error {
			status, err := client.GetNodeStatus(ctx, node)
			if err == nil && status["lifecycle_state"] != "RUNNING" {
				t.Fatalf("node_status = %v", status)
			}
			return err
		}, nil},
	}
	for _, c := range cases {
		if err := c.call(); err != nil {
			t.Fatalf("%s: %v", c.action, err)
		}
		request, params := admin.last(t)
		if request.AdminTarget != fluxbeesdk.PrimaryAdminNode || request.Target != "worker1" || request.Action != c.action || request.Timeout != c.timeout {
			t.Fatalf("%s: request = %+v", c.action, request)
		}
		if params["node_name"] != node {
			t.Fatalf("%s: node_name = %v", c.action, params["node_name"])
		}
		if c.check != nil {
			c.check(params)
		}
	}
}

func TestRunNodeLeavesLaunchedByToTheAdminWhenNobodyIsKnown(t *testing.T) {
	admin := &recordingAdmin{reply: map[string]any{"status": "ok", "payload": map[string]any{"status": "ok"}}}
	client := &adminNodeClient{send: admin.send, hive: "motherbee"}
	if _, err := client.RunNode(context.Background(), "WF.x@motherbee", "wf.x", "1", nil, " "); err != nil {
		t.Fatal(err)
	}
	if _, params := admin.last(t); params["launched_by"] != nil {
		t.Fatalf("params = %v", params)
	}
}

// A refusal keeps the orchestrator's code: the deploy tells a node that does not exist yet, or a
// package still on its way to a worker, from a real failure.
func TestANodeActionRefusalKeepsTheOrchestratorCode(t *testing.T) {
	admin := &recordingAdmin{reply: map[string]any{
		"status":       "error",
		"error_code":   "RUNTIME_NOT_PRESENT",
		"error_detail": map[string]any{"message": "runtime package wf.x 2 is not on this hive yet"},
		"payload":      map[string]any{"status": "error", "error_code": "RUNTIME_NOT_PRESENT"},
	}}
	client := &adminNodeClient{send: admin.send, hive: "worker1"}
	_, err := client.RunNode(context.Background(), "WF.x@worker1", "wf.x", "2", nil, "operator")
	var actionErr *nodeActionError
	if !errors.As(err, &actionErr) || actionErr.Code != "RUNTIME_NOT_PRESENT" || actionErr.Message != "runtime package wf.x 2 is not on this hive yet" {
		t.Fatalf("err = %#v", err)
	}
	if !runtimeNotSyncedYet(err) {
		t.Fatal("a package still syncing is retried")
	}
}

func TestTheDeployCarriesWhoAskedForIt(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeNodeClient{}
	svc.nodes = fake
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.ApplyWorkflowAndDeploy(ApplyRequest{WorkflowName: "invoice", AutoSpawn: true, TenantID: "tnt:1", LaunchedBy: "archi"}); err != nil {
		t.Fatal(err)
	}
	if fake.runCalls != 1 || fake.launchedBy != "archi" {
		t.Fatalf("run calls = %d, launched_by = %q", fake.runCalls, fake.launchedBy)
	}

	compileReq, err := decodeCompileRequest([]byte(`{"workflow_name": "invoice", "launched_by": "operator"}`))
	if err != nil || compileReq.LaunchedBy != "operator" {
		t.Fatalf("compile_apply keeps the admin's stamp: %+v, %v", compileReq, err)
	}
}

func TestAWFNodeThatIsNotRunningIsNotAskedForItsInstances(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatal(err)
	}
	svc.nodes = &fakeNodeClient{
		getNodeConfigFunc: func(_ context.Context, nodeName string) (map[string]any, error) {
			return map[string]any{"status": "ok", "config": map[string]any{"_system": map[string]any{"runtime_version": "1"}}}, nil
		},
		getNodeStatusFunc: func(_ context.Context, nodeName string) (map[string]any, error) {
			return map[string]any{"lifecycle_state": "STOPPED", "health_state": "UNKNOWN"}, nil
		},
	}
	svc.wfNodes = &fakeWFNodeClient{countRunningInstancesFunc: func(string) (int, error) {
		t.Fatal("a node that is not running is not asked")
		return 0, nil
	}}
	status, err := svc.GetWorkflowStatus(GetStatusRequest{WorkflowName: "invoice"})
	if err != nil {
		t.Fatal(err)
	}
	if !status.WFNode.ConfigExists || status.WFNode.Running || status.WFNode.ActiveInstances != nil {
		t.Fatalf("snapshot = %#v", status.WFNode)
	}
}

// recordingSender captures what SY.wf-rules sends back. Its first refuse sends fail with err, or
// the way the SDK refuses a frame over the limit when err is nil.
type recordingSender struct {
	sent   []fluxbeesdk.Message
	refuse int
	err    error
}

func (r *recordingSender) Send(msg fluxbeesdk.Message) error {
	if r.refuse > 0 {
		r.refuse--
		if r.err != nil {
			return r.err
		}
		return &fluxbeesdk.MessageTooLargeError{Size: 200 * 1024, Max: 128 * 1024}
	}
	r.sent = append(r.sent, msg)
	return nil
}
func (r *recordingSender) UUID() string     { return "wf-rules-uuid" }
func (r *recordingSender) FullName() string { return "SY.wf-rules@motherbee" }

// Workflow operations are user actions: SY.wf-rules takes them from SY.admin only, by the
// router-stamped sender, so another node cannot have it deploy or delete WF nodes (A-68).
func TestWorkflowOperationsComeFromTheAdminOnly(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatal(err)
	}
	killed := false
	svc.nodes = &fakeNodeClient{
		getNodeConfigFunc: func(_ context.Context, nodeName string) (map[string]any, error) {
			return map[string]any{"status": "ok", "config": map[string]any{}}, nil
		},
		killNodeFunc: func(_ context.Context, nodeName string, force, purge bool) (map[string]any, error) {
			killed = true
			return map[string]any{"status": "ok"}, nil
		},
	}
	sender := &recordingSender{}
	svc.sender = sender
	request := func(src string) fluxbeesdk.Message {
		action := "delete_workflow"
		msg := fluxbeesdk.Message{
			Routing: fluxbeesdk.Routing{Src: "caller-uuid", TraceID: "t-1"},
			Meta:    fluxbeesdk.Meta{MsgType: "command", Action: &action},
			Payload: json.RawMessage(`{"workflow_name":"invoice","force":true}`),
		}
		if src != "" {
			msg.Routing.SrcL2Name = &src
		}
		return msg
	}
	reply := func() map[string]any {
		var payload map[string]any
		if err := json.Unmarshal(sender.sent[len(sender.sent)-1].Payload, &payload); err != nil {
			t.Fatal(err)
		}
		return payload
	}

	for _, src := range []string{"", "AI.evil@motherbee", "SY.admin@worker1", "SY.wf-rules@motherbee"} {
		svc.handleMessage(request(src))
		if got := reply(); got["error_code"] != "UNAUTHORIZED" || killed {
			t.Fatalf("from %q: reply = %v, killed = %v", src, got, killed)
		}
	}
	if !svc.store.WorkflowExists("invoice") {
		t.Fatal("a refused delete changes nothing")
	}

	svc.handleMessage(request(fluxbeesdk.PrimaryAdminNode))
	if got := reply(); got["status"] == "error" || !killed {
		t.Fatalf("from the admin: reply = %v, killed = %v", got, killed)
	}
}

// A WF node whose config is not in use answers its WF_* requests with node_not_configured: that is
// an answer, not a timeout, and a delete without force says what the node said (A-68).
func TestAWFNodeThatCannotCountItsInstancesSaysWhy(t *testing.T) {
	if code, detail := wfControlError(map[string]any{"ok": false, "error": "node_not_configured", "detail": "config not in use"}); code != "node_not_configured" || detail != "config not in use" {
		t.Fatalf("string form: %q %q", code, detail)
	}
	if code, detail := wfControlError(map[string]any{"ok": false, "error": map[string]any{"code": "x", "message": "y"}}); code != "x" || detail != "y" {
		t.Fatalf("object form: %q %q", code, detail)
	}

	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatal(err)
	}
	svc.nodes = &fakeNodeClient{
		getNodeConfigFunc: func(_ context.Context, nodeName string) (map[string]any, error) {
			return map[string]any{"status": "ok", "config": map[string]any{}}, nil
		},
		getNodeStatusFunc: func(_ context.Context, nodeName string) (map[string]any, error) {
			return map[string]any{"lifecycle_state": "RUNNING", "health_state": "DEGRADED"}, nil
		},
	}
	svc.wfNodes = &fakeWFNodeClient{countRunningInstancesFunc: func(string) (int, error) {
		return 0, WfRulesError{Code: "node_not_configured", Detail: "the WF node's config is not in use"}
	}}
	status, err := svc.GetWorkflowStatus(GetStatusRequest{WorkflowName: "invoice"})
	if err != nil {
		t.Fatal(err)
	}
	if status.WFNode.Timeout || !strings.Contains(status.WFNode.InstancesError, "node_not_configured") || status.WFNode.HealthState != "DEGRADED" {
		t.Fatalf("snapshot = %#v", status.WFNode)
	}
	_, err = svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice"})
	if err == nil || !strings.Contains(err.Error(), "INSTANCES_UNKNOWN") || !strings.Contains(err.Error(), "node_not_configured") {
		t.Fatalf("delete without force: %v", err)
	}
}

// On a worker the package travels through Syncthing: the deploy asks the motherbee and this hive
// to scan now before it rebinds or launches the node, and nudges this hive while the package is
// still missing. On the motherbee the package is already local (A-68, 0.1.71 gate).
func TestAWorkerDeployHasTheMotherbeeAnnounceItsPackage(t *testing.T) {
	for _, hive := range []string{"motherbee", "worker1"} {
		svc := newTestService(t)
		svc.cfg.HiveID = hive
		attempts := 0
		svc.nodes = &fakeNodeClient{runNodeFunc: func(context.Context, string, string, string, map[string]any, string) (map[string]any, error) {
			attempts++
			if attempts < 2 {
				return nil, &nodeActionError{Status: "error", Code: "RUNTIME_NOT_PRESENT"}
			}
			return map[string]any{"status": "ok"}, nil
		}}
		if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
			t.Fatal(err)
		}
		result, err := svc.ApplyWorkflowAndDeploy(ApplyRequest{WorkflowName: "invoice", AutoSpawn: true, TenantID: "tnt:1"})
		if err != nil || result.WFNode.Action != "restarted" {
			t.Fatalf("%s: result = %#v, err = %v", hive, result, err)
		}
		hints := strings.Join(svc.admin.(*fakeAdminClient).syncHints, ",")
		want := ""
		if hive == "worker1" {
			want = "motherbee/5000,motherbee/0"
		}
		if hints != want {
			t.Fatalf("%s: sync hints = %q, want %q", hive, hints, want)
		}
	}
}
