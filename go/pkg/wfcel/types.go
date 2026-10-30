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
	WFSchemaVersion string `json:"wf_schema_version"`
	WorkflowType    string `json:"workflow_type"`
	Description     string `json:"description"`
	// StartFrom names the nodes whose messages start an instance: a message from any other node
	// starts none (wf-orchestration-v1 §5.2.1). Each is a node name with its hive, never a SY.* or
	// RT.* node.
	StartFrom      []string          `json:"start_from"`
	InputSchema    map[string]any    `json:"input_schema"`
	InitialState   string            `json:"initial_state"`
	TerminalStates []string          `json:"terminal_states"`
	States         []StateDefinition `json:"states"`
	inputValidator *jsonschema.Schema
}

type StateDefinition struct {
	Name         string                 `json:"name"`
	Description  string                 `json:"description"`
	EntryActions []ActionDefinition     `json:"entry_actions"`
	ExitActions  []ActionDefinition     `json:"exit_actions"`
	Transitions  []TransitionDefinition `json:"transitions"`
}

type TransitionDefinition struct {
	EventMatch EventMatch `json:"event_match"`
	Guard      string     `json:"guard"`
	// TargetState is the state the transition goes to. Without one the transition is internal: it
	// runs its actions and stays in its state, with no exit or entry actions.
	TargetState string             `json:"target_state,omitempty"`
	Actions     []ActionDefinition `json:"actions"`
	Program     cel.Program        `json:"-"`
}

// EventMatch names the event a transition takes: by its meta.msg, by its sender (src, the name the
// router stamps), or by both, and then both must match. A person's message and an agent's answer
// to it have no msg, so they are matched by their sender. It has no type: every message a WF takes
// is a user message, but a TIMER_FIRED, which its msg names (event_match.type left the definition
// language in 0.1.90, and a definition that still has it is refused).
type EventMatch struct {
	Msg string `json:"msg,omitempty"`
	Src string `json:"src,omitempty"`
}

// ActionMeta is an action's meta. It has no type: every message a WF makes is a user message
// (meta.type left the definition language in 0.1.90, and a definition that still has it is refused).
type ActionMeta struct {
	// Msg names the message a send_message or an emit_internal_event makes; a forward keeps the
	// message's own.
	Msg string `json:"msg,omitempty"`
	// Context is an object written on the outbound message's meta.context, resolved with the same
	// $ref rules as payload: a send_message's whole context, the keys a forward writes over the
	// message's own. Its primary use is requesting AI structured output via
	// meta.context.response_envelope (gap-3) — ai-generic reads that to shape its reply. It never
	// writes wf, the engine's own block (ContextWF). Nil = no context is set.
	Context any `json:"context,omitempty"`
}

// ContextWF is the key of the WF's block in the meta.context of every message a WF sends:
// {"trail": [the nodes the message went through], "vars": {what the action's vars writes}}.
const ContextWF = "wf"

type ActionDefinition struct {
	Type string `json:"type"`
	// Target is the destination of a send_message or a forward: a node name written in the
	// definition (e.g. "AI.specialist@motherbee"), never a SY.* or RT.* node. A WF that chooses
	// among destinations does it with guards, one transition per destination (target_ref, a
	// destination taken from data, left the definition language in 0.1.90, and a definition that
	// still has it is refused). Every message an instance sends carries the instance's thread
	// (thread_id_ref left the language in 0.1.91).
	Target string      `json:"target,omitempty"`
	Meta   *ActionMeta `json:"meta,omitempty"`
	// Message is the kept message a forward sends (keep_message's name); without it, a forward
	// sends the event's message.
	Message string `json:"message,omitempty"`
	// Vars is an object, $ref-resolved, that a send_message or a forward writes in the WF's block
	// (meta.context.wf.vars): the deterministic context the WF gives the receiver.
	Vars     any    `json:"vars,omitempty"`
	Payload  any    `json:"payload,omitempty"`
	TimerKey string `json:"timer_key,omitempty"`
	// FireIn (a duration) and FireAt (an RFC3339 UTC time) are a literal string or a $ref, resolved
	// when the action runs: a time can come from state or from an agent's answer.
	FireIn         any    `json:"fire_in,omitempty"`
	FireAt         any    `json:"fire_at,omitempty"`
	MissedPolicy   string `json:"missed_policy,omitempty"`
	MissedWithinMS *int64 `json:"missed_within_ms,omitempty"`
	// Name is the variable a set_variable sets, or the name a keep_message keeps its message under.
	Name  string `json:"name,omitempty"`
	Value any    `json:"value,omitempty"`
	// OnError is the failure policy for this action (gap-2): "continue" logs the error and proceeds
	// (legacy behavior), "fail" halts the transition and routes the instance to a loud terminal
	// failure. Empty = the per-type default (EffectiveOnError): "fail" for the actions that drive the
	// instance's state, "continue" for the timer actions.
	OnError string `json:"on_error,omitempty"`
}

// Action failure policies (gap-2).
const (
	OnErrorContinue = "continue"
	OnErrorFail     = "fail"
)

// EffectiveOnError returns the failure policy actually in force for an action: the explicit OnError
// if set, else the per-type default (the actions that drive the instance's state fail loudly; the
// timer actions continue).
func (a ActionDefinition) EffectiveOnError() string {
	switch a.OnError {
	case OnErrorContinue, OnErrorFail:
		return a.OnError
	}
	switch a.Type {
	// State-driving actions default to fail-loud: a lost message, an unset variable, a dropped
	// internal event (which was meant to trigger the next transition) or a message not kept (which a
	// later forward sends) all leave the instance inconsistent — exactly what gap-2 eliminates.
	// Timer ops stay best-effort by default.
	case "send_message", "forward", "keep_message", "set_variable", "emit_internal_event":
		return OnErrorFail
	default:
		return OnErrorContinue
	}
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
