package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

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
	if result.PackagesLeft == nil || len(result.PackagesLeft) != 0 || result.Warning != "" {
		t.Fatalf("a purge that removed every version leaves none and warns of none: %#v", result)
	}
}

// deleteWorkflowCommand is SY.admin's delete_workflow for workflow.
func deleteWorkflowCommand(t *testing.T, workflow string, force bool) fluxbeesdk.Message {
	t.Helper()
	action, admin := "delete_workflow", fluxbeesdk.PrimaryAdminNode
	payload, err := json.Marshal(map[string]any{"workflow_name": workflow, "force": force})
	if err != nil {
		t.Fatal(err)
	}
	return fluxbeesdk.Message{
		Routing: fluxbeesdk.Routing{Src: "admin-uuid", SrcL2Name: &admin, TraceID: "t-delete"},
		Meta:    fluxbeesdk.Meta{MsgType: "command", Action: &action},
		Payload: payload,
	}
}

// deleteAnswer is SY.wf-rules' answer to a delete_workflow, as SY.admin reads it.
type deleteAnswer struct {
	Status       string             `json:"status"`
	Deleted      bool               `json:"deleted"`
	WFNode       WFNodeActionResult `json:"wf_node"`
	PackagesLeft []PackageLeft      `json:"packages_left"`
	Warning      string             `json:"warning"`
	ErrorCode    string             `json:"error_code"`
	ErrorDetail  string             `json:"error_detail"`
}

// usageUnknown is SY.admin's refusal of a removal while some hive's nodes cannot be read (FINDINGS
// A-281), as SY.wf-rules reads it: its code and detail, and its payload, which names the hives not
// read and says asking again may pass.
func usageUnknown(detail string, unanswered ...map[string]any) *adminActionError {
	hives := []any{}
	for _, hive := range unanswered {
		hives = append(hives, hive)
	}
	return &adminActionError{Status: "error", Code: "RUNTIME_USAGE_UNKNOWN", Message: detail, Payload: map[string]any{
		"status":       "error",
		"action":       "remove_runtime_version",
		"error_code":   "RUNTIME_USAGE_UNKNOWN",
		"error_detail": detail,
		"payload": map[string]any{
			"usage_global_visible": map[string]any{"in_use": nil},
			"unanswered_hives":     hives,
			"retryable":            true,
		},
	}}
}

