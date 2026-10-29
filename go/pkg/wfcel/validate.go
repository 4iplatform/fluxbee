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
	if errs := removedFieldErrors(data); len(errs) > 0 {
		return nil, errs
	}
	var def WorkflowDefinition
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()
	dec.UseNumber()
	if err := dec.Decode(&def); err != nil {
		if source == "" {
			return nil, err
		}
		return nil, fmt.Errorf("%s: %w", source, err)
	}
	normalizeActionNumbers(&def)
	if errs := ValidateDefinition(&def, clock); len(errs) > 0 {
		return nil, errs
	}
	return &def, nil
}

// normalizeActionNumbers gives the literals of the definition's actions (payloads, meta.context,
// set_variable values, $ref defaults) DecodeJSON's types: an integer literal is an int64 and any
// other number a float64, so a send puts 3 on the wire as 3, and 1.0 as 1.0 (decoded as float64,
// 3 went out as 3.0, which a Rust u64 field refuses). The input_schema keeps its numbers as written
// (json.Number), which is how they reach the schema compiler and WF_HELP.
func normalizeActionNumbers(def *WorkflowDefinition) {
	normalize := func(actions []ActionDefinition) {
		for i := range actions {
			actions[i].Payload = normalizeNumbers(actions[i].Payload)
			actions[i].Value = normalizeNumbers(actions[i].Value)
			if actions[i].Meta != nil {
				actions[i].Meta.Context = normalizeNumbers(actions[i].Meta.Context)
			}
		}
	}
	for i := range def.States {
		normalize(def.States[i].EntryActions)
		normalize(def.States[i].ExitActions)
		for j := range def.States[i].Transitions {
			normalize(def.States[i].Transitions[j].Actions)
		}
	}
}

