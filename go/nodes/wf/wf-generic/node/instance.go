package node

import (
	"context"
	"encoding/json"
	"errors"
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

// maxAgentCallsPerTrigger bounds the messages an instance sends to agents (AI.*) between two
// triggers (wf-orchestration-v1 §5.3.3): a trigger is a message from a node that is no agent (an IO
// node, a WF) or a timer's fire. Past it a send to an agent is not made, the transition goes on,
// and the instance gets one AGENT_CALL_CAP internal event per trigger, an error it may take. So a
// definition whose agents keep answering each other, or whose catch asks a failing agent again,
// stops there (FINDINGS A-125), and the instance never fails for it.
const maxAgentCallsPerTrigger = 8

// msgAgentCallCap names the internal event of a trigger that reached the cap, and codeAgentCallCap
// is the code of its error payload.
const (
	msgAgentCallCap  = "AGENT_CALL_CAP"
	codeAgentCallCap = "agent_call_cap"
)

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
	// causeThreadTaken: another running instance holds the instance's thread, and a message on it
	// reaches that one only.
	causeThreadTaken = "thread_taken"
)

// WFInstance is the in-memory representation of a running workflow instance.
// All mutations must be performed under mu.
type WFInstance struct {
	mu sync.Mutex

	InstanceID string
	// ThreadID is the instance's thread, the conversation's: the thread_id of the message that
	// started it, or InstanceID when that message had none. Every message the instance sends carries
	// it, so the agents keep one memory per conversation and their answers come back here
	// (wf-orchestration-v1 §5.2.1, item 1).
	ThreadID     string
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
	// kept holds the messages keep_message kept, by name, and keptSince names those its next commit
	// writes: kept since the last commit that went through.
	kept      map[string]keptMessage
	keptSince map[string]bool
	// calls counts the messages sent to agents since the instance's last trigger. It lives in memory:
	// a restart starts the count again.
	calls agentCalls
}

// agentCalls is an instance's count of the messages it sent to agents (AI.*) since its last trigger
// (maxAgentCallsPerTrigger).
type agentCalls struct {
	sent int
	// skipped is the agent of the trigger's first send past the cap, and reported says its
	// AGENT_CALL_CAP event went.
	skipped  string
	reported bool
}