// SY.admin removes a version only once every hive that may run it was read (FINDINGS A-281): with a
// worker that does not answer, it refuses the delete's purge, RUNTIME_USAGE_UNKNOWN. The WF node and
// the state are gone by then, so the delete is done and answers ok: packages_left names every
// version the purge did not remove, the one SY.admin refused with its refusal (the hives it did not
// read, and that asking again may pass) and each it had still to remove as PURGE_STOPPED, since what
// refused one refuses the next, and the warning says the workflow is deleted and what removes them.
// Before 0.1.133 the delete answered PACKAGE_PUBLISH_FAILED, though nothing could be asked again
// (WORKFLOW_NOT_FOUND).
func TestADeleteWhosePurgeIsRefusedIsDoneAndNamesThePackagesLeft(t *testing.T) {
	svc := newTestService(t)
	for range 2 {
		applyWorkflowForDelete(t, svc)
	}
	admin := svc.admin.(*fakeAdminClient)
	admin.removedVersions = nil
	const refusal = "cannot tell whether runtime 'wf.invoice' version '0.0.1' is in use: the nodes of worker1 (UNREACHABLE: hive 'worker1' is stale in the LSA) were not read, and nothing was removed. Ask again once each hive answers; a hive gone for good is removed with remove_hive first"
	worker1 := map[string]any{"hive": "worker1", "reason": "UNREACHABLE", "detail": "hive 'worker1' is stale in the LSA"}
	admin.removeErr = map[string]error{"0.0.1": usageUnknown(refusal, worker1)}
	killed := false
	svc.nodes = &fakeNodeClient{getNodeConfigFunc: deployedWFNodeConfig, killNodeFunc: purgingKill(t, &killed)}
	svc.wfNodes = &fakeWFNodeClient{countRunningInstancesFunc: func(string) (int, error) { return 0, nil }}
	sender := &recordingSender{}
	svc.sender = sender

	svc.handleMessage(deleteWorkflowCommand(t, "invoice", false))

	if len(sender.sent) != 1 {
		t.Fatalf("want one answer, got %d", len(sender.sent))
	}
	var answer deleteAnswer
	if err := json.Unmarshal(sender.sent[0].Payload, &answer); err != nil {
		t.Fatal(err)
	}
	if answer.Status != "ok" || !answer.Deleted || answer.WFNode.Action != "deleted" || answer.ErrorCode != "" || !killed {
		t.Fatalf("the delete is done: answer %+v, killed %v", answer, killed)
	}
	if svc.store.WorkflowExists("invoice") {
		t.Fatal("the workflow's state goes")
	}
	want := []PackageLeft{
		{Runtime: "wf.invoice", Version: "0.0.1", ErrorCode: "RUNTIME_USAGE_UNKNOWN", Detail: refusal, Retryable: true, UnansweredHives: []any{worker1}},
		{Runtime: "wf.invoice", Version: "0.0.2", ErrorCode: "PURGE_STOPPED", Detail: "the purge stopped at wf.invoice 0.0.1 (RUNTIME_USAGE_UNKNOWN) before removing it", Retryable: true},
	}
	if !reflect.DeepEqual(answer.PackagesLeft, want) {
		t.Fatalf("packages_left:\n got %+v\nwant %+v", answer.PackagesLeft, want)
	}
	if got := strings.Join(admin.removedVersions, ","); got != "wf.invoice@0.0.1" {
		t.Fatalf("the purge stops at the first refusal: asked %s", got)
	}
	const warning = "workflow invoice is deleted, but its purge did not remove 2 versions of wf.invoice from the motherbee's dist (packages_left): it stopped at 0.0.1, RUNTIME_USAGE_UNKNOWN. remove_runtime_version removes each version once every hive answers and no node runs it, and a later deploy of invoice purges them too."
	if answer.Warning != warning {
		t.Fatalf("warning:\n got %q\nwant %q", answer.Warning, warning)
	}

	// The delete happened: asked again, the workflow is not found.
	svc.handleMessage(deleteWorkflowCommand(t, "invoice", false))
	if err := json.Unmarshal(sender.sent[1].Payload, &answer); err != nil || answer.ErrorCode != "WORKFLOW_NOT_FOUND" {
		t.Fatalf("asked again: %+v, %v", answer, err)
	}
}

// A removal SY.admin does not answer in time is no failure of the delete either: packages_left names
// it TIMEOUT (SY.admin may still remove it) and each version after it, PURGE_STOPPED. The delete's
// removal waits up to runtimeRemovalTimeout (35 s), long enough for SY.admin's read of every hive's
// use (30 s at most), where it waited 5 s before 0.1.133.
func TestADeleteWhosePurgeTimesOutIsDoneAndNamesThePackagesLeft(t *testing.T) {
	svc := newTestService(t)
	for range 2 {
		applyWorkflowForDelete(t, svc)
	}
	admin := svc.admin.(*fakeAdminClient)
	admin.removedVersions, admin.removeWaits = nil, nil
	admin.removeErr = map[string]error{
		"0.0.1": &fluxbeesdk.RpcError{Kind: fluxbeesdk.RpcErrTimeout, TraceID: "t-remove", Target: fluxbeesdk.PrimaryAdminNode, Verb: "ADMIN_COMMAND"},
	}
	killed := false
	svc.nodes = &fakeNodeClient{getNodeConfigFunc: deployedWFNodeConfig, killNodeFunc: purgingKill(t, &killed)}

	result, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice", Force: true})
	if err != nil || !result.Deleted || result.WFNode.Action != "deleted" || !killed || svc.store.WorkflowExists("invoice") {
		t.Fatalf("the delete is done: result %#v, err %v, killed %v", result, err, killed)
	}
	if len(result.PackagesLeft) != 2 {
		t.Fatalf("packages_left = %+v", result.PackagesLeft)
	}
	timedOut := result.PackagesLeft[0]
	if timedOut.Runtime != "wf.invoice" || timedOut.Version != "0.0.1" || timedOut.ErrorCode != "TIMEOUT" ||
		!strings.Contains(timedOut.Detail, "no answer in time (remove wf.invoice 0.0.1 on SY.admin@motherbee: rpc timeout") ||
		!strings.Contains(timedOut.Detail, "SY.admin may still remove it") {
		t.Fatalf("the version that timed out: %+v", timedOut)
	}
	if !timedOut.Retryable || timedOut.UnansweredHives != nil {
		t.Fatalf("a removal that got no answer may pass when asked again: %+v", timedOut)
	}
	notRemoved := PackageLeft{Runtime: "wf.invoice", Version: "0.0.2", ErrorCode: "PURGE_STOPPED", Detail: "the purge stopped at wf.invoice 0.0.1 (TIMEOUT) before removing it", Retryable: true}
	if !reflect.DeepEqual(result.PackagesLeft[1], notRemoved) || !strings.Contains(result.Warning, "it stopped at 0.0.1, TIMEOUT") {
		t.Fatalf("after it: %+v, warning %q", result.PackagesLeft[1], result.Warning)
	}
	if len(admin.removeWaits) != 1 || admin.removeWaits[0] <= 30*time.Second || admin.removeWaits[0] > runtimeRemovalTimeout {
		t.Fatalf("the removal waits up to %s for SY.admin, could wait %v", runtimeRemovalTimeout, admin.removeWaits)
	}
}

