package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"slices"
	"strings"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	wfcel "github.com/4iplatform/json-router/pkg/wfcel"
	"github.com/google/uuid"
)

// wfMsgType is the type of every message a WF makes, sent or internal: a WF always sends user
// messages (meta.type left the definition language in 0.1.90). An agent answers a system message
// from its control plane, and the WF never saw that answer: the instance hung (FINDINGS A-117).
const wfMsgType = "user"

// Dispatcher abstracts sending L2 messages (testable without a real network). The note is what the
// WF's entry in the message's trace adds (docs/message-trace-v1.md §5), the SDK writing the entry.
type Dispatcher interface {
	SendMsg(msg sdk.Message, note sdk.TraceNote) error
	NodeL2Name() string
	NodeUUID() string
}

// TimerSender abstracts SY.timer operations (testable without a real SY.timer). ScheduleIn,
// Schedule, CancelByClientRef, RescheduleInByClientRef and RescheduleByClientRef send their request
// without waiting for SY.timer's answer; a schedule and a reschedule return the trace_id their
// request went under, which SY.timer's TIMER_RESPONSE and the router's UNREACHABLE carry back
// (FINDINGS A-254). CancelByClientRefConfirmed and List wait for the answer.
//
// ScheduleIn and RescheduleInByClientRef send a delay (fire_in_ms, new_fire_in_ms), which SY.timer
// counts from when it gets the request, on its own clock, its 60 s floor included; Schedule and
// RescheduleByClientRef send a time (fire_at_utc_ms, new_fire_at_utc_ms), for a fire_at.
type TimerSender interface {
	ScheduleIn(ctx context.Context, d time.Duration, opts sdk.ScheduleOptions) (string, error)
	Schedule(ctx context.Context, fireAt time.Time, opts sdk.ScheduleOptions) (string, error)
	CancelByClientRef(ctx context.Context, clientRef string) error
	CancelByClientRefConfirmed(ctx context.Context, clientRef string) error
	RescheduleInByClientRef(ctx context.Context, clientRef string, d time.Duration) (string, error)
	RescheduleByClientRef(ctx context.Context, clientRef string, newFireAt time.Time) (string, error)
	List(ctx context.Context, filter sdk.ListFilter) ([]sdk.TimerInfo, error)
}

// ActionContext bundles all dependencies needed to execute a workflow action.
type ActionContext struct {
	Store      *Store
	Dispatcher Dispatcher
	Timer      TimerSender
	// Requests are the timer requests SY.timer has not answered yet, which a refusal of marks their
	// timer in the index to go again (FINDINGS A-254); nil keeps none.
	Requests *timerRequests
	Clock    ClockFunc
	// SelfILK is the WF node's own ILK (FLUXBEE_NODE_ILK_ID): with the node's name, the writer its
	// own messages name (docs/cognition-hands-v1.md §5).
	SelfILK string
	// TransitionActions are the kinds of action the step a send belongs to runs, in order: a
	// transition's exit, own and entry actions (RunTransition), else the actions run together
	// (runActions). The WF's entry in the message's trace names them (traceNote).
	TransitionActions []string
}

// executeAction executes a single action definition. Its error goes to the caller (runActions),
// which halts the transition when the action's on_error is fail.
func executeAction(ctx context.Context, path string, action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext, emitted *[]InternalEventRow) error {
	switch action.Type {
	case "send_message":
		// A send to an agent counts against the cap of the instance's trigger, past which it is not
		// made (maxAgentCallsPerTrigger).
		if err := inst.callAgent(action.Target); err != nil {
			return err
		}
		return execSendMessage(ctx, action, inst, event, actx)
	case "forward":
		if err := inst.callAgent(action.Target); err != nil {
			return err
		}
		return execForward(ctx, action, inst, event, actx)
	case "keep_message":
		return execKeepMessage(action, inst, event, actx)
	case "emit_internal_event":
		return execEmitInternalEvent(action, inst, event, actx, emitted)
	case "schedule_timer":
		return execScheduleTimer(ctx, action, inst, event, actx)
	case "cancel_timer":
		return execCancelTimer(ctx, action, inst, actx)
	case "reschedule_timer":
		return execRescheduleTimer(ctx, action, inst, event, actx)
	case "set_variable":
		return execSetVariable(ctx, action, inst, event, actx)
	default:
		return fmt.Errorf("%s: unknown action type %q", path, action.Type)
	}
}

