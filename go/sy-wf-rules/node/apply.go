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
		return nil, invalidWorkflowName(workflowName)
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
	if err := s.checkSlotLoads(workflowName, "staged", stagedMeta.Version); err != nil {
		return nil, err
	}
	// The staged version's package first, the slots after it (publishSlot): a publish refused or not
	// answered leaves the version staged, and current and backup as they were.
	publish, err := s.publishSlot(ctx, workflowName, "staged", *stagedMeta)
	if err != nil {
		return nil, err
	}
	if err := s.store.RotateToApply(workflowName); err != nil {
		return nil, slotsNotMoved(publish, err)
	}
	currentMeta, err := s.store.ReadCurrentMetadata(workflowName)
	if err != nil {
		return nil, err
	}
	s.purgeAfterPublish(ctx, workflowName)
	return &ApplyResult{
		Current: *currentMeta,
		Package: *publish,
	}, nil
}
