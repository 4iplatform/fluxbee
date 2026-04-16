package wfcel

import (
	"time"

	"github.com/google/cel-go/cel"
	jsonschema "github.com/santhosh-tekuri/jsonschema/v6"
)

const (
	WorkflowSchemaVersion = "1"
	GuardEvalTimeout      = 10 * time.Millisecond
	MinTimerDuration      = 60 * time.Second
)

type ClockFunc func() time.Time

type WorkflowDefinition struct {
	WFSchemaVersion string            `json:"wf_schema_version"`
	WorkflowType    string            `json:"workflow_type"`
	Description     string            `json:"description"`
	InputSchema     map[string]any    `json:"input_schema"`
	InitialState    string            `json:"initial_state"`
	TerminalStates  []string          `json:"terminal_states"`
	States          []StateDefinition `json:"states"`
	inputValidator  *jsonschema.Schema
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
	Program     cel.Program        `json:"-"`
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

type ValidationError struct {
	Path    string
	Message string
}

func (e ValidationError) Error() string {
	if e.Path == "" {
		return e.Message
	}
	return e.Path + ": " + e.Message
}

type ValidationErrors []ValidationError

func (errs ValidationErrors) Error() string {
	if len(errs) == 0 {
		return ""
	}
	out := errs[0].Error()
	for i := 1; i < len(errs); i++ {
		out += "; " + errs[i].Error()
	}
	return out
}