func execEmitInternalEvent(action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext, emitted *[]InternalEventRow) error {
	if emitted == nil {
		return fmt.Errorf("emit_internal_event: no event sink configured")
	}

	eventMap := messageToMap(event)
	resolvedPayload, err := Resolve(action.Payload, inst.Input, inst.StateVars, eventMap)
	if err != nil {
		return fmt.Errorf("emit_internal_event: resolve payload: %w", err)
	}

	payloadJSON, err := marshalJSON(resolvedPayload)
	if err != nil {
		return fmt.Errorf("emit_internal_event: marshal payload: %w", err)
	}

	msgName := ""
	if action.Meta != nil {
		msgName = action.Meta.Msg
	}
	traceID := inst.CurrentTraceID
	if traceID == "" {
		traceID = event.Routing.TraceID
	}

	*emitted = append(*emitted, InternalEventRow{
		InstanceID:  inst.InstanceID,
		MsgType:     wfMsgType,
		MsgName:     msgName,
		PayloadJSON: payloadJSON,
		TraceID:     traceID,
		CreatedAtMS: nowMS(actx.Clock),
	})
	return nil
}

// execSendMessage sends a message the WF writes: its msg and payload from the definition, on the
// instance's thread (every message the instance sends carries it, so the agents keep one memory per
// conversation and their answers come back here), with its meta.context and the WF's block. Its ILKs
// are the WF's to say, since it routes (FINDINGS A-139), and come from the instance's person, when it
// has one (an instance whose thread several people write on has none, notePerson):
//   - to an IO node (IO.*) it names no sender (no src_ilk): the WF wrote it, not the person, and the
//     router's stamp names the WF. Who said what travels in the facts (vars). To the person's own IO
//     node (personsIO) it names the person as its dst_ilk and the person's channel as its ich
//     (toPerson); to any other IO node, no person and no ich;
//   - to any other node, an agent above all, it carries the person as its src_ilk and ich: an agent
//     files it under the person, as it does a forwarded message (wf-orchestration-v1 §5.4.3).
func execSendMessage(ctx context.Context, action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) error {
	if actx.Dispatcher == nil {
		return fmt.Errorf("send_message: no dispatcher configured")
	}

	eventMap := messageToMap(event)
	resolvedPayload, err := Resolve(action.Payload, inst.Input, inst.StateVars, eventMap)
	if err != nil {
		return fmt.Errorf("send_message: resolve payload: %w", err)
	}

	raw, err := wfcel.EncodeJSON(resolvedPayload)
	if err != nil {
		return fmt.Errorf("send_message: marshal payload: %w", err)
	}

	msgName := ""
	if action.Meta != nil {
		msgName = action.Meta.Msg
	}

	// gap-3: an optional meta.context (an IO node's context.io, say), resolved with the same $ref
	// rules as the payload, the trace of the cycle the instance is handling, and the WF's block.
	metaContext := map[string]any{}
	if err := writeActionContext(metaContext, action, inst, eventMap); err != nil {
		return fmt.Errorf("send_message: %w", err)
	}
	inst.withCycleTrace(metaContext)
	metaContext[sdk.WriterKey] = actx.writer()
	contextRaw, err := withWFBlock(metaContext, action, inst, eventMap)
	if err != nil {
		return fmt.Errorf("send_message: %w", err)
	}

	// The target is a node name written in the definition, validated when it loaded (a valid L2
	// name, never a SY.* or RT.* node).
	threadID := inst.ThreadID
	msg := sdk.Message{
		Routing: sdk.Routing{
			Src:     actx.Dispatcher.NodeUUID(),
			Dst:     sdk.UnicastDestination(action.Target),
			TTL:     16,
			TraceID: transitionTraceID(inst, event),
		},
		Meta: sdk.Meta{
			MsgType:  wfMsgType,
			Msg:      &msgName,
			ThreadID: &threadID,
			Context:  contextRaw,
		},
		Payload: raw,
	}
	switch {
	case inst.personsIO(action.Target):
		inst.toPerson(&msg.Meta)
	case !wfcel.IsIO(action.Target):
		if person := inst.PersonILK; person != "" {
			msg.Meta.SrcIlk = &person
		}
		if ich := inst.PersonICH; ich != "" {
			msg.Meta.Ich = &ich
		}
	}
	return sendWithRetry(ctx, actx, "send_message", msg, traceNote("send_message", actx))
}

// personsIO reports whether target is the person's IO node: the IO node (IO.*) whose message started
// the instance (PersonIO), whose messages carry the instance's person (notePerson). What the WF sends
// it names the person (toPerson). Another IO node of start_from is not the person's: when two
// channels open conversations, the person writes on one, and the other may be the team's.
func (inst *WFInstance) personsIO(target string) bool {
	return wfcel.IsIO(target) && inst.PersonIO != "" && target == inst.PersonIO
}

// toPerson names the instance's person on meta, a message to the person's IO node (FINDINGS A-139):
// the person as its dst_ilk, and the person's channel as its ich, none when the person's message had
// none. A forwarded message loses those of whoever it answered: an agent answers whoever spoke, on the
// channel they spoke from. Without a person it changes nothing.
func (inst *WFInstance) toPerson(meta *sdk.Meta) {
	person, ich := inst.PersonILK, inst.PersonICH
	if person == "" {
		return
	}
	meta.DstIlk, meta.Ich = &person, nil
	if ich != "" {
		meta.Ich = &ich
	}
}

