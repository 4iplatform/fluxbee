package node

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"sync"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// InstanceRegistry is a thread-safe in-memory map of running instances.
type InstanceRegistry struct {
	mu        sync.RWMutex
	instances map[string]*WFInstance
}

func NewInstanceRegistry() *InstanceRegistry {
	return &InstanceRegistry{instances: make(map[string]*WFInstance)}
}

func (r *InstanceRegistry) Add(inst *WFInstance) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.instances[inst.InstanceID] = inst
}

func (r *InstanceRegistry) Get(instanceID string) (*WFInstance, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	inst, ok := r.instances[instanceID]
	return inst, ok
}

func (r *InstanceRegistry) Remove(instanceID string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	delete(r.instances, instanceID)
}

func (r *InstanceRegistry) Count() int {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return len(r.instances)
}

// CorrelateAndDispatch routes an inbound message to the correct instance
// or triggers new instance creation. It implements the strict correlation order:
//
//  1. TIMER_FIRED + instance_id in payload → route to that instance
//  2. meta.thread_id matches an existing instance → route to it
//  3. Otherwise → attempt new instance creation
func CorrelateAndDispatch(ctx context.Context, msg sdk.Message, reg *InstanceRegistry, def *WorkflowDefinition, store *Store, actx ActionContext) error {
	msgName := ""
	if msg.Meta.Msg != nil {
		msgName = *msg.Meta.Msg
	}

	// 1. TIMER_FIRED: correlate by instance_id in timer payload
	if msgName == sdk.MsgTimerFired {
		instanceID, err := extractTimerInstanceID(msg)
		if err != nil {
			log.Printf("correlate: TIMER_FIRED parse error: %v", err)
			return nil
		}
		inst, ok := reg.Get(instanceID)
		if !ok {
			log.Printf("correlate: TIMER_FIRED for unknown instance %q (orphaned timer)", instanceID)
			return nil
		}
		return dispatchToInstance(ctx, inst, msg, reg, actx)
	}

	// 2. thread_id matches a running instance
	if msg.Meta.ThreadID != nil && *msg.Meta.ThreadID != "" {
		threadID := *msg.Meta.ThreadID
		if inst, ok := reg.Get(threadID); ok {
			return dispatchToInstance(ctx, inst, msg, reg, actx)
		}
	}

	// 3. New instance trigger
	return createAndDispatch(ctx, msg, reg, def, store, actx)
}

// dispatchToInstance delivers the event to the instance under its mutex.
func dispatchToInstance(ctx context.Context, inst *WFInstance, msg sdk.Message, reg *InstanceRegistry, actx ActionContext) error {
	inst.Lock()
	defer inst.Unlock()

	if err := inst.RunTransition(ctx, msg, actx); err != nil {
		return fmt.Errorf("instance %s: run transition: %w", inst.InstanceID, err)
	}

	// If the instance reached a terminal state, remove from the active registry
	if inst.Status != "running" && inst.Status != "cancelling" {
		reg.Remove(inst.InstanceID)
	}
	return nil
}

// createAndDispatch validates the input, creates a new instance, and runs its
// initial entry_actions.
func createAndDispatch(ctx context.Context, msg sdk.Message, reg *InstanceRegistry, def *WorkflowDefinition, store *Store, actx ActionContext) error {
	input := map[string]any{}
	if len(msg.Payload) > 0 {
		if err := json.Unmarshal(msg.Payload, &input); err != nil {
			return sendErrorResponse(ctx, msg, "INVALID_INPUT", "payload is not a JSON object", actx)
		}
	}
	if err := validateInputPayload(def, input); err != nil {
		return sendErrorResponse(ctx, msg, "INVALID_INPUT", err.Error(), actx)
	}

	instanceID := newInstanceID()
	inst := NewInstance(def, instanceID, def.WorkflowType, input, actx.Clock)

	// Persist before running entry_actions (act-then-persist model: trace_id first)
	inst.CurrentTraceID = msg.Routing.TraceID
	row, err := inst.ToRow(actx.Clock)
	if err != nil {
		return fmt.Errorf("create instance: serialize: %w", err)
	}
	if err := store.CreateInstance(ctx, row); err != nil {
		return fmt.Errorf("create instance: store: %w", err)
	}

	reg.Add(inst)

	inst.Lock()
	defer inst.Unlock()

	// Run initial state entry_actions
	initialState := inst.findState(def.InitialState)
	if initialState != nil {
		inst.executeActions(ctx, initialState.EntryActions, msg, actx)
	}

	// Persist updated state (clears trace_id, records any state_json changes)
	inst.CurrentTraceID = ""
	if err := store.UpdateInstance(ctx, mustToRow(inst, actx.Clock)); err != nil {
		log.Printf("instance %s: persist after creation: %v", instanceID, err)
	}

	return nil
}

// extractTimerInstanceID parses the instance_id from a TIMER_FIRED message.
// The WF sets the timer payload to {"instance_id": "...", "timer_key": "..."}.
func extractTimerInstanceID(msg sdk.Message) (string, error) {
	var fired sdk.FiredEvent
	if err := json.Unmarshal(msg.Payload, &fired); err != nil {
		return "", fmt.Errorf("parse FiredEvent: %w", err)
	}
	if fired.ClientRef != nil && *fired.ClientRef != "" {
		instanceID, _, err := parseTimerClientRef(*fired.ClientRef)
		if err == nil {
			return instanceID, nil
		}
	}
	// Fallback: parse from user_payload
	var up struct {
		InstanceID string `json:"instance_id"`
	}
	if err := json.Unmarshal(fired.UserPayload, &up); err == nil && up.InstanceID != "" {
		return up.InstanceID, nil
	}
	return "", fmt.Errorf("cannot determine instance_id from TIMER_FIRED payload")
}

// sendErrorResponse sends an error message back to the sender of msg.
func sendErrorResponse(ctx context.Context, msg sdk.Message, code, detail string, actx ActionContext) error {
	if actx.Dispatcher == nil {
		return nil
	}
	replyTo := msg.Routing.Src
	if replyTo == "" {
		return nil
	}
	payload, _ := sdk.MarshalPayload(map[string]any{
		"ok":     false,
		"error":  code,
		"detail": detail,
	})
	msgName := code
	resp := sdk.Message{
		Routing: sdk.Routing{
			Src:     actx.Dispatcher.NodeUUID(),
			Dst:     sdk.UnicastDestination(replyTo),
			TTL:     16,
			TraceID: msg.Routing.TraceID,
		},
		Meta: sdk.Meta{
			MsgType: "system",
			Msg:     &msgName,
		},
		Payload: payload,
	}
	return actx.Dispatcher.SendMsg(resp)
}
