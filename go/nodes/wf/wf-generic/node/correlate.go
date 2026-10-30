package node

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log"
	"runtime/debug"
	"strings"
	"sync"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	wfcel "github.com/4iplatform/json-router/pkg/wfcel"
)

// InstanceRegistry is a thread-safe in-memory map of running instances, by their id and by their
// thread (one running instance per thread: a message on it reaches that instance, and a new one
// starts on a thread only when none runs there).
type InstanceRegistry struct {
	mu        sync.RWMutex
	instances map[string]*WFInstance
	byThread  map[string]*WFInstance
}

func NewInstanceRegistry() *InstanceRegistry {
	return &InstanceRegistry{instances: make(map[string]*WFInstance), byThread: make(map[string]*WFInstance)}
}

// Add puts inst in the registry. It refuses an instance whose thread another running instance
// holds: a message on a thread reaches one instance, so the second would never be reached again.
func (r *InstanceRegistry) Add(inst *WFInstance) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if holder, held := r.byThread[inst.ThreadID]; held && holder.InstanceID != inst.InstanceID {
		return fmt.Errorf("thread %q is held by running instance %s", inst.ThreadID, holder.InstanceID)
	}
	r.instances[inst.InstanceID] = inst
	r.byThread[inst.ThreadID] = inst
	return nil
}

func (r *InstanceRegistry) Get(instanceID string) (*WFInstance, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	inst, ok := r.instances[instanceID]
	return inst, ok
}

// GetByThread returns the running instance on threadID.
func (r *InstanceRegistry) GetByThread(threadID string) (*WFInstance, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	inst, ok := r.byThread[threadID]
	return inst, ok
}

func (r *InstanceRegistry) Remove(instanceID string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if inst, ok := r.instances[instanceID]; ok && r.byThread[inst.ThreadID] == inst {
		delete(r.byThread, inst.ThreadID)
	}
	delete(r.instances, instanceID)
}

func (r *InstanceRegistry) Count() int {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return len(r.instances)
}

func (r *InstanceRegistry) All() []*WFInstance {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make([]*WFInstance, 0, len(r.instances))
	for _, inst := range r.instances {
		out = append(out, inst)
	}
	return out
}

// dispatchWorkflowEvent routes workflow traffic (no control verb, no notice, no system message but a
// TIMER_FIRED: Dispatch has checked that a node sent it) to the instance it belongs to, or starts
// one. The strict correlation order (wf-orchestration-v1 §5.2.1, item 3):
//
//  1. TIMER_FIRED, from SY.timer of the WF's own hive, for a timer this node scheduled → the instance
//     its client_ref names
//  2. from a sender the definition does not involve (no node of its start_from, of an
//     event_match.src or a target of its sends: FINDINGS A-123) → dropped, never answered; on the
//     thread of a running instance, that instance's definition decides, otherwise the current one
//  3. meta.thread_id is the thread of a running instance → that instance
//  4. from a start_from node, and no reply (a *_RESPONSE, an error answer) → a new instance on its
//     thread, on the current definition, as its input_schema allows
//  5. anything else (an answer to an instance that ended, say) → dropped, never answered
//
// So a person who writes again after their conversation's instance ended starts a new one, on the
// same thread, at the initial state; an agent's late answer starts nothing.
func (rt *NodeRuntime) dispatchWorkflowEvent(ctx context.Context, msg sdk.Message) error {
	name := rawMsgName(msg)
	sender := msg.SourceL2Name()
	threadID := derefStringPtr(msg.Meta.ThreadID)
	log.Printf(
		"wf: correlate inbound type=%s msg=%q src_uuid=%s src_l2=%q thread_id=%q trace_id=%s target=%q payload=%s",
		msg.Meta.MsgType,
		name,
		msg.Routing.Src,
		sender,
		threadID,
		msg.Routing.TraceID,
		derefStringPtr(msg.Meta.Target),
		payloadPreview(msg.Payload),
	)

	if name == sdk.MsgTimerFired {
		return rt.takeTimerFired(ctx, msg)
	}
	inst, onThread := rt.Registry.GetByThread(threadID)
	onThread = onThread && threadID != ""
	def := rt.Def
	if onThread {
		def = inst.def
	}
	if !def.Involves(sender) {
		log.Printf("wf: ignoring %s from %q (thread_id=%q trace_id=%s): the definition does not involve its sender",
			msgName(msg), sender, threadID, msg.Routing.TraceID)
		return nil
	}
	if onThread {
		log.Printf("wf: correlate matched thread_id=%s instance_id=%s", threadID, inst.InstanceID)
		return rt.deliver(ctx, inst, msg, "")
	}
	if !rt.Def.StartsFrom(sender) || isReply(name) {
		// SY.timer's answers to the engine's own timer requests never get here (it is no node of the
		// definition), and an agent's late answer ends here.
		log.Printf("wf: ignoring %s from %q (thread_id=%q trace_id=%s): no running instance on its thread, and it starts none",
			msgName(msg), sender, threadID, msg.Routing.TraceID)
		return nil
	}
	return rt.create(ctx, msg)
}