// removedFieldErrors names, by their paths, the fields that left the definition language in 0.1.90
// (wf-orchestration-v1 §5.1): the strict decoder refuses them too, but only as an unknown field,
// without saying where or why. A definition that is not JSON is the decoder's to report.
func removedFieldErrors(data []byte) ValidationErrors {
	var doc struct {
		States []struct {
			EntryActions []map[string]any `json:"entry_actions"`
			ExitActions  []map[string]any `json:"exit_actions"`
			Transitions  []struct {
				EventMatch map[string]any   `json:"event_match"`
				Actions    []map[string]any `json:"actions"`
			} `json:"transitions"`
		} `json:"states"`
	}
	if json.Unmarshal(data, &doc) != nil {
		return nil
	}
	var errs ValidationErrors
	check := func(path string, actions []map[string]any) {
		for i, action := range actions {
			actionPath := fmt.Sprintf("%s[%d]", path, i)
			if _, ok := action["target_ref"]; ok {
				errs = append(errs, ValidationError{Path: actionPath + ".target_ref", Message: "is not part of the definition language: a WF sends to node names written in the definition, one transition per destination"})
			}
			if meta, ok := action["meta"].(map[string]any); ok {
				if _, ok := meta["type"]; ok {
					errs = append(errs, ValidationError{Path: actionPath + ".meta.type", Message: "is not part of the definition language: every message a WF sends is a user message"})
				}
			}
		}
	}
	for i, state := range doc.States {
		path := fmt.Sprintf("states[%d]", i)
		check(path+".entry_actions", state.EntryActions)
		check(path+".exit_actions", state.ExitActions)
		for j, transition := range state.Transitions {
			tPath := fmt.Sprintf("%s.transitions[%d]", path, j)
			if _, ok := transition.EventMatch["type"]; ok {
				errs = append(errs, ValidationError{Path: tPath + ".event_match.type", Message: "is not part of the definition language: every message a WF takes is a user message, but a TIMER_FIRED, which event_match.msg names"})
			}
			check(tPath+".actions", transition.Actions)
		}
	}
	return errs
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
	terminalSet := make(map[string]bool, len(def.TerminalStates))
	for i, name := range def.TerminalStates {
		terminalSet[name] = true
		if _, ok := stateIndex[name]; !ok {
			errs = append(errs, ValidationError{
				Path:    fmt.Sprintf("terminal_states[%d]", i),
				Message: "must reference an existing state",
			})
		}
	}
	// gap-2: "failed" is the reserved error-sink state name routeToFailed drives an instance into on a
	// fatal action failure (terminating it). If a definition declares a state named "failed" it MUST be
	// terminal, so the reservation can never silently hijack an author's non-terminal "failed" state.
	if _, ok := stateIndex["failed"]; ok && !terminalSet["failed"] {
		errs = append(errs, ValidationError{
			Path:    "states",
			Message: `a state named "failed" is reserved as the fatal-error sink and must be listed in terminal_states`,
		})
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

// IsForbiddenSystemTarget reports whether an L2 name is a SY.* / RT.* system/router node — which a WF
// send_message must never target (those names bypass VPN isolation at the router).
func IsForbiddenSystemTarget(name string) bool {
	// Case-INSENSITIVE: block "sy.vault" / "rt.gateway" evasion attempts too. (Node-name casing is not
	// a reliable isolation boundary, so do not let a lowercased system name slip through.)
	n := strings.ToUpper(strings.TrimSpace(name))
	return strings.HasPrefix(n, "SY.") || strings.HasPrefix(n, "RT.")
}

// isValidRefPath reports whether s is a well-formed $ref dot-path rooted at state/input/event with at
// least one field (e.g. "event.payload.thread_id") — the form a thread_id_ref is resolved against.
func isValidRefPath(s string) bool {
	parts := strings.Split(strings.TrimSpace(s), ".")
	if len(parts) < 2 {
		return false
	}
	switch parts[0] {
	case "state", "input", "event":
	default:
		return false
	}
	for _, p := range parts {
		if strings.TrimSpace(p) == "" {
			return false
		}
	}
	return true
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
	// gap-2: on_error, when present, must be a known policy.
	if action.OnError != "" && action.OnError != OnErrorContinue && action.OnError != OnErrorFail {
		errs = append(errs, ValidationError{Path: path + ".on_error", Message: `must be "continue" or "fail" when present`})
	}
	switch action.Type {
	case "send_message":
		// The target is a node name written in the definition. A WF may NOT message SY.*/RT.*
		// system nodes via send_message (that path bypasses VPN isolation in the router; timers use
		// schedule_timer, not send_message).
		switch {
		case strings.TrimSpace(action.Target) == "":
			errs = append(errs, ValidationError{Path: path + ".target", Message: "must not be empty"})
		case !IsValidL2Name(action.Target):
			errs = append(errs, ValidationError{Path: path + ".target", Message: "must be a valid L2 name"})
		case IsForbiddenSystemTarget(action.Target):
			errs = append(errs, ValidationError{Path: path + ".target", Message: "must not target a SY.* / RT.* system node"})
		}
		// gap-5: thread_id_ref, when set, must be a well-formed $ref dot-path (resolved at send time to
		// the external caller's thread_id for a terminal reply).
		if strings.TrimSpace(action.ThreadIDRef) != "" && !isValidRefPath(action.ThreadIDRef) {
			errs = append(errs, ValidationError{Path: path + ".thread_id_ref", Message: `must be a $ref dot-path rooted at state/input/event (e.g. "event.payload.thread_id")`})
		}
		if action.Meta == nil || strings.TrimSpace(action.Meta.Msg) == "" {
			errs = append(errs, ValidationError{Path: path + ".meta.msg", Message: "must not be empty"})
		}
		// gap-3: meta.context (used for response_envelope) is resolved like a payload — validate its
		// $ref shape the same way so a bad ref is caught at publish, not silently dropped at send.
		if action.Meta != nil && action.Meta.Context != nil {
			errs = append(errs, validateRefPayload(path+".meta.context", action.Meta.Context)...)
		}
		errs = append(errs, validateRefPayload(path+".payload", action.Payload)...)
	case "emit_internal_event":
		if action.Meta == nil || strings.TrimSpace(action.Meta.Msg) == "" {
			errs = append(errs, ValidationError{Path: path + ".meta.msg", Message: "must not be empty"})
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
			} else if _, err := CompileValue(valueExpr, clock); err != nil {
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

// Ref is a $ref in a payload or a meta.context: an object whose "$ref" is a dot path
// (IsWorkflowRef), which validation requires to be rooted at input., state. or event.
// {"$ref": path} gives the value at the path, and fails the action when the path does not resolve;
// {"$ref": path, "default": value} gives the literal value when the path does not resolve or holds
// null. Any other object, a JSON Schema's own "$ref" ("#/definitions/x") included, is a literal.
type Ref struct {
	Path       string
	Default    any
	HasDefault bool
}

// IsWorkflowRef reports whether object is a WF $ref: its "$ref" is a dot path, a string without the
// "#", "/" or ":" of a JSON Schema's own reference ("#/$defs/x", a URL, "./other.json"). A path with
// another root ("inptu.x") is a WF $ref too, so validation refuses it rather than sending it as a
// literal.
func IsWorkflowRef(object map[string]any) bool {
	path, ok := object["$ref"].(string)
	if !ok {
		return false
	}
	return !strings.ContainsAny(path, "#/:")
}

// AsRef reads object as a Ref: a WF $ref holding only "$ref" and, optionally, "default" (validation
// refuses one with any other key).
func AsRef(object map[string]any) (Ref, bool) {
	if !IsWorkflowRef(object) {
		return Ref{}, false
	}
	defaultValue, hasDefault := object["default"]
	switch {
	case len(object) == 1:
	case len(object) == 2 && hasDefault:
	default:
		return Ref{}, false
	}
	return Ref{Path: strings.TrimSpace(object["$ref"].(string)), Default: defaultValue, HasDefault: hasDefault}, true
}

func validateRefPayload(path string, value any) ValidationErrors {
	switch typed := value.(type) {
	case nil:
		return nil
	case map[string]any:
		if refPath, ok := typed["$ref"]; ok {
			if _, isString := refPath.(string); !isString {
				return ValidationErrors{{Path: path + ".$ref", Message: "must be a string"}}
			}
		}
		if IsWorkflowRef(typed) {
			ref, ok := AsRef(typed)
			if !ok {
				// A typo ("defualt") would otherwise send the object as a literal.
				return ValidationErrors{{Path: path, Message: `a $ref holds only $ref and default`}}
			}
			if err := validateRefPath(ref.Path); err != nil {
				return ValidationErrors{{Path: path + ".$ref", Message: err.Error()}}
			}
			if ref.HasDefault && holdsWorkflowRef(ref.Default) {
				return ValidationErrors{{Path: path + ".default", Message: "a default is a literal: it holds no $ref"}}
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

// holdsWorkflowRef reports whether value holds a WF $ref anywhere.
func holdsWorkflowRef(value any) bool {
	switch typed := value.(type) {
	case map[string]any:
		if IsWorkflowRef(typed) {
			return true
		}
		for _, child := range typed {
			if holdsWorkflowRef(child) {
				return true
			}
		}
	case []any:
		for _, child := range typed {
			if holdsWorkflowRef(child) {
				return true
			}
		}
	}
	return false
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
