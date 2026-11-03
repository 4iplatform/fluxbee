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

// CompileWorkflow validates a definition (loadWorkflow) and stages it as written, under the version
// the request gives or else the next one.
func (s *Service) CompileWorkflow(req CompileRequest) (*WfRulesMetadata, error) {
	loaded, err := s.loadWorkflow(req)
	if err != nil {
		return nil, err
	}
	version := req.Version
	if version == 0 {
		version = s.store.NextVersion(loaded.name)
	}
	meta := WfRulesMetadata{
		Version:         version,
		Hash:            HashDefinition(loaded.definitionBytes),
		WorkflowName:    loaded.name,
		WorkflowType:    loaded.def.WorkflowType,
		WFSchemaVersion: loaded.def.WFSchemaVersion,
		CompiledAt:      s.clock().UTC().Format(timeRFC3339),
		GuardCount:      countGuards(loaded.def),
		StateCount:      len(loaded.def.States),
		ActionCount:     countActions(loaded.def),
	}
	if err := s.store.WriteStaged(loaded.name, loaded.definitionBytes, meta); err != nil {
		return nil, err
	}
	return &meta, nil
}

// WorkflowCheck is what a check answers of a definition that loads: its workflow, and the counts a
// compile would stage.
type WorkflowCheck struct {
	WorkflowName string
	GuardCount   int
	StateCount   int
}

// CheckWorkflow validates a definition as CompileWorkflow does (loadWorkflow: the same load, the
// same limits, the same errors with their paths) and writes nothing (wf-orchestration-v1 §5.5.1):
// no staged definition, no package, no file of the store. It takes no version, and takes up none.
// A check is a read, as SY.opa-rules' is, so one between a compile and its apply leaves the staged
// definition and the current one as they were.
func (s *Service) CheckWorkflow(req CompileRequest) (*WorkflowCheck, error) {
	loaded, err := s.loadWorkflow(req)
	if err != nil {
		return nil, err
	}
	return &WorkflowCheck{
		WorkflowName: loaded.name,
		GuardCount:   countGuards(loaded.def),
		StateCount:   len(loaded.def.States),
	}, nil
}

// loadedWorkflow is a compile request whose definition loads: the workflow's name, the definition's
// bytes as a compile stages them, and the definition the WF node loads from them.
type loadedWorkflow struct {
	name            string
	definitionBytes []byte
	def             *wfcel.WorkflowDefinition
}

// loadWorkflow validates what a compile and a check are given: the workflow's name
// (INVALID_WORKFLOW_NAME), a definition (INVALID_CONFIG_SET without one), and the definition loaded
// as the WF node loads it, with wfcel.LoadDefinitionBytes (COMPILE_ERROR, naming each path).
func (s *Service) loadWorkflow(req CompileRequest) (loadedWorkflow, error) {
	workflowName := strings.TrimSpace(req.WorkflowName)
	if !workflowNamePattern.MatchString(workflowName) {
		return loadedWorkflow{}, invalidWorkflowName(workflowName)
	}
	if req.Definition == nil {
		return loadedWorkflow{}, WfRulesError{Code: "INVALID_CONFIG_SET", Detail: "definition is required"}
	}
	definitionBytes, err := normalizeDefinitionBytes(req.Definition)
	if err != nil {
		return loadedWorkflow{}, WfRulesError{Code: "INVALID_CONFIG_SET", Detail: err.Error()}
	}
	def, err := wfcel.LoadDefinitionBytes(definitionBytes, "", s.clock)
	if err != nil {
		return loadedWorkflow{}, WfRulesError{Code: "COMPILE_ERROR", Detail: err.Error()}
	}
	return loadedWorkflow{name: workflowName, definitionBytes: definitionBytes, def: def}, nil
}

// checkSlotLoads loads the definition of a slot as the WF node will, before an apply publishes the
// staged one or a rollback the backup: one staged or backed up by an earlier release may no longer
// load (a known option written whole, say), and published, it left the node FAILED_CONFIG at its
// next start. A definition that does not load is a COMPILE_ERROR, with the paths the load names, and
// the slots stay as they were.
func (s *Service) checkSlotLoads(workflowName, slot string, version uint64) error {
	definitionBytes, err := s.store.ReadDefinitionBytes(workflowName, slot)
	if err != nil {
		return err
	}
	if _, err := wfcel.LoadDefinitionBytes(definitionBytes, "", s.clock); err != nil {
		return WfRulesError{Code: "COMPILE_ERROR", Detail: fmt.Sprintf("the %s definition (version %d) no longer loads: %v", slot, version, err)}
	}
	return nil
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
