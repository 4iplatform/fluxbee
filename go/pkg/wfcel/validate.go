package wfcel

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

var (
	l2NamePattern   = regexp.MustCompile(`^[A-Za-z][A-Za-z0-9_.-]*@[A-Za-z0-9][A-Za-z0-9.-]*$`)
	variablePattern = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)
	dayDurationRE   = regexp.MustCompile(`^([0-9]+)d$`)
)

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
	if errs := ValidateDefinition(&def, clock); len(errs) > 0 {
		return nil, errs
	}
	return &def, nil
}

func ValidateDefinition(def *WorkflowDefinition, clock ClockFunc) ValidationErrors {
	var errs ValidationErrors
	if def == nil {
		return ValidationErrors{{Message: "definition is nil"}}
	}

	if def.WFSchemaVersion != WorkflowSchemaVersion {
		errs = append(errs, ValidationError{Path: "wf_schema_version", Message: `must be "1"`})
	}
	if strings.TrimSpace(def.WorkflowType) == "" {
		errs = append(errs, ValidationError{Path: "workflow_type", Message: "must not be empty"})
	}
	if len(def.InputSchema) == 0 {
		errs = append(errs, ValidationError{Path: "input_schema", Message: "must not be empty"})
	} else {
		inputValidator, err := compileJSONSchema(def.InputSchema)
		if err != nil {
			errs = append(errs, ValidationError{Path: "input_schema", Message: err.Error()})
		} else {
			def.inputValidator = inputValidator
		}
	}
	if len(def.States) == 0 {
		errs = append(errs, ValidationError{Path: "states", Message: "must contain at least one state"})
	}

	stateIndex := map[string]struct{}{}
	for i := range def.States {
		state := def.States[i]
		path := fmt.Sprintf("states[%d]", i)
		if strings.TrimSpace(state.Name) == "" {
			errs = append(errs, ValidationError{Path: path + ".name", Message: "must not be empty"})
		} else if _, exists := stateIndex[state.Name]; exists {
			errs = append(errs, ValidationError{Path: path + ".name", Message: "duplicate state name"})
		} else {
			stateIndex[state.Name] = struct{}{}
		}
		errs = append(errs, validateActions(path+".entry_actions", state.EntryActions, clock)...)
		errs = append(errs, validateActions(path+".exit_actions", state.ExitActions, clock)...)

		for j := range state.Transitions {
			transition := state.Transitions[j]
			tPath := fmt.Sprintf("%s.transitions[%d]", path, j)
			if strings.TrimSpace(transition.EventMatch.Msg) == "" {
				errs = append(errs, ValidationError{Path: tPath + ".event_match.msg", Message: "must not be empty"})
			}
			if transition.EventMatch.Type != nil && strings.TrimSpace(*transition.EventMatch.Type) == "" {
				errs = append(errs, ValidationError{Path: tPath + ".event_match.type", Message: "must not be empty when present"})
			}
			if strings.TrimSpace(transition.TargetState) == "" {
				errs = append(errs, ValidationError{Path: tPath + ".target_state", Message: "must not be empty"})
			}
			if strings.TrimSpace(transition.Guard) == "" {
				errs = append(errs, ValidationError{Path: tPath + ".guard", Message: "must not be empty"})
			} else {
				program, err := CompileGuard(transition.Guard, clock)
				if err != nil {
					errs = append(errs, ValidationError{Path: tPath + ".guard", Message: err.Error()})
				} else {
					def.States[i].Transitions[j].Program = program
				}
			}
			errs = append(errs, validateActions(tPath+".actions", transition.Actions, clock)...)
		}
	}

	if _, ok := stateIndex[def.InitialState]; !ok {
		errs = append(errs, ValidationError{Path: "initial_state", Message: "must reference an existing state"})
	}
	for i, name := range def.TerminalStates {
		if _, ok := stateIndex[name]; !ok {
			errs = append(errs, ValidationError{
				Path:    fmt.Sprintf("terminal_states[%d]", i),
				Message: "must reference an existing state",
			})
		}
	}
	for i, state := range def.States {
		for j, transition := range state.Transitions {
			if _, ok := stateIndex[transition.TargetState]; !ok {
				errs = append(errs, ValidationError{
					Path:    fmt.Sprintf("states[%d].transitions[%d].target_state", i, j),
					Message: "must reference an existing state",
				})
			}
		}
	}
	return errs
}