// takeTimerFired takes a TIMER_FIRED only from SY.timer of the WF's own hive, the one its timers are
// scheduled on, and only for a timer this node scheduled: anyone else could fire an instance's timer
// by its client_ref, and SY.timer lets any node schedule a timer whose fire goes to another, under
// SY.timer's own name (FINDINGS A-119). A refused or orphaned fire is dropped, never answered.
func (rt *NodeRuntime) takeTimerFired(ctx context.Context, msg sdk.Message) error {
	authority := timerAuthority(rt.NodeName)
	if sender := msg.SourceL2Name(); sender != authority {
		log.Printf("wf: TIMER_FIRED from %q refused: taken only from %s (trace_id=%s)", sender, authority, msg.Routing.TraceID)
		return nil
	}
	var fired sdk.FiredEvent
	if err := json.Unmarshal(msg.Payload, &fired); err != nil {
		log.Printf("wf: TIMER_FIRED dropped (trace_id=%s): %v", msg.Routing.TraceID, err)
		return nil
	}
	if fired.OwnerL2Name != rt.NodeName {
		log.Printf("wf: TIMER_FIRED refused (trace_id=%s): the timer was scheduled by %q, not by %s",
			msg.Routing.TraceID, fired.OwnerL2Name, rt.NodeName)
		return nil
	}
	instanceID, timerKey, err := firedTimer(msg)
	if err != nil {
		log.Printf("wf: TIMER_FIRED dropped (trace_id=%s): %v", msg.Routing.TraceID, err)
		return nil
	}
	inst, ok := rt.Registry.Get(instanceID)
	if !ok {
		log.Printf("wf: TIMER_FIRED %q for instance %q, which is not running: dropped (orphaned timer)", timerKey, instanceID)
		return nil
	}
	return rt.deliver(ctx, inst, msg, timerKey)
}

// deliver gives msg to inst at least once: it is journaled first, and its transition's commit
// deletes it (firedTimerKey: a TIMER_FIRED's timer, whose index row goes with the journaling).
func (rt *NodeRuntime) deliver(ctx context.Context, inst *WFInstance, msg sdk.Message, firedTimerKey string) error {
	inboundID, err := rt.journal(ctx, inst.InstanceID, inboundEvent, msg, firedTimerKey)
	if err != nil {
		return err
	}
	return rt.runEvent(ctx, inst, msg, inboundID)
}

func (rt *NodeRuntime) journal(ctx context.Context, instanceID, kind string, msg sdk.Message, firedTimerKey string) (int64, error) {
	// As a frame carries it: a payload a transition forwards goes out byte for byte after a replay
	// too.
	raw, err := sdk.MarshalMessage(msg)
	if err != nil {
		return 0, fmt.Errorf("instance %s: journal %s: %w", instanceID, rawMsgName(msg), err)
	}
	id, err := rt.Store.JournalInbound(ctx, InboundEventRow{
		InstanceID:   instanceID,
		Kind:         kind,
		MessageJSON:  string(raw),
		ReceivedAtMS: nowMS(rt.ActCtx.Clock),
	}, firedTimerKey)
	if err != nil {
		return 0, fmt.Errorf("instance %s: journal %s: %w", instanceID, rawMsgName(msg), err)
	}
	return id, nil
}

