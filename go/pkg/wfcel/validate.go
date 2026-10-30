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
	"unicode/utf8"

	"github.com/google/cel-go/cel"
	celast "github.com/google/cel-go/common/ast"
	"github.com/google/cel-go/common/operators"
	"github.com/google/cel-go/common/types"
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
	for i := range def.Catch {
		normalize(def.Catch[i].Actions)
	}
}

// actionFields are the fields each action type takes besides type and on_error; actionMetaFields,
// those of its meta. A field of another type is refused (fieldErrors): a forward with a name
// instead of a message loaded, and forwarded the event.
var (
	actionFields = map[string][]string{
		"send_message":        {"target", "meta", "vars", "menu", "payload"},
		"forward":             {"target", "message", "meta", "vars", "menu"},
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
	// The fields of a menu, of its options and of their params: the agent refuses a menu with any
	// other.
	menuFields       = []string{"help", "options"}
	menuOptionFields = []string{"name", "description", "params"}
	menuParamFields  = []string{"type", "description", "enum"}
)

// fieldErrors names, by their paths, the fields an action has that are not its type's (another
// type's field: a key no type has is the strict decoder's to report), the fields a menu, its options
// or their params have that are not theirs, and the fields that left the definition language in
// 0.1.90 (wf-orchestration-v1 §5.1) and 0.1.91 (thread_id_ref, §5.2.1), which the strict decoder
// refuses too, but only as an unknown field, without saying where or why. A definition that is not
// JSON is the decoder's to report.
func fieldErrors(data []byte) ValidationErrors {
	type transition struct {
		EventMatch map[string]any   `json:"event_match"`
		Actions    []map[string]any `json:"actions"`
	}
	var doc struct {
		States []struct {
			EntryActions []map[string]any `json:"entry_actions"`
			ExitActions  []map[string]any `json:"exit_actions"`
			Transitions  []transition     `json:"transitions"`
		} `json:"states"`
		Catch []transition `json:"catch"`
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
			if menu, ok := action["menu"].(map[string]any); ok && slices.Contains(fields, "menu") {
				errs = append(errs, menuFieldErrors(actionPath+".menu", menu)...)
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
	checkTransition := func(path string, t transition) {
		if _, ok := t.EventMatch["type"]; ok {
			errs = append(errs, ValidationError{Path: path + ".event_match.type", Message: "is not part of the definition language: every message a WF takes is a user message, but a TIMER_FIRED, which event_match.msg names"})
		}
		check(path+".actions", t.Actions)
	}
	for i, state := range doc.States {
		path := fmt.Sprintf("states[%d]", i)
		check(path+".entry_actions", state.EntryActions)
		check(path+".exit_actions", state.ExitActions)
		for j, t := range state.Transitions {
			checkTransition(fmt.Sprintf("%s.transitions[%d]", path, j), t)
		}
	}
	for j, t := range doc.Catch {
		checkTransition(fmt.Sprintf("catch[%d]", j), t)
	}
	return errs
}

// menuFieldErrors names, by their paths, the fields a menu, its options or their params have that are
// not theirs (menuFields, menuOptionFields, menuParamFields). A value of the wrong JSON type is the
// strict decoder's to report.
func menuFieldErrors(path string, menu map[string]any) ValidationErrors {
	var errs ValidationErrors
	unknown := func(path string, object map[string]any, fields []string, of string) {
		for _, field := range sortedKeys(object) {
			if !slices.Contains(fields, field) {
				errs = append(errs, ValidationError{Path: path + "." + field, Message: "is not a field of " + of})
			}
		}
	}
	unknown(path, menu, menuFields, "a menu")
	options, _ := menu["options"].([]any)
	for i, item := range options {
		option, ok := item.(map[string]any)
		if !ok {
			continue
		}
		optionPath := fmt.Sprintf("%s.options[%d]", path, i)
		unknown(optionPath, option, menuOptionFields, "a menu option")
		params, _ := option["params"].(map[string]any)
		for _, name := range sortedKeys(params) {
			if param, ok := params[name].(map[string]any); ok {
				unknown(optionPath+".params."+name, param, menuParamFields, "a menu param")
			}
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

	// What the definition's menus offer is what its guards, values and $refs may read of an agent's
	// evaluation.
	names := def.menuNames()
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
		errs = append(errs, validateActions(path+".entry_actions", state.EntryActions, clock, names)...)
		errs = append(errs, validateActions(path+".exit_actions", state.ExitActions, clock, names)...)
		for j := range state.Transitions {
			errs = append(errs, validateTransition(fmt.Sprintf("%s.transitions[%d]", path, j), &def.States[i].Transitions[j], false, clock, names)...)
		}
	}
	for j := range def.Catch {
		errs = append(errs, validateTransition(fmt.Sprintf("catch[%d]", j), &def.Catch[j], true, clock, names)...)
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
	targetErrors := func(path string, transitions []TransitionDefinition) {
		for j, transition := range transitions {
			// A transition without a target_state is internal: it stays in its state.
			if transition.TargetState == "" {
				continue
			}
			if _, ok := stateIndex[transition.TargetState]; !ok {
				errs = append(errs, ValidationError{
					Path:    fmt.Sprintf("%s[%d].target_state", path, j),
					Message: "must reference an existing state",
				})
			}
		}
	}
	for i, state := range def.States {
		targetErrors(fmt.Sprintf("states[%d].transitions", i), state.Transitions)
	}
	targetErrors("catch", def.Catch)
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
	errs = append(errs, eventlessErrors(def)...)
	return errs
}

// catchActions are the action types a catch transition takes. Timers belong to states: a catch that
// needs one goes to a state whose entry actions set it.
var catchActions = map[string]bool{
	"send_message": true, "forward": true, "keep_message": true, "set_variable": true, "emit_internal_event": true,
}

// validateTransition checks a transition of a state, or of the catch (inCatch): its event_match, by
// a msg, a src or both (in the catch it may name neither, and then it matches every event); its
// guard, compiled into its Program; and its actions, which in the catch set no timers.
func validateTransition(path string, transition *TransitionDefinition, inCatch bool, clock ClockFunc, names menuNames) ValidationErrors {
	var errs ValidationErrors
	match := transition.EventMatch
	switch {
	case !inCatch && strings.TrimSpace(match.Msg) == "" && match.Src == "":
		errs = append(errs, ValidationError{Path: path + ".event_match", Message: "must name a msg, a src or both"})
	case match.Msg != "" && strings.TrimSpace(match.Msg) == "":
		errs = append(errs, ValidationError{Path: path + ".event_match.msg", Message: "must not be blank"})
	}
	if match.Src != "" {
		errs = append(errs, validateNodeName(path+".event_match.src", match.Src)...)
	}
	if strings.TrimSpace(transition.Guard) == "" {
		errs = append(errs, ValidationError{Path: path + ".guard", Message: "must not be empty"})
	} else if program, checked, err := compileGuard(transition.Guard, clock); err != nil {
		errs = append(errs, ValidationError{Path: path + ".guard", Message: err.Error()})
	} else {
		transition.Program = program
		errs = append(errs, names.exprErrors(path+".guard", checked)...)
	}
	errs = append(errs, validateActions(path+".actions", transition.Actions, clock, names)...)
	if !inCatch {
		return errs
	}
	for k, action := range transition.Actions {
		// An unknown type is validateAction's to report.
		if _, known := actionFields[action.Type]; known && !catchActions[action.Type] {
			errs = append(errs, ValidationError{Path: fmt.Sprintf("%s.actions[%d].type", path, k), Message: "is a timer action, and the catch sets no timers: it goes to a state whose entry actions set them"})
		}
	}
	return errs
}

// eventlessErrors refuses, where an event may be no node's message, what needs one: a keep_message,
// and a forward without message (of the event), either of which would fail the instance.
//   - A state's transition matched on msg TIMER_FIRED takes a timer's fire. Its own actions run on
//     it, and so do its state's exit actions and its target's entry actions.
//   - A catch transition whose event_match names no src may take a timer's fire or an internal event
//     (wf-orchestration-v1 §5.3.6: a keep_message, or a forward of the event, needs a src). Its own
//     actions run on it, and with a target_state so do its target's entry actions and the exit
//     actions of the state it leaves, which may be any state an instance waits in.
func eventlessErrors(def *WorkflowDefinition) ValidationErrors {
	stateAt := map[string]int{}
	for i, state := range def.States {
		stateAt[state.Name] = i
	}
	var errs ValidationErrors
	reported := map[string]bool{}
	// eventless is where an event may be no node's message: on names it, and why says why; keepFix and
	// forwardFix, when there are, say what to do instead.
	type eventless struct{ on, why, keepFix, forwardFix string }
	check := func(path string, actions []ActionDefinition, where eventless) {
		for i, action := range actions {
			actionPath := fmt.Sprintf("%s[%d]", path, i)
			if reported[actionPath] {
				continue
			}
			switch {
			case action.Type == "keep_message":
				errs = append(errs, ValidationError{Path: actionPath, Message: "a keep_message runs on " + where.on + ", " + where.why + " to keep" + where.keepFix})
			case action.Type == "forward" && action.Message == "":
				errs = append(errs, ValidationError{Path: actionPath, Message: "a forward without message runs on " + where.on + ", " + where.why + " to forward" + where.forwardFix})
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
			where := eventless{
				on:         fmt.Sprintf("the TIMER_FIRED of states[%d].transitions[%d]", i, j),
				why:        "which is no node's message",
				forwardFix: ": forward a kept one",
			}
			check(fmt.Sprintf("states[%d].transitions[%d].actions", i, j), transition.Actions, where)
			if transition.TargetState == "" {
				continue
			}
			check(fmt.Sprintf("states[%d].exit_actions", i), state.ExitActions, where)
			if target, ok := stateAt[transition.TargetState]; ok {
				check(fmt.Sprintf("states[%d].entry_actions", target), def.States[target].EntryActions, where)
			}
		}
	}
	terminal := map[string]bool{}
	for _, name := range def.TerminalStates {
		terminal[name] = true
	}
	for j, transition := range def.Catch {
		if transition.EventMatch.Src != "" {
			continue
		}
		where := eventless{
			on:         fmt.Sprintf("catch[%d], whose event_match names no src", j),
			why:        "so its event may be a timer's fire or an internal event, no node's message",
			keepFix:    ": name a src",
			forwardFix: ": forward a kept one, or name a src",
		}
		check(fmt.Sprintf("catch[%d].actions", j), transition.Actions, where)
		if transition.TargetState == "" {
			continue
		}
		for i, state := range def.States {
			if !terminal[state.Name] {
				check(fmt.Sprintf("states[%d].exit_actions", i), state.ExitActions, where)
			}
		}
		if target, ok := stateAt[transition.TargetState]; ok {
			check(fmt.Sprintf("states[%d].entry_actions", target), def.States[target].EntryActions, where)
		}
	}
	return errs
}

// eachAction calls visit with every action of def and its path, state by state (its entry actions,
// its exit actions, then its transitions' actions), and then the catch's: the catch's actions count
// as the definition's own.
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
	for j, transition := range def.Catch {
		for k, action := range transition.Actions {
			visit(fmt.Sprintf("catch[%d].actions[%d]", j, k), action)
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
// an event_match.src, and the targets of its send_message and forward actions, the catch's
// included.
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
	for _, transition := range def.Catch {
		if transition.EventMatch.Src == name {
			involved = true
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

// IsAgent reports whether an L2 name is an agent's (AI.*), case-insensitive as
// IsForbiddenSystemTarget: an agent only answers, a menu is offered to one, and the engine counts
// the messages it sends to agents between two triggers.
func IsAgent(name string) bool {
	return strings.HasPrefix(strings.ToUpper(strings.TrimSpace(name)), "AI.")
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
	if IsAgent(name) {
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

func validateActions(path string, actions []ActionDefinition, clock ClockFunc, names menuNames) ValidationErrors {
	var errs ValidationErrors
	for i, action := range actions {
		errs = append(errs, validateAction(fmt.Sprintf("%s[%d]", path, i), action, clock, names)...)
	}
	return errs
}

func validateAction(path string, action ActionDefinition, clock ClockFunc, names menuNames) ValidationErrors {
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
		errs = append(errs, validateMenu(path, action)...)
		errs = append(errs, validateRefPayload(path+".payload", action.Payload)...)
	case "forward":
		// A forward sends the message as it was received: its payload is never decoded, and its meta
		// keeps the message's msg. What it changes is metadata (wf-orchestration-v1 §5.2.1, item 5).
		errs = append(errs, validateTarget(path, action.Target)...)
		if action.Message != "" && !variablePattern.MatchString(action.Message) {
			errs = append(errs, ValidationError{Path: path + ".message", Message: "must be a valid identifier"})
		}
		errs = append(errs, validateSentContext(path, action)...)
		errs = append(errs, validateMenu(path, action)...)
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
			} else if _, checked, err := compileValue(valueExpr, clock); err != nil {
				errs = append(errs, ValidationError{Path: path + ".value", Message: err.Error()})
			} else {
				errs = append(errs, names.exprErrors(path+".value", checked)...)
			}
		}
	default:
		errs = append(errs, ValidationError{Path: path + ".type", Message: "unknown action type"})
	}
	errs = append(errs, names.refErrors(path, action)...)
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
// the WF's own block, nor response_envelope (a WF asks an agent with menu); and vars, an object whose
// values go in that block, $ref-resolved. Both follow the $ref rules of a payload (gap-3), so a bad
// ref is caught at publish, not at send.
func validateSentContext(path string, action ActionDefinition) ValidationErrors {
	var errs ValidationErrors
	if action.Meta != nil && action.Meta.Context != nil {
		context, ok := action.Meta.Context.(map[string]any)
		switch {
		case !ok:
			errs = append(errs, ValidationError{Path: path + ".meta.context", Message: "must be a JSON object"})
		case !IsWorkflowRef(context):
			if _, writesWF := context[ContextWF]; writesWF {
				errs = append(errs, ValidationError{Path: path + ".meta.context." + ContextWF, Message: "is the WF's own block (trail, vars and menu): the engine writes it, from the action's vars and menu"})
			}
			if _, asks := context["response_envelope"]; asks {
				errs = append(errs, ValidationError{Path: path + ".meta.context.response_envelope", Message: "is not part of the definition language: a WF asks an agent with menu"})
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

// The limits of a menu (wf-orchestration-v1 §5.3.6), which the agent checks again. Characters are
// Unicode code points.
const (
	maxMenuHelp        = 600
	maxMenuDescription = 200
	maxMenuOptions     = 8
	maxMenuParams      = 8
	maxMenuEnum        = 20
)

// menuNamePattern is the pattern of a menu option's name and of a param's.
var menuNamePattern = regexp.MustCompile(`^[a-z][a-z0-9_]{0,31}$`)

// validateMenu checks the menu of a send_message or a forward, offered only to an agent (AI.*):
// help, when there is one, up to maxMenuHelp characters; 1 to maxMenuOptions options, each with a
// name of its own in its menu, never none (the agent adds it), a description of 1 to
// maxMenuDescription characters and up to maxMenuParams params; each param with a name, a type
// (string, boolean, integer or number), a description up to maxMenuDescription characters and, for
// a string only, an enum of 1 to maxMenuEnum values (validateMenuEnum). The menu is literal: the
// strict decoder refuses a $ref in it, and fieldErrors a field it does not have. The engine writes
// it with no empty field (an empty help, params or description is left out), which the agent would
// refuse.
func validateMenu(path string, action ActionDefinition) ValidationErrors {
	menu := action.Menu
	if menu == nil {
		return nil
	}
	path += ".menu"
	if !IsAgent(action.Target) {
		return ValidationErrors{{Path: path, Message: "is only for an agent (AI.*) target: it is what the WF offers the agent to choose from"}}
	}
	var errs ValidationErrors
	switch {
	case menu.Help != "" && strings.TrimSpace(menu.Help) == "":
		errs = append(errs, ValidationError{Path: path + ".help", Message: "must not be blank"})
	case utf8.RuneCountInString(menu.Help) > maxMenuHelp:
		errs = append(errs, ValidationError{Path: path + ".help", Message: fmt.Sprintf("must be %d characters at most", maxMenuHelp)})
	}
	if len(menu.Options) == 0 || len(menu.Options) > maxMenuOptions {
		errs = append(errs, ValidationError{Path: path + ".options", Message: fmt.Sprintf("must hold 1 to %d options (the agent adds none)", maxMenuOptions)})
	}
	named := map[string]bool{}
	for i, option := range menu.Options {
		optionPath := fmt.Sprintf("%s.options[%d]", path, i)
		switch {
		case option.Name == NoneOption:
			errs = append(errs, ValidationError{Path: optionPath + ".name", Message: `must not be "none": the agent adds none to every menu`})
		case !menuNamePattern.MatchString(option.Name):
			errs = append(errs, ValidationError{Path: optionPath + ".name", Message: "must be a lowercase name of 32 characters at most (" + menuNamePattern.String() + ")"})
		case named[option.Name]:
			errs = append(errs, ValidationError{Path: optionPath + ".name", Message: fmt.Sprintf("must be unique in its menu (%q is taken)", option.Name)})
		}
		named[option.Name] = true
		errs = append(errs, validateMenuDescription(optionPath+".description", option.Description, true)...)
		if len(option.Params) > maxMenuParams {
			errs = append(errs, ValidationError{Path: optionPath + ".params", Message: fmt.Sprintf("must hold %d params at most", maxMenuParams)})
		}
		for _, name := range sortedKeys(option.Params) {
			param, paramPath := option.Params[name], optionPath+".params."+name
			if !menuNamePattern.MatchString(name) {
				errs = append(errs, ValidationError{Path: paramPath, Message: "must be named with a lowercase name of 32 characters at most (" + menuNamePattern.String() + ")"})
			}
			switch param.Type {
			case "string", "boolean", "integer", "number":
			case "":
				errs = append(errs, ValidationError{Path: paramPath + ".type", Message: "is required"})
			default:
				errs = append(errs, ValidationError{Path: paramPath + ".type", Message: "must be string, boolean, integer or number"})
			}
			errs = append(errs, validateMenuDescription(paramPath+".description", param.Description, false)...)
			if param.Enum != nil {
				errs = append(errs, validateMenuEnum(paramPath+".enum", param)...)
			}
		}
	}
	return errs
}

// validateMenuEnum checks the enum of a menu param: for a string only, 1 to maxMenuEnum values, each
// one not empty and of its own.
func validateMenuEnum(path string, param MenuParam) ValidationErrors {
	var errs ValidationErrors
	switch {
	case param.Type != "string":
		errs = append(errs, ValidationError{Path: path, Message: "is only for a param of type string"})
	case len(param.Enum) == 0 || len(param.Enum) > maxMenuEnum:
		errs = append(errs, ValidationError{Path: path, Message: fmt.Sprintf("must hold 1 to %d values", maxMenuEnum)})
	}
	taken := map[string]bool{}
	for i, value := range param.Enum {
		valuePath := fmt.Sprintf("%s[%d]", path, i)
		switch {
		case strings.TrimSpace(value) == "":
			errs = append(errs, ValidationError{Path: valuePath, Message: "must not be empty"})
		case taken[value]:
			errs = append(errs, ValidationError{Path: valuePath, Message: fmt.Sprintf("must be unique in its enum (%q is taken)", value)})
		}
		taken[value] = true
	}
	return errs
}

// validateMenuDescription checks a description of a menu option (required) or of a param (optional):
// maxMenuDescription characters at most.
func validateMenuDescription(path, description string, required bool) ValidationErrors {
	switch {
	case required && strings.TrimSpace(description) == "":
		return ValidationErrors{{Path: path, Message: "must not be empty"}}
	case description != "" && strings.TrimSpace(description) == "":
		return ValidationErrors{{Path: path, Message: "must not be blank"}}
	case utf8.RuneCountInString(description) > maxMenuDescription:
		return ValidationErrors{{Path: path, Message: fmt.Sprintf("must be %d characters at most", maxMenuDescription)}}
	}
	return nil
}

// menuNames are the option and param names the definition's menus offer, with the agent's own none
// and reason: what a guard, a set_variable value or a $ref may read of an agent's evaluation.
type menuNames struct {
	options map[string]bool
	params  map[string]bool
}

func (def *WorkflowDefinition) menuNames() menuNames {
	names := menuNames{options: map[string]bool{NoneOption: true}, params: map[string]bool{ReasonParam: true}}
	def.eachAction(func(_ string, action ActionDefinition) {
		if action.Menu == nil {
			return
		}
		for _, option := range action.Menu.Options {
			names.options[option.Name] = true
			for param := range option.Params {
				names.params[param] = true
			}
		}
	})
	return names
}

// Where a guard, a value or a $ref reads an agent's evaluation (its answer's meta.context.evaluation).
var (
	evaluationOption = []string{"event", "context", "evaluation", "option"}
	evaluationParams = []string{"event", "context", "evaluation", "params"}
)

// exprErrors checks, in the checked AST of a CEL expression (a guard, a set_variable value), what it
// reads of an agent's evaluation against the definition's menus: a comparison of
// event.context.evaluation.option with a string literal (==, !=, in) names an option of one of them,
// or none, and a read of event.context.evaluation.params.<p> a param of one of them, or reason. A
// name no menu has is a typo: a guard that never matches, a value that fails the instance.
func (names menuNames) exprErrors(path string, checked *cel.Ast) ValidationErrors {
	var errs ValidationErrors
	reported := map[string]bool{}
	report := func(message string) {
		if !reported[message] {
			reported[message] = true
			errs = append(errs, ValidationError{Path: path, Message: message})
		}
	}
	option := func(e celast.Expr) {
		if name, ok := stringLiteral(e); ok && !names.options[name] {
			report(fmt.Sprintf("compares event.context.evaluation.option with %q, an option no menu of the definition offers: %s", name, nameList(names.options, NoneOption)))
		}
	}
	readsOption := func(e celast.Expr) bool {
		read, ok := readPath(e)
		return ok && slices.Equal(read, evaluationOption)
	}
	celast.PreOrderVisit(checked.NativeRep().Expr(), celast.NewExprVisitor(func(e celast.Expr) {
		if read, ok := readPath(e); ok {
			if param, ok := evaluationParam(read); ok && !names.params[param] {
				report(names.paramMessage(param))
			}
		}
		if e.Kind() != celast.CallKind || len(e.AsCall().Args()) != 2 {
			return
		}
		args := e.AsCall().Args()
		switch e.AsCall().FunctionName() {
		case operators.Equals, operators.NotEquals:
			if readsOption(args[0]) {
				option(args[1])
			}
			if readsOption(args[1]) {
				option(args[0])
			}
		case operators.In:
			if readsOption(args[0]) && args[1].Kind() == celast.ListKind {
				for _, element := range args[1].AsList().Elements() {
					option(element)
				}
			}
		}
	}))
	return errs
}

// refErrors checks the $refs of an action (in its payload, meta.context, vars and timer times) that
// read event.context.evaluation.params.<p>: p is a param of one of the definition's menus, or
// reason.
func (names menuNames) refErrors(path string, action ActionDefinition) ValidationErrors {
	var errs ValidationErrors
	check := func(field string, value any) {
		eachRef(path+"."+field, value, func(refPath string, ref Ref) {
			if param, ok := evaluationParam(strings.Split(ref.Path, ".")); ok && !names.params[param] {
				errs = append(errs, ValidationError{Path: refPath + ".$ref", Message: names.paramMessage(param)})
			}
		})
	}
	check("payload", action.Payload)
	if action.Meta != nil {
		check("meta.context", action.Meta.Context)
	}
	check("vars", action.Vars)
	check("fire_in", action.FireIn)
	check("fire_at", action.FireAt)
	return errs
}

func (names menuNames) paramMessage(param string) string {
	return fmt.Sprintf("reads event.context.evaluation.params.%s, a param no menu of the definition offers: %s", param, nameList(names.params, ReasonParam))
}

// evaluationParam is the param a read path names under event.context.evaluation.params.
func evaluationParam(read []string) (string, bool) {
	if len(read) <= len(evaluationParams) || !slices.Equal(read[:len(evaluationParams)], evaluationParams) {
		return "", false
	}
	return read[len(evaluationParams)], true
}

// readPath is the path an expression reads when it is a chain of field selections, or of indexes by
// a string literal, from a variable: event.context.evaluation.option and
// event['context'].evaluation['option'] read [event context evaluation option]. Any other
// expression reads none.
func readPath(e celast.Expr) ([]string, bool) {
	switch e.Kind() {
	case celast.IdentKind:
		return []string{e.AsIdent()}, true
	case celast.SelectKind:
		selected := e.AsSelect()
		read, ok := readPath(selected.Operand())
		if !ok {
			return nil, false
		}
		return append(read, selected.FieldName()), true
	case celast.CallKind:
		call := e.AsCall()
		if call.FunctionName() != operators.Index || len(call.Args()) != 2 {
			return nil, false
		}
		key, ok := stringLiteral(call.Args()[1])
		if !ok {
			return nil, false
		}
		read, ok := readPath(call.Args()[0])
		if !ok {
			return nil, false
		}
		return append(read, key), true
	}
	return nil, false
}

// stringLiteral is the value of a string literal.
func stringLiteral(e celast.Expr) (string, bool) {
	if e.Kind() != celast.LiteralKind {
		return "", false
	}
	text, ok := e.AsLiteral().(types.String)
	return string(text), ok
}

// nameList lists names, sorted, with last at the end: "answer, consult, invoice or none".
func nameList(names map[string]bool, last string) string {
	var listed []string
	for _, name := range sortedKeys(names) {
		if name != last {
			listed = append(listed, name)
		}
	}
	if len(listed) == 0 {
		return last
	}
	return strings.Join(listed, ", ") + " or " + last
}

// eachRef calls visit with every WF $ref value holds, and its path.
func eachRef(path string, value any, visit func(path string, ref Ref)) {
	switch typed := value.(type) {
	case map[string]any:
		if ref, ok := AsRef(typed); ok {
			visit(path, ref)
			return
		}
		for _, key := range sortedKeys(typed) {
			eachRef(path+"."+key, typed[key], visit)
		}
	case []any:
		for i, child := range typed {
			eachRef(fmt.Sprintf("%s[%d]", path, i), child, visit)
		}
	}
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