func ValidateInputPayload(def *WorkflowDefinition, input map[string]any) error {
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

func ParseWorkflowDuration(value string) (time.Duration, error) {
	if match := dayDurationRE.FindStringSubmatch(strings.TrimSpace(value)); len(match) == 2 {
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

func IsValidL2Name(value string) bool {
	return l2NamePattern.MatchString(strings.TrimSpace(value))
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

func validateActions(path string, actions []ActionDefinition, clock ClockFunc) ValidationErrors {
	var errs ValidationErrors
	for i, action := range actions {
		errs = append(errs, validateAction(fmt.Sprintf("%s[%d]", path, i), action, clock)...)
	}
	return errs
}

func validateAction(path string, action ActionDefinition, clock ClockFunc) ValidationErrors {
	var errs ValidationErrors
	switch action.Type {
	case "send_message":
		if !IsValidL2Name(action.Target) {
			errs = append(errs, ValidationError{Path: path + ".target", Message: "must be a valid L2 name"})
		}
		if action.Meta == nil || strings.TrimSpace(action.Meta.Msg) == "" {
			errs = append(errs, ValidationError{Path: path + ".meta.msg", Message: "must not be empty"})
		}
		if action.Meta != nil && action.Meta.Type != "" && action.Meta.Type != "system" && action.Meta.Type != "user" {
			errs = append(errs, ValidationError{Path: path + ".meta.type", Message: `must be "system" or "user" when present`})
		}
		errs = append(errs, validateRefPayload(path+".payload", action.Payload)...)
	case "schedule_timer":
		if strings.TrimSpace(action.TimerKey) == "" {
			errs = append(errs, ValidationError{Path: path + ".timer_key", Message: "must not be empty"})
		}
		errs = append(errs, validateTimerFireSpec(path, action.FireIn, action.FireAt)...)
		if action.FireIn != "" {
			duration, err := ParseWorkflowDuration(action.FireIn)
			if err != nil {
				errs = append(errs, ValidationError{Path: path + ".fire_in", Message: err.Error()})
			} else if duration < MinTimerDuration {
				errs = append(errs, ValidationError{Path: path + ".fire_in", Message: "must be at least 60s"})
			}
		}
		errs = append(errs, validateMissedPolicy(path, action.MissedPolicy, action.MissedWithinMS)...)
	case "cancel_timer":
		if strings.TrimSpace(action.TimerKey) == "" {
			errs = append(errs, ValidationError{Path: path + ".timer_key", Message: "must not be empty"})
		}
	case "reschedule_timer":
		if strings.TrimSpace(action.TimerKey) == "" {
			errs = append(errs, ValidationError{Path: path + ".timer_key", Message: "must not be empty"})
		}
		errs = append(errs, validateTimerFireSpec(path, action.FireIn, action.FireAt)...)
		if action.FireIn != "" {
			duration, err := ParseWorkflowDuration(action.FireIn)
			if err != nil {
				errs = append(errs, ValidationError{Path: path + ".fire_in", Message: err.Error()})
			} else if duration < MinTimerDuration {
				errs = append(errs, ValidationError{Path: path + ".fire_in", Message: "must be at least 60s"})
			}
		}
	case "set_variable":
		if !variablePattern.MatchString(action.Name) {
			errs = append(errs, ValidationError{Path: path + ".name", Message: "must be a valid identifier"})
		}
		if action.Value == nil {
			errs = append(errs, ValidationError{Path: path + ".value", Message: "must not be null"})
		} else {
			valueExpr, ok := action.Value.(string)
			if !ok || strings.TrimSpace(valueExpr) == "" {
				errs = append(errs, ValidationError{Path: path + ".value", Message: "must be a non-empty CEL expression string"})
			} else if _, err := CompileGuard(valueExpr, clock); err != nil {
				errs = append(errs, ValidationError{Path: path + ".value", Message: err.Error()})
			}
		}
	default:
		errs = append(errs, ValidationError{Path: path + ".type", Message: "unknown action type"})
	}
	return errs
}

func validateTimerFireSpec(path, fireIn, fireAt string) ValidationErrors {
	hasFireIn := strings.TrimSpace(fireIn) != ""
	hasFireAt := strings.TrimSpace(fireAt) != ""
	switch {
	case hasFireIn && hasFireAt:
		return ValidationErrors{{Path: path, Message: "fire_in and fire_at are mutually exclusive"}}
	case !hasFireIn && !hasFireAt:
		return ValidationErrors{{Path: path, Message: "exactly one of fire_in or fire_at is required"}}
	case hasFireAt:
		if _, err := time.Parse(time.RFC3339, fireAt); err != nil {
			return ValidationErrors{{Path: path + ".fire_at", Message: "must be RFC3339 UTC timestamp"}}
		}
	}
	return nil
}

func validateMissedPolicy(path, policy string, within *int64) ValidationErrors {
	switch policy {
	case "", "fire", "drop":
		if within != nil {
			return ValidationErrors{{Path: path + ".missed_within_ms", Message: "allowed only with missed_policy=fire_if_within"}}
		}
	case "fire_if_within":
		if within == nil || *within <= 0 {
			return ValidationErrors{{Path: path + ".missed_within_ms", Message: "must be > 0 when missed_policy=fire_if_within"}}
		}
	default:
		return ValidationErrors{{Path: path + ".missed_policy", Message: "must be fire, drop, or fire_if_within"}}
	}
	return nil
}

func validateRefPayload(path string, value any) ValidationErrors {
	switch typed := value.(type) {
	case nil:
		return nil
	case map[string]any:
		if refValue, ok := typed["$ref"]; ok && len(typed) == 1 {
			refPath, ok := refValue.(string)
			if !ok || strings.TrimSpace(refPath) == "" {
				return ValidationErrors{{Path: path + ".$ref", Message: "must be a non-empty string"}}
			}
			if err := validateRefPath(refPath); err != nil {
				return ValidationErrors{{Path: path + ".$ref", Message: err.Error()}}
			}
			return nil
		}
		var errs ValidationErrors
		for key, child := range typed {
			errs = append(errs, validateRefPayload(path+"."+key, child)...)
		}
		return errs
	case []any:
		var errs ValidationErrors
		for i, child := range typed {
			errs = append(errs, validateRefPayload(fmt.Sprintf("%s[%d]", path, i), child)...)
		}
		return errs
	default:
		return nil
	}
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
