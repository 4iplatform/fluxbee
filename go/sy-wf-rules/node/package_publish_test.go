package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strings"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

func TestPublishWorkflowPackageWritesDefaultConfigTemplate(t *testing.T) {
	svc := newTestService(t)
	fakeAdmin := &fakeAdminClient{
		publishRuntimePackageFunc: func(_ context.Context, packageFiles map[string]string) (*PackagePublishResult, error) {
			rawConfig, ok := packageFiles["config/default-config.json"]
			if !ok {
				t.Fatalf("missing default config in package files")
			}
			var decoded map[string]any
			if err := json.Unmarshal([]byte(rawConfig), &decoded); err != nil {
				t.Fatalf("Unmarshal(default-config.json): %v", err)
			}
			if decoded["sy_timer_l2_name"] != "SY.timer@motherbee" {
				t.Fatalf("unexpected sy_timer_l2_name %#v", decoded["sy_timer_l2_name"])
			}
			if int(decoded["gc_retention_days"].(float64)) != defaultWFGCRetentionDays {
				t.Fatalf("unexpected gc_retention_days %#v", decoded["gc_retention_days"])
			}
			if int(decoded["gc_interval_seconds"].(float64)) != defaultWFGCIntervalSeconds {
				t.Fatalf("unexpected gc_interval_seconds %#v", decoded["gc_interval_seconds"])
			}
			rawPackage, ok := packageFiles["package.json"]
			if !ok {
				t.Fatalf("missing package.json in package files")
			}
			var packageMeta map[string]any
			if err := json.Unmarshal([]byte(rawPackage), &packageMeta); err != nil {
				t.Fatalf("Unmarshal(package.json): %v", err)
			}
			return &PackagePublishResult{
				RuntimeName: fmt.Sprintf("%v", packageMeta["name"]),
				Version:     fmt.Sprintf("%v", packageMeta["version"]),
				PackagePath: filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", "0.0.1"),
			}, nil
		},
	}
	svc.admin = fakeAdmin
	definitionBytes, err := normalizeDefinitionBytes(validWorkflowDefinition())
	if err != nil {
		t.Fatalf("normalizeDefinitionBytes: %v", err)
	}
	meta := WfRulesMetadata{
		Version:         1,
		Hash:            HashDefinition(definitionBytes),
		WorkflowName:    "invoice",
		WorkflowType:    "invoice",
		WFSchemaVersion: "1",
		CompiledAt:      "2026-04-16T12:00:00Z",
	}

	publish, err := svc.PublishWorkflowPackage("invoice", meta, definitionBytes)
	if err != nil {
		t.Fatalf("PublishWorkflowPackage: %v", err)
	}
	if fakeAdmin.publishCalls != 1 {
		t.Fatalf("expected one admin publish call, got %d", fakeAdmin.publishCalls)
	}
	if publish.RuntimeName != "wf.invoice" || publish.Version != "0.0.1" {
		t.Fatalf("unexpected publish result %#v", publish)
	}
}

