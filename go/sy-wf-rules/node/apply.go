package node

import (
	"context"
	"fmt"
	"os"
	"strings"
)

type ApplyRequest struct {
	WorkflowName string `json:"workflow_name"`
	Version      uint64 `json:"version,omitempty"`
	AutoSpawn    bool   `json:"auto_spawn,omitempty"`
	TenantID     string `json:"tenant_id,omitempty"`
	// LaunchedBy is who asked for the deploy, stamped by SY.admin (operator, archi).
	LaunchedBy string `json:"launched_by,omitempty"`
}

type ApplyResult struct {
	Current WfRulesMetadata
	Package PackagePublishResult
}

func (s *Service) ApplyWorkflow(req ApplyRequest) (*ApplyResult, error) {
	ctx, cancel := newRequestContext()
	defer cancel()
	return s.applyWorkflow(ctx, req)
}

func (s *Service) applyWorkflow(ctx context.Context, req ApplyRequest) (*ApplyResult, error) {
	workflowName := strings.TrimSpace(req.WorkflowName)
	if !workflowNamePattern.MatchString(workflowName) {
		return nil, WfRulesError{Code: "INVALID_WORKFLOW_NAME", Detail: fmt.Sprintf("invalid workflow_name %q", workflowName)}
	}
	stagedMeta, err := s.store.ReadStagedMetadata(workflowName)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, WfRulesError{Code: "NOTHING_STAGED", Detail: "no staged workflow definition exists"}
		}
		return nil, err
	}
	if req.Version > 0 && stagedMeta.Version != req.Version {
		return nil, WfRulesError{
			Code:   "VERSION_MISMATCH",
			Detail: fmt.Sprintf("staged version=%d does not match requested version=%d", stagedMeta.Version, req.Version),
		}
	}
	if err := s.store.RotateToApply(workflowName); err != nil {
		return nil, err
	}
	currentMeta, err := s.store.ReadCurrentMetadata(workflowName)
	if err != nil {
		return nil, err
	}
	definitionBytes, err := s.store.ReadCurrentDefinition(workflowName)
	if err != nil {
		return nil, err
	}
	publish, err := s.publishWorkflowPackage(ctx, workflowName, *currentMeta, definitionBytes)
	if err != nil {
		return nil, WfRulesError{Code: "PACKAGE_PUBLISH_FAILED", Detail: err.Error()}
	}
	_ = s.purgeWorkflowPackages(ctx, workflowName, true)
	return &ApplyResult{
		Current: *currentMeta,
		Package: *publish,
	}, nil
}