// keptMessage is a message keep_message kept: as received, and as the store keeps it.
type keptMessage struct {
	msg sdk.Message
	row KeptMessageRow
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

// NewInstance creates a new WFInstance for the given definition and input, on its own id as its
// thread (the creation sets the thread of the message that starts it, when it has one).
func NewInstance(def *WorkflowDefinition, instanceID, workflowType string, input map[string]any, clock ClockFunc) *WFInstance {
	now := nowMS(clock)
	return &WFInstance{
		InstanceID:   instanceID,
		ThreadID:     instanceID,
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

// FromRow restores an instance from its persistent row and the messages it kept.
func FromRow(row WFInstanceRow, def *WorkflowDefinition, kept []KeptMessageRow) (*WFInstance, error) {
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
		ThreadID:       row.ThreadID,
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
	for _, keptRow := range kept {
		var msg sdk.Message
		if err := json.Unmarshal([]byte(keptRow.MessageJSON), &msg); err != nil {
			return nil, fmt.Errorf("instance %s: bad kept message %q: %w", row.InstanceID, keptRow.Name, err)
		}
		if inst.kept == nil {
			inst.kept = map[string]keptMessage{}
		}
		inst.kept[keptRow.Name] = keptMessage{msg: msg, row: keptRow}
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
		ThreadID:       inst.ThreadID,
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
// The first transition of the current state whose event_match and guard take the event runs; when
// none does, the first of the definition's catch (wf-orchestration-v1 §5.3.2); when the catch takes
// nothing either, the default logs the event, in the instance's log and in the node's, and the
// instance stays where it was.
//
// Execution model (act-then-commit, at least once): the event is durable before this runs (in the
// inbound journal or the internal-event queue, taken), the actions run (exit → transition →
// entry), and one commit writes the new state, enqueues the internal events and deletes the taken
// event. A crash before the commit leaves the event for recovery, which runs it again: its sends
// go again, with the same trace_id.
func (inst *WFInstance) RunTransition(ctx context.Context, event sdk.Message, actx ActionContext, taken takenEvent) error {
	if startsATrigger(event) {
		inst.calls = agentCalls{}
	}

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

	// The first transition that takes the event: the state's, then the catch's.
	chosen, _ := inst.chooseTransition(stateDef.Transitions, event, eventMap, matchesEvent)
	caughtBy := -1
	if chosen == nil {
		chosen, caughtBy = inst.chooseTransition(inst.def.Catch, event, eventMap, catchMatches)
	}

	if chosen == nil {
		// The default: the event is logged, in the instance's log and in the node's (both local,
		// wf-orchestration-v1 §5.3.2), and the instance stays where it was.
		log.Printf("instance %s: unhandled event %s in state %s (trace_id=%s): no transition of the state nor of the catch takes it",
			inst.InstanceID, eventName(event), inst.CurrentState, event.Routing.TraceID)
		now := nowMS(actx.Clock)
		_ = actx.Store.AppendLog(ctx, WFLogEntry{
			InstanceID:  inst.InstanceID,
			LoggedAtMS:  now,
			ActionType:  "event",
			Summary:     fmt.Sprintf("unhandled event %s in state %s", eventName(event), inst.CurrentState),
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

	// A transition without a target_state is internal: it runs its own actions and stays, with no
	// exit or entry actions (an event that arrives early is noted without leaving the state).
	internal := chosen.TargetState == ""
	var targetStateDef *StateDefinition
	if !internal {
		if targetStateDef = inst.findState(chosen.TargetState); targetStateDef == nil {
			return inst.routeToFailed(ctx, event, actx, fmt.Errorf("%s: target state %q is not in the instance's definition", causeStateNotInDefinition, chosen.TargetState), taken)
		}
	}

	// The event's trace_id goes on every send of the transition (and on a send again when recovery
	// runs the event again).
	inst.CurrentTraceID = event.Routing.TraceID

	// Execute exit_actions → transition.actions → entry_actions. A fatal action failure (gap-2) at
	// any step routes the instance to a loud terminal failure instead of advancing with inconsistent
	// state; internal events emitted before the failure are cleared by routeToFailed.
	var emitted []InternalEventRow
	if !internal {
		if err := inst.executeActions(ctx, stateDef.ExitActions, event, actx, &emitted); err != nil {
			return inst.routeToFailed(ctx, event, actx, err, taken)
		}
	}
	if err := inst.executeActions(ctx, chosen.Actions, event, actx, &emitted); err != nil {
		return inst.routeToFailed(ctx, event, actx, err, taken)
	}

	// Advance state
	prevState := inst.CurrentState
	if !internal {
		inst.CurrentState = chosen.TargetState
		if err := inst.executeActions(ctx, targetStateDef.EntryActions, event, actx, &emitted); err != nil {
			return inst.routeToFailed(ctx, event, actx, err, taken)
		}
	}
	// A send the cap on agent calls did not make tells the instance, after the transition.
	if capped, ok := inst.agentCallCapEvent(event, actx); ok {
		emitted = append(emitted, capped)
	}

	// Commit the new state
	inst.CurrentTraceID = ""
	now := nowMS(actx.Clock)
	inst.UpdatedAtMS = now

	isTerminal := !internal && inst.isTerminalState(chosen.TargetState)
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

	summary := fmt.Sprintf("transition %s → %s", prevState, inst.CurrentState)
	if internal {
		summary = fmt.Sprintf("internal transition in %s on %s", inst.CurrentState, eventName(event))
	}
	if caughtBy >= 0 {
		summary = fmt.Sprintf("catch[%d]: %s", caughtBy, summary)
	}
	_ = actx.Store.AppendLog(ctx, WFLogEntry{
		InstanceID: inst.InstanceID,
		LoggedAtMS: now,
		ActionType: "transition",
		Summary:    summary,
		OK:         true,
	})

	return nil
}

// commit writes the instance as it is now, with mutation, in one transaction: with the messages it
// kept since its last commit, or, once it ended, dropping every message it kept.
func (inst *WFInstance) commit(ctx context.Context, actx ActionContext, mutation InstanceCommitMutation) error {
	row, err := inst.ToRow(actx.Clock)
	if err != nil {
		return fmt.Errorf("instance %s: serialize: %w", inst.InstanceID, err)
	}
	mutation.DropKeptMessages = inst.TerminatedAtMS != nil
	if !mutation.DropKeptMessages {
		for name := range inst.keptSince {
			mutation.KeepMessages = append(mutation.KeepMessages, inst.kept[name].row)
		}
	}
	if err := actx.Store.CommitInstanceMutation(ctx, row, mutation); err != nil {
		return err
	}
	inst.keptSince = nil
	return nil
}

// keep keeps msg, as received, under name (keep_message): the instance's next commit writes it,
// replacing the message kept under that name before.
func (inst *WFInstance) keep(name string, msg sdk.Message, actx ActionContext) error {
	raw, err := sdk.MarshalMessage(msg)
	if err != nil {
		return err
	}
	if inst.kept == nil {
		inst.kept = map[string]keptMessage{}
	}
	if inst.keptSince == nil {
		inst.keptSince = map[string]bool{}
	}
	inst.kept[name] = keptMessage{
		msg: msg,
		row: KeptMessageRow{InstanceID: inst.InstanceID, Name: name, MessageJSON: string(raw), KeptAtMS: nowMS(actx.Clock)},
	}
	inst.keptSince[name] = true
	return nil
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
		var capped agentCallCapError
		switch {
		case err == nil:
		case errors.As(err, &capped):
			// Past the cap on agent calls the send is not made, whatever its on_error, and the
			// transition's other actions run.
			detail = err.Error()
			log.Printf("instance %s: %s (%s) not sent: %v", inst.InstanceID, path, action.Type, err)
		default:
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
		msg, err := buildInternalEventMessage(row, inst.ThreadID)
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

// chooseTransition is the first of transitions whose event_match matches event (by matches) and
// whose guard passes, and its index; nil and -1 when none does.
func (inst *WFInstance) chooseTransition(transitions []TransitionDefinition, event sdk.Message, eventMap map[string]any, matches func(EventMatch, sdk.Message) bool) (*TransitionDefinition, int) {
	for i := range transitions {
		t := &transitions[i]
		if !matches(t.EventMatch, event) {
			continue
		}
		if t.Program == nil {
			log.Printf("instance %s: transition %d has no compiled program, skipping", inst.InstanceID, i)
			continue
		}
		if EvalGuard(t.Program, inst.Input, inst.StateVars, eventMap) {
			return t, i
		}
	}
	return nil, -1
}

// matchesEvent checks whether a transition's event_match matches the incoming message: by its msg,
// by its sender (src, the name the router stamps), or by both, and then both must match. An
// event_match with neither (validation refuses it in a state) matches nothing.
func matchesEvent(match EventMatch, msg sdk.Message) bool {
	if strings.TrimSpace(match.Msg) == "" && match.Src == "" {
		return false
	}
	if match.Msg != "" && (msg.Meta.Msg == nil || *msg.Meta.Msg != match.Msg) {
		return false
	}
	return match.Src == "" || msg.SourceL2Name() == match.Src
}

// catchMatches is matchesEvent for a transition of the catch, whose empty event_match matches every
// event.
func catchMatches(match EventMatch, msg sdk.Message) bool {
	if strings.TrimSpace(match.Msg) == "" && match.Src == "" {
		return true
	}
	return matchesEvent(match, msg)
}

// startsATrigger reports whether event starts a new count of the instance's agent calls
// (maxAgentCallsPerTrigger): a timer's fire, or a message from a node that is no agent (an IO node,
// a WF). An agent's answer and an internal event go on with the count of the trigger they follow.
func startsATrigger(event sdk.Message) bool {
	if sdk.IsSystemKind(event.Meta.MsgType) {
		return rawMsgName(event) == sdk.MsgTimerFired
	}
	sender := event.SourceL2Name()
	return sender != "" && !wfcel.IsAgent(sender)
}

// callAgent counts a send to target against the cap of the instance's trigger when target is an
// agent: one past maxAgentCallsPerTrigger is refused (agentCallCapError), and is not made.
func (inst *WFInstance) callAgent(target string) error {
	if !wfcel.IsAgent(target) {
		return nil
	}
	if inst.calls.sent >= maxAgentCallsPerTrigger {
		if inst.calls.skipped == "" {
			inst.calls.skipped = target
		}
		return agentCallCapError{target: target}
	}
	inst.calls.sent++
	return nil
}

// agentCallCapError is a send to an agent past the cap of its trigger: it is not made, whatever its
// on_error.
type agentCallCapError struct {
	target string
}

func (e agentCallCapError) Error() string {
	return codeAgentCallCap + ": " + agentCallCapMessage(e.target)
}

// agentCallCapMessage says why the send to target was not made.
func agentCallCapMessage(target string) string {
	return fmt.Sprintf("%d messages went to agents since the last trigger (a message from an IO node or a WF, or a timer's fire), the most there may be: the one to %s was not sent",
		maxAgentCallsPerTrigger, target)
}

// agentCallCapEvent is the internal event an instance gets after a transition whose send to an agent
// the cap did not make, once per trigger: AGENT_CALL_CAP, with the error payload {type: error, code:
// agent_call_cap, message, target: the agent}. It goes through the state's transitions, the catch
// and the default, as any event.
func (inst *WFInstance) agentCallCapEvent(event sdk.Message, actx ActionContext) (InternalEventRow, bool) {
	if inst.calls.skipped == "" || inst.calls.reported {
		return InternalEventRow{}, false
	}
	inst.calls.reported = true
	payload, err := marshalJSON(map[string]any{
		"type":    "error",
		"code":    codeAgentCallCap,
		"message": agentCallCapMessage(inst.calls.skipped),
		"target":  inst.calls.skipped,
	})
	if err != nil {
		log.Printf("instance %s: %s not queued: %v", inst.InstanceID, msgAgentCallCap, err)
		return InternalEventRow{}, false
	}
	return InternalEventRow{
		InstanceID:  inst.InstanceID,
		MsgType:     wfMsgType,
		MsgName:     msgAgentCallCap,
		PayloadJSON: payload,
		TraceID:     transitionTraceID(inst, event),
		CreatedAtMS: nowMS(actx.Clock),
	}, true
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

// eventName names an event in the instance's log: its msg, and its sender when it has one.
func eventName(msg sdk.Message) string {
	if src := msg.SourceL2Name(); src != "" {
		return fmt.Sprintf("%s from %s", msgName(msg), src)
	}
	return msgName(msg)
}

// buildInternalEventMessage is the event an internal event row makes, on the instance's thread.
func buildInternalEventMessage(row InternalEventRow, threadID string) (sdk.Message, error) {
	msgName := row.MsgName
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
	case "send_message", "forward":
		if action.Message != "" {
			return fmt.Sprintf("%s %s to %s", action.Type, action.Message, action.Target)
		}
		if action.Target != "" {
			return fmt.Sprintf("%s to %s", action.Type, action.Target)
		}
	case "keep_message":
		if action.Name != "" {
			return fmt.Sprintf("keep_message %s", action.Name)
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