func TestPurgeWorkflowPackagesKeepsCurrentBackupAndBoundVersion(t *testing.T) {
	svc := newTestService(t)
	definitionBytes, err := normalizeDefinitionBytes(validWorkflowDefinition())
	if err != nil {
		t.Fatalf("normalizeDefinitionBytes: %v", err)
	}
	for version := uint64(1); version <= 4; version++ {
		meta := WfRulesMetadata{
			Version:         version,
			Hash:            HashDefinition(definitionBytes),
			WorkflowName:    "invoice",
			WorkflowType:    "invoice",
			WFSchemaVersion: "1",
			CompiledAt:      "2026-04-16T12:00:00Z",
		}
		if _, err := svc.PublishWorkflowPackage("invoice", meta, definitionBytes); err != nil {
			t.Fatalf("PublishWorkflowPackage(%d): %v", version, err)
		}
	}
	if err := svc.store.WriteSlot("invoice", "current", definitionBytes, WfRulesMetadata{
		Version:         4,
		Hash:            HashDefinition(definitionBytes),
		WorkflowName:    "invoice",
		WorkflowType:    "invoice",
		WFSchemaVersion: "1",
		CompiledAt:      "2026-04-16T12:00:00Z",
	}); err != nil {
		t.Fatalf("WriteSlot current: %v", err)
	}
	if err := svc.store.WriteSlot("invoice", "backup", definitionBytes, WfRulesMetadata{
		Version:         3,
		Hash:            HashDefinition(definitionBytes),
		WorkflowName:    "invoice",
		WorkflowType:    "invoice",
		WFSchemaVersion: "1",
		CompiledAt:      "2026-04-16T12:00:00Z",
	}); err != nil {
		t.Fatalf("WriteSlot backup: %v", err)
	}
	svc.nodes = &fakeNodeClient{
		getNodeConfigFunc: func(_ context.Context, nodeName string) (map[string]any, error) {
			return map[string]any{
				"status": "ok",
				"config": map[string]any{
					"_system": map[string]any{
						"runtime_version": "0.0.1",
					},
				},
			}, nil
		},
	}

	if left, err := svc.PurgeWorkflowPackages("invoice", true); err != nil || len(left) != 0 {
		t.Fatalf("PurgeWorkflowPackages: left %v, err %v", left, err)
	}

	// Current (4), backup (3) and the version the node is bound to (1) stay; 2 goes, through
	// SY.admin, which writes the motherbee's dist and manifest (A-68): wf-rules does not, even on
	// the motherbee.
	admin := svc.admin.(*fakeAdminClient)
	if strings.Join(admin.removedVersions, ",") != "wf.invoice@0.0.2" {
		t.Fatalf("want only 0.0.2 removed through the admin, got %v", admin.removedVersions)
	}
	for _, version := range []string{"0.0.1", "0.0.2", "0.0.3", "0.0.4"} {
		if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", version)); err != nil {
			t.Fatalf("wf-rules leaves the dist tree to the admin, %s gone: %v", version, err)
		}
	}
}

// A node config read that got no answer leaves the bound version unknown, and any version the
// purge would remove may be it: the purge removes nothing and says why. Before, it took the
// timeout for no node, and since SY.admin refuses to remove only a version a node runs, a stopped
// node could lose the version its config names (the review of A-270). The apply and the rollback
// go on without the purge.
func TestPurgeWorkflowPackagesRemovesNothingWhileTheBoundVersionIsUnknown(t *testing.T) {
	svc := newTestService(t)
	for range 3 {
		if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
			t.Fatalf("CompileWorkflow: %v", err)
		}
		if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
			t.Fatalf("ApplyWorkflow: %v", err)
		}
	}
	admin := svc.admin.(*fakeAdminClient)
	admin.removedVersions = nil
	svc.nodes = &fakeNodeClient{getNodeConfigFunc: func(context.Context, string) (map[string]any, error) {
		return nil, context.DeadlineExceeded
	}}

	left, err := svc.PurgeWorkflowPackages("invoice", true)
	if err == nil || !strings.Contains(err.Error(), "cannot tell the version WF.invoice@motherbee is bound to: its config read (get_node_config) timed out") || left != nil {
		t.Fatalf("want the purge refused, naming the read, got left %v, err %v", left, err)
	}
	if len(admin.removedVersions) != 0 {
		t.Fatalf("a purge that cannot tell the bound version removes none, removed %v", admin.removedVersions)
	}

	// The next purge that reads the bound version removes what this one left.
	svc.nodes = &fakeNodeClient{getNodeConfigFunc: func(context.Context, string) (map[string]any, error) {
		return map[string]any{"status": "ok", "config": map[string]any{"_system": map[string]any{"runtime_version": "0.0.3"}}}, nil
	}}
	if left, err := svc.PurgeWorkflowPackages("invoice", true); err != nil || len(left) != 0 {
		t.Fatalf("PurgeWorkflowPackages: left %v, err %v", left, err)
	}
	if strings.Join(admin.removedVersions, ",") != "wf.invoice@0.0.1" {
		t.Fatalf("want 0.0.1 removed once the bound version is known, got %v", admin.removedVersions)
	}
}

