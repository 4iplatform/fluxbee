package node

import (
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

	ctx, cancel := newRequestContext()
	defer cancel()
	snapshot, err := s.inspectWFNode(ctx, workflowName, true)
	if err != nil {
		return nil, err
	}

	if !req.Force {
		if snapshot.Timeout {
			return nil, WfRulesError{Code: "INSTANCES_UNKNOWN", Detail: "cannot confirm active instances because wf node status timed out"}
		}
		if snapshot.InstancesError != "" {
			return nil, WfRulesError{Code: "INSTANCES_UNKNOWN", Detail: fmt.Sprintf("cannot confirm active instances: the wf node answered %s (delete with force to remove it anyway)", snapshot.InstancesError)}
		}
		if snapshot.ConfigExists && snapshot.ActiveInstances != nil && *snapshot.ActiveInstances > 0 {
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
		if s.nodes == nil {
			return nil, WfRulesError{Code: "ORCHESTRATOR_ERROR", Detail: "admin client unavailable"}
		}
		if _, err := s.nodes.KillNode(ctx, snapshot.NodeName, true, true); err != nil {
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
	if err := s.purgeWorkflowPackages(ctx, workflowName, false); err != nil {
		return nil, WfRulesError{Code: "PACKAGE_PUBLISH_FAILED", Detail: err.Error()}
	}
	return result, nil
}