// runEvent runs a journaled event on inst, under its mutex, and the internal events that follow it.
func (rt *NodeRuntime) runEvent(ctx context.Context, inst *WFInstance, msg sdk.Message, inboundID int64) error {
	inst.Lock()
	defer inst.Unlock()
	taken := takenEvent{inboundID: &inboundID}
	err := rt.protect(ctx, inst, msg, taken, func() error {
		if err := inst.RunTransition(ctx, msg, rt.ActCtx, taken); err != nil {
			return fmt.Errorf("instance %s: run transition: %w", inst.InstanceID, err)
		}
		if err := inst.drainInternalEvents(ctx, rt.ActCtx); err != nil {
			return fmt.Errorf("instance %s: drain internal events: %w", inst.InstanceID, err)
		}
		return nil
	})
	rt.settle(ctx, inst)
	return err
}

// create starts an instance from msg on the current definition, or answers why it does not.
func (rt *NodeRuntime) create(ctx context.Context, msg sdk.Message) error {
	input, err := creationInput(rt.Def, msg)
	if err != nil {
		log.Printf("wf: create rejected trace_id=%s detail=%v input=%s", msg.Routing.TraceID, err, payloadPreview(msg.Payload))
		return rt.answerInvalidInput(ctx, msg, err.Error())
	}
	instanceID := newInstanceID()
	inboundID, err := rt.journal(ctx, instanceID, inboundCreate, msg, "")
	if err != nil {
		return err
	}
	return rt.runCreation(ctx, msg, instanceID, input, inboundID)
}

// creationInput is the input msg starts an instance of def with: its payload, a JSON object (or
// none) that def's input_schema accepts.
func creationInput(def *WorkflowDefinition, msg sdk.Message) (map[string]any, error) {
	input := map[string]any{}
	if payload := bytes.TrimSpace(msg.Payload); len(payload) > 0 && !bytes.Equal(payload, []byte("null")) {
		decoded, err := wfcel.DecodeJSONObject(payload)
		if err != nil {
			return nil, fmt.Errorf("payload is not a JSON object")
		}
		input = decoded
	}
	if err := validateInputPayload(def, input); err != nil {
		return nil, err
	}
	return input, nil
}