// On a worker the dist tree is a receive-only mirror of the motherbee's: purging must ask SY.admin
// to remove the obsolete versions there and leave the local copy alone (lab 2026-09-30: a local
// delete on worker1 was never restored, and the next deploy of that version found no files).
func TestPurgeWorkflowPackagesOnAMirrorHiveGoesThroughTheAdmin(t *testing.T) {
	svc := newTestService(t)
	definitionBytes, err := normalizeDefinitionBytes(validWorkflowDefinition())
	if err != nil {
		t.Fatalf("normalizeDefinitionBytes: %v", err)
	}
	for version := uint64(1); version <= 3; version++ {
		meta := WfRulesMetadata{
			Version:         version,
			Hash:            HashDefinition(definitionBytes),
			WorkflowName:    "invoice",
			WorkflowType:    "invoice",
			WFSchemaVersion: "1",
			CompiledAt:      "2026-04-16T12:00:00Z",
		}
		if _, err := svc.PublishWorkflowPackage("invoice", meta, definitionBytes); err != nil {
			t.Fatalf("PublishWorkflowPackage(%d): %v", version, err)
		}
	}
	manifestPath := filepath.Join(svc.cfg.DistRuntimeRoot, "manifest.json")
	manifestBefore, err := os.ReadFile(manifestPath)
	if err != nil {
		t.Fatalf("read manifest: %v", err)
	}
	// The motherbee refuses to remove the current version while others remain: make 0.0.1 the
	// current one and expect it last. And 0.0.2 is already gone there (a directory the manifest
	// no longer lists): that counts as removed.
	manifest, err := loadRuntimeManifest(manifestPath)
	if err != nil {
		t.Fatalf("load manifest: %v", err)
	}
	entry := manifest.Runtimes["wf.invoice"]
	entry.Current = "0.0.1"
	manifest.Runtimes["wf.invoice"] = entry
	if err := writeRuntimeManifest(manifestPath, manifest); err != nil {
		t.Fatalf("write manifest: %v", err)
	}
	manifestBefore, err = os.ReadFile(manifestPath)
	if err != nil {
		t.Fatalf("read manifest: %v", err)
	}
	svc.cfg.HiveID = "worker1"
	admin := svc.admin.(*fakeAdminClient)
	admin.removeErr = map[string]error{
		"0.0.2": &adminActionError{Status: "error", Code: "RUNTIME_VERSION_NOT_FOUND", Message: "gone"},
	}

	if left, err := svc.PurgeWorkflowPackages("invoice", false); err != nil || len(left) != 0 {
		t.Fatalf("PurgeWorkflowPackages: left %v, err %v", left, err)
	}

	want := []string{"wf.invoice@0.0.2", "wf.invoice@0.0.3", "wf.invoice@0.0.1"}
	if strings.Join(admin.removedVersions, ",") != strings.Join(want, ",") {
		t.Fatalf("want removals %v through the admin, got %v", want, admin.removedVersions)
	}
	for _, version := range []string{"0.0.1", "0.0.2", "0.0.3"} {
		if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", version)); err != nil {
			t.Fatalf("the local mirror must be left alone, %s gone: %v", version, err)
		}
	}
	if manifestAfter, _ := os.ReadFile(manifestPath); string(manifestAfter) != string(manifestBefore) {
		t.Fatal("the local manifest must be left alone on a mirror hive")
	}
}

type fakeAdminClient struct {
	publishRuntimePackageFunc func(ctx context.Context, packageFiles map[string]string) (*PackagePublishResult, error)
	publishCalls              int
	lastPackageFiles          map[string]string
	removedVersions           []string
	removeErr                 map[string]error
	// currentOnPrimary is refused with RUNTIME_CURRENT_CONFLICT while other versions remain, as
	// the motherbee's admin does.
	currentOnPrimary  string
	versionsOnPrimary int
	syncHints         []string
	// removeWaits is how long each removal could wait for SY.admin's answer: the time left to its
	// context's deadline when it was asked.
	removeWaits []time.Duration
}

func (f *fakeAdminClient) SyncRuntimes(_ context.Context, hive string, waitMs int) error {
	f.syncHints = append(f.syncHints, fmt.Sprintf("%s/%d", hive, waitMs))
	return nil
}

