package node

import (
	"context"
	"fmt"
	"strings"
)

type DeleteWorkflowResult struct {
	Deleted bool
	WFNode  WFNodeActionResult
}

func (s *Service) DeleteWorkflow(req DeleteWorkflowRequest) (*DeleteWorkflowResult, error) {
	workflowName := strings.TrimSpace(req.WorkflowName)
	if !workflowNamePattern.MatchString(workflowName) {
		return nil, WfRulesError{Code: "INVALID_WORKFLOW_NAME", Detail: fmt.Sprintf("invalid workflow_name %q", workflowName)}
	}
	if !s.store.WorkflowExists(workflowName) {
		return nil, WfRulesError{Code: "WORKFLOW_NOT_FOUND", Detail: fmt.Sprintf("workflow %q not found", workflowName)}
	}

	ctx, cancel := context.WithTimeout(context.Background(), wfNodeRPCTimeout)
	defer cancel()
	snapshot, err := s.inspectWFNode(ctx, workflowName, true)
	if err != nil {
		return nil, err
	}

	if !req.Force && snapshot.ConfigExists {
		if snapshot.Timeout {
			return nil, WfRulesError{Code: "INSTANCES_UNKNOWN", Detail: "wf node did not respond to instance count query"}
		}
		if snapshot.ActiveInstances != nil && *snapshot.ActiveInstances > 0 {
			return nil, WfRulesError{Code: "INSTANCES_ACTIVE", Detail: fmt.Sprintf("wf node still has %d running instances", *snapshot.ActiveInstances)}
		}
	}

	result := &DeleteWorkflowResult{
		Deleted: true,
		WFNode: WFNodeActionResult{
			NodeName: snapshot.NodeName,
			Action:   "none",
			Reason:   "node not managed",
		},
	}
	if snapshot.ConfigExists {
		if s.orchestrator == nil {
			return nil, WfRulesError{Code: "ORCHESTRATOR_ERROR", Detail: "orchestrator client unavailable"}
		}
		ctx, cancel = context.WithTimeout(context.Background(), orchestratorRPCTimeout)
		defer cancel()
		if _, err := s.orchestrator.KillNode(ctx, s.cfg.OrchestratorTarget, snapshot.NodeName, true, true); err != nil {
			return nil, WfRulesError{Code: "ORCHESTRATOR_ERROR", Detail: err.Error()}
		}
		result.WFNode = WFNodeActionResult{
			NodeName: snapshot.NodeName,
			Action:   "deleted",
			Status:   "ok",
		}
	}

	if err := s.store.DeleteWorkflowState(workflowName); err != nil {
		return nil, err
	}
	if err := s.PurgeWorkflowPackages(workflowName, false); err != nil {
		return nil, WfRulesError{Code: "PACKAGE_PUBLISH_FAILED", Detail: err.Error()}
	}
	return result, nil
}