// execForward sends a message on as it was received (wf-orchestration-v1 §5.2.1, item 5): the
// event's, or the one kept under action.Message. Its payload goes out byte for byte, never decoded,
// and its meta is the message's (type, msg, src_ilk, dst_ilk, ich, scope, priority and context)
// without memory_package. What the WF changes is metadata: the destination (a target written in
// the definition), the thread (the instance's), the context keys the action writes, the trace,
// which is the cycle's the instance is handling (a kept message's own is of an earlier cycle), the
// WF's block, and toward an IO node whom it names. Its src_ilk, who speaks, travels intact:
//   - to the person's IO node (personsIO) its dst_ilk and ich are the instance's person and the
//     person's channel, when it has a person (toPerson, FINDINGS A-139): an agent answers whoever
//     spoke, on the channel they spoke from, so its answer to a supervisor names the supervisor and
//     the supervisors' channel;
//   - to any other IO node (the team's, say) it names no one, no dst_ilk and no ich, as a send does
//     (FINDINGS A-215): whoever the message was said to, on whatever channel, is not there, and
//     cognition takes a relay's addressee from its dst_ilk (the last person who wrote through that
//     node when it names none).
func execForward(ctx context.Context, action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) error {
	if actx.Dispatcher == nil {
		return fmt.Errorf("forward: no dispatcher configured")
	}
	source, err := inst.messageToForward(action.Message, event)
	if err != nil {
		return fmt.Errorf("forward: %w", err)
	}

	metaContext := map[string]any{}
	if len(source.Meta.Context) > 0 && string(source.Meta.Context) != "null" {
		if metaContext, err = wfcel.DecodeJSONObject(source.Meta.Context); err != nil {
			return fmt.Errorf("forward: the message's context: %w", err)
		}
	}
	eventMap := messageToMap(event)
	if err := writeActionContext(metaContext, action, inst, eventMap); err != nil {
		return fmt.Errorf("forward: %w", err)
	}
	inst.withCycleTrace(metaContext)
	withSourceWriter(metaContext, source)
	contextRaw, err := withWFBlock(metaContext, action, inst, eventMap)
	if err != nil {
		return fmt.Errorf("forward: %w", err)
	}

	meta := source.Meta
	meta.MemoryPackage = nil
	threadID := inst.ThreadID
	meta.ThreadID = &threadID
	meta.Context = contextRaw
	switch {
	case inst.personsIO(action.Target):
		inst.toPerson(&meta)
	case wfcel.IsIO(action.Target):
		meta.DstIlk, meta.Ich = nil, nil
	}
	msg := sdk.Message{
		Routing: sdk.Routing{
			Src:     actx.Dispatcher.NodeUUID(),
			Dst:     sdk.UnicastDestination(action.Target),
			TTL:     16,
			TraceID: transitionTraceID(inst, event),
		},
		Meta:    meta,
		Payload: source.Payload,
	}
	return sendWithRetry(ctx, actx, "forward", msg, traceNote("forward", actx))
}

// execKeepMessage keeps the event's message, whole and as received, under the action's name: a later
// forward sends it (wf-orchestration-v1 §5.2.1, item 6). A later keep under the same name replaces
// it; the instance's commit writes it, and it goes when the instance ends.
func execKeepMessage(action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) error {
	if !receivedFromANode(event) {
		return fmt.Errorf("keep_message %q: %s is no message a node sent (a TIMER_FIRED or an internal event is never kept)", action.Name, msgName(event))
	}
	if err := inst.keep(action.Name, event, actx); err != nil {
		return fmt.Errorf("keep_message %q: %w", action.Name, err)
	}
	return nil
}

// messageToForward is the message a forward sends: the one kept under name, or else the event's,
// when a node sent it.
func (inst *WFInstance) messageToForward(name string, event sdk.Message) (sdk.Message, error) {
	if name != "" {
		kept, ok := inst.kept[name]
		if !ok {
			return sdk.Message{}, fmt.Errorf("no message is kept under %q", name)
		}
		return kept.msg, nil
	}
	if !receivedFromANode(event) {
		return sdk.Message{}, fmt.Errorf("%s is no message a node sent (a TIMER_FIRED or an internal event is never forwarded)", msgName(event))
	}
	return event, nil
}

// receivedFromANode reports whether msg is a message a node sent the WF, which keep_message keeps and
// forward sends: it carries its sender's router-stamped name (an internal event has none) and is no
// system message (a TIMER_FIRED, a control verb).
func receivedFromANode(msg sdk.Message) bool {
	return msg.SourceL2Name() != "" && !sdk.IsSystemKind(msg.Meta.MsgType)
}

