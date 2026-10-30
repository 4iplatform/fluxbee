package wfcel

import (
	"bytes"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"regexp"
	"slices"
	"sort"
	"strconv"
	"strings"
	"time"

	jsonschema "github.com/santhosh-tekuri/jsonschema/v6"
)

// maxDurationDays is the most days a time.Duration holds.
const maxDurationDays = math.MaxInt64 / int64(24*time.Hour)

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
	if errs := fieldErrors(data); len(errs) > 0 {
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
// vars, timer times, set_variable values, $ref defaults) DecodeJSON's types: an integer literal is
// an int64 and any other number a float64, so a send puts 3 on the wire as 3, and 1.0 as 1.0
// (decoded as float64, 3 went out as 3.0, which a Rust u64 field refuses). The input_schema keeps
// its numbers as written (json.Number), which is how they reach the schema compiler and WF_HELP.
func normalizeActionNumbers(def *WorkflowDefinition) {
	normalize := func(actions []ActionDefinition) {
		for i := range actions {
			actions[i].Payload = normalizeNumbers(actions[i].Payload)
			actions[i].Vars = normalizeNumbers(actions[i].Vars)
			actions[i].FireIn = normalizeNumbers(actions[i].FireIn)
			actions[i].FireAt = normalizeNumbers(actions[i].FireAt)
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

// actionFields are the fields each action type takes besides type and on_error; actionMetaFields,
// those of its meta. A field of another type is refused (fieldErrors): a forward with a name
// instead of a message loaded, and forwarded the event.
var (
	actionFields = map[string][]string{
		"send_message":        {"target", "meta", "vars", "payload"},
		"forward":             {"target", "message", "meta", "vars"},
		"keep_message":        {"name"},
		"emit_internal_event": {"meta", "payload"},
		"schedule_timer":      {"timer_key", "fire_in", "fire_at", "missed_policy", "missed_within_ms"},
		"cancel_timer":        {"timer_key"},
		"reschedule_timer":    {"timer_key", "fire_in", "fire_at"},
		"set_variable":        {"name", "value"},
	}
	actionMetaFields = map[string][]string{
		"send_message":        {"msg", "context"},
		"forward":             {"context"},
		"emit_internal_event": {"msg"},
	}
	// fieldHints say why a field is not a type's, where the reason is not obvious.
	fieldHints = map[string]string{
		"forward.name":     "a forward names the kept message it sends with message",
		"forward.payload":  "a forward sends the message's payload as it was received",
		"forward.meta.msg": "a forward keeps the message's own msg",
	}
)

// fieldErrors names, by their paths, the fields an action has that are not its type's (another
// type's field: a key no type has is the strict decoder's to report), and the fields that left the
// definition language in 0.1.90 (wf-orchestration-v1 §5.1) and 0.1.91 (thread_id_ref, §5.2.1),
// which the strict decoder refuses too, but only as an unknown field, without saying where or why.
// A definition that is not JSON is the decoder's to report.
func fieldErrors(data []byte) ValidationErrors {
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
	known := map[string]bool{}
	for _, fields := range actionFields {
		for _, field := range fields {
			known[field] = true
		}
	}
	var errs ValidationErrors
	notOfType := func(path, actionType, field string) {
		message := "is not a field of " + actionType
		if hint, ok := fieldHints[actionType+"."+field]; ok {
			message += ": " + hint
		}
		errs = append(errs, ValidationError{Path: path + "." + field, Message: message})
	}
	check := func(path string, actions []map[string]any) {
		for i, action := range actions {
			actionPath := fmt.Sprintf("%s[%d]", path, i)
			if _, ok := action["target_ref"]; ok {
				errs = append(errs, ValidationError{Path: actionPath + ".target_ref", Message: "is not part of the definition language: a WF sends to node names written in the definition, one transition per destination"})
			}
			if _, ok := action["thread_id_ref"]; ok {
				errs = append(errs, ValidationError{Path: actionPath + ".thread_id_ref", Message: "is not part of the definition language: every message an instance sends carries the instance's thread, the one of the message that started it"})
			}
			meta, _ := action["meta"].(map[string]any)
			if _, ok := meta["type"]; ok {
				errs = append(errs, ValidationError{Path: actionPath + ".meta.type", Message: "is not part of the definition language: every message a WF sends is a user message"})
			}
			actionType, _ := action["type"].(string)
			fields, ok := actionFields[actionType]
			if !ok {
				// An unknown type is validateAction's to report.
				continue
			}
			for _, field := range sortedKeys(action) {
				if known[field] && !slices.Contains(fields, field) {
					notOfType(actionPath, actionType, field)
				}
			}
			if !slices.Contains(fields, "meta") {
				continue
			}
			for _, field := range sortedKeys(meta) {
				if (field == "msg" || field == "context") && !slices.Contains(actionMetaFields[actionType], field) {
					notOfType(actionPath, actionType, "meta."+field)
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

// sortedKeys are object's keys, sorted, so errors come in the same order every time.
func sortedKeys[V any](object map[string]V) []string {
	keys := make([]string, 0, len(object))
	for key := range object {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
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
	if len(def.StartFrom) == 0 {
		errs = append(errs, ValidationError{Path: "start_from", Message: "must name the nodes whose messages start an instance"})
	}
	for i, name := range def.StartFrom {
		errs = append(errs, validateStarter(fmt.Sprintf("start_from[%d]", i), name)...)
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
			switch {
			case strings.TrimSpace(transition.EventMatch.Msg) == "" && transition.EventMatch.Src == "":
				errs = append(errs, ValidationError{Path: tPath + ".event_match", Message: "must name a msg, a src or both"})
			case transition.EventMatch.Msg != "" && strings.TrimSpace(transition.EventMatch.Msg) == "":
				errs = append(errs, ValidationError{Path: tPath + ".event_match.msg", Message: "must not be blank"})
			}
			if transition.EventMatch.Src != "" {
				errs = append(errs, validateNodeName(tPath+".event_match.src", transition.EventMatch.Src)...)
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
			// A transition without a target_state is internal: it stays in its state.
			if transition.TargetState == "" {
				continue
			}
			if _, ok := stateIndex[transition.TargetState]; !ok {
				errs = append(errs, ValidationError{
					Path:    fmt.Sprintf("states[%d].transitions[%d].target_state", i, j),
					Message: "must reference an existing state",
				})
			}
		}
	}
	// A forward sends a kept message by a name some keep_message keeps: any other name is a typo,
	// which at run time would fail the instance.
	kept := map[string]bool{}
	def.eachAction(func(_ string, action ActionDefinition) {
		if action.Type == "keep_message" {
			kept[action.Name] = true
		}
	})
	def.eachAction(func(path string, action ActionDefinition) {
		if action.Type == "forward" && action.Message != "" && !kept[action.Message] {
			errs = append(errs, ValidationError{Path: path + ".message", Message: "must name a message a keep_message keeps"})
		}
	})
	errs = append(errs, timerFiredErrors(def)...)
	return errs
}

// timerFiredErrors refuses, on a transition matched on msg TIMER_FIRED, what needs a node's message:
// a keep_message, and a forward without message (of the event). A TIMER_FIRED is no message a node
// sent, so either would fail the instance when the timer fires. The transition's own actions run on
// it, and so do its state's exit actions and its target's entry actions.
func timerFiredErrors(def *WorkflowDefinition) ValidationErrors {
	stateAt := map[string]int{}
	for i, state := range def.States {
		stateAt[state.Name] = i
	}
	var errs ValidationErrors
	reported := map[string]bool{}
	check := func(path string, actions []ActionDefinition, on string) {
		for i, action := range actions {
			actionPath := fmt.Sprintf("%s[%d]", path, i)
			if reported[actionPath] {
				continue
			}
			switch {
			case action.Type == "keep_message":
				errs = append(errs, ValidationError{Path: actionPath, Message: "a keep_message runs on the TIMER_FIRED of " + on + ", which is no node's message to keep"})
			case action.Type == "forward" && action.Message == "":
				errs = append(errs, ValidationError{Path: actionPath, Message: "a forward without message runs on the TIMER_FIRED of " + on + ", which is no node's message to forward: forward a kept one"})
			default:
				continue
			}
			reported[actionPath] = true
		}
	}
	for i, state := range def.States {
		for j, transition := range state.Transitions {
			if transition.EventMatch.Msg != "TIMER_FIRED" {
				continue
			}
			on := fmt.Sprintf("states[%d].transitions[%d]", i, j)
			check(on+".actions", transition.Actions, on)
			if transition.TargetState == "" {
				continue
			}
			check(fmt.Sprintf("states[%d].exit_actions", i), state.ExitActions, on)
			if target, ok := stateAt[transition.TargetState]; ok {
				check(fmt.Sprintf("states[%d].entry_actions", target), def.States[target].EntryActions, on)
			}
		}
	}
	return errs
}

// eachAction calls visit with every action of def and its path, state by state: its entry actions,
// its exit actions, then its transitions' actions.
func (def *WorkflowDefinition) eachAction(visit func(path string, action ActionDefinition)) {
	for i, state := range def.States {
		path := fmt.Sprintf("states[%d]", i)
		for j, action := range state.EntryActions {
			visit(fmt.Sprintf("%s.entry_actions[%d]", path, j), action)
		}
		for j, action := range state.ExitActions {
			visit(fmt.Sprintf("%s.exit_actions[%d]", path, j), action)
		}
		for j, transition := range state.Transitions {
			for k, action := range transition.Actions {
				visit(fmt.Sprintf("%s.transitions[%d].actions[%d]", path, j, k), action)
			}
		}
	}
}

// StartsFrom reports whether a message from name, the router-stamped name of its sender, starts an
// instance of def: name is in its start_from.
func (def *WorkflowDefinition) StartsFrom(name string) bool {
	if def == nil || name == "" {
		return false
	}
	for _, starter := range def.StartFrom {
		if starter == name {
			return true
		}
	}
	return false
}

// Involves reports whether name, the router-stamped name of a message's sender, may drive an
// instance of def: only the nodes written in it may (FINDINGS A-123), those of its start_from, of
// an event_match.src, and the targets of its send_message and forward actions.
func (def *WorkflowDefinition) Involves(name string) bool {
	if def.StartsFrom(name) {
		return true
	}
	if def == nil || name == "" {
		return false
	}
	involved := false
	for _, state := range def.States {
		for _, transition := range state.Transitions {
			if transition.EventMatch.Src == name {
				involved = true
			}
		}
	}
	def.eachAction(func(_ string, action ActionDefinition) {
		if (action.Type == "send_message" || action.Type == "forward") && action.Target == name {
			involved = true
		}
	})
	return involved
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
		days, err := strconv.ParseInt(match[1], 10, 64)
		if err != nil || days > maxDurationDays {
			// A day count past what a duration holds wrapped ("213504d" was 25m26s), and an agent's
			// answer reaches it through a $ref.
			return 0, fmt.Errorf("invalid duration %q: at most %dd", value, maxDurationDays)
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

// validateNodeName checks a node name a definition names a sender by (a start_from entry, an
// event_match.src): a name with its hive, written as the router stamps it, never a SY.* or RT.*
// node.
func validateNodeName(path, name string) ValidationErrors {
	switch {
	case name != strings.TrimSpace(name) || !IsValidL2Name(name):
		return ValidationErrors{{Path: path, Message: "must be a node name with its hive (e.g. \"IO.api.sales@worker1\")"}}
	case IsForbiddenSystemTarget(name):
		return ValidationErrors{{Path: path, Message: "must not be a SY.* / RT.* system node"}}
	}
	return nil
}

// validateStarter checks a start_from entry: a node name (validateNodeName), never an agent's. An
// agent only answers (wf-orchestration-v1 §1), and one that starts instances and that the WF also
// asks has its late answer start a new instance, which asks it again: a WF↔AI loop.
func validateStarter(path, name string) ValidationErrors {
	if errs := validateNodeName(path, name); len(errs) > 0 {
		return errs
	}
	if strings.HasPrefix(strings.ToUpper(name), "AI.") {
		return ValidationErrors{{Path: path, Message: "must not be an AI.* node: an agent only answers, and its late answer would start a new instance (a WF↔AI loop)"}}
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
		errs = append(errs, validateTarget(path, action.Target)...)
		if action.Meta == nil || strings.TrimSpace(action.Meta.Msg) == "" {
			errs = append(errs, ValidationError{Path: path + ".meta.msg", Message: "must not be empty"})
		}
		errs = append(errs, validateSentContext(path, action)...)
		errs = append(errs, validateRefPayload(path+".payload", action.Payload)...)
	case "forward":
		// A forward sends the message as it was received: its payload is never decoded, and its meta
		// keeps the message's msg. What it changes is metadata (wf-orchestration-v1 §5.2.1, item 5).
		errs = append(errs, validateTarget(path, action.Target)...)
		if action.Message != "" && !variablePattern.MatchString(action.Message) {
			errs = append(errs, ValidationError{Path: path + ".message", Message: "must be a valid identifier"})
		}
		errs = append(errs, validateSentContext(path, action)...)
	case "keep_message":
		if !variablePattern.MatchString(action.Name) {
			errs = append(errs, ValidationError{Path: path + ".name", Message: "must be a valid identifier"})
		}
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

// validateTarget checks the target of a send_message or a forward: a node name written in the
// definition. A WF may NOT message SY.*/RT.* system nodes (that path bypasses VPN isolation in the
// router; timers use schedule_timer, not send_message).
func validateTarget(path, target string) ValidationErrors {
	switch {
	case strings.TrimSpace(target) == "":
		return ValidationErrors{{Path: path + ".target", Message: "must not be empty"}}
	case target != strings.TrimSpace(target):
		return ValidationErrors{{Path: path + ".target", Message: "must be a node name without spaces around it"}}
	case !IsValidL2Name(target):
		return ValidationErrors{{Path: path + ".target", Message: "must be a valid L2 name"}}
	case IsForbiddenSystemTarget(target):
		return ValidationErrors{{Path: path + ".target", Message: "must not target a SY.* / RT.* system node"}}
	}
	return nil
}

// validateSentContext checks what a send_message or a forward writes in the message's meta.context:
// meta.context, an object (or a $ref, resolved to one when the action runs) that never writes wf,
// the WF's own block; and vars, an object whose values go in that block, $ref-resolved. Both follow
// the $ref rules of a payload (gap-3), so a bad ref is caught at publish, not at send.
func validateSentContext(path string, action ActionDefinition) ValidationErrors {
	var errs ValidationErrors
	if action.Meta != nil && action.Meta.Context != nil {
		context, ok := action.Meta.Context.(map[string]any)
		switch {
		case !ok:
			errs = append(errs, ValidationError{Path: path + ".meta.context", Message: "must be a JSON object"})
		case !IsWorkflowRef(context):
			if _, writesWF := context[ContextWF]; writesWF {
				errs = append(errs, ValidationError{Path: path + ".meta.context." + ContextWF, Message: "is the WF's own block (trail and vars): the engine writes it, and an action's vars fills it"})
			}
			if envelope, ok := context["response_envelope"]; ok && !holdsWorkflowRef(envelope) {
				errs = append(errs, validateResponseEnvelope(path+".meta.context.response_envelope", envelope)...)
			}
		}
		errs = append(errs, validateRefPayload(path+".meta.context", action.Meta.Context)...)
	}
	if action.Vars != nil {
		vars, ok := action.Vars.(map[string]any)
		if !ok || IsWorkflowRef(vars) {
			errs = append(errs, ValidationError{Path: path + ".vars", Message: "must be a JSON object, its values literals or $refs"})
		} else {
			errs = append(errs, validateRefPayload(path+".vars", vars)...)
		}
	}
	return errs
}

// validateResponseEnvelope checks a response contract written in the definition by the rules an
// agent reads it with (response_contract_to_output_schema_spec and extract_response_envelope in
// crates/fluxbee_ai_sdk/src/message.rs): an agent refuses a contract that breaks one, and the
// instance waits for an answer that never comes. strict is taken as the agent takes it: true when
// it is the boolean true, false otherwise.
func validateResponseEnvelope(path string, envelope any) ValidationErrors {
	contract, ok := envelope.(map[string]any)
	if !ok {
		return ValidationErrors{{Path: path, Message: "must be a JSON object"}}
	}
	var errs ValidationErrors
	if kind, ok := contract["kind"].(string); !ok || kind != "json_object_v1" {
		errs = append(errs, ValidationError{Path: path + ".kind", Message: `must be "json_object_v1"`})
	}
	required, ok := contract["required"].([]any)
	if !ok || len(required) == 0 {
		errs = append(errs, ValidationError{Path: path + ".required", Message: "must be a non-empty list of property names"})
	}
	properties, ok := contract["properties"].(map[string]any)
	if !ok || len(properties) == 0 {
		errs = append(errs, ValidationError{Path: path + ".properties", Message: "must be a non-empty object"})
	}
	for i, field := range required {
		fieldPath := fmt.Sprintf("%s.required[%d]", path, i)
		name, ok := field.(string)
		switch {
		case !ok:
			errs = append(errs, ValidationError{Path: fieldPath, Message: "must be a string"})
		case strings.TrimSpace(name) == "":
			errs = append(errs, ValidationError{Path: fieldPath, Message: "must not be empty"})
		case properties != nil && !hasKey(properties, name):
			errs = append(errs, ValidationError{Path: fieldPath, Message: fmt.Sprintf("must be one of the properties (%q is not)", name)})
		}
	}
	propertyTypes := map[string]string{}
	for _, name := range sortedKeys(properties) {
		propertyPath := path + ".properties." + name
		schema, ok := properties[name].(map[string]any)
		if !ok {
			errs = append(errs, ValidationError{Path: propertyPath, Message: "must be an object"})
			continue
		}
		propertyType, _ := schema["type"].(string)
		switch propertyType {
		case "string", "boolean", "integer", "number":
			propertyTypes[name] = propertyType
		case "":
			errs = append(errs, ValidationError{Path: propertyPath + ".type", Message: "is required"})
		default:
			errs = append(errs, ValidationError{Path: propertyPath + ".type", Message: "must be string, boolean, integer or number"})
		}
		enum, hasEnum := schema["enum"]
		if !hasEnum {
			continue
		}
		items, ok := enum.([]any)
		switch {
		case !ok:
			errs = append(errs, ValidationError{Path: propertyPath + ".enum", Message: "must be a list"})
		case propertyType != "string":
			errs = append(errs, ValidationError{Path: propertyPath + ".enum", Message: "is only for a property of type string"})
		case slices.ContainsFunc(items, func(item any) bool { _, isString := item.(string); return !isString }):
			errs = append(errs, ValidationError{Path: propertyPath + ".enum", Message: "must hold strings only"})
		}
	}
	if textField, ok := contract["text_field"]; ok {
		name, isString := textField.(string)
		switch {
		case !isString:
			errs = append(errs, ValidationError{Path: path + ".text_field", Message: "must be a string"})
		case !slices.Contains(required, any(name)):
			errs = append(errs, ValidationError{Path: path + ".text_field", Message: "must be listed in required"})
		case propertyTypes[name] != "string":
			errs = append(errs, ValidationError{Path: path + ".text_field", Message: "must be a property of type string"})
		}
	}
	return errs
}

// hasKey reports whether object has key, whatever its value.
func hasKey(object map[string]any, key string) bool {
	_, ok := object[key]
	return ok
}

// validateTimerFireSpec checks a timer action's time: exactly one of fire_in (a duration, 60 s at
// least) and fire_at (an RFC3339 UTC time), each a literal string or a $ref. A $ref is resolved when
// the action runs, and the 60 s floor checked then; its default, a literal, is checked here.
func validateTimerFireSpec(path string, fireIn, fireAt any) ValidationErrors {
	hasFireIn, hasFireAt := IsTimeSet(fireIn), IsTimeSet(fireAt)
	switch {
	case hasFireIn && hasFireAt:
		return ValidationErrors{{Path: path, Message: "fire_in and fire_at are mutually exclusive"}}
	case !hasFireIn && !hasFireAt:
		return ValidationErrors{{Path: path, Message: "exactly one of fire_in or fire_at is required"}}
	case hasFireIn:
		return validateTimeValue(path+".fire_in", fireIn, `a duration (e.g. "5m")`, CheckFireIn)
	default:
		return validateTimeValue(path+".fire_at", fireAt, "an RFC3339 UTC timestamp", CheckFireAt)
	}
}

// IsTimeSet reports whether a timer action sets a time field, fire_in or fire_at: anything but
// nothing or a blank string.
func IsTimeSet(value any) bool {
	text, isString := value.(string)
	return value != nil && (!isString || strings.TrimSpace(text) != "")
}

// validateTimeValue checks a timer time, a literal string or a $ref: the literal (or the $ref's
// default) with check.
func validateTimeValue(path string, value any, kind string, check func(string) error) ValidationErrors {
	literal, isLiteral := value.(string)
	if object, isObject := value.(map[string]any); isObject && IsWorkflowRef(object) {
		ref, ok := AsRef(object)
		if !ok {
			return ValidationErrors{{Path: path, Message: `a $ref holds only $ref and default`}}
		}
		if err := validateRefPath(ref.Path); err != nil {
			return ValidationErrors{{Path: path + ".$ref", Message: err.Error()}}
		}
		if !ref.HasDefault {
			return nil
		}
		if literal, isLiteral = ref.Default.(string); !isLiteral {
			return ValidationErrors{{Path: path + ".default", Message: "must be " + kind}}
		}
		path += ".default"
	} else if !isLiteral {
		return ValidationErrors{{Path: path, Message: "must be " + kind + " or a $ref"}}
	}
	if err := check(literal); err != nil {
		return ValidationErrors{{Path: path, Message: err.Error()}}
	}
	return nil
}

// CheckFireIn checks a fire_in duration, written in the definition or resolved from a $ref: it
// parses (ParseWorkflowDuration) and is 60 s at least, SY.timer's floor.
func CheckFireIn(value string) error {
	duration, err := ParseWorkflowDuration(value)
	if err != nil {
		return err
	}
	if duration < MinTimerDuration {
		return fmt.Errorf("must be at least 60s")
	}
	return nil
}

// CheckFireAt checks a fire_at time: an RFC3339 UTC timestamp. Its 60 s floor depends on when the
// action runs, and is checked then.
func CheckFireAt(value string) error {
	if _, err := time.Parse(time.RFC3339, value); err != nil {
		return fmt.Errorf("must be RFC3339 UTC timestamp")
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
