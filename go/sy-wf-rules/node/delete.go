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
		return nil, invalidWorkflowName(workflowName)
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
	// No answer is not no node: with its config read unanswered, the delete cannot tell whether the
	// WF node exists. A forced delete took it for none: it dropped the workflow and its packages
	// and left the node running with its instances, where no delete reaches it any more (the review
	// of A-270). It is refused, with force or without, as a config read the orchestrator refuses
	// is, and nothing changes.
	if snapshot.Timeout && !snapshot.ConfigExists {
		return nil, WfRulesError{Code: "ORCHESTRATOR_ERROR", Detail: fmt.Sprintf("cannot tell whether %s exists: its %s; nothing was changed, try again", snapshot.NodeName, snapshot.ReadFailure)}
	}

	if !req.Force {
		if reason := instancesUnconfirmed(snapshot); reason != "" {
			return nil, WfRulesError{Code: "INSTANCES_UNKNOWN", Detail: fmt.Sprintf("cannot confirm active instances: %s (delete with force to remove it anyway)", reason)}
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

// instancesUnconfirmed is why a delete without force cannot tell that the WF node runs no
// instances, or "" when it can: the node has no config (never deployed, or already purged), or it
// counted them. A read of the node that failed is named, with how. Only a RUNNING node is asked to
// count. One that crashed, was stopped or is restarting keeps its instances in its store, and the
// kill's purge removes the store with the node: before 0.1.133, a delete a second after a
// restart_node read STARTING, took the node for one with none, and purged them (A-270).
func instancesUnconfirmed(snapshot wfNodeSnapshot) string {
	switch {
	case snapshot.ReadFailure != "":
		return fmt.Sprintf("the wf node's %s", snapshot.ReadFailure)
	case !snapshot.ConfigExists:
		return ""
	case !snapshot.Running && snapshot.LifecycleState == "":
		return "the wf node's status names no lifecycle_state"
	case !snapshot.Running:
		return fmt.Sprintf("the wf node is %s, not RUNNING", snapshot.LifecycleState)
	case snapshot.InstancesError != "":
		return fmt.Sprintf("the wf node answered %s", snapshot.InstancesError)
	case snapshot.ActiveInstances == nil:
		return "the wf node's running instances were not counted"
	}
	return ""
}