func (f *fakeAdminClient) RemoveRuntimeVersion(ctx context.Context, runtimeName, version string) error {
	f.removedVersions = append(f.removedVersions, runtimeName+"@"+version)
	if deadline, ok := ctx.Deadline(); ok {
		f.removeWaits = append(f.removeWaits, time.Until(deadline))
	}
	if version == f.currentOnPrimary && f.versionsOnPrimary > 1 {
		return &adminActionError{Status: "error", Code: "RUNTIME_CURRENT_CONFLICT", Message: "cannot delete current version"}
	}
	if err := f.removeErr[version]; err != nil {
		return err
	}
	if f.versionsOnPrimary > 0 {
		f.versionsOnPrimary--
	}
	return nil
}

func (f *fakeAdminClient) PublishRuntimePackage(ctx context.Context, packageFiles map[string]string) (*PackagePublishResult, error) {
	f.publishCalls++
	f.lastPackageFiles = packageFiles
	if f.publishRuntimePackageFunc != nil {
		return f.publishRuntimePackageFunc(ctx, packageFiles)
	}
	return nil, fmt.Errorf("publishRuntimePackageFunc not configured")
}

func installWorkflowPackageViaFakeAdmin(svc *Service, packageFiles map[string]string) (*PackagePublishResult, error) {
	rawPackage, ok := packageFiles["package.json"]
	if !ok {
		return nil, fmt.Errorf("missing package.json")
	}
	var packageMeta struct {
		Name    string `json:"name"`
		Version string `json:"version"`
	}
	if err := json.Unmarshal([]byte(rawPackage), &packageMeta); err != nil {
		return nil, err
	}
	if packageMeta.Name == "" || packageMeta.Version == "" {
		return nil, fmt.Errorf("package metadata missing name/version")
	}
	packageDir := filepath.Join(svc.cfg.DistRuntimeRoot, packageMeta.Name, packageMeta.Version)
	// As SY.admin's install does (src/runtime_package.rs): a version already there is published
	// again only with the same contents, and refused otherwise.
	for relPath, contents := range packageFiles {
		existing, err := os.ReadFile(filepath.Join(packageDir, filepath.FromSlash(relPath)))
		if err == nil && string(existing) != contents {
			return nil, &adminActionError{Status: "error", Code: "VERSION_CONFLICT", Message: fmt.Sprintf(
				"install conflict: target runtime version already exists with different contents '%s'", packageDir)}
		}
	}
	for relPath, contents := range packageFiles {
		targetPath := filepath.Join(packageDir, filepath.FromSlash(relPath))
		if err := os.MkdirAll(filepath.Dir(targetPath), 0o755); err != nil {
			return nil, err
		}
		if err := os.WriteFile(targetPath, []byte(contents), 0o644); err != nil {
			return nil, err
		}
	}
	if err := svc.updateRuntimeManifest(packageMeta.Name, packageMeta.Version); err != nil {
		return nil, err
	}
	return &PackagePublishResult{
		RuntimeName: packageMeta.Name,
		Version:     packageMeta.Version,
		PackagePath: packageDir,
	}, nil
}

