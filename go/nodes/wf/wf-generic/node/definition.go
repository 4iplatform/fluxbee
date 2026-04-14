package node

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"regexp"
	"strconv"
	"strings"
	"time"

	jsonschema "github.com/santhosh-tekuri/jsonschema/v6"
)

const (
	workflowSchemaVersion = "1"
	minTimerDuration      = 60 * time.Second
)

var (
	l2NamePattern     = regexp.MustCompile(`^[A-Za-z][A-Za-z0-9_.-]*@[A-Za-z0-9][A-Za-z0-9.-]*$`)
	variablePattern   = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)
	dayDurationRegexp = regexp.MustCompile(`^([0-9]+)d$`)
)

type WorkflowDefinition struct {
	WFSchemaVersion string            `json:"wf_schema_version"`
	WorkflowType    string            `json:"workflow_type"`
	Description     string            `json:"description"`
	InputSchema     map[string]any    `json:"input_schema"`
	InitialState    string            `json:"initial_state"`
	TerminalStates  []string          `json:"terminal_states"`
	States          []StateDefinition `json:"states"`
	inputValidator  *jsonschema.Schema `json:"-"`
}

type StateDefinition struct {
	Name         string                 `json:"name"`
	Description  string                 `json:"description"`
	EntryActions []ActionDefinition     `json:"entry_actions"`
	ExitActions  []ActionDefinition     `json:"exit_actions"`
	Transitions  []TransitionDefinition `json:"transitions"`
}

type TransitionDefinition struct {
	EventMatch  EventMatch         `json:"event_match"`
	Guard       string             `json:"guard"`
	TargetState string             `json:"target_state"`
	Actions     []ActionDefinition `json:"actions"`
	program     any                `json:"-"`
}

type EventMatch struct {
	Msg  string  `json:"msg"`
	Type *string `json:"type,omitempty"`
}

type ActionMeta struct {
	Msg  string `json:"msg"`
	Type string `json:"type,omitempty"`
}

type ActionDefinition struct {
	Type           string      `json:"type"`
	Target         string      `json:"target,omitempty"`
	Meta           *ActionMeta `json:"meta,omitempty"`
	Payload        any         `json:"payload,omitempty"`
	TimerKey       string      `json:"timer_key,omitempty"`
	FireIn         string      `json:"fire_in,omitempty"`
	FireAt         string      `json:"fire_at,omitempty"`
	MissedPolicy   string      `json:"missed_policy,omitempty"`
	MissedWithinMS *int64      `json:"missed_within_ms,omitempty"`
	Name           string      `json:"name,omitempty"`
	Value          any         `json:"value,omitempty"`
}

type LoadError struct {
	Path    string
	Message string
}

func (e *LoadError) Error() string {
	if e.Path == "" {
		return e.Message
	}
	return fmt.Sprintf("%s: %s", e.Path, e.Message)
}

func LoadDefinitionFile(path string, clock ClockFunc) (*WorkflowDefinition, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	return LoadDefinitionBytes(data, path, clock)
}

func LoadDefinitionBytes(data []byte, source string, clock ClockFunc) (*WorkflowDefinition, error) {
	var def WorkflowDefinition
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()
	if err := dec.Decode(&def); err != nil {
		if source == "" {
			return nil, err
		}
		return nil, fmt.Errorf("%s: %w", source, err)
	}
	if err := ValidateDefinition(&def, clock); err != nil {
		return nil, err
	}
	return &def, nil
}