// writer is who wrote what the WF itself sends: the WF node, by its ILK and its name
// (docs/cognition-hands-v1.md §5). An agent that gets it does not analyze it as a person's words,
// though its src_ilk names the person, whose conversation it is.
func (actx ActionContext) writer() sdk.Writer {
	writer := sdk.Writer{ILK: actx.SelfILK}
	if actx.Dispatcher != nil {
		writer.Name = actx.Dispatcher.NodeL2Name()
	}
	return writer
}

// withSourceWriter puts on metaContext the writer of the message a forward sends, in place of any
// the action's meta.context wrote: the forward passes the text on, and its writer with it. A message
// that names none goes on naming none.
func withSourceWriter(metaContext map[string]any, source sdk.Message) {
	if writer, ok := sdk.WriterOf(source.Meta.Context); ok {
		metaContext[sdk.WriterKey] = writer
		return
	}
	delete(metaContext, sdk.WriterKey)
}

// withCycleTrace puts the trace of the cycle the instance is handling on metaContext, in place of
// any the message brought or the action wrote, or takes it off when the cycle has none yet (a
// timer's fire starts one): the SDK adds the WF's entry as it sends (docs/message-trace-v1.md §5).
func (inst *WFInstance) withCycleTrace(metaContext map[string]any) {
	if inst.cycleTrace != nil {
		metaContext[sdk.TraceKey] = inst.cycleTrace
		return
	}
	delete(metaContext, sdk.TraceKey)
}

// traceNote is what the WF's entry in a message's trace says it did (docs/message-trace-v1.md §5):
// the kind of action that sent it, then the step's other kinds of action, so an entry may hold more
// than one.
func traceNote(sent string, actx ActionContext) sdk.TraceNote {
	act := []string{sent}
	for _, kind := range actx.TransitionActions {
		if kind != sent && !slices.Contains(act, kind) {
			act = append(act, kind)
		}
	}
	return sdk.TraceNote{Act: act}
}

// actionKinds is the kinds of action of lists, in order, each once.
func actionKinds(lists ...[]ActionDefinition) []string {
	var kinds []string
	for _, list := range lists {
		for _, action := range list {
			if !slices.Contains(kinds, action.Type) {
				kinds = append(kinds, action.Type)
			}
		}
	}
	return kinds
}

// writeActionContext writes the keys of the action's meta.context, $ref-resolved, over metaContext:
// a send_message's whole context, the keys a forward changes on the message's own.
func writeActionContext(metaContext map[string]any, action ActionDefinition, inst *WFInstance, eventMap map[string]any) error {
	if action.Meta == nil || action.Meta.Context == nil {
		return nil
	}
	resolved, err := Resolve(action.Meta.Context, inst.Input, inst.StateVars, eventMap)
	if err != nil {
		return fmt.Errorf("resolve meta.context: %w", err)
	}
	written, ok := resolved.(map[string]any)
	if !ok {
		return fmt.Errorf("meta.context resolved to %T, not a JSON object", resolved)
	}
	for key, value := range written {
		metaContext[key] = value
	}
	return nil
}

// withWFBlock is metaContext with the WF's block (meta.context.wf, wf-orchestration-v1 §5.2.1,
// item 7), as the message goes: vars, what the action's vars writes, $ref-resolved: the
// deterministic context the WF gives the receiver; menu, the action's menu as it loaded, its known
// options written in, what an agent or an IO node's people may choose from (§5.3.6, §5.4.4); and
// subject, the mark an IO node places the message by (§5.4.2). Each is there only when the action
// has it, and the block only with one of them: a message with none carries no block, not even the
// one a forwarded message brings from another WF. The nodes the message went through are its
// trace's (docs/message-trace-v1.md), which replaced the block's trail.
func withWFBlock(metaContext map[string]any, action ActionDefinition, inst *WFInstance, eventMap map[string]any) (json.RawMessage, error) {
	block := map[string]any{}
	if action.Vars != nil {
		vars, err := Resolve(action.Vars, inst.Input, inst.StateVars, eventMap)
		if err != nil {
			return nil, fmt.Errorf("resolve vars: %w", err)
		}
		block["vars"] = vars
	}
	if action.Menu != nil {
		block["menu"] = action.Menu
	}
	if action.Subject != "" {
		block["subject"] = action.Subject
	}
	if len(block) == 0 {
		delete(metaContext, wfcel.ContextWF)
	} else {
		metaContext[wfcel.ContextWF] = block
	}
	raw, err := wfcel.EncodeJSON(metaContext)
	if err != nil {
		return nil, fmt.Errorf("marshal meta.context: %w", err)
	}
	return raw, nil
}

