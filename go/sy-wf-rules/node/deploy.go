package node

import (
	"context"
	"errors"
	"fmt"
	"time"
)

// wfRulesRequestBudget bounds one request to SY.wf-rules end to end (publish, purge, the node's
// rebind and restart, or its launch), so it answers before SY.admin stops waiting for it (45 s),
// and Archi waits longer still (60 s). A deploy that outlived its callers would change state after
// they had reported a timeout (A-68).
const wfRulesRequestBudget = 35 * time.Second

// runtimeSyncBudget bounds how long a deploy waits for the package it just published to reach this
// hive. Publishing happens on the motherbee; a worker gets the package through Syncthing (~11 s
// measured on the 8.x hive), while on the motherbee it is there at once.
const runtimeSyncBudget = 20 * time.Second

func newRequestContext() (context.Context, context.CancelFunc) {
	return context.WithTimeout(context.Background(), wfRulesRequestBudget)
}

// runtimeNotSyncedYet reports a refusal that only means the package published on the motherbee has
// not reached this hive's dist tree yet.
func runtimeNotSyncedYet(err error) bool {
	var actionErr *nodeActionError
	if !errors.As(err, &actionErr) {
		return false
	}
	switch actionErr.Code {
	case "RUNTIME_NOT_AVAILABLE", "RUNTIME_NOT_PRESENT", "BASE_RUNTIME_NOT_AVAILABLE":
		return true
	}
	return false
}

// untilRuntimeSynced runs call, and runs it again while the answer is that the runtime has not
// reached this hive yet — until it succeeds, fails for another reason, the budget runs out, or the
// request's deadline passes.
func untilRuntimeSynced(ctx context.Context, budget time.Duration, call func() error) error {
	deadline := time.Now().Add(budget)
	if requestDeadline, ok := ctx.Deadline(); ok && requestDeadline.Before(deadline) {
		deadline = requestDeadline
	}
	delay := time.Second
	for {
		err := call()
		if err == nil || !runtimeNotSyncedYet(err) || time.Now().Add(delay).After(deadline) {
			return err
		}
		select {
		case <-ctx.Done():
			return err
		case <-time.After(delay):
		}
		if delay < 4*time.Second {
			delay *= 2
		}
	}
}

type WFNodeActionResult struct {
	NodeName string `json:"node_name"`
	Action   string `json:"action"`
	Status   string `json:"status,omitempty"`
	Reason   string `json:"reason,omitempty"`
	Error    string `json:"error,omitempty"`
}

type ApplyExecutionResult struct {
	Current WfRulesMetadata
	Package PackagePublishResult
	WFNode  WFNodeActionResult
	Warning string
}

type RollbackExecutionResult struct {
	Current WfRulesMetadata
	Package PackagePublishResult
	WFNode  WFNodeActionResult
	Warning string
}

func (s *Service) ApplyWorkflowAndDeploy(req ApplyRequest) (*ApplyExecutionResult, error) {
	ctx, cancel := newRequestContext()
	defer cancel()
	result, err := s.applyWorkflow(ctx, req)
	if err != nil {
		return nil, err
	}
	wfNode, warning := s.deployPublishedWorkflow(ctx, req.WorkflowName, req.AutoSpawn, req.TenantID, req.LaunchedBy, result.Package)
	return &ApplyExecutionResult{
		Current: result.Current,
		Package: result.Package,
		WFNode:  wfNode,
		Warning: warning,
	}, nil
}

func (s *Service) RollbackWorkflowAndDeploy(req RollbackRequest) (*RollbackExecutionResult, error) {
	ctx, cancel := newRequestContext()
	defer cancel()
	result, err := s.rollbackWorkflow(ctx, req)
	if err != nil {
		return nil, err
	}
	wfNode, warning := s.deployPublishedWorkflow(ctx, req.WorkflowName, req.AutoSpawn, req.TenantID, req.LaunchedBy, result.Package)
	return &RollbackExecutionResult{
		Current: result.Current,
		Package: result.Package,
		WFNode:  wfNode,
		Warning: warning,
	}, nil
}

