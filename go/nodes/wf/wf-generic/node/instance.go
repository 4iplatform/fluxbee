package node

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"strings"
	"sync"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// WFInstance is the in-memory representation of a running workflow instance.
// All mutations must be performed under mu.
type WFInstance struct {
	mu sync.Mutex

	InstanceID     string
	WorkflowType   string
	def            *WorkflowDefinition
	Status         string // running | cancelling | completed | cancelled | failed
	CurrentState   string
	Input          map[string]any
	StateVars      map[string]any // mutable state variables (state_json)
	CurrentTraceID string         // trace_id of event being processed (for crash recovery)
	CreatedAtMS    int64
	UpdatedAtMS    int64
	TerminatedAtMS *int64
}

// NewInstance creates a new WFInstance for the given definition and input.
func NewInstance(def *WorkflowDefinition, instanceID, workflowType string, input map[string]any, clock ClockFunc) *WFInstance {
	now := nowMS(clock)
	return &WFInstance{
		InstanceID:   instanceID,
		WorkflowType: workflowType,
		def:          def,
		Status:       "running",
		CurrentState: def.InitialState,
		Input:        input,
		StateVars:    map[string]any{},
		CreatedAtMS:  now,
		UpdatedAtMS:  now,
	}
}

// FromRow restores an instance from its persistent row.
func FromRow(row WFInstanceRow, def *WorkflowDefinition) (*WFInstance, error) {
	input, err := unmarshalJSON(row.InputJSON)
	if err != nil {
		return nil, fmt.Errorf("instance %s: bad input_json: %w", row.InstanceID, err)
	}
	state, err := unmarshalJSON(row.StateJSON)
	if err != nil {
		return nil, fmt.Errorf("instance %s: bad state_json: %w", row.InstanceID, err)
	}
	inst := &WFInstance{
		InstanceID:     row.InstanceID,
		WorkflowType:   row.WorkflowType,
		def:            def,
		Status:         row.Status,
		CurrentState:   row.CurrentState,
		Input:          input,
		StateVars:      state,
		CurrentTraceID: row.CurrentTraceID,
		CreatedAtMS:    row.CreatedAtMS,
		UpdatedAtMS:    row.UpdatedAtMS,
	}
	if row.TerminatedAtMS.Valid {
		v := row.TerminatedAtMS.Int64
		inst.TerminatedAtMS = &v
	}
	return inst, nil
}

// ToRow serializes the instance to its persistent row.
// Must be called under mu.
func (inst *WFInstance) ToRow(clock ClockFunc) (WFInstanceRow, error) {
	inputJSON, err := marshalJSON(inst.Input)
	if err != nil {
		return WFInstanceRow{}, err
	}
	stateJSON, err := marshalJSON(inst.StateVars)
	if err != nil {
		return WFInstanceRow{}, err
	}
	row := WFInstanceRow{
		InstanceID:     inst.InstanceID,
		WorkflowType:   inst.WorkflowType,
		Status:         inst.Status,
		CurrentState:   inst.CurrentState,
		InputJSON:      inputJSON,
		StateJSON:      stateJSON,
		CurrentTraceID: inst.CurrentTraceID,
		CreatedAtMS:    inst.CreatedAtMS,
		UpdatedAtMS:    nowMS(clock),
	}
	if inst.TerminatedAtMS != nil {
		row.TerminatedAtMS = newNullInt64(*inst.TerminatedAtMS)
	}
	return row, nil
}

// Lock acquires the per-instance mutex.
func (inst *WFInstance) Lock() { inst.mu.Lock() }

// Unlock releases the per-instance mutex.
func (inst *WFInstance) Unlock() { inst.mu.Unlock() }

