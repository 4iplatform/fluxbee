package node

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

func TestDeleteWorkflowRejectsUnknownInstancesWhenWFDoesNotRespond(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatalf("ApplyWorkflow: %v", err)
	}
	svc.nodes = &fakeNodeClient{
		getNodeConfigFunc: func(_ context.Context, nodeName string) (map[string]any, error) {
			return map[string]any{
				"status": "ok",
				"config": map[string]any{
					"_system": map[string]any{"runtime_version": "1"},
				},
			}, nil
		},
	}
	svc.wfNodes = &fakeWFNodeClient{
		countRunningInstancesFunc: func(nodeName string) (int, error) {
			return 0, context.DeadlineExceeded
		},
	}

	_, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice", Force: false})
	if err == nil || !strings.Contains(err.Error(), "INSTANCES_UNKNOWN") {
		t.Fatalf("expected INSTANCES_UNKNOWN, got %v", err)
	}
}

func TestDeleteWorkflowRejectsActiveInstances(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatalf("ApplyWorkflow: %v", err)
	}
	svc.nodes = &fakeNodeClient{
		getNodeConfigFunc: func(_ context.Context, nodeName string) (map[string]any, error) {
			return map[string]any{
				"status": "ok",
				"config": map[string]any{
					"_system": map[string]any{"runtime_version": "1"},
				},
			}, nil
		},
	}
	svc.wfNodes = &fakeWFNodeClient{
		countRunningInstancesFunc: func(nodeName string) (int, error) {
			return 2, nil
		},
	}

	_, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice", Force: false})
	if err == nil || !strings.Contains(err.Error(), "INSTANCES_ACTIVE") {
		t.Fatalf("expected INSTANCES_ACTIVE, got %v", err)
	}
}

func TestDeleteWorkflowForceRemovesStateAndPackages(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatalf("ApplyWorkflow: %v", err)
	}
	killed := false
	svc.nodes = &fakeNodeClient{
		getNodeConfigFunc: func(_ context.Context, nodeName string) (map[string]any, error) {
			return map[string]any{
				"status": "ok",
				"config": map[string]any{
					"_system": map[string]any{"runtime_version": "1"},
				},
			}, nil
		},
		killNodeFunc: func(_ context.Context, nodeName string, force, purgeInstance bool) (map[string]any, error) {
			killed = true
			if !force || !purgeInstance {
				t.Fatalf("expected force=true purge_instance=true")
			}
			return map[string]any{"status": "ok"}, nil
		},
	}
	svc.wfNodes = &fakeWFNodeClient{
		countRunningInstancesFunc: func(nodeName string) (int, error) {
			return 5, nil
		},
	}

	result, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice", Force: true})
	if err != nil {
		t.Fatalf("DeleteWorkflow: %v", err)
	}
	if !result.Deleted || !killed {
		t.Fatalf("unexpected result %#v killed=%v", result, killed)
	}
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice")); !os.IsNotExist(err) {
		t.Fatalf("expected local state removed, err=%v", err)
	}
	// Its packages are removed through SY.admin, which owns the dist tree (A-68).
	if removed := svc.admin.(*fakeAdminClient).removedVersions; len(removed) != 1 || removed[0] != "wf.invoice@0.0.1" {
		t.Fatalf("want the workflow's package removed through the admin, got %v", removed)
	}
}

// applyWorkflowForDelete compiles and applies workflow "invoice" on this hive.
func applyWorkflowForDelete(t *testing.T, svc *Service) {
	t.Helper()
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatalf("ApplyWorkflow: %v", err)
	}
}

// deployedWFNodeConfig is get_node_config's answer for a WF node that exists.
func deployedWFNodeConfig(context.Context, string) (map[string]any, error) {
	return map[string]any{"status": "ok", "config": map[string]any{"_system": map[string]any{"runtime_version": "0.0.1"}}}, nil
}

// purgingKill is a kill_node that records it ran, with the force and the purge a delete asks.
func purgingKill(t *testing.T, killed *bool) func(context.Context, string, bool, bool) (map[string]any, error) {
	return func(_ context.Context, _ string, force, purgeInstance bool) (map[string]any, error) {
		*killed = true
		if !force || !purgeInstance {
			t.Fatalf("a delete kills with force and purge_instance, got force=%v purge_instance=%v", force, purgeInstance)
		}
		return map[string]any{"status": "ok"}, nil
	}
}

// refusedDelete checks a delete refused with code, its detail saying each of says, that changed
// nothing: the node was not killed, and the workflow and its packages are there.
func refusedDelete(t *testing.T, svc *Service, err error, killed bool, code string, says ...string) {
	t.Helper()
	var refusal WfRulesError
	if !errors.As(err, &refusal) || refusal.Code != code {
		t.Fatalf("want %s, got %v", code, err)
	}
	for _, text := range says {
		if !strings.Contains(refusal.Detail, text) {
			t.Fatalf("%s's detail %q does not say %q", code, refusal.Detail, text)
		}
	}
	removed := svc.admin.(*fakeAdminClient).removedVersions
	if killed || !svc.store.WorkflowExists("invoice") || len(removed) != 0 {
		t.Fatalf("a refused delete changes nothing: killed=%v, workflow kept=%v, packages removed=%v", killed, svc.store.WorkflowExists("invoice"), removed)
	}
}

