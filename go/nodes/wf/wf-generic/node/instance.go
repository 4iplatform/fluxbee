package node

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"strings"
	"sync"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	wfcel "github.com/4iplatform/json-router/pkg/wfcel"
)

// maxInternalEventsPerEvent bounds the internal events one inbound event may lead to (its own
// transition's and those that follow from them): past it the instance fails (causeInternalEventCap)
// instead of an emit_internal_event loop holding the node's only receive loop forever.
const maxInternalEventsPerEvent = 100

// maxReplayAttempts is how many starts may run a leftover event again (a journaled inbound event, a
// queued internal event): a row run that many times without finishing is a poison event that takes
// the node down, and the next start fails its instance instead (causeReplayExhausted).
const maxReplayAttempts = 3

// Why the engine fails an instance on its own. The "FAILED in <state>" entry of the instance's log
// (and the journal's line) starts its detail with the cause's name.
const (
	// causeTransitionPanic: the transition panicked. The instance fails, never the node.
	causeTransitionPanic = "transition_panic"
	// causeInternalEventCap: more than maxInternalEventsPerEvent internal events followed one event.
	causeInternalEventCap = "internal_event_cap"
	// causeDefinitionUnavailable: the definition the instance was born with is not kept, or no longer
	// loads.
	causeDefinitionUnavailable = "definition_unavailable"
	// causeStateNotInDefinition: the instance is in a state its own definition does not have.
	causeStateNotInDefinition = "state_not_in_definition"
	// causeReplayExhausted: an event of the instance ran at maxReplayAttempts starts without finishing.
	causeReplayExhausted = "replay_exhausted"
)

// WFInstance is the in-memory representation of a running workflow instance.
// All mutations must be performed under mu.
type WFInstance struct {
	mu sync.Mutex

	InstanceID   string
	WorkflowType string
	// DefinitionHash and def are the definition the instance was born with: it runs on it until it
	// ends, whatever is deployed meanwhile.
	DefinitionHash string
	def            *WorkflowDefinition
	Status         string // running | cancelling | completed | cancelled | failed
	CurrentState   string
	Input          map[string]any
	StateVars      map[string]any // mutable state variables (state_json)
	CurrentTraceID string         // trace_id of the event being processed: its sends carry it
	CreatedAtMS    int64
	UpdatedAtMS    int64
	TerminatedAtMS *int64
}

// takenEvent is the durable copy of the event a transition takes: its row in the inbound journal or
// in the internal-event queue, deleted in the commit of the transition.
type takenEvent struct {
	inboundID  *int64
	internalID *int64
}

func (t takenEvent) durable() bool { return t.inboundID != nil || t.internalID != nil }

