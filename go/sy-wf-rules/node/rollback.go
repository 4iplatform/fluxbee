package node

import (
	"context"
	"os"
	"strings"
)

type RollbackRequest struct {
	WorkflowName string `json:"workflow_name"`
	AutoSpawn    bool   `json:"auto_spawn,omitempty"`
	TenantID     string `json:"tenant_id,omitempty"`
	// LaunchedBy is who asked for the deploy, stamped by SY.admin (operator, archi).
	LaunchedBy string `json:"launched_by,omitempty"`
}

type RollbackResult struct {
	Current WfRulesMetadata
	Package PackagePublishResult
}

func (s *Service) RollbackWorkflow(req RollbackRequest) (*RollbackResult, error) {
	ctx, cancel := newRequestContext()
	defer cancel()
	return s.rollbackWorkflow(ctx, req)
}

func (s *Service) rollbackWorkflow(ctx context.Context, req RollbackRequest) (*RollbackResult, error) {
	workflowName := strings.TrimSpace(req.WorkflowName)
	if !workflowNamePattern.MatchString(workflowName) {
		return nil, invalidWorkflowName(workflowName)
	}
	backupMeta, err := s.store.ReadBackupMetadata(workflowName)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, WfRulesError{Code: "NO_BACKUP", Detail: "no backup workflow definition exists"}
		}
		return nil, err
	}
	if err := s.checkSlotLoads(workflowName, "backup", backupMeta.Version); err != nil {
		return nil, err
	}
	if err := s.store.RotateToRollback(workflowName); err != nil {
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
	s.purgeAfterPublish(ctx, workflowName)
	return &RollbackResult{
		Current: *currentMeta,
		Package: *publish,
	}, nil
}