// SY.admin answers a removal only after its read of the version's use on every hive, which it waits
// 30 s for (FINDINGS A-281): the removal waits runtimeRemovalTimeout, 35 s, for that answer, where it
// waited 5 s and a read that took longer was a timeout. A refusal keeps SY.admin's code and detail,
// which packages_left names. The publish waits packagePublishTimeout, 10 s: SY.admin's admission
// (3 s at most) and its own lock's wait for SY.wf-rules (4 s at most) come first, so a
// RUNTIME_MANIFEST_BUSY reaches it (0.1.139's integration review: it waited 5 s).
func TestARemovalWaitsForSYAdminsReadOfEveryHive(t *testing.T) {
	const refusal = "cannot tell whether runtime 'wf.invoice' version '0.0.1' is in use: the nodes of worker1 (UNREACHABLE) were not read, and nothing was removed"
	admin := &recordingAdmin{reply: map[string]any{
		"status":       "error",
		"action":       "remove_runtime_version",
		"error_code":   "RUNTIME_USAGE_UNKNOWN",
		"error_detail": refusal,
		"payload": map[string]any{
			"unanswered_hives": []any{map[string]any{"hive": "worker1", "reason": "UNREACHABLE", "detail": "stale in the LSA"}},
			"retryable":        true,
		},
	}}
	client := &l2AdminClient{send: admin.send, targetNode: fluxbeesdk.PrimaryAdminNode}

	err := client.RemoveRuntimeVersion(context.Background(), "wf.invoice", "0.0.1")
	request, params := admin.last(t)
	if runtimeRemovalTimeout != 35*time.Second || request.Timeout != runtimeRemovalTimeout {
		t.Fatalf("the removal waits %v for SY.admin, want 35s", request.Timeout)
	}
	if request.AdminTarget != fluxbeesdk.PrimaryAdminNode || request.Action != "remove_runtime_version" || request.Target != fluxbeesdk.PrimaryHiveID ||
		params["runtime"] != "wf.invoice" || params["runtime_version"] != "0.0.1" {
		t.Fatalf("request = %+v", request)
	}
	var refused *adminActionError
	if !errors.As(err, &refused) || refused.Code != "RUNTIME_USAGE_UNKNOWN" || refused.Message != refusal {
		t.Fatalf("want SY.admin's refusal, got %v", err)
	}
	// packages_left keeps what SY.admin's payload says: the hives it did not read, and that asking
	// again may pass.
	want := PackageLeft{ErrorCode: "RUNTIME_USAGE_UNKNOWN", Detail: refusal, Retryable: true, UnansweredHives: []any{
		map[string]any{"hive": "worker1", "reason": "UNREACHABLE", "detail": "stale in the LSA"},
	}}
	if left := removalFailure(err); !reflect.DeepEqual(left, want) {
		t.Fatalf("packages_left would name:\n got %+v\nwant %+v", left, want)
	}

	admin.reply = map[string]any{"status": "ok", "payload": map[string]any{"runtime_name": "wf.invoice", "runtime_version": "0.0.2"}}
	if _, err := client.PublishRuntimePackage(context.Background(), map[string]string{"package.json": "{}"}); err != nil {
		t.Fatal(err)
	}
	if request, _ := admin.last(t); request.Action != "publish_runtime_package" || request.Timeout != packagePublishTimeout || packagePublishTimeout != 10*time.Second {
		t.Fatalf("the publish: %+v", request)
	}
}