// transitionTraceID is the trace_id of the event the instance takes, which every send of its
// transition carries (and a send again, when recovery runs the event again).
func transitionTraceID(inst *WFInstance, event sdk.Message) string {
	if inst.CurrentTraceID != "" {
		return inst.CurrentTraceID
	}
	return event.Routing.TraceID
}

// sendWithRetry sends msg, the message of an action of type verb. A transient backpressure/reconnect
// makes NodeSender.Send return "sender queue full" (a non-blocking channel send). With send_message
// and forward defaulting to on_error=fail (gap-2), a single such blip would needlessly route a
// healthy instance to terminal failure. Retry a few times with a short, ctx-aware backoff so a
// momentarily full queue drains before the error is treated as fatal; a permanent condition (dead
// connection) still fails after the attempts. A message over the frame limit is refused the same
// way every time, so it fails at once.
func sendWithRetry(ctx context.Context, actx ActionContext, verb string, msg sdk.Message, note sdk.TraceNote) error {
	var sendErr error
	for attempt := 0; attempt < sendMaxAttempts; attempt++ {
		if attempt > 0 {
			select {
			case <-ctx.Done():
				return fmt.Errorf("%s: cancelled during send retry: %w", verb, ctx.Err())
			case <-time.After(sendRetryBackoff):
			}
		}
		if sendErr = actx.Dispatcher.SendMsg(msg, note); sendErr == nil {
			return nil
		}
		var tooLarge *sdk.MessageTooLargeError
		if errors.As(sendErr, &tooLarge) {
			return fmt.Errorf("%s: %w", verb, sendErr)
		}
	}
	return fmt.Errorf("%s: dispatch failed after %d attempts: %w", verb, sendMaxAttempts, sendErr)
}

const (
	sendMaxAttempts  = 4
	sendRetryBackoff = 50 * time.Millisecond
)

// timerTime resolves a timer action's time when the action runs: its fire_in or its fire_at, each a
// literal or a $ref (a time taken from state, or from an agent's answer), checked then: a duration
// that parses, or an RFC3339 time, 60 s ahead at least (SY.timer's floor) and 366 days at most (its
// ceiling, past which it refuses the schedule every time, FINDINGS A-254). It returns when the timer
// fires, and the fire_in it was given (zero for a fire_at).
func timerTime(action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) (time.Time, time.Duration, error) {
	resolve := func(field string, value any) (string, error) {
		resolved, err := Resolve(value, inst.Input, inst.StateVars, messageToMap(event))
		if err != nil {
			return "", fmt.Errorf("resolve %s: %w", field, err)
		}
		text, ok := resolved.(string)
		if !ok {
			return "", fmt.Errorf("%s resolved to %T, not a string", field, resolved)
		}
		return text, nil
	}
	now := actx.Clock()
	if wfcel.IsTimeSet(action.FireAt) {
		text, err := resolve("fire_at", action.FireAt)
		if err != nil {
			return time.Time{}, 0, err
		}
		if err := wfcel.CheckFireAt(text); err != nil {
			return time.Time{}, 0, fmt.Errorf("fire_at %q: %w", text, err)
		}
		fireAt, _ := time.Parse(time.RFC3339, text)
		switch ahead := fireAt.Sub(now); {
		case ahead < minTimerDuration:
			return time.Time{}, 0, fmt.Errorf("fire_at %s is less than 60s ahead", text)
		case ahead > wfcel.MaxTimerDelay:
			return time.Time{}, 0, fmt.Errorf("fire_at %s is more than 366d ahead", text)
		}
		return fireAt, 0, nil
	}
	text, err := resolve("fire_in", action.FireIn)
	if err != nil {
		return time.Time{}, 0, err
	}
	if err := wfcel.CheckFireIn(text); err != nil {
		return time.Time{}, 0, fmt.Errorf("fire_in %q: %w", text, err)
	}
	in, _ := parseWorkflowDuration(text)
	return now.Add(in), in, nil
}

func execScheduleTimer(ctx context.Context, action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) error {
	if actx.Timer == nil {
		return fmt.Errorf("schedule_timer: no timer sender configured")
	}

	opts := timerScheduleOptions(actx.Dispatcher.NodeL2Name(), inst.InstanceID, action.TimerKey)
	if action.MissedPolicy != "" {
		opts.MissedPolicy = sdk.MissedPolicy(action.MissedPolicy)
	}
	if action.MissedWithinMS != nil {
		opts.MissedWithinMS = action.MissedWithinMS
	}

	fireAt, in, err := timerTime(action, inst, event, actx)
	if err != nil {
		return fmt.Errorf("schedule_timer %q: %w", action.TimerKey, err)
	}
	// The index first: from here on the timer is the instance's, whatever becomes of the request,
	// which goes without waiting for SY.timer's answer. A request SY.timer does not take, or that does
	// not go, marks the timer to go again (FINDINGS A-254).
	now := nowMS(actx.Clock)
	if err := actx.Store.RegisterTimer(ctx, inst.InstanceID, action.TimerKey, now, fireAt.UnixMilli()); err != nil {
		return fmt.Errorf("schedule_timer %q: %w", action.TimerKey, err)
	}
	var traceID string
	if in > 0 {
		traceID, err = actx.Timer.ScheduleIn(ctx, in, opts)
	} else {
		traceID, err = actx.Timer.Schedule(ctx, fireAt, opts)
	}
	req := timerRequest{timer: timerRef{inst.InstanceID, action.TimerKey}, verb: verbTimerSchedule, traceID: traceID, sentAtMS: now}
	if err := actx.timerRequestSent(ctx, req, err); err != nil {
		return fmt.Errorf("schedule_timer %q: %w", action.TimerKey, err)
	}
	return nil
}