// runCreation creates the instance instanceID from the journaled msg and runs its initial
// entry_actions. One commit writes the instance, enqueues its internal events and consumes the
// journaled creation: a crash before it leaves the creation for recovery, which runs it again under
// the same instance_id (and the same trace_id), so no instance is ever half-started. The instance's
// thread is msg's, or its own id when msg has none.
func (rt *NodeRuntime) runCreation(ctx context.Context, msg sdk.Message, instanceID string, input map[string]any, inboundID int64) error {
	def := rt.Def
	inst := NewInstance(def, instanceID, def.WorkflowType, input, rt.ActCtx.Clock)
	inst.ThreadID = creationThread(msg, instanceID)
	inst.DefinitionHash = rt.DefHash
	inst.CurrentTraceID = msg.Routing.TraceID
	log.Printf(
		"wf: create accepted instance_id=%s thread_id=%s workflow_type=%s initial_state=%s trace_id=%s src_l2=%q",
		instanceID,
		inst.ThreadID,
		def.WorkflowType,
		def.InitialState,
		msg.Routing.TraceID,
		msg.SourceL2Name(),
	)

	inst.Lock()
	defer inst.Unlock()
	taken := takenEvent{inboundID: &inboundID}
	err := rt.protect(ctx, inst, msg, taken, func() error {
		initialState := inst.findState(def.InitialState)
		var emitted []InternalEventRow
		if err := inst.executeActions(ctx, initialState.EntryActions, msg, rt.ActCtx, &emitted); err != nil {
			// gap-2: a fatal entry-action failure at creation lands the new instance in failed rather
			// than a half-initialized running state.
			return inst.routeToFailed(ctx, msg, rt.ActCtx, err, taken)
		}
		if inst.isTerminalState(initialState.Name) {
			now := nowMS(rt.ActCtx.Clock)
			inst.Status = terminalStatus(initialState.Name, inst.def.TerminalStates)
			inst.TerminatedAtMS = &now
			inst.cancelAllTimers(ctx, rt.ActCtx)
			emitted = nil
		}
		inst.CurrentTraceID = ""
		mutation := taken.mutation()
		mutation.EnqueueInternalEvents = emitted
		mutation.ClearInternalEvents = inst.TerminatedAtMS != nil
		if err := inst.commit(ctx, rt.ActCtx, mutation); err != nil {
			return fmt.Errorf("instance %s: commit creation: %w", instanceID, err)
		}
		log.Printf("wf: create committed instance_id=%s state=%s status=%s", instanceID, inst.CurrentState, inst.Status)
		if inst.Status == "running" {
			if err := rt.Registry.Add(inst); err != nil {
				// An instance starts only on a thread no running instance holds (correlation, and
				// recovery's replay of a creation), so this is a bug: the new instance fails, loudly,
				// instead of taking the other's thread.
				log.Printf("wf: REFUSED instance %s: %v", instanceID, err)
				return rt.failInStore(ctx, inst, fmt.Errorf("%s: %v", causeThreadTaken, err))
			}
		}
		return inst.drainInternalEvents(ctx, rt.ActCtx)
	})
	rt.settle(ctx, inst)
	return err
}

// creationThread is the thread of the instance msg starts as instanceID: msg's, or the instance's own
// id when msg has none.
func creationThread(msg sdk.Message, instanceID string) string {
	if threadID := derefStringPtr(msg.Meta.ThreadID); strings.TrimSpace(threadID) != "" {
		return threadID
	}
	return instanceID
}

// protect runs the processing of one event for inst. A panic in it fails that instance
// (causeTransitionPanic), never the node: the receive loop is the node's only one, and the event,
// still journaled, would panic it again at every start.
func (rt *NodeRuntime) protect(ctx context.Context, inst *WFInstance, event sdk.Message, taken takenEvent, run func() error) (err error) {
	defer func() {
		recovered := recover()
		if recovered == nil {
			return
		}
		cause := fmt.Errorf("%s: %v", causeTransitionPanic, recovered)
		log.Printf("instance %s: %v\n%s", inst.InstanceID, cause, debug.Stack())
		err = rt.failAfterPanic(ctx, inst, event, cause, taken)
	}()
	return run()
}

// failAfterPanic fails inst through routeToFailed (its failed state's entry actions, its timers,
// its log, the taken event consumed), or straight in the store when that panics or fails too.
func (rt *NodeRuntime) failAfterPanic(ctx context.Context, inst *WFInstance, event sdk.Message, cause error, taken takenEvent) (err error) {
	defer func() {
		if recovered := recover(); recovered != nil {
			log.Printf("instance %s: failing it panicked too (%v): failed in the store", inst.InstanceID, recovered)
			err = rt.failInStore(ctx, inst, cause)
		}
	}()
	if err := inst.routeToFailed(ctx, event, rt.ActCtx, cause, taken); err != nil {
		log.Printf("instance %s: failing it: %v: failed in the store", inst.InstanceID, err)
		return rt.failInStore(ctx, inst, cause)
	}
	return fmt.Errorf("instance %s failed: %w", inst.InstanceID, cause)
}