// A purge that cannot list the workflow's versions removes none and says so: the delete is done,
// and packages_left names the runtime with no version, DIST_UNREADABLE, and why, which asking again
// does not change. Its warning says to fix this hive's dist first: until then a compile of the name
// that names no version is refused too (highestPublishedVersion), so neither every hive answering
// nor a later deploy removes anything, as the warning of a refused removal says.
func TestADeleteThatCannotListItsPackagesSaysSo(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	// wf.invoice is no directory, so it cannot be listed.
	if err := os.MkdirAll(svc.cfg.DistRuntimeRoot, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice"), []byte("not a directory"), 0o644); err != nil {
		t.Fatal(err)
	}

	result, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice"})
	if err != nil || !result.Deleted || svc.store.WorkflowExists("invoice") {
		t.Fatalf("the delete is done: result %#v, err %v", result, err)
	}
	left := result.PackagesLeft
	if len(left) != 1 || left[0].Runtime != "wf.invoice" || left[0].Version != "" || left[0].ErrorCode != "DIST_UNREADABLE" ||
		!strings.Contains(left[0].Detail, "cannot list the versions of wf.invoice in this hive's dist") || left[0].Retryable {
		t.Fatalf("packages_left = %+v", left)
	}
	const warning = "workflow invoice is deleted, but its purge could not list the versions of wf.invoice in this hive's dist (packages_left: DIST_UNREADABLE), so it removed none: fix this hive's dist/runtimes/wf.invoice first (a compile of invoice that names no version is refused until it can be listed); then remove_runtime_version removes each version of wf.invoice the motherbee's get_runtime lists, or a deploy of invoice purges them."
	if result.Warning != warning {
		t.Fatalf("warning:\n got %q\nwant %q", result.Warning, warning)
	}
	if removed := svc.admin.(*fakeAdminClient).removedVersions; len(removed) != 0 {
		t.Fatalf("nothing is removed, asked %v", removed)
	}
	// What the warning says of a compile: refused while the dist cannot be listed.
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err == nil ||
		!strings.Contains(err.Error(), "cannot list the versions of wf.invoice in this hive's dist") {
		t.Fatalf("a compile of the name while the dist cannot be listed: %v", err)
	}
}

