package node

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"strings"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
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

// decodeCompileRequest decodes a compile request with its definition's numbers as written
// (json.Number), so they are staged as written: decoded as float64, 1.0 was staged as 1, an integer
// to the engine, and an integer past 2^53 lost its last digits.
func decodeCompileRequest(raw []byte) (CompileRequest, error) {
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	var req CompileRequest
	if err := dec.Decode(&req); err != nil {
		return CompileRequest{}, err
	}
	if _, err := dec.Token(); err != io.EOF {
		return CompileRequest{}, fmt.Errorf("invalid compile request: data after its JSON object")
	}
	return req, nil
}

// configCompileRequest is the compile request a CONFIG_SET's config holds (decodeCompileRequest).
func configCompileRequest(msg fluxbeesdk.Message) (CompileRequest, error) {
	var payload struct {
		Config json.RawMessage `json:"config"`
	}
	if err := json.Unmarshal(msg.Payload, &payload); err != nil {
		return CompileRequest{}, err
	}
	return decodeCompileRequest(payload.Config)
}

// countGuards and countActions count the guards and actions of the states' transitions and of the
// catch's, which are the definition's own as well.
func countGuards(def *wfcel.WorkflowDefinition) int {
	total := 0
	count := func(transitions []wfcel.TransitionDefinition) {
		for _, transition := range transitions {
			if strings.TrimSpace(transition.Guard) != "" {
				total++
			}
		}
	}
	for _, state := range def.States {
		count(state.Transitions)
	}
	count(def.Catch)
	return total
}

func countActions(def *wfcel.WorkflowDefinition) int {
	total := 0
	count := func(transitions []wfcel.TransitionDefinition) {
		for _, transition := range transitions {
			total += len(transition.Actions)
		}
	}
	for _, state := range def.States {
		total += len(state.EntryActions)
		total += len(state.ExitActions)
		count(state.Transitions)
	}
	count(def.Catch)
	return total
}