func ValidateDefinition(def *WorkflowDefinition, clock ClockFunc) error {
	if def == nil {
		return &LoadError{Message: "definition is nil"}
	}
	if def.WFSchemaVersion != workflowSchemaVersion {
		return &LoadError{Path: "wf_schema_version", Message: `must be "1"`}
	}
	if strings.TrimSpace(def.WorkflowType) == "" {
		return &LoadError{Path: "workflow_type", Message: "must not be empty"}
	}
	if len(def.InputSchema) == 0 {
		return &LoadError{Path: "input_schema", Message: "must not be empty"}
	}
	inputValidator, err := compileJSONSchema(def.InputSchema)
	if err != nil {
		return &LoadError{Path: "input_schema", Message: err.Error()}
	}
	def.inputValidator = inputValidator
	if len(def.States) == 0 {
		return &LoadError{Path: "states", Message: "must contain at least one state"}
	}
	stateIndex := map[string]struct{}{}
	for i, state := range def.States {
		path := fmt.Sprintf("states[%d]", i)
		if strings.TrimSpace(state.Name) == "" {
			return &LoadError{Path: path + ".name", Message: "must not be empty"}
		}
		if _, exists := stateIndex[state.Name]; exists {
			return &LoadError{Path: path + ".name", Message: "duplicate state name"}
		}
		stateIndex[state.Name] = struct{}{}
		if err := validateActions(path+".entry_actions", state.EntryActions, clock); err != nil {
			return err
		}
		if err := validateActions(path+".exit_actions", state.ExitActions, clock); err != nil {
			return err
		}
		for j, transition := range state.Transitions {
			tPath := fmt.Sprintf("%s.transitions[%d]", path, j)
			if strings.TrimSpace(transition.EventMatch.Msg) == "" {
				return &LoadError{Path: tPath + ".event_match.msg", Message: "must not be empty"}
			}
			if transition.EventMatch.Type != nil && strings.TrimSpace(*transition.EventMatch.Type) == "" {
				return &LoadError{Path: tPath + ".event_match.type", Message: "must not be empty when present"}
			}
			if strings.TrimSpace(transition.TargetState) == "" {
				return &LoadError{Path: tPath + ".target_state", Message: "must not be empty"}
			}
			if strings.TrimSpace(transition.Guard) == "" {
				return &LoadError{Path: tPath + ".guard", Message: "must not be empty"}
			}
			program, err := compileGuard(transition.Guard, clock)
			if err != nil {
				return &LoadError{Path: tPath + ".guard", Message: err.Error()}
			}
			def.States[i].Transitions[j].program = program
			if err := validateActions(tPath+".actions", transition.Actions, clock); err != nil {
				return err
			}
		}
	}
	if _, ok := stateIndex[def.InitialState]; !ok {
		return &LoadError{Path: "initial_state", Message: "must reference an existing state"}
	}
	for i, name := range def.TerminalStates {
		if _, ok := stateIndex[name]; !ok {
			return &LoadError{Path: fmt.Sprintf("terminal_states[%d]", i), Message: "must reference an existing state"}
		}
	}
	for i, state := range def.States {
		for j, transition := range state.Transitions {
			if _, ok := stateIndex[transition.TargetState]; !ok {
				return &LoadError{
					Path:    fmt.Sprintf("states[%d].transitions[%d].target_state", i, j),
					Message: "must reference an existing state",
				}
			}
		}
	}
	return nil
}

func compileJSONSchema(schema map[string]any) (*jsonschema.Schema, error) {
	data, err := json.Marshal(schema)
	if err != nil {
		return nil, err
	}
	doc, err := jsonschema.UnmarshalJSON(bytes.NewReader(data))
	if err != nil {
		return nil, err
	}
	compiler := jsonschema.NewCompiler()
	if err := compiler.AddResource("wf-input-schema.json", doc); err != nil {
		return nil, err
	}
	return compiler.Compile("wf-input-schema.json")
}

func validateInputPayload(def *WorkflowDefinition, input map[string]any) error {
	if def == nil {
		return fmt.Errorf("workflow definition is nil")
	}
	if input == nil {
		input = map[string]any{}
	}
	if def.inputValidator == nil {
		inputValidator, err := compileJSONSchema(def.InputSchema)
		if err != nil {
			return err
		}
		def.inputValidator = inputValidator
	}
	return def.inputValidator.Validate(input)
}

func validateActions(path string, actions []ActionDefinition, clock ClockFunc) error {
	for i, action := range actions {
		if err := validateAction(fmt.Sprintf("%s[%d]", path, i), action, clock); err != nil {
			return err
		}
	}
	return nil
}

func validateAction(path string, action ActionDefinition, clock ClockFunc) error {
	switch action.Type {
	case "send_message":
		if !isValidL2Name(action.Target) {
			return &LoadError{Path: path + ".target", Message: "must be a valid L2 name"}
		}
		if action.Meta == nil || strings.TrimSpace(action.Meta.Msg) == "" {
			return &LoadError{Path: path + ".meta.msg", Message: "must not be empty"}
		}
		if action.Meta.Type != "" && action.Meta.Type != "system" && action.Meta.Type != "user" {
			return &LoadError{Path: path + ".meta.type", Message: `must be "system" or "user" when present`}
		}
		if err := validateRefPayload(path+".payload", action.Payload); err != nil {
			return err
		}
	case "schedule_timer":
		if strings.TrimSpace(action.TimerKey) == "" {
			return &LoadError{Path: path + ".timer_key", Message: "must not be empty"}
		}
		if err := validateTimerFireSpec(path, action.FireIn, action.FireAt); err != nil {
			return err
		}
		if action.FireIn != "" {
			duration, err := parseWorkflowDuration(action.FireIn)
			if err != nil {
				return &LoadError{Path: path + ".fire_in", Message: err.Error()}
			}
			if duration < minTimerDuration {
				return &LoadError{Path: path + ".fire_in", Message: "must be at least 60s"}
			}
		}
		if err := validateMissedPolicy(path, action.MissedPolicy, action.MissedWithinMS); err != nil {
			return err
		}
	case "cancel_timer":
		if strings.TrimSpace(action.TimerKey) == "" {
			return &LoadError{Path: path + ".timer_key", Message: "must not be empty"}
		}
	case "reschedule_timer":
		if strings.TrimSpace(action.TimerKey) == "" {
			return &LoadError{Path: path + ".timer_key", Message: "must not be empty"}
		}
		if err := validateTimerFireSpec(path, action.FireIn, action.FireAt); err != nil {
			return err
		}
		if action.FireIn != "" {
			duration, err := parseWorkflowDuration(action.FireIn)
			if err != nil {
				return &LoadError{Path: path + ".fire_in", Message: err.Error()}
			}
			if duration < minTimerDuration {
				return &LoadError{Path: path + ".fire_in", Message: "must be at least 60s"}
			}
		}
	case "set_variable":
		if !variablePattern.MatchString(action.Name) {
			return &LoadError{Path: path + ".name", Message: "must be a valid identifier"}
		}
		if action.Value == nil {
			return &LoadError{Path: path + ".value", Message: "must not be null"}
		}
		valueExpr, ok := action.Value.(string)
		if !ok || strings.TrimSpace(valueExpr) == "" {
			return &LoadError{Path: path + ".value", Message: "must be a non-empty CEL expression string"}
		}
		if _, err := compileGuard(valueExpr, clock); err != nil {
			return &LoadError{Path: path + ".value", Message: err.Error()}
		}
	default:
		return &LoadError{Path: path + ".type", Message: "unknown action type"}
	}
	return nil
}

