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
	// Context is an arbitrary object placed on the outbound frame's meta.context, resolved with the
	// same $ref/${...} rules as payload. Its primary use is requesting AI structured output via
	// meta.context.response_envelope (gap-3) — ai-generic reads that to shape its reply. Nil = no
	// context is set (unchanged behavior).
	Context any `json:"context,omitempty"`
}

type ActionDefinition struct {
	Type string `json:"type"`
	// Target is a STATIC destination L2 name (e.g. "AI.specialist@motherbee"). For a DYNAMIC target
	// (routing to a node computed at runtime) set TargetRef instead — a $ref dot-path rooted at
	// state/input/event whose resolved string value is the destination. Exactly one of Target /
	// TargetRef must be set on a send_message action. Either way the (resolved) name is re-validated
	// and SY.*/RT.* system nodes are refused at send time.
	Target    string `json:"target,omitempty"`
	TargetRef string `json:"target_ref,omitempty"`
	// ThreadIDRef overrides the outbound thread_id with a $ref-resolved value (a dot-path rooted at
	// state/input/event). By DEFAULT a send_message uses the WF instance_id as thread_id so replies
	// correlate back to this instance (see correlate.go). Set ThreadIDRef ONLY for a terminal reply
	// to an EXTERNAL ingress caller — e.g. answering an IO.api conversation on the caller's own
	// thread_id — where no correlation-back to the instance is expected. Overriding it for a message
	// you still expect a reply to WILL break correlation. (gap-5)
	ThreadIDRef    string      `json:"thread_id_ref,omitempty"`
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