// Each version a purge leaves says whether asking for its removal again, nothing changed first, may
// remove it (the operator's rule: a refusal that may pass says so, and names what was not read).
// SY.admin's word, its payload's retryable, decides for its refusals; without one, a use it could
// not read (RUNTIME_USAGE_UNKNOWN) or the connected nodes it could not read for A-62
// (INVENTORY_UNAVAILABLE) may pass, and a node that runs the version, or dependents, need a change
// first. No answer, no SY.admin reached, and a version not asked may pass. The hives SY.admin did
// not read are kept as it names them.
func TestEachVersionLeftSaysWhetherAskingAgainMayRemoveIt(t *testing.T) {
	refusal := func(code string, payload map[string]any) error {
		return fmt.Errorf("remove wf.invoice 0.0.1 on %s: %w", fluxbeesdk.PrimaryAdminNode, &adminActionError{
			Status: "error", Code: code, Message: "refused",
			Payload: map[string]any{"status": "error", "error_code": code, "error_detail": "refused", "payload": payload},
		})
	}
	worker1 := map[string]any{"hive": "worker1", "reason": "TIMEOUT", "detail": "no answer within 10 s"}
	for _, tc := range []struct {
		name       string
		err        error
		code       string
		retryable  bool
		unanswered []any
	}{
		{name: "a use not read, as SY.admin says it", err: refusal("RUNTIME_USAGE_UNKNOWN", map[string]any{"unanswered_hives": []any{worker1}, "retryable": true}),
			code: "RUNTIME_USAGE_UNKNOWN", retryable: true, unanswered: []any{worker1}},
		{name: "a use not read, SY.admin saying nothing", err: refusal("RUNTIME_USAGE_UNKNOWN", nil), code: "RUNTIME_USAGE_UNKNOWN", retryable: true},
		{name: "SY.admin's word wins", err: refusal("RUNTIME_USAGE_UNKNOWN", map[string]any{"retryable": false}), code: "RUNTIME_USAGE_UNKNOWN"},
		{name: "a node runs it, another hive not read", err: refusal("RUNTIME_IN_USE", map[string]any{"unanswered_hives": []any{worker1}}),
			code: "RUNTIME_IN_USE", unanswered: []any{worker1}},
		{name: "a node runs it", err: refusal("RUNTIME_IN_USE", map[string]any{"unanswered_hives": []any{}}), code: "RUNTIME_IN_USE"},
		{name: "dependents", err: refusal("RUNTIME_HAS_DEPENDENTS", map[string]any{"dependents": []any{"x"}}), code: "RUNTIME_HAS_DEPENDENTS"},
		{name: "the connected nodes not read", err: refusal("INVENTORY_UNAVAILABLE", nil), code: "INVENTORY_UNAVAILABLE", retryable: true},
		{name: "no answer in time", err: fmt.Errorf("remove: %w", context.DeadlineExceeded), code: "TIMEOUT", retryable: true},
		{name: "SY.admin not reached", err: &fluxbeesdk.RpcError{Kind: fluxbeesdk.RpcErrDisconnected}, code: "TRANSPORT_ERROR", retryable: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			left := removalFailure(tc.err)
			if left.ErrorCode != tc.code || left.Retryable != tc.retryable || !reflect.DeepEqual(left.UnansweredHives, tc.unanswered) {
				t.Fatalf("got %+v, want %s retryable %v unanswered %v", left, tc.code, tc.retryable, tc.unanswered)
			}
		})
	}
	// A version the purge had still to remove was not asked: asking may remove it.
	left := purgeStoppedAt("wf.invoice", "0.0.1", refusal("RUNTIME_IN_USE", nil), []string{"0.0.2"})
	if len(left) != 2 || left[0].Retryable || !left[1].Retryable || left[1].ErrorCode != "PURGE_STOPPED" {
		t.Fatalf("purge stopped at a version in use: %+v", left)
	}
	// But not after a use SY.admin could not read and says no retry passes before something changes
	// (a join that ended not read, the motherbee's own nodes unread): its removal reads the same
	// hives (FINDINGS A-281, the integration's review), where each said asking may remove it.
	for _, retryable := range []bool{true, false} {
		stopped := refusal("RUNTIME_USAGE_UNKNOWN", map[string]any{"unanswered_hives": []any{worker1}, "retryable": retryable})
		left := purgeStoppedAt("wf.invoice", "0.0.1", stopped, []string{"0.0.2", "0.0.3"})
		if len(left) != 3 || left[1].ErrorCode != "PURGE_STOPPED" || left[2].ErrorCode != "PURGE_STOPPED" {
			t.Fatalf("purge stopped at a use not read: %+v", left)
		}
		for _, version := range left {
			if version.Retryable != retryable {
				t.Fatalf("purge stopped at a use not read, retryable %v: %+v", retryable, left)
			}
		}
	}
	// Nor after an UNAUTHORIZED (FINDINGS A-304): SY.admin admits this hive's removals by the
	// workflow, so another hive's WF.<workflow> refuses every version alike, and each may be that
	// hive's current or backup; one offered as retryable was one the operator's own
	// remove_runtime_version, which A-304 does not refuse, could take from it (0.1.139's
	// integration review).
	unauthorized := purgeStoppedAt("wf.invoice", "0.0.1", refusal("UNAUTHORIZED", nil), []string{"0.0.2", "0.0.3"})
	if len(unauthorized) != 3 || unauthorized[0].ErrorCode != "UNAUTHORIZED" || unauthorized[1].ErrorCode != "PURGE_STOPPED" || unauthorized[2].ErrorCode != "PURGE_STOPPED" {
		t.Fatalf("purge stopped at another hive's workflow: %+v", unauthorized)
	}
	for _, version := range unauthorized {
		if version.Retryable {
			t.Fatalf("purge stopped at another hive's workflow, none retryable: %+v", unauthorized)
		}
	}
}