func (s *Service) deployPublishedWorkflow(ctx context.Context, workflowName string, autoSpawn bool, tenantID, launchedBy string, pkg PackagePublishResult) (WFNodeActionResult, string) {
	nodeName := fmt.Sprintf("WF.%s@%s", workflowName, s.cfg.HiveID)
	if s.nodes == nil {
		return WFNodeActionResult{
			NodeName: nodeName,
			Action:   "none",
			Reason:   "admin client unavailable",
		}, "Package published, but the admin client is unavailable. Deployment did not run."
	}

	_, err := s.nodes.GetNodeConfig(ctx, nodeName)
	if err == nil {
		// The rebind changes only the binding: set_node_config merges the patch into the node's
		// config, so its operator config stays as it is without copying it back (copying it
		// carried along keys an older node had written, A-68).
		binding := buildManagedRuntimeBinding(pkg)
		rebind := func() error {
			_, err := s.nodes.SetNodeConfig(ctx, nodeName, map[string]any{}, &binding)
			return err
		}
		if err := untilRuntimeSynced(ctx, runtimeSyncBudget, rebind); err != nil {
			return WFNodeActionResult{
					NodeName: nodeName,
					Action:   "restart_failed",
					Error:    err.Error(),
				},
				"Package published, but sy.wf-rules could not rebind the existing node config."
		}
		restart := func() error {
			_, err := s.nodes.RestartNode(ctx, nodeName)
			return err
		}
		if err := untilRuntimeSynced(ctx, runtimeSyncBudget, restart); err != nil {
			retryErr := err
			select {
			case <-ctx.Done():
			case <-time.After(time.Second):
				retryErr = restart()
			}
			if retryErr != nil {
				return WFNodeActionResult{
						NodeName: nodeName,
						Action:   "restart_failed",
						Error:    retryErr.Error(),
					},
					"Package published and config rebound, but restart of the existing WF node failed."
			}
		}
		return WFNodeActionResult{
			NodeName: nodeName,
			Action:   "restarted",
			Status:   "ok",
		}, ""
	}
	if actionErr, ok := err.(*nodeActionError); ok {
		if actionErr.Code != "NODE_CONFIG_NOT_FOUND" {
			return WFNodeActionResult{
					NodeName: nodeName,
					Action:   "none",
					Reason:   "node query failed",
					Error:    actionErr.Error(),
				},
				"Package published, but sy.wf-rules could not determine the node's current state."
		}
	} else if err != nil {
		return WFNodeActionResult{
				NodeName: nodeName,
				Action:   "none",
				Reason:   "node query failed",
				Error:    err.Error(),
			},
			"Package published, but sy.wf-rules could not determine the node's current state."
	}

	if !autoSpawn {
		return WFNodeActionResult{
			NodeName: nodeName,
			Action:   "none",
			Reason:   "auto_spawn disabled",
		}, ""
	}
	if tenantID == "" {
		return WFNodeActionResult{
				NodeName: nodeName,
				Action:   "none",
				Reason:   "tenant_id required for first deploy",
				Error:    "first deploy of a managed WF node requires explicit tenant_id in the request",
			},
			"Package published, but first deploy was skipped because tenant_id is required."
	}

	runtimeName := pkg.RuntimeName
	config := s.buildManagedWFConfig(tenantID)
	err = untilRuntimeSynced(ctx, runtimeSyncBudget, func() error {
		_, err := s.nodes.RunNode(ctx, nodeName, runtimeName, pkg.Version, config, launchedBy)
		return err
	})
	if err != nil {
		return WFNodeActionResult{
				NodeName: nodeName,
				Action:   "restart_failed",
				Error:    err.Error(),
			},
			"Package published, but the first deploy spawn failed."
	}
	return WFNodeActionResult{
		NodeName: nodeName,
		Action:   "restarted",
		Status:   "ok",
	}, ""
}
