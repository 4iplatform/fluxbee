package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
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

// Dispatcher abstracts sending L2 messages (testable without a real network).
type Dispatcher interface {
	SendMsg(msg sdk.Message) error
	NodeL2Name() string
	NodeUUID() string
}

// TimerSender abstracts SY.timer operations (testable without a real SY.timer).
type TimerSender interface {
	ScheduleIn(ctx context.Context, d time.Duration, opts sdk.ScheduleOptions) (sdk.TimerID, error)
	Schedule(ctx context.Context, fireAt time.Time, opts sdk.ScheduleOptions) (sdk.TimerID, error)
	CancelByClientRef(ctx context.Context, clientRef string) error
	CancelByClientRefConfirmed(ctx context.Context, clientRef string) error
	RescheduleByClientRef(ctx context.Context, clientRef string, newFireAt time.Time) error
	List(ctx context.Context, filter sdk.ListFilter) ([]sdk.TimerInfo, error)
}

// ActionContext bundles all dependencies needed to execute a workflow action.
type ActionContext struct {
	Store      *Store
	Dispatcher Dispatcher
	Timer      TimerSender
	Clock      ClockFunc
}

// executeAction executes a single action definition.
// Errors are returned for logging by the caller; they do NOT halt the transition.
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
// conversation and their answers come back here), with its meta.context and the WF's block.
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
	// rules as the payload, and the WF's block over it, a trail that starts at this node.
	metaContext := map[string]any{}
	if err := writeActionContext(metaContext, action, inst, eventMap); err != nil {
		return fmt.Errorf("send_message: %w", err)
	}
	contextRaw, err := withWFBlock(metaContext, []string{actx.Dispatcher.NodeL2Name()}, action, inst, eventMap)
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
	return sendWithRetry(ctx, actx, "send_message", msg)
}

// execForward sends a message on as it was received (wf-orchestration-v1 §5.2.1, item 5): the
// event's, or the one kept under action.Message. Its payload goes out byte for byte, never decoded,
// and its meta is the message's (type, msg, src_ilk, dst_ilk, ich, scope, priority and context)
// without memory_package. What the WF changes is metadata: the destination (a target written in
// the definition), the thread (the instance's), the context keys the action writes and the WF's
// block, whose trail is the message's own (or its sender's name) and this node.
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
	trail := append(messageTrail(source, metaContext), actx.Dispatcher.NodeL2Name())
	eventMap := messageToMap(event)
	if err := writeActionContext(metaContext, action, inst, eventMap); err != nil {
		return fmt.Errorf("forward: %w", err)
	}
	contextRaw, err := withWFBlock(metaContext, trail, action, inst, eventMap)
	if err != nil {
		return fmt.Errorf("forward: %w", err)
	}

	meta := source.Meta
	meta.MemoryPackage = nil
	threadID := inst.ThreadID
	meta.ThreadID = &threadID
	meta.Context = contextRaw
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
	return sendWithRetry(ctx, actx, "forward", msg)
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