func execCancelTimer(ctx context.Context, action ActionDefinition, inst *WFInstance, actx ActionContext) error {
	if actx.Timer == nil {
		return fmt.Errorf("cancel_timer: no timer sender configured")
	}

	timers, err := actx.Store.ListTimersForInstance(ctx, inst.InstanceID)
	if err != nil {
		return fmt.Errorf("cancel_timer: list timers: %w", err)
	}
	found := false
	for _, t := range timers {
		if t.TimerKey == action.TimerKey {
			found = true
			break
		}
	}
	if !found {
		log.Printf("instance %s: cancel_timer: timer_key %q not registered (no-op)", inst.InstanceID, action.TimerKey)
		return nil
	}

	clientRef := timerClientRef(inst.InstanceID, action.TimerKey)
	if err := actx.Timer.CancelByClientRef(ctx, clientRef); err != nil {
		return fmt.Errorf("cancel_timer %q: %w", action.TimerKey, err)
	}
	return actx.Store.DeleteTimer(ctx, inst.InstanceID, action.TimerKey)
}

func execRescheduleTimer(ctx context.Context, action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) error {
	if actx.Timer == nil {
		return fmt.Errorf("reschedule_timer: no timer sender configured")
	}

	timers, err := actx.Store.ListTimersForInstance(ctx, inst.InstanceID)
	if err != nil {
		return fmt.Errorf("reschedule_timer: list timers: %w", err)
	}
	found := false
	for _, t := range timers {
		if t.TimerKey == action.TimerKey {
			found = true
			break
		}
	}
	if !found {
		log.Printf("instance %s: reschedule_timer: timer_key %q not registered (no-op)", inst.InstanceID, action.TimerKey)
		return nil
	}

	clientRef := timerClientRef(inst.InstanceID, action.TimerKey)
	newFireAt, in, err := timerTime(action, inst, event, actx)
	if err != nil {
		return fmt.Errorf("reschedule_timer %q: %w", action.TimerKey, err)
	}
	// The index first, as a schedule's: its refusal, if SY.timer has not taken the timer, stays, so the
	// timer goes again at its new time (FINDINGS A-254).
	now := nowMS(actx.Clock)
	if err := actx.Store.MoveTimer(ctx, inst.InstanceID, action.TimerKey, now, newFireAt.UnixMilli()); err != nil {
		return fmt.Errorf("reschedule_timer %q: %w", action.TimerKey, err)
	}
	// A fire_in goes as the delay it is, as a schedule's: SY.timer counts it from when it gets the
	// request, its 60 s floor included. Sent as a time counted on the node's clock, before the move's
	// commit, a move of "1m" reached SY.timer less than 60 s ahead, and SY.timer refused it
	// (TIMER_BELOW_MINIMUM) practically every time (FINDINGS A-254's review).
	var traceID string
	if in > 0 {
		traceID, err = actx.Timer.RescheduleInByClientRef(ctx, clientRef, in)
	} else {
		traceID, err = actx.Timer.RescheduleByClientRef(ctx, clientRef, newFireAt)
	}
	req := timerRequest{timer: timerRef{inst.InstanceID, action.TimerKey}, verb: verbTimerReschedule, traceID: traceID, sentAtMS: now}
	if err := actx.timerRequestSent(ctx, req, err); err != nil {
		return fmt.Errorf("reschedule_timer %q: %w", action.TimerKey, err)
	}
	return nil
}

func execSetVariable(ctx context.Context, action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) error {
	valueExpr, ok := action.Value.(string)
	if !ok || strings.TrimSpace(valueExpr) == "" {
		return fmt.Errorf("set_variable %q: value must be a non-empty string", action.Name)
	}

	// A value has a deterministic bound (wfcel.ValueCostLimit): past it the evaluation stops with an
	// error, which fails the action under its on_error.
	program, err := compileValue(valueExpr, actx.Clock)
	if err != nil {
		return fmt.Errorf("set_variable %q: compile value expression: %w", action.Name, err)
	}

	val, err := wfcel.EvalValue(program, inst.Input, inst.StateVars, messageToMap(event))
	if err != nil {
		return fmt.Errorf("set_variable %q: eval: %w", action.Name, err)
	}

	// The state holds plain JSON, the value the store writes and reads back: a CEL map literal is a
	// map[ref.Val]ref.Val, which panicked the node when the state was written (FINDINGS A-118).
	value, err := wfcel.ValueToJSON(val)
	if err != nil {
		return fmt.Errorf("set_variable %q: %w", action.Name, err)
	}
	inst.StateVars[action.Name] = value
	return nil
}