// RunTransition processes an incoming event against the current state.
// It must be called with mu held by the caller.
//
// Execution model (act-then-persist):
//  1. Persist current_trace_id so recovery can re-use the same trace_id.
//  2. Determine the transition to take.
//  3. Execute actions (exit → transition → entry).
//  4. Persist new state.
func (inst *WFInstance) RunTransition(ctx context.Context, event sdk.Message, actx ActionContext) error {
	// Fast-path: cancelling → immediate transition to 'cancelled'
	if inst.Status == "cancelling" {
		return inst.runCancelTransition(ctx, event, actx)
	}

	// Find the current state definition
	stateDef := inst.findState(inst.CurrentState)
	if stateDef == nil {
		return fmt.Errorf("instance %s: state %q not found in definition", inst.InstanceID, inst.CurrentState)
	}

	// Build the event map for CEL + $ref resolution
	eventMap := messageToMap(event)

	// Find the first matching + passing transition
	var chosen *TransitionDefinition
	for i := range stateDef.Transitions {
		t := &stateDef.Transitions[i]
		if !matchesEvent(t.EventMatch, event) {
			continue
		}
		if t.Program == nil {
			log.Printf("instance %s: transition %d has no compiled program, skipping", inst.InstanceID, i)
			continue
		}
		if EvalGuard(t.Program, inst.Input, inst.StateVars, eventMap) {
			chosen = t
			break
		}
	}

	if chosen == nil {
		// Log unhandled event and return without state change
		_ = actx.Store.AppendLog(ctx, WFLogEntry{
			InstanceID:  inst.InstanceID,
			LoggedAtMS:  nowMS(actx.Clock),
			ActionType:  "event",
			Summary:     fmt.Sprintf("unhandled event %s in state %s", msgName(event), inst.CurrentState),
			OK:          false,
			ErrorDetail: "no matching transition",
		})
		return nil
	}

	// Step 1: persist the trace_id before executing any actions
	inst.CurrentTraceID = event.Routing.TraceID
	if err := actx.Store.UpdateInstance(ctx, mustToRow(inst, actx.Clock)); err != nil {
		return fmt.Errorf("persist trace_id: %w", err)
	}

	targetStateDef := inst.findState(chosen.TargetState)
	if targetStateDef == nil {
		return fmt.Errorf("instance %s: target state %q not found", inst.InstanceID, chosen.TargetState)
	}

	// Step 2: execute exit_actions → transition.actions → entry_actions
	inst.executeActions(ctx, stateDef.ExitActions, event, actx)
	inst.executeActions(ctx, chosen.Actions, event, actx)

	// Step 3: apply set_variable actions to StateVars before entry_actions fire
	// (already done inline in executeActions via set_variable handler)

	// Advance state
	prevState := inst.CurrentState
	inst.CurrentState = chosen.TargetState

	inst.executeActions(ctx, targetStateDef.EntryActions, event, actx)

	// Step 4: persist new state
	inst.CurrentTraceID = ""
	now := nowMS(actx.Clock)
	inst.UpdatedAtMS = now

	isTerminal := inst.isTerminalState(chosen.TargetState)
	if isTerminal {
		inst.Status = terminalStatus(chosen.TargetState, inst.def.TerminalStates)
		inst.TerminatedAtMS = &now
		// Cancel all registered timers
		inst.cancelAllTimers(ctx, actx)
	}

	if err := actx.Store.UpdateInstance(ctx, mustToRow(inst, actx.Clock)); err != nil {
		return fmt.Errorf("persist transition %s→%s: %w", prevState, inst.CurrentState, err)
	}

	_ = actx.Store.AppendLog(ctx, WFLogEntry{
		InstanceID: inst.InstanceID,
		LoggedAtMS: now,
		ActionType: "transition",
		Summary:    fmt.Sprintf("transition %s → %s", prevState, inst.CurrentState),
		OK:         true,
	})

	return nil
}

func (inst *WFInstance) runCancelTransition(ctx context.Context, event sdk.Message, actx ActionContext) error {
	cancelledState := inst.findState("cancelled")
	if cancelledState == nil {
		// Fallback: just mark as cancelled
		now := nowMS(actx.Clock)
		inst.Status = "cancelled"
		inst.TerminatedAtMS = &now
		inst.CurrentTraceID = ""
		return actx.Store.UpdateInstance(ctx, mustToRow(inst, actx.Clock))
	}

	prevState := inst.CurrentState
	inst.CurrentTraceID = event.Routing.TraceID
	if err := actx.Store.UpdateInstance(ctx, mustToRow(inst, actx.Clock)); err != nil {
		return fmt.Errorf("persist trace_id for cancel: %w", err)
	}

	// Run current state's exit_actions then cancelled entry_actions
	if stateDef := inst.findState(prevState); stateDef != nil {
		inst.executeActions(ctx, stateDef.ExitActions, event, actx)
	}
	inst.CurrentState = "cancelled"
	inst.executeActions(ctx, cancelledState.EntryActions, event, actx)

	now := nowMS(actx.Clock)
	inst.Status = "cancelled"
	inst.TerminatedAtMS = &now
	inst.CurrentTraceID = ""
	inst.cancelAllTimers(ctx, actx)

	return actx.Store.UpdateInstance(ctx, mustToRow(inst, actx.Clock))
}

