package node

import (
	"fmt"
	"strings"
)

type DeleteWorkflowResult struct {
	Deleted bool
	WFNode  WFNodeActionResult
	// PackagesLeft are the versions of wf.<workflow> the delete's purge did not remove from the
	// motherbee's dist, each with why; empty when it removed them all.
	PackagesLeft []PackageLeft
	// Warning says, when PackagesLeft names any, that the workflow is deleted all the same, and what
	// removes them.
	Warning string
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
		PackagesLeft: []PackageLeft{},
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

	// The versions the slots name go too, even where this hive's dist does not list them (yet): read
	// before the state goes.
	slotted := s.slotPackageVersions(workflowName)
	if err := s.store.DeleteWorkflowState(workflowName); err != nil {
		return nil, err
	}
	// The workflow is deleted now: its node and its state are gone, and a delete asked again answers
	// WORKFLOW_NOT_FOUND. So a purge that cannot finish does not fail it: the answer names what
	// stayed. Before 0.1.133 the delete answered PACKAGE_PUBLISH_FAILED, the factory reset counted it
	// as failed, and nothing named the versions left in the motherbee's dist. With A-281, SY.admin
	// refuses any removal while a hive that may run the version cannot be read (a worker down or
	// restarting, a join under way), RUNTIME_USAGE_UNKNOWN.
	left, err := s.purgeWorkflowPackages(ctx, workflowName, false, slotted...)
	if err != nil {
		left = []PackageLeft{{Runtime: workflowRuntimeName(workflowName), ErrorCode: "DIST_UNREADABLE", Detail: err.Error()}}
	}
	result.PackagesLeft = append(result.PackagesLeft, left...)
	result.Warning = packagesLeftWarning(workflowName, left)
	return result, nil
}

// slotPackageVersions is each version of wf.<workflow> the workflow's slots name, as its package
// numbers it: staged (a rollback's former current, or a compile never applied), backup, and current
// last, the motherbee's current, which SY.admin removes only once no other version is left (the
// order purgeOnPrimary keeps when this hive's copy of the manifest does not say which is current).
func (s *Service) slotPackageVersions(workflowName string) []string {
	var versions []string
	for _, slot := range []string{"staged", "backup", "current"} {
		if meta, err := s.store.ReadMetadata(workflowName, slot); err == nil {
			versions = append(versions, workflowRuntimeVersion(meta.Version))
		}
	}
	return versions
}

// packagesLeftWarning is a delete's warning when its purge left versions of the workflow's package
// in the motherbee's dist: the workflow is deleted, how many versions stayed and why the purge
// stopped (packages_left names each), and what removes them. "" when it left none.
func packagesLeftWarning(workflowName string, left []PackageLeft) string {
	if len(left) == 0 {
		return ""
	}
	runtimeName := workflowRuntimeName(workflowName)
	stopped := left[0]
	if stopped.Version == "" {
		// DIST_UNREADABLE: nothing was removed, and until this hive's dist can be listed again its
		// compiles that name no version are refused (highestPublishedVersion), so no deploy of the
		// name purges anything, and no hive's answer would change that.
		return fmt.Sprintf("workflow %s is deleted, but its purge could not list the versions of %s in this hive's dist (packages_left: %s), so it removed none: fix this hive's dist/runtimes/%s first (a compile of %s that names no version is refused until it can be listed); then remove_runtime_version removes each version of %s the motherbee's get_runtime lists, or a deploy of %s purges them.", workflowName, runtimeName, stopped.ErrorCode, runtimeName, workflowName, runtimeName, workflowName)
	}
	what := fmt.Sprintf("its purge did not remove %d versions of %s from the motherbee's dist (packages_left): it stopped at %s, %s", len(left), runtimeName, stopped.Version, stopped.ErrorCode)
	if len(left) == 1 {
		what = fmt.Sprintf("its purge did not remove %s %s from the motherbee's dist (packages_left: %s)", runtimeName, stopped.Version, stopped.ErrorCode)
	}
	return fmt.Sprintf("workflow %s is deleted, but %s. remove_runtime_version removes each version once every hive answers and no node runs it, and a later deploy of %s purges them too.", workflowName, what, workflowName)
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