// timerScheduleOptions is how the WF schedules an instance's timer with SY.timer: the fire comes to
// this node, the client_ref names the instance and the key, and the payload carries both too.
func timerScheduleOptions(nodeName, instanceID, timerKey string) sdk.ScheduleOptions {
	return sdk.ScheduleOptions{
		TargetL2Name: nodeName,
		ClientRef:    timerClientRef(instanceID, timerKey),
		Payload: map[string]any{
			"instance_id": instanceID,
			"timer_key":   timerKey,
		},
		MissedPolicy: sdk.MissedPolicyFire,
	}
}

// instanceIDPrefix starts every instance id (and the thread of every send of an instance).
const instanceIDPrefix = "wfi:"

// newInstanceID generates a new workflow instance ID.
func newInstanceID() string {
	return instanceIDPrefix + uuid.NewString()
}

// SDKDispatcher wraps *sdk.NodeSender to implement Dispatcher.
type SDKDispatcher struct {
	sender *sdk.NodeSender
}

func NewSDKDispatcher(sender *sdk.NodeSender) *SDKDispatcher {
	return &SDKDispatcher{sender: sender}
}

func (d *SDKDispatcher) SendMsg(msg sdk.Message, note sdk.TraceNote) error {
	return d.sender.SendTraced(msg, note)
}

func (d *SDKDispatcher) NodeL2Name() string {
	return d.sender.FullName()
}

func (d *SDKDispatcher) NodeUUID() string {
	return d.sender.UUID()
}

// SDKTimerSender wraps *sdk.TimerClient to implement TimerSender.
type SDKTimerSender struct {
	sender     *sdk.NodeSender
	timerNode  string
	syncClient *sdk.TimerClient
}

// NewSDKTimerSender constructs a TimerSender backed by the shared
// `RouterDispatcher`. The dispatcher multiplexes TIMER_RESPONSE messages
// by trace_id, so the TimerClient and the wf-generic main loop can
// coexist on the same router connection without aliasing the receiver
// (the latent multiplexing bug noted in
// routerdispatcher_unification_plan.md §5).
func NewSDKTimerSender(dispatcher *sdk.RouterDispatcher, timerNode string) (*SDKTimerSender, error) {
	if dispatcher == nil {
		return nil, fmt.Errorf("dispatcher must be non-nil")
	}
	sender := dispatcher.SenderSnapshot()
	timerNode = strings.TrimSpace(timerNode)
	if timerNode == "" {
		_, hiveID, ok := strings.Cut(strings.TrimSpace(sender.FullName()), "@")
		if !ok || strings.TrimSpace(hiveID) == "" {
			return nil, fmt.Errorf("cannot derive local timer node name from sender %q", sender.FullName())
		}
		timerNode = "SY.timer@" + strings.TrimSpace(hiveID)
	}
	client, err := sdk.NewTimerClient(dispatcher, sdk.TimerClientConfig{
		TimerNode: timerNode,
	})
	if err != nil {
		return nil, err
	}
	return &SDKTimerSender{
		sender:     sender,
		timerNode:  timerNode,
		syncClient: client,
	}, nil
}

func (t *SDKTimerSender) ScheduleIn(ctx context.Context, d time.Duration, opts sdk.ScheduleOptions) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	if d < sdk.MinTimerDuration {
		return "", fmt.Errorf("minimum timer duration is %d seconds", int(sdk.MinTimerDuration.Seconds()))
	}
	payload := map[string]any{
		"fire_in_ms": d.Milliseconds(),
	}
	if err := applyScheduleOptions(payload, opts); err != nil {
		return "", err
	}
	return t.sendTimerRequest(verbTimerSchedule, payload)
}

func (t *SDKTimerSender) Schedule(ctx context.Context, fireAt time.Time, opts sdk.ScheduleOptions) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	if fireAt.IsZero() {
		return "", fmt.Errorf("fire_at must be non-zero")
	}
	if time.Until(fireAt) < sdk.MinTimerDuration {
		return "", fmt.Errorf("minimum timer duration is %d seconds", int(sdk.MinTimerDuration.Seconds()))
	}
	payload := map[string]any{
		"fire_at_utc_ms": fireAt.UTC().UnixMilli(),
	}
	if err := applyScheduleOptions(payload, opts); err != nil {
		return "", err
	}
	return t.sendTimerRequest(verbTimerSchedule, payload)
}

