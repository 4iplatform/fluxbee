package node

import (
	"encoding/json"
	"fmt"
	"strings"

	wfcel "github.com/4iplatform/json-router/pkg/wfcel"
)

func (s *Service) CompileWorkflow(req CompileRequest) (*WfRulesMetadata, error) {
	workflowName := strings.TrimSpace(req.WorkflowName)
	if !workflowNamePattern.MatchString(workflowName) {
		return nil, WfRulesError{Code: "INVALID_WORKFLOW_NAME", Detail: fmt.Sprintf("invalid workflow_name %q", workflowName)}
	}
	if req.Definition == nil {
		return nil, WfRulesError{Code: "INVALID_CONFIG_SET", Detail: "definition is required"}
	}

	definitionBytes, err := normalizeDefinitionBytes(req.Definition)
	if err != nil {
		return nil, WfRulesError{Code: "INVALID_CONFIG_SET", Detail: err.Error()}
	}
	def, err := wfcel.LoadDefinitionBytes(definitionBytes, "", s.clock)
	if err != nil {
		return nil, WfRulesError{Code: "COMPILE_ERROR", Detail: err.Error()}
	}

	version := req.Version
	if version == 0 {
		version = s.store.NextVersion(workflowName)
	}
	meta := WfRulesMetadata{
		Version:         version,
		Hash:            HashDefinition(definitionBytes),
		WorkflowName:    workflowName,
		WorkflowType:    def.WorkflowType,
		WFSchemaVersion: def.WFSchemaVersion,
		CompiledAt:      s.clock().UTC().Format(timeRFC3339),
		GuardCount:      countGuards(def),
		StateCount:      len(def.States),
		ActionCount:     countActions(def),
	}
	if err := s.store.WriteStaged(workflowName, definitionBytes, meta); err != nil {
		return nil, err
	}
	return &meta, nil
}

func decodeCompileRequestFromMap(config map[string]any) (CompileRequest, error) {
	raw, err := json.Marshal(config)
	if err != nil {
		return CompileRequest{}, err
	}
	var req CompileRequest
	if err := json.Unmarshal(raw, &req); err != nil {
		return CompileRequest{}, err
	}
	return req, nil
}

func countGuards(def *wfcel.WorkflowDefinition) int {
	total := 0
	for _, state := range def.States {
		for _, transition := range state.Transitions {
			if strings.TrimSpace(transition.Guard) != "" {
				total++
			}
		}
	}
	return total
}

func countActions(def *wfcel.WorkflowDefinition) int {
	total := 0
	for _, state := range def.States {
		total += len(state.EntryActions)
		total += len(state.ExitActions)
		for _, transition := range state.Transitions {
			total += len(transition.Actions)
		}
	}
	return total
}