func (t takenEvent) mutation() InstanceCommitMutation {
	return InstanceCommitMutation{ConsumedInboundEventID: t.inboundID, ConsumedInternalEventID: t.internalID}
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
		DefinitionHash: row.DefinitionHash,
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
		DefinitionHash: inst.DefinitionHash,
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
// Execution model (act-then-commit, at least once): the event is durable before this runs (in the
// inbound journal or the internal-event queue, taken), the actions run (exit → transition →
// entry), and one commit writes the new state, enqueues the internal events and deletes the taken
// event. A crash before the commit leaves the event for recovery, which runs it again: its sends
// go again, with the same trace_id.
func (inst *WFInstance) RunTransition(ctx context.Context, event sdk.Message, actx ActionContext, taken takenEvent) error {
	// Fast-path: cancelling → immediate transition to 'cancelled'
	if inst.Status == "cancelling" {
		return inst.runCancelTransition(ctx, event, actx, taken)
	}

	// Find the current state definition
	stateDef := inst.findState(inst.CurrentState)
	if stateDef == nil {
		return inst.routeToFailed(ctx, event, actx, fmt.Errorf("%s: state %q is not in the instance's definition", causeStateNotInDefinition, inst.CurrentState), taken)
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
		now := nowMS(actx.Clock)
		_ = actx.Store.AppendLog(ctx, WFLogEntry{
			InstanceID:  inst.InstanceID,
			LoggedAtMS:  now,
			ActionType:  "event",
			Summary:     fmt.Sprintf("unhandled event %s in state %s", msgName(event), inst.CurrentState),
			OK:          false,
			ErrorDetail: "no matching transition",
		})
		if taken.durable() {
			inst.UpdatedAtMS = now
			if err := inst.commit(ctx, actx, taken.mutation()); err != nil {
				return fmt.Errorf("consume unhandled event: %w", err)
			}
		}
		return nil
	}

	targetStateDef := inst.findState(chosen.TargetState)
	if targetStateDef == nil {
		return inst.routeToFailed(ctx, event, actx, fmt.Errorf("%s: target state %q is not in the instance's definition", causeStateNotInDefinition, chosen.TargetState), taken)
	}

	// The event's trace_id goes on every send of the transition (and on a send again when recovery
	// runs the event again).
	inst.CurrentTraceID = event.Routing.TraceID

	// Execute exit_actions → transition.actions → entry_actions. A fatal action failure (gap-2) at
	// any step routes the instance to a loud terminal failure instead of advancing with inconsistent
	// state; internal events emitted before the failure are cleared by routeToFailed.
	var emitted []InternalEventRow
	if err := inst.executeActions(ctx, stateDef.ExitActions, event, actx, &emitted); err != nil {
		return inst.routeToFailed(ctx, event, actx, err, taken)
	}
	if err := inst.executeActions(ctx, chosen.Actions, event, actx, &emitted); err != nil {
		return inst.routeToFailed(ctx, event, actx, err, taken)
	}

	// Advance state
	prevState := inst.CurrentState
	inst.CurrentState = chosen.TargetState

	if err := inst.executeActions(ctx, targetStateDef.EntryActions, event, actx, &emitted); err != nil {
		return inst.routeToFailed(ctx, event, actx, err, taken)
	}

	// Commit the new state
	inst.CurrentTraceID = ""
	now := nowMS(actx.Clock)
	inst.UpdatedAtMS = now

	isTerminal := inst.isTerminalState(chosen.TargetState)
	if isTerminal {
		inst.Status = terminalStatus(chosen.TargetState, inst.def.TerminalStates)
		inst.TerminatedAtMS = &now
		// Cancel all registered timers
		inst.cancelAllTimers(ctx, actx)
		emitted = nil
	}

	mutation := taken.mutation()
	mutation.EnqueueInternalEvents = emitted
	mutation.ClearInternalEvents = isTerminal
	if err := inst.commit(ctx, actx, mutation); err != nil {
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

// commit writes the instance as it is now, with mutation, in one transaction.
func (inst *WFInstance) commit(ctx context.Context, actx ActionContext, mutation InstanceCommitMutation) error {
	row, err := inst.ToRow(actx.Clock)
	if err != nil {
		return fmt.Errorf("instance %s: serialize: %w", inst.InstanceID, err)
	}
	return actx.Store.CommitInstanceMutation(ctx, row, mutation)
}

func (inst *WFInstance) runCancelTransition(ctx context.Context, event sdk.Message, actx ActionContext, taken takenEvent) error {
	mutation := taken.mutation()
	mutation.ClearInternalEvents = true
	cancelledState := inst.findState("cancelled")
	if cancelledState == nil {
		// Fallback: just mark as cancelled
		now := nowMS(actx.Clock)
		inst.Status = "cancelled"
		inst.TerminatedAtMS = &now
		inst.CurrentTraceID = ""
		inst.cancelAllTimers(ctx, actx)
		return inst.commit(ctx, actx, mutation)
	}

	prevState := inst.CurrentState
	inst.CurrentTraceID = event.Routing.TraceID

	// Run current state's exit_actions then cancelled entry_actions. Best-effort: the instance is
	// already terminating into cancelled, so an action failure here is logged, not routed to failed.
	var emitted []InternalEventRow
	if stateDef := inst.findState(prevState); stateDef != nil {
		inst.executeActionsBestEffort(ctx, stateDef.ExitActions, event, actx, &emitted)
	}
	inst.CurrentState = "cancelled"
	inst.executeActionsBestEffort(ctx, cancelledState.EntryActions, event, actx, &emitted)

	now := nowMS(actx.Clock)
	inst.Status = "cancelled"
	inst.TerminatedAtMS = &now
	inst.CurrentTraceID = ""
	inst.cancelAllTimers(ctx, actx)

	return inst.commit(ctx, actx, mutation)
}

// executeActions runs a slice of actions, logging each result. On the FIRST action whose effective
// on_error policy is "fail", the failure STOPS the slice and is returned, so the caller routes the
// instance to a loud terminal failure instead of silently advancing with inconsistent state (gap-2).
// Actions with on_error=continue are logged and skipped, as before.
func (inst *WFInstance) executeActions(ctx context.Context, actions []ActionDefinition, event sdk.Message, actx ActionContext, emitted *[]InternalEventRow) error {
	return inst.runActions(ctx, actions, event, actx, emitted, false)
}

// executeActionsBestEffort runs every action, logging failures but never halting — for paths where
// routing to failure is impossible or undesirable: the cancel path (already terminating) and the
// failed-state's OWN entry actions (so failure handling can never recurse).
func (inst *WFInstance) executeActionsBestEffort(ctx context.Context, actions []ActionDefinition, event sdk.Message, actx ActionContext, emitted *[]InternalEventRow) {
	_ = inst.runActions(ctx, actions, event, actx, emitted, true)
}

func (inst *WFInstance) runActions(ctx context.Context, actions []ActionDefinition, event sdk.Message, actx ActionContext, emitted *[]InternalEventRow, bestEffort bool) error {
	for i, action := range actions {
		path := fmt.Sprintf("action[%d]", i)
		err := executeAction(ctx, path, action, inst, event, actx, emitted)
		ok := err == nil
		detail := ""
		fatal := false
		if err != nil {
			detail = err.Error()
			fatal = !bestEffort && action.EffectiveOnError() == OnErrorFail
			if fatal {
				log.Printf("instance %s: %s (%s) FAILED (on_error=fail): %v", inst.InstanceID, path, action.Type, err)
			} else {
				log.Printf("instance %s: %s (%s) error (on_error=continue): %v", inst.InstanceID, path, action.Type, err)
			}
		}
		_ = actx.Store.AppendLog(ctx, WFLogEntry{
			InstanceID:  inst.InstanceID,
			LoggedAtMS:  nowMS(actx.Clock),
			ActionType:  action.Type,
			Summary:     actionSummary(action),
			OK:          ok,
			ErrorDetail: detail,
		})
		if fatal {
			return fmt.Errorf("%s (%s): %w", path, action.Type, err)
		}
	}
	return nil
}

// routeToFailed terminates the instance in a loud "failed" state after a fatal error (a fatal action
// failure, gap-2, or one of the engine's causes), instead of silently advancing with inconsistent
// state. If the definition declares a "failed" state, the instance moves there and its
// entry_actions run best-effort (never fatal, so failure handling can't recurse); otherwise the
// instance is simply marked failed. Either way Status=failed, the instance terminates, timers are
// cancelled, pending internal events are cleared, the taken event is consumed, and the cause is
// logged loudly.
func (inst *WFInstance) routeToFailed(ctx context.Context, event sdk.Message, actx ActionContext, cause error, taken takenEvent) error {
	prevState := inst.CurrentState
	now := nowMS(actx.Clock)
	log.Printf("instance %s: fatal failure in state %q — routing to failed: %v", inst.InstanceID, prevState, cause)

	var emitted []InternalEventRow
	if failedState := inst.findState("failed"); failedState != nil && prevState != "failed" {
		inst.CurrentState = "failed"
		inst.executeActionsBestEffort(ctx, failedState.EntryActions, event, actx, &emitted)
	}
	inst.Status = "failed"
	inst.TerminatedAtMS = &now
	inst.CurrentTraceID = ""
	inst.UpdatedAtMS = now
	inst.cancelAllTimers(ctx, actx)

	_ = actx.Store.AppendLog(ctx, WFLogEntry{
		InstanceID:  inst.InstanceID,
		LoggedAtMS:  now,
		ActionType:  "transition",
		Summary:     fmt.Sprintf("FAILED in %s", prevState),
		OK:          false,
		ErrorDetail: cause.Error(),
	})

	mutation := taken.mutation()
	mutation.ClearInternalEvents = true
	return inst.commit(ctx, actx, mutation)
}

// drainInternalEvents runs the instance's queued internal events in order, up to
// maxInternalEventsPerEvent of them: one more fails the instance (causeInternalEventCap).
func (inst *WFInstance) drainInternalEvents(ctx context.Context, actx ActionContext) error {
	return inst.drain(ctx, actx, false)
}

// drainLeftoverInternalEvents is the drain a start runs on the internal events a crash left: each one
// counts its attempt, in its own commit, before it runs, and one already run at maxReplayAttempts
// starts fails the instance instead (causeReplayExhausted), with no action run.
func (inst *WFInstance) drainLeftoverInternalEvents(ctx context.Context, actx ActionContext) error {
	return inst.drain(ctx, actx, true)
}

func (inst *WFInstance) drain(ctx context.Context, actx ActionContext, countAttempts bool) error {
	for drained := 0; inst.Status == "running" || inst.Status == "cancelling"; drained++ {
		row, ok, err := actx.Store.GetNextInternalEvent(ctx, inst.InstanceID)
		if err != nil {
			return fmt.Errorf("load internal event: %w", err)
		}
		if !ok {
			return nil
		}
		if countAttempts {
			if row.Attempts >= maxReplayAttempts {
				now := nowMS(actx.Clock)
				inst.Status = "failed"
				inst.TerminatedAtMS = &now
				return failStoredInstance(ctx, actx, inst.InstanceID, inst.CurrentState,
					fmt.Errorf("%s: internal event %d (%s) ran at %d starts without finishing", causeReplayExhausted, row.EventID, row.MsgName, row.Attempts))
			}
			if err := actx.Store.CountInternalAttempt(ctx, row.EventID); err != nil {
				return fmt.Errorf("count the attempt of internal event %d: %w", row.EventID, err)
			}
		}
		msg, err := buildInternalEventMessage(row)
		if err != nil {
			return fmt.Errorf("build internal event %d: %w", row.EventID, err)
		}
		taken := takenEvent{internalID: &row.EventID}
		if drained == maxInternalEventsPerEvent {
			cause := fmt.Errorf("%s: %d internal events followed one event, the most one may lead to (an emit_internal_event loop?); next was %s",
				causeInternalEventCap, drained, row.MsgName)
			return inst.routeToFailed(ctx, msg, actx, cause, taken)
		}
		if err := inst.RunTransition(ctx, msg, actx, taken); err != nil {
			return fmt.Errorf("run internal event %d: %w", row.EventID, err)
		}
	}
	return nil
}

func (inst *WFInstance) cancelAllTimers(ctx context.Context, actx ActionContext) {
	cancelInstanceTimers(ctx, actx, inst.InstanceID)
}

// cancelInstanceTimers cancels in SY.timer every timer the instance has in the index, and drops them
// from the index.
func cancelInstanceTimers(ctx context.Context, actx ActionContext, instanceID string) {
	timers, err := actx.Store.ListTimersForInstance(ctx, instanceID)
	if err != nil {
		log.Printf("instance %s: list timers for cleanup: %v", instanceID, err)
		return
	}
	for _, t := range timers {
		ref := timerClientRef(instanceID, t.TimerKey)
		if actx.Timer != nil {
			if err := actx.Timer.CancelByClientRef(ctx, ref); err != nil {
				log.Printf("instance %s: cancel timer %s: %v", instanceID, t.TimerKey, err)
			}
		}
		_ = actx.Store.DeleteTimer(ctx, instanceID, t.TimerKey)
	}
}

func (inst *WFInstance) findState(name string) *StateDefinition {
	if inst.def == nil {
		return nil
	}
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

// matchesEvent checks whether a transition's event_match matches the incoming message, by its msg.
func matchesEvent(match EventMatch, msg sdk.Message) bool {
	if strings.TrimSpace(match.Msg) == "" {
		return false
	}
	return msg.Meta.Msg != nil && *msg.Meta.Msg == match.Msg
}

// messageToMap converts an sdk.Message to a plain map for use in CEL evaluation
// and $ref resolution. The map has keys: "msg", "type", "thread_id", "trace_id",
// "payload", and — for messages that carry them — "src" (the router-stamped source
// L2 name), "context" (the parsed meta.context object) and, for a TIMER_FIRED,
// "timer_key" (the key the WF scheduled the timer with).
//
// "src" + "context" let a workflow read WHO sent an ingress message and its
// io_context (channel/conversation/reply_target...) so it can decide routing and
// reply back to the originating IO node. "timer_key" spares a guard SY.timer's
// envelope (the key is at payload.user_payload.timer_key). They are all under the
// single "event" root, and because this one map is what both $ref resolution and
// CEL evaluation consume, they light up in both at once. Numbers follow
// wfcel.DecodeJSON: an integer is an int, any other number a double.
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
	if src := msg.SourceL2Name(); src != "" {
		m["src"] = src
	}
	if len(msg.Meta.Context) > 0 {
		if context, err := wfcel.DecodeJSONObject(msg.Meta.Context); err == nil {
			m["context"] = context
		}
	}
	if len(msg.Payload) > 0 {
		if payload, err := wfcel.DecodeJSONObject(msg.Payload); err == nil {
			m["payload"] = payload
		}
	}
	if rawMsgName(msg) == sdk.MsgTimerFired {
		if _, key, err := firedTimer(msg); err == nil {
			m["timer_key"] = key
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

func buildInternalEventMessage(row InternalEventRow) (sdk.Message, error) {
	msgName := row.MsgName
	threadID := row.InstanceID
	payload := []byte(row.PayloadJSON)
	if len(payload) == 0 {
		payload = []byte(`{}`)
	}
	if !json.Valid(payload) {
		return sdk.Message{}, fmt.Errorf("invalid payload_json for internal event %d", row.EventID)
	}
	return sdk.Message{
		Routing: sdk.Routing{
			TraceID: row.TraceID,
		},
		Meta: sdk.Meta{
			MsgType:  row.MsgType,
			Msg:      &msgName,
			ThreadID: &threadID,
		},
		Payload: payload,
	}, nil
}

func actionSummary(action ActionDefinition) string {
	switch action.Type {
	case "send_message":
		if action.Target != "" {
			return fmt.Sprintf("send_message to %s", action.Target)
		}
	case "emit_internal_event":
		if action.Meta != nil && action.Meta.Msg != "" {
			return fmt.Sprintf("emit_internal_event %s", action.Meta.Msg)
		}
	case "schedule_timer", "cancel_timer", "reschedule_timer":
		if action.TimerKey != "" {
			return fmt.Sprintf("%s %s", action.Type, action.TimerKey)
		}
	case "set_variable":
		if action.Name != "" {
			return fmt.Sprintf("set_variable %s", action.Name)
		}
	}
	return action.Type
}
