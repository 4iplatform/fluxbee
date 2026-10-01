package node

import (
	"context"
	"errors"
	"fmt"
	"time"
)

// runtimeSyncBudget bounds how long a deploy waits for the package it just published to reach this
// hive. Publishing happens on the motherbee; a worker gets the package through Syncthing (~11 s
// measured on the 8.x hive), while on the motherbee it is there at once. Stays inside SY.admin's
// 30 s request timeout.
const runtimeSyncBudget = 20 * time.Second

// runtimeNotSyncedYet reports an orchestrator refusal that only means the package published on the
// motherbee has not reached this hive's dist tree yet.
func runtimeNotSyncedYet(err error) bool {
	var actionErr *orchestratorActionError
	if !errors.As(err, &actionErr) {
		return false
	}
	switch actionErr.Code {
	case "RUNTIME_NOT_AVAILABLE", "RUNTIME_NOT_PRESENT", "BASE_RUNTIME_NOT_AVAILABLE":
		return true
	}
	return false
}

// untilRuntimeSynced runs call, and runs it again while the orchestrator answers that the runtime
// has not reached this hive yet — until it succeeds, fails for another reason, or budget runs out.
func untilRuntimeSynced(budget time.Duration, call func() error) error {
	deadline := time.Now().Add(budget)
	delay := time.Second
	for {
		err := call()
		if err == nil || !runtimeNotSyncedYet(err) || time.Now().Add(delay).After(deadline) {
			return err
		}
		time.Sleep(delay)
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
	result, err := s.ApplyWorkflow(req)
	if err != nil {
		return nil, err
	}
	wfNode, warning := s.deployPublishedWorkflow(req.WorkflowName, req.AutoSpawn, req.TenantID, result.Package)
	return &ApplyExecutionResult{
		Current: result.Current,
		Package: result.Package,
		WFNode:  wfNode,
		Warning: warning,
	}, nil
}

func (s *Service) RollbackWorkflowAndDeploy(req RollbackRequest) (*RollbackExecutionResult, error) {
	result, err := s.RollbackWorkflow(req)
	if err != nil {
		return nil, err
	}
	wfNode, warning := s.deployPublishedWorkflow(req.WorkflowName, req.AutoSpawn, req.TenantID, result.Package)
	return &RollbackExecutionResult{
		Current: result.Current,
		Package: result.Package,
		WFNode:  wfNode,
		Warning: warning,
	}, nil
}

func (s *Service) deployPublishedWorkflow(workflowName string, autoSpawn bool, tenantID string, pkg PackagePublishResult) (WFNodeActionResult, string) {
	nodeName := fmt.Sprintf("WF.%s@%s", workflowName, s.cfg.HiveID)
	if s.orchestrator == nil {
		return WFNodeActionResult{
			NodeName: nodeName,
			Action:   "none",
			Reason:   "orchestrator client unavailable",
		}, "Package published, but orchestrator client is unavailable. Deployment did not run."
	}

	rpcCtx, cancel := context.WithTimeout(context.Background(), orchestratorRPCTimeout)
	defer cancel()
	existingConfigPayload, err := s.orchestrator.GetNodeConfig(rpcCtx, s.cfg.OrchestratorTarget, nodeName)
	if err == nil {
		existingConfig := configMapFromNodeConfigPayload(existingConfigPayload)
		config := s.buildManagedWFConfig(existingConfig, tenantID)
		binding := buildManagedRuntimeBinding(pkg)
		rebind := func() error {
			rpcCtx, cancel := context.WithTimeout(context.Background(), orchestratorRPCTimeout)
			defer cancel()
			_, err := s.orchestrator.SetNodeConfig(rpcCtx, s.cfg.OrchestratorTarget, nodeName, config, &binding)
			return err
		}
		if err := untilRuntimeSynced(runtimeSyncBudget, rebind); err != nil {
			return WFNodeActionResult{
					NodeName: nodeName,
					Action:   "restart_failed",
					Error:    err.Error(),
				},
				"Package published, but sy.wf-rules could not rebind the existing node config."
		}
		restart := func() error {
			rpcCtx, cancel := context.WithTimeout(context.Background(), orchestratorRPCTimeout)
			defer cancel()
			_, err := s.orchestrator.RestartNode(rpcCtx, s.cfg.OrchestratorTarget, nodeName)
			return err
		}
		if err := untilRuntimeSynced(runtimeSyncBudget, restart); err != nil {
			time.Sleep(1 * time.Second)
			if retryErr := restart(); retryErr != nil {
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
	if actionErr, ok := err.(*orchestratorActionError); ok {
		if actionErr.Code != "NODE_CONFIG_NOT_FOUND" {
			return WFNodeActionResult{
					NodeName: nodeName,
					Action:   "none",
					Reason:   "orchestrator query failed",
					Error:    actionErr.Error(),
				},
				"Package published, but sy.wf-rules could not determine current node state from orchestrator."
		}
	} else if err != nil {
		return WFNodeActionResult{
				NodeName: nodeName,
				Action:   "none",
				Reason:   "orchestrator query failed",
				Error:    err.Error(),
			},
			"Package published, but sy.wf-rules could not determine current node state from orchestrator."
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
	config := s.buildManagedWFConfig(nil, tenantID)
	err = untilRuntimeSynced(runtimeSyncBudget, func() error {
		rpcCtx, cancel := context.WithTimeout(context.Background(), orchestratorRPCTimeout)
		defer cancel()
		_, err := s.orchestrator.RunNode(rpcCtx, s.cfg.OrchestratorTarget, nodeName, runtimeName, pkg.Version, config)
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
