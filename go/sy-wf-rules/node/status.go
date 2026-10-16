package node

import (
	"context"
	"errors"
	"fmt"
	"path/filepath"
	"strings"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

type wfNodeSnapshot struct {
	NodeName        string
	ConfigExists    bool
	RuntimeVersion  string
	Running         bool
	StatusReachable bool
	HealthState     string
	ActiveInstances *int
	Timeout         bool
	// InstancesError is why a WF node that answered could not count its running instances (e.g.
	// node_not_configured while its config is not in use); a timeout is Timeout instead.
	InstancesError string
}

type WorkflowStatusView struct {
	WorkflowName     string
	CurrentVersion   *uint64
	CurrentHash      string
	StagedVersion    *uint64
	LastError        string
	PublishedVersion *string
	WFNode           wfNodeSnapshot
}

type ListedWorkflowStatus struct {
	WorkflowName      string
	CurrentVersion    *uint64
	CurrentHash       string
	PublishedVersion  *string
	WFNodeRunning     bool
	WFNodeReachable   bool
	WFNodeHealthState string
	ActiveInstances   *int
	WFNodeTimeout     bool
	InstancesError    string
	DeployedVersion   string
}

func (s *Service) GetWorkflowStatus(req GetStatusRequest) (*WorkflowStatusView, error) {
	workflowName := strings.TrimSpace(req.WorkflowName)
	if !workflowNamePattern.MatchString(workflowName) {
		return nil, WfRulesError{Code: "INVALID_WORKFLOW_NAME", Detail: fmt.Sprintf("invalid workflow_name %q", workflowName)}
	}
	if !s.store.WorkflowExists(workflowName) {
		return nil, WfRulesError{Code: "WORKFLOW_NOT_FOUND", Detail: fmt.Sprintf("workflow %q not found", workflowName)}
	}
	view := &WorkflowStatusView{WorkflowName: workflowName}
	if meta, err := s.store.ReadCurrentMetadata(workflowName); err == nil {
		view.CurrentVersion = uint64Ptr(meta.Version)
		view.CurrentHash = meta.Hash
	}
	if meta, err := s.store.ReadStagedMetadata(workflowName); err == nil {
		view.StagedVersion = uint64Ptr(meta.Version)
	}
	if published := s.currentPublishedVersion(workflowName); published != "" {
		view.PublishedVersion = stringPtr(published)
	}
	ctx, cancel := newRequestContext()
	defer cancel()
	snapshot, err := s.inspectWFNode(ctx, workflowName, true)
	if err != nil {
		return nil, err
	}
	view.WFNode = snapshot
	return view, nil
}

func (s *Service) ListWorkflowStatuses() ([]ListedWorkflowStatus, error) {
	workflows, err := s.store.ListWorkflows()
	if err != nil {
		return nil, err
	}
	// One deadline for the whole list: a node that does not answer shows as a timeout, and the
	// ones after it as well once the request's time is up, rather than SY.admin giving up first.
	ctx, cancel := newRequestContext()
	defer cancel()
	out := make([]ListedWorkflowStatus, 0, len(workflows))
	for _, workflowName := range workflows {
		item := ListedWorkflowStatus{WorkflowName: workflowName}
		if meta, err := s.store.ReadCurrentMetadata(workflowName); err == nil {
			item.CurrentVersion = uint64Ptr(meta.Version)
			item.CurrentHash = meta.Hash
		}
		if published := s.currentPublishedVersion(workflowName); published != "" {
			item.PublishedVersion = stringPtr(published)
		}
		snapshot, err := s.inspectWFNode(ctx, workflowName, true)
		if err != nil {
			return nil, err
		}
		item.WFNodeRunning = snapshot.Running
		item.WFNodeReachable = snapshot.StatusReachable
		item.WFNodeHealthState = snapshot.HealthState
		item.ActiveInstances = snapshot.ActiveInstances
		item.WFNodeTimeout = snapshot.Timeout
		item.InstancesError = snapshot.InstancesError
		item.DeployedVersion = snapshot.RuntimeVersion
		out = append(out, item)
	}
	return out, nil
}

func (s *Service) inspectWFNode(rpcCtx context.Context, workflowName string, queryInstances bool) (wfNodeSnapshot, error) {
	nodeName := fmt.Sprintf("WF.%s@%s", workflowName, s.cfg.HiveID)
	snapshot := wfNodeSnapshot{NodeName: nodeName}
	if s.nodes == nil {
		return snapshot, nil
	}
	configPayload, err := s.nodes.GetNodeConfig(rpcCtx, nodeName)
	if err != nil {
		if actionErr, ok := err.(*nodeActionError); ok && actionErr.Code == "NODE_CONFIG_NOT_FOUND" {
			return snapshot, nil
		}
		if isRPCTimeout(rpcCtx, err) {
			snapshot.Timeout = true
			return snapshot, nil
		}
		return snapshot, WfRulesError{Code: "ORCHESTRATOR_ERROR", Detail: err.Error()}
	}
	snapshot.ConfigExists = true
	snapshot.RuntimeVersion = runtimeVersionFromNodeConfig(configPayload)
	if !queryInstances {
		return snapshot, nil
	}
	nodeStatus, err := s.nodes.GetNodeStatus(rpcCtx, nodeName)
	if err != nil {
		if isRPCTimeout(rpcCtx, err) {
			snapshot.Timeout = true
		}
		return snapshot, nil
	}
	if !strings.EqualFold(stringValueFromMap(nodeStatus, "lifecycle_state"), "RUNNING") {
		return snapshot, nil
	}
	snapshot.Running = true
	snapshot.StatusReachable = true
	snapshot.HealthState = strings.TrimSpace(stringValueFromMap(nodeStatus, "health_state"))
	if s.wfNodes == nil {
		return snapshot, nil
	}
	count, err := s.wfNodes.CountRunningInstances(rpcCtx, nodeName)
	if err != nil {
		var refusal WfRulesError
		if errors.As(err, &refusal) && !isRPCTimeout(rpcCtx, err) {
			snapshot.InstancesError = refusal.Error()
			return snapshot, nil
		}
		snapshot.Timeout = true
		return snapshot, nil
	}
	snapshot.ActiveInstances = intPtr(count)
	return snapshot, nil
}

func isRPCTimeout(rpcCtx context.Context, err error) bool {
	if err == nil {
		return false
	}
	if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
		return true
	}
	var rpcErr *fluxbeesdk.RpcError
	if errors.As(err, &rpcErr) && rpcErr.Kind == fluxbeesdk.RpcErrTimeout {
		return true
	}
	if rpcCtx != nil && rpcCtx.Err() != nil {
		return true
	}
	return strings.Contains(strings.ToLower(err.Error()), "context deadline exceeded")
}