// A WF node that is not RUNNING keeps its instances in its store, and only the node counts them:
// a delete without force refuses it, naming its state, and changes nothing; with force it goes
// anyway. A delete a second after a restart read STARTING and purged the live instances (A-270).
func TestDeleteWorkflowWithoutForceRefusesAWFNodeThatIsNotRunning(t *testing.T) {
	for _, tc := range []struct{ state, says string }{
		{"STARTING", "the wf node is STARTING, not RUNNING"},
		{"STOPPED", "the wf node is STOPPED, not RUNNING"},
		{"FAILED", "the wf node is FAILED, not RUNNING"},
		{"UNKNOWN", "the wf node is UNKNOWN, not RUNNING"},
		{"", "the wf node's status names no lifecycle_state"},
	} {
		t.Run(fmt.Sprintf("state %q", tc.state), func(t *testing.T) {
			svc := newTestService(t)
			applyWorkflowForDelete(t, svc)
			killed := false
			svc.nodes = &fakeNodeClient{
				getNodeConfigFunc: deployedWFNodeConfig,
				getNodeStatusFunc: func(context.Context, string) (map[string]any, error) {
					return map[string]any{"lifecycle_state": tc.state, "health_state": "UNKNOWN"}, nil
				},
				killNodeFunc: purgingKill(t, &killed),
			}
			svc.wfNodes = &fakeWFNodeClient{countRunningInstancesFunc: func(string) (int, error) {
				t.Fatal("a node that is not running is not asked for its instances")
				return 0, nil
			}}

			_, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice"})
			refusedDelete(t, svc, err, killed, "INSTANCES_UNKNOWN", tc.says, "delete with force to remove it anyway")

			result, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice", Force: true})
			if err != nil || !result.Deleted || result.WFNode.Action != "deleted" || !killed {
				t.Fatalf("with force: result=%#v, err=%v, killed=%v", result, err, killed)
			}
			if svc.store.WorkflowExists("invoice") {
				t.Fatal("with force the workflow goes")
			}
		})
	}
}

// A status read that failed says nothing of the node's instances either: a delete without force
// refuses, naming the read and how it failed (A-270), as it refused one that timed out.
func TestDeleteWorkflowWithoutForceRefusesWhenTheWFNodeStatusCannotBeRead(t *testing.T) {
	for _, tc := range []struct {
		name string
		err  error
		says string
	}{
		{"refused", &nodeActionError{Status: "error", Code: "NODE_STATUS_GET_FAILED", Message: "worker1 did not answer"}, "the wf node's status read (get_node_status) failed: NODE_STATUS_GET_FAILED: worker1 did not answer"},
		{"malformed", fmt.Errorf("get_node_status answered without node_status"), "the wf node's status read (get_node_status) failed: get_node_status answered without node_status"},
		{"timed out", context.DeadlineExceeded, "the wf node's status read (get_node_status) timed out"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			svc := newTestService(t)
			applyWorkflowForDelete(t, svc)
			killed := false
			svc.nodes = &fakeNodeClient{
				getNodeConfigFunc: deployedWFNodeConfig,
				getNodeStatusFunc: func(context.Context, string) (map[string]any, error) { return nil, tc.err },
				killNodeFunc:      purgingKill(t, &killed),
			}
			svc.wfNodes = &fakeWFNodeClient{}

			_, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice"})
			refusedDelete(t, svc, err, killed, "INSTANCES_UNKNOWN", tc.says, "delete with force to remove it anyway")
		})
	}
}

// A count of a RUNNING node that got no answer, or one that is no count, is named as such: the
// refusal said "the wf node status timed out" for both, though the status had answered RUNNING
// (the review of A-270). get_status still shows either as wf_node_timeout.
func TestDeleteWorkflowWithoutForceNamesTheInstanceCountThatFailed(t *testing.T) {
	for _, tc := range []struct {
		name string
		err  error
		says string
	}{
		{"timed out", context.DeadlineExceeded, "the wf node's instance count (WF_LIST_INSTANCES) got no answer within 2s"},
		{"unreachable", &fluxbeesdk.RpcError{Kind: fluxbeesdk.RpcErrTerminalTransport, Message: "UNREACHABLE (NODE_NOT_FOUND)"}, "the wf node's instance count (WF_LIST_INSTANCES) failed: rpc terminal transport error: UNREACHABLE (NODE_NOT_FOUND)"},
		{"no count", fmt.Errorf("wf node response missing count"), "the wf node's instance count (WF_LIST_INSTANCES) failed: wf node response missing count"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			svc := newTestService(t)
			applyWorkflowForDelete(t, svc)
			killed := false
			svc.nodes = &fakeNodeClient{getNodeConfigFunc: deployedWFNodeConfig, killNodeFunc: purgingKill(t, &killed)}
			svc.wfNodes = &fakeWFNodeClient{countRunningInstancesFunc: func(string) (int, error) { return 0, tc.err }}

			_, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice"})
			refusedDelete(t, svc, err, killed, "INSTANCES_UNKNOWN", tc.says, "delete with force to remove it anyway")

			status, err := svc.GetWorkflowStatus(GetStatusRequest{WorkflowName: "invoice"})
			if err != nil || !status.WFNode.Timeout || !status.WFNode.Running || status.WFNode.ActiveInstances != nil || status.WFNode.InstancesError != "" {
				t.Fatalf("get_status: snapshot=%#v, err=%v", status, err)
			}
		})
	}
}

