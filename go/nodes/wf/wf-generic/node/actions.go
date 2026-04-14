package node

import (
	"context"
	"fmt"
	"log"
	"strings"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/cel-go/cel"
	"github.com/google/uuid"
)

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
func executeAction(ctx context.Context, path string, action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) error {
	switch action.Type {
	case "send_message":
		return execSendMessage(ctx, action, inst, event, actx)
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

func execSendMessage(ctx context.Context, action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) error {
	if actx.Dispatcher == nil {
		return fmt.Errorf("send_message: no dispatcher configured")
	}

	eventMap := messageToMap(event)
	resolvedPayload, err := Resolve(action.Payload, inst.Input, inst.StateVars, eventMap)
	if err != nil {
		return fmt.Errorf("send_message: resolve payload: %w", err)
	}

	raw, err := sdk.MarshalPayload(resolvedPayload)
	if err != nil {
		return fmt.Errorf("send_message: marshal payload: %w", err)
	}

	msgType := "system"
	if action.Meta != nil && action.Meta.Type != "" {
		msgType = action.Meta.Type
	}
	msgName := ""
	if action.Meta != nil {
		msgName = action.Meta.Msg
	}

	// thread_id = instance_id (enables response correlation)
	threadID := inst.InstanceID
	// trace_id = original event trace_id (idempotency on re-execution)
	traceID := inst.CurrentTraceID
	if traceID == "" {
		traceID = event.Routing.TraceID
	}

	src := actx.Dispatcher.NodeUUID()

	msgNameCopy := msgName
	threadIDCopy := threadID
	msg := sdk.Message{
		Routing: sdk.Routing{
			Src:     src,
			Dst:     sdk.UnicastDestination(action.Target),
			TTL:     16,
			TraceID: traceID,
		},
		Meta: sdk.Meta{
			MsgType:  msgType,
			Msg:      &msgNameCopy,
			ThreadID: &threadIDCopy,
		},
		Payload: raw,
	}

	return actx.Dispatcher.SendMsg(msg)
}

func execScheduleTimer(ctx context.Context, action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) error {
	if actx.Timer == nil {
		return fmt.Errorf("schedule_timer: no timer sender configured")
	}

	clientRef := timerClientRef(inst.InstanceID, action.TimerKey)
	timerPayload := map[string]any{
		"instance_id": inst.InstanceID,
		"timer_key":   action.TimerKey,
	}
	opts := sdk.ScheduleOptions{
		TargetL2Name: actx.Dispatcher.NodeL2Name(),
		ClientRef:    clientRef,
		Payload:      timerPayload,
		MissedPolicy: sdk.MissedPolicy(action.MissedPolicy),
	}
	if action.MissedPolicy == "" {
		opts.MissedPolicy = sdk.MissedPolicyFire
	}
	if action.MissedWithinMS != nil {
		opts.MissedWithinMS = action.MissedWithinMS
	}

	var fireAtMS int64

	if action.FireIn != "" {
		d, err := parseWorkflowDuration(action.FireIn)
		if err != nil {
			return fmt.Errorf("schedule_timer: parse fire_in %q: %w", action.FireIn, err)
		}
		if d < minTimerDuration {
			return fmt.Errorf("schedule_timer: fire_in %s is less than 60s minimum", action.FireIn)
		}
		fireAtMS = actx.Clock().Add(d).UnixMilli()
		if _, err := actx.Timer.ScheduleIn(ctx, d, opts); err != nil {
			return fmt.Errorf("schedule_timer %q: %w", action.TimerKey, err)
		}
	} else {
		fireAt, err := time.Parse(time.RFC3339, action.FireAt)
		if err != nil {
			return fmt.Errorf("schedule_timer: parse fire_at %q: %w", action.FireAt, err)
		}
		fireAtMS = fireAt.UnixMilli()
		if _, err := actx.Timer.Schedule(ctx, fireAt, opts); err != nil {
			return fmt.Errorf("schedule_timer %q: %w", action.TimerKey, err)
		}
	}

	// Record in local timer index (fire-and-forget to SY.timer is already done above)
	return actx.Store.RegisterTimer(ctx, inst.InstanceID, action.TimerKey, nowMS(actx.Clock), fireAtMS)
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

	var newFireAtMS int64
	if action.FireIn != "" {
		d, err := parseWorkflowDuration(action.FireIn)
		if err != nil {
			return fmt.Errorf("reschedule_timer: parse fire_in: %w", err)
		}
		newFireAt := actx.Clock().Add(d)
		newFireAtMS = newFireAt.UnixMilli()
		if err := actx.Timer.RescheduleByClientRef(ctx, clientRef, newFireAt); err != nil {
			return fmt.Errorf("reschedule_timer %q: %w", action.TimerKey, err)
		}
	} else {
		newFireAt, err := time.Parse(time.RFC3339, action.FireAt)
		if err != nil {
			return fmt.Errorf("reschedule_timer: parse fire_at: %w", err)
		}
		newFireAtMS = newFireAt.UnixMilli()
		if err := actx.Timer.RescheduleByClientRef(ctx, clientRef, newFireAt); err != nil {
			return fmt.Errorf("reschedule_timer %q: %w", action.TimerKey, err)
		}
	}

	return actx.Store.RegisterTimer(ctx, inst.InstanceID, action.TimerKey, nowMS(actx.Clock), newFireAtMS)
}

func execSetVariable(ctx context.Context, action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) error {
	valueExpr, ok := action.Value.(string)
	if !ok || strings.TrimSpace(valueExpr) == "" {
		return fmt.Errorf("set_variable %q: value must be a non-empty string", action.Name)
	}

	program, err := compileGuard(valueExpr, actx.Clock)
	if err != nil {
		return fmt.Errorf("set_variable %q: compile value expression: %w", action.Name, err)
	}

	eventMap := messageToMap(event)
	val, _, err := program.Eval(map[string]any{
		"input": inst.Input,
		"state": inst.StateVars,
		"event": eventMap,
	})
	if err != nil {
		return fmt.Errorf("set_variable %q: eval: %w", action.Name, err)
	}

	inst.StateVars[action.Name] = val.Value()
	return nil
}

// newInstanceID generates a new workflow instance ID.
func newInstanceID() string {
	return "wfi:" + uuid.NewString()
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

func NewSDKTimerSender(sender *sdk.NodeSender, receiver *sdk.NodeReceiver, timerNode string) (*SDKTimerSender, error) {
	timerNode = strings.TrimSpace(timerNode)
	if timerNode == "" {
		_, hiveID, ok := strings.Cut(strings.TrimSpace(sender.FullName()), "@")
		if !ok || strings.TrimSpace(hiveID) == "" {
			return nil, fmt.Errorf("cannot derive local timer node name from sender %q", sender.FullName())
		}
		timerNode = "SY.timer@" + strings.TrimSpace(hiveID)
	}
	client, err := sdk.NewTimerClient(sender, receiver, sdk.TimerClientConfig{
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

// compileValueExpr is a thin alias used in set_variable; reuses compileGuard
// since both compile CEL expressions in the same environment.
var _ = cel.Program(nil) // keep cel import alive
