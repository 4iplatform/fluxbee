package node

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"
)

type WorkflowView struct {
	Metadata   WfRulesMetadata
	Definition json.RawMessage
}

func (s *Service) GetWorkflow(req GetWorkflowRequest) (*WorkflowView, error) {
	workflowName := strings.TrimSpace(req.WorkflowName)
	if !workflowNamePattern.MatchString(workflowName) {
		return nil, WfRulesError{Code: "INVALID_WORKFLOW_NAME", Detail: fmt.Sprintf("invalid workflow_name %q", workflowName)}
	}
	meta, err := s.store.ReadCurrentMetadata(workflowName)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, WfRulesError{Code: "WORKFLOW_NOT_FOUND", Detail: fmt.Sprintf("workflow %q has no current definition", workflowName)}
		}
		return nil, err
	}
	definitionBytes, err := s.store.ReadCurrentDefinition(workflowName)
	if err != nil {
		return nil, err
	}
	return &WorkflowView{
		Metadata:   *meta,
		Definition: json.RawMessage(definitionBytes),
	}, nil
}