// messageTrail is the trail a message brings: its own (meta.context.wf.trail, the nodes it went
// through), or its sender's name when it has none.
func messageTrail(msg sdk.Message, metaContext map[string]any) []string {
	if block, ok := metaContext[wfcel.ContextWF].(map[string]any); ok {
		if hops, ok := block["trail"].([]any); ok && len(hops) > 0 {
			trail := make([]string, 0, len(hops)+1)
			for _, hop := range hops {
				name, ok := hop.(string)
				if !ok {
					return []string{msg.SourceL2Name()}
				}
				trail = append(trail, name)
			}
			return trail
		}
	}
	return []string{msg.SourceL2Name()}
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
// item 7), as the message goes: trail, the nodes the message went through; vars, what the action's
// vars writes, $ref-resolved: the deterministic context the WF gives the receiver; and menu, the
// action's menu as written, what an agent may choose from (§5.3.6). vars and menu are there only
// when the action has them.
func withWFBlock(metaContext map[string]any, trail []string, action ActionDefinition, inst *WFInstance, eventMap map[string]any) (json.RawMessage, error) {
	hops := make([]any, len(trail))
	for i, name := range trail {
		hops[i] = name
	}
	block := map[string]any{"trail": hops}
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
	metaContext[wfcel.ContextWF] = block
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
func sendWithRetry(ctx context.Context, actx ActionContext, verb string, msg sdk.Message) error {
	var sendErr error
	for attempt := 0; attempt < sendMaxAttempts; attempt++ {
		if attempt > 0 {
			select {
			case <-ctx.Done():
				return fmt.Errorf("%s: cancelled during send retry: %w", verb, ctx.Err())
			case <-time.After(sendRetryBackoff):
			}
		}
		if sendErr = actx.Dispatcher.SendMsg(msg); sendErr == nil {
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
// that parses, or an RFC3339 time, 60 s ahead at least (SY.timer's floor). It returns when the timer
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
		if fireAt.Sub(now) < minTimerDuration {
			return time.Time{}, 0, fmt.Errorf("fire_at %s is less than 60s ahead", text)
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
	if in > 0 {
		_, err = actx.Timer.ScheduleIn(ctx, in, opts)
	} else {
		_, err = actx.Timer.Schedule(ctx, fireAt, opts)
	}
	if err != nil {
		return fmt.Errorf("schedule_timer %q: %w", action.TimerKey, err)
	}

	// Record in local timer index (fire-and-forget to SY.timer is already done above)
	return actx.Store.RegisterTimer(ctx, inst.InstanceID, action.TimerKey, nowMS(actx.Clock), fireAt.UnixMilli())
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
	newFireAt, _, err := timerTime(action, inst, event, actx)
	if err != nil {
		return fmt.Errorf("reschedule_timer %q: %w", action.TimerKey, err)
	}
	if err := actx.Timer.RescheduleByClientRef(ctx, clientRef, newFireAt); err != nil {
		return fmt.Errorf("reschedule_timer %q: %w", action.TimerKey, err)
	}

	return actx.Store.RegisterTimer(ctx, inst.InstanceID, action.TimerKey, nowMS(actx.Clock), newFireAt.UnixMilli())
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

func (d *SDKDispatcher) SendMsg(msg sdk.Message) error {
	return d.sender.Send(msg)
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

func (t *SDKTimerSender) ScheduleIn(ctx context.Context, d time.Duration, opts sdk.ScheduleOptions) (sdk.TimerID, error) {
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
	return "", t.sendTimerRequest("TIMER_SCHEDULE", payload)
}

func (t *SDKTimerSender) Schedule(ctx context.Context, fireAt time.Time, opts sdk.ScheduleOptions) (sdk.TimerID, error) {
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
	return "", t.sendTimerRequest("TIMER_SCHEDULE", payload)
}

func (t *SDKTimerSender) CancelByClientRef(ctx context.Context, clientRef string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	clientRef = strings.TrimSpace(clientRef)
	if clientRef == "" {
		return fmt.Errorf("client_ref must be non-empty")
	}
	return t.sendTimerRequest("TIMER_CANCEL", map[string]any{"client_ref": clientRef})
}

func (t *SDKTimerSender) CancelByClientRefConfirmed(ctx context.Context, clientRef string) error {
	return t.syncClient.CancelByClientRef(ctx, clientRef)
}

func (t *SDKTimerSender) RescheduleByClientRef(ctx context.Context, clientRef string, newFireAt time.Time) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	clientRef = strings.TrimSpace(clientRef)
	if clientRef == "" {
		return fmt.Errorf("client_ref must be non-empty")
	}
	if newFireAt.IsZero() {
		return fmt.Errorf("new_fire_at must be non-zero")
	}
	return t.sendTimerRequest("TIMER_RESCHEDULE", map[string]any{
		"client_ref":         clientRef,
		"new_fire_at_utc_ms": newFireAt.UTC().UnixMilli(),
	})
}

func (t *SDKTimerSender) List(ctx context.Context, filter sdk.ListFilter) ([]sdk.TimerInfo, error) {
	return t.syncClient.List(ctx, filter)
}

func (t *SDKTimerSender) sendTimerRequest(verb string, payload map[string]any) error {
	msg, err := sdk.BuildSystemRequest(
		t.sender.UUID(),
		t.timerNode,
		verb,
		payload,
		uuid.NewString(),
		sdk.SystemEnvelopeOptions{},
	)
	if err != nil {
		return err
	}
	return t.sender.Send(msg)
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
