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
	wfNode, warning := s.deployPublishedWorkflow(req.WorkflowName, req.AutoSpawn, result.Package)
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
	wfNode, warning := s.deployPublishedWorkflow(req.WorkflowName, req.AutoSpawn, result.Package)
	return &RollbackExecutionResult{
		Current: result.Current,
		Package: result.Package,
		WFNode:  wfNode,
		Warning: warning,
	}, nil
}

func (s *Service) deployPublishedWorkflow(workflowName string, autoSpawn bool, pkg PackagePublishResult) (WFNodeActionResult, string) {
	nodeName := fmt.Sprintf("WF.%s@%s", workflowName, s.cfg.HiveID)
	if s.orchestrator == nil {
		return WFNodeActionResult{
			NodeName: nodeName,
			Action:   "none",
			Reason:   "orchestrator client unavailable",
		}, "Package published, but orchestrator client is unavailable. Deployment did not run."
	}

	ctx, cancel := context.WithTimeout(context.Background(), orchestratorRPCTimeout)
	defer cancel()
	_, err := s.orchestrator.GetNodeConfig(ctx, s.cfg.OrchestratorTarget, nodeName)
	if err == nil {
		patch := map[string]any{
			"_system": map[string]any{
				"runtime":         pkg.RuntimeName,
				"runtime_version": pkg.Version,
				"runtime_base":    workflowRuntimeBase,
				"package_path":    pkg.PackagePath,
			},
		}
		ctx, cancel = context.WithTimeout(context.Background(), orchestratorRPCTimeout)
		defer cancel()
		if _, err := s.orchestrator.SetNodeConfig(ctx, s.cfg.OrchestratorTarget, nodeName, patch, false); err != nil {
			return WFNodeActionResult{
					NodeName: nodeName,
					Action:   "restart_failed",
					Error:    err.Error(),
				},
				"Package published, but sy.wf-rules could not rebind the existing node config."
		}
		ctx, cancel = context.WithTimeout(context.Background(), orchestratorRPCTimeout)
		defer cancel()
		if _, err := s.orchestrator.RestartNode(ctx, s.cfg.OrchestratorTarget, nodeName); err != nil {
			time.Sleep(1 * time.Second)
			ctx, cancel = context.WithTimeout(context.Background(), orchestratorRPCTimeout)
			defer cancel()
			if _, retryErr := s.orchestrator.RestartNode(ctx, s.cfg.OrchestratorTarget, nodeName); retryErr != nil {
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
	}

	if !autoSpawn {
		return WFNodeActionResult{
			NodeName: nodeName,
			Action:   "none",
			Reason:   "auto_spawn disabled",
		}, ""
	}

	ctx, cancel = context.WithTimeout(context.Background(), orchestratorRPCTimeout)
	defer cancel()
	runtimeName := pkg.RuntimeName
	_, err = s.orchestrator.RunNode(ctx, s.cfg.OrchestratorTarget, nodeName, runtimeName, pkg.Version, map[string]any{})
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