func validateTimerFireSpec(path, fireIn, fireAt string) error {
	hasFireIn := strings.TrimSpace(fireIn) != ""
	hasFireAt := strings.TrimSpace(fireAt) != ""
	switch {
	case hasFireIn && hasFireAt:
		return &LoadError{Path: path, Message: "fire_in and fire_at are mutually exclusive"}
	case !hasFireIn && !hasFireAt:
		return &LoadError{Path: path, Message: "exactly one of fire_in or fire_at is required"}
	case hasFireAt:
		if _, err := time.Parse(time.RFC3339, fireAt); err != nil {
			return &LoadError{Path: path + ".fire_at", Message: "must be RFC3339 UTC timestamp"}
		}
	}
	return nil
}

func validateMissedPolicy(path, policy string, within *int64) error {
	switch policy {
	case "", "fire", "drop":
		if within != nil {
			return &LoadError{Path: path + ".missed_within_ms", Message: "allowed only with missed_policy=fire_if_within"}
		}
	case "fire_if_within":
		if within == nil || *within <= 0 {
			return &LoadError{Path: path + ".missed_within_ms", Message: "must be > 0 when missed_policy=fire_if_within"}
		}
	default:
		return &LoadError{Path: path + ".missed_policy", Message: "must be fire, drop, or fire_if_within"}
	}
	return nil
}

func validateRefPayload(path string, value any) error {
	switch typed := value.(type) {
	case nil:
		return nil
	case map[string]any:
		if refValue, ok := typed["$ref"]; ok && len(typed) == 1 {
			refPath, ok := refValue.(string)
			if !ok || strings.TrimSpace(refPath) == "" {
				return &LoadError{Path: path + ".$ref", Message: "must be a non-empty string"}
			}
			if err := validateRefPath(refPath); err != nil {
				return &LoadError{Path: path + ".$ref", Message: err.Error()}
			}
			return nil
		}
		for key, child := range typed {
			if err := validateRefPayload(path+"."+key, child); err != nil {
				return err
			}
		}
	case []any:
		for i, child := range typed {
			if err := validateRefPayload(fmt.Sprintf("%s[%d]", path, i), child); err != nil {
				return err
			}
		}
	}
	return nil
}

func validateRefPath(path string) error {
	parts := strings.Split(path, ".")
	if len(parts) < 2 {
		return fmt.Errorf("must begin with input., state., or event. and include at least one field")
	}
	switch parts[0] {
	case "input", "state", "event":
	default:
		return fmt.Errorf("root must be input, state, or event")
	}
	for _, part := range parts[1:] {
		if strings.TrimSpace(part) == "" {
			return fmt.Errorf("path segments must not be empty")
		}
	}
	return nil
}

func parseWorkflowDuration(value string) (time.Duration, error) {
	if match := dayDurationRegexp.FindStringSubmatch(strings.TrimSpace(value)); len(match) == 2 {
		days, err := strconv.Atoi(match[1])
		if err != nil {
			return 0, fmt.Errorf("invalid duration %q", value)
		}
		return time.Duration(days) * 24 * time.Hour, nil
	}
	duration, err := time.ParseDuration(value)
	if err != nil {
		return 0, fmt.Errorf("invalid duration %q", value)
	}
	return duration, nil
}

func isValidL2Name(value string) bool {
	return l2NamePattern.MatchString(strings.TrimSpace(value))
}