func (s *Service) boundRuntimeVersionForWorkflow(ctx context.Context, workflowName string) (string, error) {
	snapshot, err := s.inspectWFNode(ctx, workflowName, false)
	if err != nil {
		return "", err
	}
	return snapshot.RuntimeVersion, nil
}

func runtimeVersionFromNodeConfig(payload map[string]any) string {
	config, _ := payload["config"].(map[string]any)
	if config == nil {
		return ""
	}
	system, _ := config["_system"].(map[string]any)
	if system == nil {
		return ""
	}
	version := stringValueFromMap(system, "runtime_version")
	if strings.TrimSpace(version) != "" {
		return strings.TrimSpace(version)
	}
	return strings.TrimSpace(stringValueFromMap(system, "resolved_version"))
}

func (s *Service) currentPublishedVersion(workflowName string) string {
	manifest, err := loadRuntimeManifest(filepath.Join(s.cfg.DistRuntimeRoot, "manifest.json"))
	if err != nil {
		return ""
	}
	entry, ok := manifest.Runtimes[workflowRuntimeName(workflowName)]
	if !ok {
		return ""
	}
	return strings.TrimSpace(entry.Current)
}

func uint64Ptr(value uint64) *uint64 {
	return &value
}

func intPtr(value int) *int {
	return &value
}

func formatOptionalUint64(value *uint64) any {
	if value == nil {
		return nil
	}
	return *value
}

func formatOptionalInt(value *int) any {
	if value == nil {
		return nil
	}
	return *value
}