func (t *SDKTimerSender) CancelByClientRef(ctx context.Context, clientRef string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	clientRef = strings.TrimSpace(clientRef)
	if clientRef == "" {
		return fmt.Errorf("client_ref must be non-empty")
	}
	_, err := t.sendTimerRequest(verbTimerCancel, map[string]any{"client_ref": clientRef})
	return err
}

func (t *SDKTimerSender) CancelByClientRefConfirmed(ctx context.Context, clientRef string) error {
	return t.syncClient.CancelByClientRef(ctx, clientRef)
}

// RescheduleInByClientRef moves the timer under clientRef to fire d from when SY.timer gets the
// request (new_fire_in_ms), as ScheduleIn's fire_in_ms.
func (t *SDKTimerSender) RescheduleInByClientRef(ctx context.Context, clientRef string, d time.Duration) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	clientRef = strings.TrimSpace(clientRef)
	if clientRef == "" {
		return "", fmt.Errorf("client_ref must be non-empty")
	}
	if d < sdk.MinTimerDuration {
		return "", fmt.Errorf("minimum timer duration is %d seconds", int(sdk.MinTimerDuration.Seconds()))
	}
	return t.sendTimerRequest(verbTimerReschedule, map[string]any{
		"client_ref":     clientRef,
		"new_fire_in_ms": d.Milliseconds(),
	})
}

// RescheduleByClientRef moves the timer under clientRef to fire at newFireAt (new_fire_at_utc_ms):
// a fire_at's.
func (t *SDKTimerSender) RescheduleByClientRef(ctx context.Context, clientRef string, newFireAt time.Time) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	clientRef = strings.TrimSpace(clientRef)
	if clientRef == "" {
		return "", fmt.Errorf("client_ref must be non-empty")
	}
	if newFireAt.IsZero() {
		return "", fmt.Errorf("new_fire_at must be non-zero")
	}
	return t.sendTimerRequest(verbTimerReschedule, map[string]any{
		"client_ref":         clientRef,
		"new_fire_at_utc_ms": newFireAt.UTC().UnixMilli(),
	})
}

func (t *SDKTimerSender) List(ctx context.Context, filter sdk.ListFilter) ([]sdk.TimerInfo, error) {
	return t.syncClient.List(ctx, filter)
}

// sendTimerRequest sends verb to SY.timer without waiting for its answer, and returns the trace_id
// it went under: SY.timer's TIMER_RESPONSE, which the node takes from its timer-answers channel
// (wfRouteProfile), and the router's UNREACHABLE carry it back (FINDINGS A-254).
func (t *SDKTimerSender) sendTimerRequest(verb string, payload map[string]any) (string, error) {
	traceID := uuid.NewString()
	msg, err := sdk.BuildSystemRequest(
		t.sender.UUID(),
		t.timerNode,
		verb,
		payload,
		traceID,
		sdk.SystemEnvelopeOptions{},
	)
	if err != nil {
		return "", err
	}
	return traceID, t.sender.Send(msg)
}

func applyScheduleOptions(payload map[string]any, opts sdk.ScheduleOptions) error {
	if err := validateTimerMissedPolicy(opts.MissedPolicy, opts.MissedWithinMS); err != nil {
		return err
	}
	if target := strings.TrimSpace(opts.TargetL2Name); target != "" {
		payload["target_l2_name"] = target
	}
	if clientRef := strings.TrimSpace(opts.ClientRef); clientRef != "" {
		payload["client_ref"] = clientRef
	}
	if opts.Payload != nil {
		payload["payload"] = opts.Payload
	}
	if opts.Metadata != nil {
		payload["metadata"] = opts.Metadata
	}
	if opts.MissedPolicy != "" {
		payload["missed_policy"] = string(opts.MissedPolicy)
	}
	if opts.MissedWithinMS != nil {
		payload["missed_within_ms"] = *opts.MissedWithinMS
	}
	return nil
}

func validateTimerMissedPolicy(policy sdk.MissedPolicy, withinMS *int64) error {
	switch policy {
	case "":
		if withinMS != nil {
			return fmt.Errorf("missed_within_ms requires missed_policy=fire_if_within")
		}
	case sdk.MissedPolicyFire, sdk.MissedPolicyDrop:
		if withinMS != nil {
			return fmt.Errorf("missed_within_ms is only valid when missed_policy=fire_if_within")
		}
	case sdk.MissedPolicyFireIfWithin:
		if withinMS == nil || *withinMS <= 0 {
			return fmt.Errorf("missed_within_ms must be set when missed_policy=fire_if_within")
		}
	default:
		return fmt.Errorf("invalid missed_policy: %s", policy)
	}
	return nil
}