// On a worker this hive's dist is a Syncthing copy of the motherbee's, which lists a version
// published there 10-60 s later. A delete within that window found no wf.<workflow> here, removed
// nothing and answered packages_left [], while the versions stayed in the motherbee's dist, named
// nowhere (the review of A-281). The delete also removes the versions its slots named, read before
// its state goes, the current last (the motherbee's current, which SY.admin removes only once no
// other is left); one the motherbee does not hold, a compile never applied, SY.admin answers
// RUNTIME_VERSION_NOT_FOUND, which counts as removed.
func TestADeleteOnAWorkerWhoseCopyLagsRemovesTheVersionsItsSlotsName(t *testing.T) {
	svc := newTestService(t)
	svc.cfg.HiveID = "worker1"
	for range 2 {
		applyWorkflowForDelete(t, svc)
	}
	// Staged and never applied: SY.admin never published 0.0.3.
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatal(err)
	}
	// This hive's copy of the motherbee's dist does not list wf.invoice yet: neither its directory
	// nor its manifest's entry has arrived.
	if err := os.RemoveAll(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice")); err != nil {
		t.Fatal(err)
	}
	manifestPath := filepath.Join(svc.cfg.DistRuntimeRoot, "manifest.json")
	manifest, err := loadRuntimeManifest(manifestPath)
	if err != nil {
		t.Fatal(err)
	}
	delete(manifest.Runtimes, "wf.invoice")
	if err := writeRuntimeManifest(manifestPath, manifest); err != nil {
		t.Fatal(err)
	}
	admin := svc.admin.(*fakeAdminClient)
	admin.removedVersions = nil
	admin.removeErr = map[string]error{
		"0.0.3": &adminActionError{Status: "error", Code: "RUNTIME_VERSION_NOT_FOUND", Message: "version '0.0.3' not available for runtime 'wf.invoice'"},
	}
	killed := false
	svc.nodes = &fakeNodeClient{getNodeConfigFunc: deployedWFNodeConfig, killNodeFunc: purgingKill(t, &killed)}

	result, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice", Force: true})
	if err != nil || !result.Deleted || !killed || svc.store.WorkflowExists("invoice") {
		t.Fatalf("the delete is done: result %#v, err %v, killed %v", result, err, killed)
	}
	if want := "wf.invoice@0.0.3,wf.invoice@0.0.1,wf.invoice@0.0.2"; strings.Join(admin.removedVersions, ",") != want {
		t.Fatalf("want the slots' versions removed through SY.admin, staged, backup and current last (%s), asked %v", want, admin.removedVersions)
	}
	if result.PackagesLeft == nil || len(result.PackagesLeft) != 0 || result.Warning != "" {
		t.Fatalf("every version went: packages_left %+v, warning %q", result.PackagesLeft, result.Warning)
	}
}

// A workflow deleted while SY.admin refused its purge leaves wf.<workflow> 0.0.1 and 0.0.2 in the
// motherbee's dist, and its state goes. Deployed again under the same name, it stages 3, above them:
// before 0.1.133 it staged 1 again, published 0.0.1 over the one left there, and SY.admin refused it
// (VERSION_CONFLICT, so PACKAGE_PUBLISH_FAILED) until the leftover was removed by hand. Its apply's
// purge then removes the leftovers, which no slot keeps.
func TestAWorkflowDeployedAgainPublishesAboveWhatItsDeleteLeftAndPurgesIt(t *testing.T) {
	svc := newTestService(t)
	for range 2 {
		applyWorkflowForDelete(t, svc)
	}
	admin := svc.admin.(*fakeAdminClient)
	admin.removeErr = map[string]error{
		"0.0.1": &adminActionError{Status: "error", Code: "RUNTIME_USAGE_UNKNOWN", Message: "the nodes of worker1 were not read"},
	}
	result, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice", Force: true})
	if err != nil || len(result.PackagesLeft) != 2 {
		t.Fatalf("the delete leaves both versions: result %#v, err %v", result, err)
	}

	// Every hive answers again, and the workflow comes back with another definition.
	admin.removeErr, admin.removedVersions = nil, nil
	definition := validWorkflowDefinition()
	definition["description"] = "issues invoices, deployed again"
	staged, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: definition})
	if err != nil || staged.Version != 3 {
		t.Fatalf("want version 3 staged, above the versions left, got %#v, %v", staged, err)
	}
	applied, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"})
	if err != nil || applied.Package.Version != "0.0.3" {
		t.Fatalf("want 0.0.3 published, got %#v, %v", applied, err)
	}
	if got := strings.Join(admin.removedVersions, ","); got != "wf.invoice@0.0.1,wf.invoice@0.0.2" {
		t.Fatalf("the apply's purge removes the versions left, asked %s", got)
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