// failInStore ends inst as failed with no transition: its timers cancelled, its log told the cause,
// and its row, internal events and journaled events settled by the store (Store.FailInstance).
func (rt *NodeRuntime) failInStore(ctx context.Context, inst *WFInstance, cause error) error {
	now := nowMS(rt.ActCtx.Clock)
	inst.Status = "failed"
	inst.TerminatedAtMS = &now
	if err := failStoredInstance(ctx, rt.ActCtx, inst.InstanceID, inst.CurrentState, cause); err != nil {
		return err
	}
	return fmt.Errorf("instance %s failed: %w", inst.InstanceID, cause)
}

// failStoredInstance fails the stored instance instanceID, in state, for cause.
func failStoredInstance(ctx context.Context, actx ActionContext, instanceID, state string, cause error) error {
	now := nowMS(actx.Clock)
	cancelInstanceTimers(ctx, actx, instanceID)
	_ = actx.Store.AppendLog(ctx, WFLogEntry{
		InstanceID:  instanceID,
		LoggedAtMS:  now,
		ActionType:  "transition",
		Summary:     fmt.Sprintf("FAILED in %s", state),
		OK:          false,
		ErrorDetail: cause.Error(),
	})
	if err := actx.Store.FailInstance(ctx, instanceID, now); err != nil {
		return fmt.Errorf("instance %s: fail in the store: %w", instanceID, err)
	}
	log.Printf("instance %s: failed in state %q: %v", instanceID, state, cause)
	return nil
}

// settle: an instance that ended leaves the registry, and the definition it was born with goes once
// no running instance uses it.
func (rt *NodeRuntime) settle(ctx context.Context, inst *WFInstance) {
	if inst.Status == "running" || inst.Status == "cancelling" {
		return
	}
	rt.Registry.Remove(inst.InstanceID)
	if inst.DefinitionHash != rt.DefHash {
		rt.pruneDefinitions(ctx)
	}
}

func (rt *NodeRuntime) pruneDefinitions(ctx context.Context) {
	dropped, err := rt.Store.PruneDefinitions(ctx, rt.DefHash)
	if err != nil {
		log.Printf("wf: prune definitions: %v", err)
		return
	}
	if dropped > 0 {
		log.Printf("wf: dropped %d definition(s) no running instance uses", dropped)
	}
}

// answerInvalidInput tells the sender of msg why it starts no instance, with an INVALID_INPUT,
// unless msg is one the engine never answers (neverAnswered): an INVALID_INPUT to another WF's
// INVALID_INPUT would bounce between the two forever (FINDINGS A-119).
func (rt *NodeRuntime) answerInvalidInput(ctx context.Context, msg sdk.Message, detail string) error {
	actx := rt.ActCtx
	if actx.Dispatcher == nil || neverAnswered(msg) {
		return nil
	}
	replyTo := msg.Routing.Src
	if replyTo == "" {
		return nil
	}
	log.Printf(
		"wf: sending error response code=%s detail=%q reply_to=%s trace_id=%s src_l2=%q",
		msgInvalidInput,
		detail,
		replyTo,
		msg.Routing.TraceID,
		msg.SourceL2Name(),
	)
	payload, _ := sdk.MarshalPayload(wfErrorPayload(msgInvalidInput, detail))
	msgName := msgInvalidInput
	resp := sdk.Message{
		Routing: sdk.Routing{
			Src:     actx.Dispatcher.NodeUUID(),
			Dst:     sdk.UnicastDestination(replyTo),
			TTL:     16,
			TraceID: msg.Routing.TraceID,
		},
		Meta: sdk.Meta{
			MsgType: sdk.SYSTEMKind,
			Msg:     &msgName,
		},
		Payload: payload,
	}
	return actx.Dispatcher.SendMsg(resp)
}

func derefStringPtr(value *string) string {
	if value == nil {
		return ""
	}
	return *value
}

func payloadPreview(raw []byte) string {
	if len(raw) == 0 {
		return "{}"
	}
	const maxLen = 240
	text := strings.TrimSpace(string(raw))
	if len(text) <= maxLen {
		return text
	}
	return text[:maxLen] + "...(truncated)"
}