// executeActions runs a slice of actions, logging each result.
// Action failures are logged but do NOT halt execution.
func (inst *WFInstance) executeActions(ctx context.Context, actions []ActionDefinition, event sdk.Message, actx ActionContext) {
	for i, action := range actions {
		path := fmt.Sprintf("action[%d]", i)
		err := executeAction(ctx, path, action, inst, event, actx)
		ok := err == nil
		detail := ""
		if err != nil {
			detail = err.Error()
			log.Printf("instance %s: %s (%s) error: %v", inst.InstanceID, path, action.Type, err)
		}
		_ = actx.Store.AppendLog(ctx, WFLogEntry{
			InstanceID:  inst.InstanceID,
			LoggedAtMS:  nowMS(actx.Clock),
			ActionType:  action.Type,
			Summary:     fmt.Sprintf("%s to %s", action.Type, action.Target),
			OK:          ok,
			ErrorDetail: detail,
		})
	}
}

func (inst *WFInstance) cancelAllTimers(ctx context.Context, actx ActionContext) {
	timers, err := actx.Store.ListTimersForInstance(ctx, inst.InstanceID)
	if err != nil {
		log.Printf("instance %s: list timers for cleanup: %v", inst.InstanceID, err)
		return
	}
	for _, t := range timers {
		ref := timerClientRef(inst.InstanceID, t.TimerKey)
		if actx.Timer != nil {
			if err := actx.Timer.CancelByClientRef(ctx, ref); err != nil {
				log.Printf("instance %s: cancel timer %s: %v", inst.InstanceID, t.TimerKey, err)
			}
		}
		_ = actx.Store.DeleteTimer(ctx, inst.InstanceID, t.TimerKey)
	}
}

func (inst *WFInstance) findState(name string) *StateDefinition {
	for i := range inst.def.States {
		if inst.def.States[i].Name == name {
			return &inst.def.States[i]
		}
	}
	return nil
}

func (inst *WFInstance) isTerminalState(name string) bool {
	for _, ts := range inst.def.TerminalStates {
		if ts == name {
			return true
		}
	}
	return false
}

// terminalStatus maps a terminal state name to the instance status string.
// "cancelled" → "cancelled", "failed" → "failed", everything else → "completed".
func terminalStatus(stateName string, terminalStates []string) string {
	switch strings.ToLower(stateName) {
	case "cancelled":
		return "cancelled"
	case "failed":
		return "failed"
	default:
		return "completed"
	}
}

// matchesEvent checks whether a transition's event_match matches the incoming message.
func matchesEvent(match EventMatch, msg sdk.Message) bool {
	if strings.TrimSpace(match.Msg) == "" {
		return false
	}
	if msg.Meta.Msg == nil || *msg.Meta.Msg != match.Msg {
		return false
	}
	if match.Type != nil && msg.Meta.MsgType != *match.Type {
		return false
	}
	return true
}

// messageToMap converts an sdk.Message to a plain map for use in CEL evaluation
// and $ref resolution. The map has keys: "msg", "type", "thread_id", "trace_id", "payload".
func messageToMap(msg sdk.Message) map[string]any {
	m := map[string]any{
		"trace_id": msg.Routing.TraceID,
	}
	if msg.Meta.Msg != nil {
		m["msg"] = *msg.Meta.Msg
	}
	m["type"] = msg.Meta.MsgType
	if msg.Meta.ThreadID != nil {
		m["thread_id"] = *msg.Meta.ThreadID
	}
	if len(msg.Payload) > 0 {
		var payload map[string]any
		if err := json.Unmarshal(msg.Payload, &payload); err == nil {
			m["payload"] = payload
		}
	}
	return m
}

func msgName(msg sdk.Message) string {
	if msg.Meta.Msg != nil {
		return *msg.Meta.Msg
	}
	return "(no msg)"
}

func mustToRow(inst *WFInstance, clock ClockFunc) WFInstanceRow {
	row, err := inst.ToRow(clock)
	if err != nil {
		panic(fmt.Sprintf("instance.ToRow: %v", err))
	}
	return row
}