// An apply's purge runs between its publish and the node's step, within deployPurgeBudget (15 s):
// a removal may wait 35 s for SY.admin now, and one that waited out a hive that does not answer
// would take 30 s of the request's 60, the time a worker's node step needs to wait for its package.
// What it cuts, the next deploy's purge removes.
func TestAnApplysPurgeLeavesTheNodeStepItsTime(t *testing.T) {
	svc := newTestService(t)
	admin := svc.admin.(*fakeAdminClient)
	for range 3 {
		if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
			t.Fatalf("CompileWorkflow: %v", err)
		}
		if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
			t.Fatalf("ApplyWorkflow: %v", err)
		}
	}
	// The third apply keeps 0.0.3 (current) and 0.0.2 (backup), and removes 0.0.1.
	if got := strings.Join(admin.removedVersions, ","); got != "wf.invoice@0.0.1" || len(admin.removeWaits) != 1 {
		t.Fatalf("removals = %s, waits = %v", got, admin.removeWaits)
	}
	if wait := admin.removeWaits[0]; wait > deployPurgeBudget || wait < deployPurgeBudget-time.Second {
		t.Fatalf("an apply's removal may wait %v, want the purge's %v", wait, deployPurgeBudget)
	}
}

// A worker's copy of the manifest can lag behind the motherbee's: after a rollback the motherbee's
// current version is the older one. A version the admin refuses as current is removed once the
// others are gone (0.1.70 gate on 8.x, A-68).
func TestPurgeRemovesTheCurrentVersionLastEvenWithALaggingManifest(t *testing.T) {
	svc := newTestService(t)
	definitionBytes, err := normalizeDefinitionBytes(validWorkflowDefinition())
	if err != nil {
		t.Fatal(err)
	}
	for version := uint64(1); version <= 2; version++ {
		meta := WfRulesMetadata{Version: version, Hash: HashDefinition(definitionBytes), WorkflowName: "invoice", WorkflowType: "invoice", WFSchemaVersion: "1", CompiledAt: "2026-04-16T12:00:00Z"}
		if _, err := svc.PublishWorkflowPackage("invoice", meta, definitionBytes); err != nil {
			t.Fatal(err)
		}
	}
	// This hive's copy says 0.0.2 is current; on the motherbee it is 0.0.1.
	svc.cfg.HiveID = "worker1"
	admin := svc.admin.(*fakeAdminClient)
	admin.currentOnPrimary = "0.0.1"
	admin.versionsOnPrimary = 2

	if left, err := svc.PurgeWorkflowPackages("invoice", false); err != nil || len(left) != 0 {
		t.Fatalf("PurgeWorkflowPackages: left %v, err %v", left, err)
	}
	want := "wf.invoice@0.0.1,wf.invoice@0.0.2,wf.invoice@0.0.1"
	if got := strings.Join(admin.removedVersions, ","); got != want {
		t.Fatalf("removals = %s, want %s", got, want)
	}
}

// The manifest writes of the test fake: the real publish goes through SY.admin.
func (s *Service) updateRuntimeManifest(runtimeName, version string) error {
	manifestPath := filepath.Join(s.cfg.DistRuntimeRoot, "manifest.json")
	manifest, err := loadRuntimeManifest(manifestPath)
	if err != nil {
		return err
	}
	entry := manifest.Runtimes[runtimeName]
	entry.PackageType = "workflow"
	entry.RuntimeBase = workflowRuntimeBase
	entry.Current = version
	entry.Available = appendIfMissing(entry.Available, version)
	sort.Strings(entry.Available)
	manifest.Runtimes[runtimeName] = entry
	manifest.SchemaVersion = 2
	manifest.Version = nextManifestVersion(manifest.Version, uint64(time.Now().UTC().UnixMilli()))
	manifest.UpdatedAt = time.Now().UTC().Format(timeRFC3339)
	manifest.Hash = nil
	return writeRuntimeManifest(manifestPath, manifest)
}

func writeRuntimeManifest(path string, manifest *runtimeManifest) error {
	data, err := json.MarshalIndent(manifest, "", "  ")
	if err != nil {
		return err
	}
	data = append(data, '\n')
	return writeFileAtomic(path, data, 0o644)
}

func appendIfMissing(values []string, target string) []string {
	for _, value := range values {
		if value == target {
			return values
		}
	}
	return append(values, target)
}

func nextManifestVersion(current, now uint64) uint64 {
	if now > current {
		return now
	}
	return current + 1
}