// A config read that got no answer cannot tell whether the WF node exists: the delete is refused
// with force too, and changes nothing. A forced delete took the node for one with no config, left
// it running with its instances, and dropped the workflow and its packages (the review of A-270).
func TestDeleteWorkflowRefusesWhenTheWFNodeConfigReadTimesOut(t *testing.T) {
	for _, force := range []bool{false, true} {
		t.Run(fmt.Sprintf("force %v", force), func(t *testing.T) {
			svc := newTestService(t)
			applyWorkflowForDelete(t, svc)
			killed := false
			svc.nodes = &fakeNodeClient{
				getNodeConfigFunc: func(context.Context, string) (map[string]any, error) { return nil, context.DeadlineExceeded },
				getNodeStatusFunc: func(context.Context, string) (map[string]any, error) {
					t.Fatal("a node whose config read timed out is not asked for its status")
					return nil, nil
				},
				killNodeFunc: purgingKill(t, &killed),
			}

			_, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice", Force: force})
			refusedDelete(t, svc, err, killed, "ORCHESTRATOR_ERROR",
				"cannot tell whether WF.invoice@motherbee exists: its config read (get_node_config) timed out; nothing was changed, try again")
			if strings.Contains(err.Error(), "force") {
				t.Fatalf("force would not remove it either: %v", err)
			}
		})
	}
}

// Only a count lets a delete without force go on: a RUNNING node whose instances were not counted
// is refused too (A-270).
func TestDeleteWorkflowWithoutForceRefusesAWFNodeWhoseInstancesWereNotCounted(t *testing.T) {
	svc := newTestService(t)
	applyWorkflowForDelete(t, svc)
	killed := false
	svc.nodes = &fakeNodeClient{getNodeConfigFunc: deployedWFNodeConfig, killNodeFunc: purgingKill(t, &killed)}
	svc.wfNodes = nil

	_, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice"})
	refusedDelete(t, svc, err, killed, "INSTANCES_UNKNOWN", "the wf node's running instances were not counted")
}

// A RUNNING node that counted no running instances is removed without force, with its purge, and
// the workflow's state and packages go.
func TestDeleteWorkflowWithoutForceRemovesARunningWFNodeWithNoInstances(t *testing.T) {
	svc := newTestService(t)
	applyWorkflowForDelete(t, svc)
	killed := false
	svc.nodes = &fakeNodeClient{getNodeConfigFunc: deployedWFNodeConfig, killNodeFunc: purgingKill(t, &killed)}
	svc.wfNodes = &fakeWFNodeClient{countRunningInstancesFunc: func(string) (int, error) { return 0, nil }}

	result, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice"})
	if err != nil || !result.Deleted || result.WFNode.Action != "deleted" || !killed {
		t.Fatalf("result=%#v, err=%v, killed=%v", result, err, killed)
	}
	if svc.store.WorkflowExists("invoice") {
		t.Fatal("the workflow's state goes")
	}
	if removed := svc.admin.(*fakeAdminClient).removedVersions; len(removed) != 1 || removed[0] != "wf.invoice@0.0.1" {
		t.Fatalf("want the workflow's package removed through the admin, got %v", removed)
	}
}

// A workflow whose WF node has no config (never deployed, or already purged) has no instances to
// lose: a delete without force removes it, with no node step, and asks no status.
func TestDeleteWorkflowWithoutForceRemovesAWorkflowWithNoWFNode(t *testing.T) {
	svc := newTestService(t)
	applyWorkflowForDelete(t, svc)
	svc.nodes = &fakeNodeClient{
		getNodeStatusFunc: func(context.Context, string) (map[string]any, error) {
			t.Fatal("a node with no config is not asked for its status")
			return nil, nil
		},
		killNodeFunc: func(context.Context, string, bool, bool) (map[string]any, error) {
			t.Fatal("a node with no config is not killed")
			return nil, nil
		},
	}

	result, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice"})
	if err != nil || !result.Deleted || result.WFNode.Action != "none" || result.WFNode.Reason != "node not managed" {
		t.Fatalf("result=%#v, err=%v", result, err)
	}
	if svc.store.WorkflowExists("invoice") {
		t.Fatal("the workflow's state goes")
	}
}
