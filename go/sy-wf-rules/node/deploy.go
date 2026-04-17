package node

import (
	"context"
	"fmt"
	"time"
)

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
		rpcCtx, cancel = context.WithTimeout(context.Background(), orchestratorRPCTimeout)
		defer cancel()
		if _, err := s.orchestrator.SetNodeConfig(rpcCtx, s.cfg.OrchestratorTarget, nodeName, config, &binding, false); err != nil {
			return WFNodeActionResult{
					NodeName: nodeName,
					Action:   "restart_failed",
					Error:    err.Error(),
				},
				"Package published, but sy.wf-rules could not rebind the existing node config."
		}
		rpcCtx, cancel = context.WithTimeout(context.Background(), orchestratorRPCTimeout)
		defer cancel()
		if _, err := s.orchestrator.RestartNode(rpcCtx, s.cfg.OrchestratorTarget, nodeName); err != nil {
			time.Sleep(1 * time.Second)
			rpcCtx, cancel = context.WithTimeout(context.Background(), orchestratorRPCTimeout)
			defer cancel()
			if _, retryErr := s.orchestrator.RestartNode(rpcCtx, s.cfg.OrchestratorTarget, nodeName); retryErr != nil {
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

	rpcCtx, cancel = context.WithTimeout(context.Background(), orchestratorRPCTimeout)
	defer cancel()
	runtimeName := pkg.RuntimeName
	config := s.buildManagedWFConfig(nil, tenantID)
	_, err = s.orchestrator.RunNode(rpcCtx, s.cfg.OrchestratorTarget, nodeName, runtimeName, pkg.Version, config)
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
